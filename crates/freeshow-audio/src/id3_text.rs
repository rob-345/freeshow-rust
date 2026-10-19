//! Narrow private read-view adaptation after structural ID3 preflight. Lofty
//! parses timestamps as dates, while music-metadata preserves their text.
use crate::{AudioConfig, AudioError};
use std::io::{self, Read, Seek, SeekFrom};

#[derive(Default)]
pub(crate) struct Plan {
    patches: Vec<Patch>,
}
struct Patch {
    offset: u64,
    alias: [u8; 4],
    original: [u8; 4],
}
fn timestamp(id: &[u8]) -> bool {
    matches!(id, b"TDEN" | b"TDOR" | b"TDRC" | b"TDRL" | b"TDTG")
}
// Only called after the original preflight validates every span and flag.
fn rows(bytes: &[u8], version: u8) -> impl Iterator<Item = (usize, &[u8])> {
    let mut position = 0usize;
    std::iter::from_fn(move || {
        let start = position;
        let header = bytes.get(start..start.checked_add(10)?)?;
        if header[0] == 0 {
            return None;
        }
        let length = if version == 4 {
            header[4..8]
                .iter()
                .fold(0, |value, byte| (value << 7) | usize::from(*byte))
        } else {
            u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize
        };
        position = start.checked_add(10)?.checked_add(length)?;
        bytes.get(start..position)?;
        Some((start, header))
    })
}
impl Plan {
    pub(crate) fn leading(
        header: &[u8],
        bytes: &[u8],
        config: &AudioConfig,
    ) -> Result<Self, AudioError> {
        let count = rows(bytes, header[3]).count();
        let recover = |frame: &[u8]| timestamp(&frame[..4]) && frame[8..10] == [0, 0];
        let adapted = rows(bytes, header[3])
            .filter(|(_, frame)| recover(frame))
            .count();
        if adapted == 0 {
            return Ok(Self::default());
        }
        // The pinned source treats encoding=2 without a BOM as UTF-16LE,
        // unlike Lofty's UTF-16BE decoder. Do not publish silently different
        // recovered timestamp values through this narrow text-only adapter.
        for (offset, _) in rows(bytes, header[3]).filter(|(_, frame)| recover(frame)) {
            if bytes.get(offset + 10) == Some(&2) {
                return Err(AudioError::UnsupportedVariant(
                    "ID3 timestamp UTF-16BE source decoding".into(),
                ));
            }
        }
        let allocation = count
            .checked_mul(std::mem::size_of::<[u8; 4]>())
            .and_then(|n| {
                adapted
                    .checked_mul(std::mem::size_of::<Patch>())
                    .and_then(|extra| n.checked_add(extra))
            })
            .ok_or_else(|| AudioError::Resource("ID3 text-view plan".into()))?;
        if count > config.tag_limit || allocation > config.allocation_limit {
            return Err(AudioError::Resource("ID3 text-view plan".into()));
        }
        let mut occupied = Vec::with_capacity(count);
        for (_, frame) in rows(bytes, header[3]) {
            occupied.push(<[u8; 4]>::try_from(&frame[..4]).unwrap());
        }
        occupied.sort_unstable();
        let mut patches = Vec::with_capacity(adapted);
        let mut next = 0u32;
        for (offset, frame) in rows(bytes, header[3]).filter(|(_, frame)| recover(frame)) {
            let alias = loop {
                // Bounded by original+adapted frame counts plus the fixed
                // reserved list (each count <=16384; namespace size 36^3).
                if next >= 36 * 36 * 36 {
                    return Err(AudioError::Resource("ID3 text-view aliases".into()));
                }
                let alphabet = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
                let alias = [
                    b'T',
                    alphabet[(next / (36 * 36)) as usize],
                    alphabet[((next / 36) % 36) as usize],
                    alphabet[(next % 36) as usize],
                ];
                next += 1;
                if !timestamp(&alias)
                    && !matches!(
                        &alias,
                        b"TXXX" | b"TIPL" | b"TMCL" | b"TYER" | b"TDAT" | b"TIME" | b"TORY"
                    )
                    && occupied.binary_search(&alias).is_err()
                {
                    break alias;
                }
            };
            patches.push(Patch {
                offset: (10 + offset) as u64,
                alias,
                original: frame[..4].try_into().unwrap(),
            });
        }
        Ok(Self { patches })
    }
    pub(crate) fn active(&self) -> bool {
        !self.patches.is_empty()
    }
    pub(crate) fn original<'a>(&'a self, id: &'a str) -> &'a str {
        self.patches
            .binary_search_by(|patch| patch.alias.as_slice().cmp(id.as_bytes()))
            .ok()
            .map(|index| std::str::from_utf8(&self.patches[index].original).unwrap())
            .unwrap_or(id)
    }
    pub(crate) fn view<'a, R>(&'a self, reader: &'a mut R) -> View<'a, R> {
        View {
            reader,
            plan: self,
            position: 0,
        }
    }
}
pub(crate) struct View<'a, R> {
    reader: &'a mut R,
    plan: &'a Plan,
    position: u64,
}
impl<R: Read> Read for View<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        // Every real read still crosses Budget, including cancellation/deadline.
        let count = self.reader.read(buffer)?;
        let end = self
            .position
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("ID3 text-view offset"))?;
        let start = self
            .plan
            .patches
            .partition_point(|patch| patch.offset + 4 <= self.position);
        for patch in &self.plan.patches[start..] {
            if patch.offset >= end {
                break;
            }
            for index in 0..4 {
                let offset = patch.offset + index;
                if offset >= self.position && offset < end {
                    buffer[(offset - self.position) as usize] = patch.alias[index as usize];
                }
            }
        }
        self.position = end;
        Ok(count)
    }
}
impl<R: Seek> Seek for View<'_, R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.position = self.reader.seek(position)?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    fn tag() -> ([u8; 10], Vec<u8>) {
        let mut bytes = Vec::new();
        for id in [b"T000", b"TDRC", b"T001", b"TDRC"] {
            bytes.extend(id);
            bytes.extend([0, 0, 0, 2, 0, 0, 3, b'x']);
        }
        let mut header = *b"ID3\x04\0\0\0\0\0\0";
        header[9] = bytes.len() as u8;
        (header, bytes)
    }
    #[test]
    fn occupied_ids_duplicates_split_reads_and_seeks_keep_original_bytes_unchanged() {
        let (header, bytes) = tag();
        let plan = Plan::leading(&header, &bytes, &AudioConfig::default()).unwrap();
        assert_eq!(plan.patches.len(), 2);
        assert_eq!(plan.patches[0].alias, *b"T002");
        assert_eq!(plan.patches[1].alias, *b"T003");
        assert_eq!(plan.original("T002"), "TDRC");
        assert_eq!(plan.original("T003"), "TDRC");
        assert_eq!(plan.original("T000"), "T000");
        let original = [header.as_slice(), &bytes].concat();
        let mut source = Cursor::new(original.clone());
        let mut view = plan.view(&mut source);
        for position in 0..original.len() {
            view.seek(SeekFrom::Start(position as u64)).unwrap();
            let mut part = [0; 3];
            let count = view.read(&mut part).unwrap();
            for (index, byte) in part[..count].iter().enumerate() {
                let offset = (position + index) as u64;
                let expected = plan
                    .patches
                    .iter()
                    .find(|p| (p.offset..p.offset + 4).contains(&offset))
                    .map(|p| p.alias[(offset - p.offset) as usize])
                    .unwrap_or(original[position + index]);
                assert_eq!(*byte, expected);
            }
        }
        assert_eq!(source.into_inner(), original);
    }
    #[test]
    fn plan_allocation_and_count_reject_before_reserving_tables_and_flags_remain_unmodified() {
        let (header, mut bytes) = tag();
        let tiny = AudioConfig {
            allocation_limit: 1,
            ..AudioConfig::default()
        };
        assert!(matches!(
            Plan::leading(&header, &bytes, &tiny),
            Err(AudioError::Resource(_))
        ));
        let few = AudioConfig {
            tag_limit: 1,
            ..AudioConfig::default()
        };
        assert!(matches!(
            Plan::leading(&header, &bytes, &few),
            Err(AudioError::Resource(_))
        ));
        bytes[12 + 8] = 0x40;
        bytes[36 + 8] = 0x40;
        assert!(!Plan::leading(&header, &bytes, &AudioConfig::default())
            .unwrap()
            .active());
    }
    #[test]
    fn compatibility_view_cannot_bypass_actual_read_operation_or_cancel_budgets() {
        let (header, bytes) = tag();
        let plan = Plan::leading(&header, &bytes, &AudioConfig::default()).unwrap();
        let config = AudioConfig {
            read_limit: 3,
            ..AudioConfig::default()
        };
        let token = tokio_util::sync::CancellationToken::new();
        let mut source = crate::parser::Budget::new(
            Cursor::new([header.as_slice(), &bytes].concat()),
            &config,
            token.clone(),
        );
        let mut view = plan.view(&mut source);
        assert_eq!(view.read(&mut [0; 8]).unwrap(), 3);
        assert!(view.read(&mut [0; 1]).is_err());
        assert!(source.exceeded);
        let config = AudioConfig {
            operation_limit: 1,
            ..AudioConfig::default()
        };
        let mut source = crate::parser::Budget::new(Cursor::new(bytes), &config, token.clone());
        let mut view = plan.view(&mut source);
        view.seek(SeekFrom::Start(0)).unwrap();
        assert!(view.seek(SeekFrom::Start(0)).is_err());
        token.cancel();
        assert!(view.read(&mut [0; 1]).is_err());
        assert!(source.cancelled);
    }
}

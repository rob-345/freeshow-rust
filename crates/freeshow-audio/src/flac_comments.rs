//! Retain source physical FLAC comment order from the existing guarded pass.
use crate::{common::Collector, AudioConfig, AudioError};
#[derive(Default)]
pub(crate) struct Plan {
    entries: Vec<(String, String)>,
    charge: usize,
}
impl Plan {
    // `count` and every span were structurally validated by preflight::comments
    // before this method. It does not read a File or parse an image.
    pub(crate) fn append(
        &mut self,
        bytes: &[u8],
        count: usize,
        mut position: usize,
        config: &AudioConfig,
    ) -> Result<(), AudioError> {
        let resource = || AudioError::Resource("aggregate FLAC comment plan".into());
        let total = self.entries.len().checked_add(count).ok_or_else(resource)?;
        if total > config.tag_limit {
            return Err(resource());
        }
        for _ in 0..count {
            let length =
                u32::from_le_bytes(bytes[position..position + 4].try_into().unwrap()) as usize;
            position += 4;
            let field = std::str::from_utf8(&bytes[position..position + length]).map_err(|_| {
                AudioError::UnsupportedVariant("FLAC comment UTF8 replacement decoding".into())
            })?;
            position += length;
            let (key, value) = field.split_once('=').ok_or_else(|| {
                AudioError::UnsupportedVariant("FLAC comment without separator".into())
            })?;
            if key.is_empty() || !key.bytes().all(|byte| (0x20..=0x7d).contains(&byte)) {
                return Err(AudioError::UnsupportedVariant(
                    "FLAC noncanonical comment key".into(),
                ));
            }
            // Double each table entry plus two initial slots covers Vec's
            // minimum four-entry allocation/growth. ASCII keys cannot expand.
            let charge = key
                .len()
                .checked_add(value.len())
                .and_then(|n| {
                    n.checked_add(
                        (if self.entries.is_empty() { 4 } else { 2 })
                            * std::mem::size_of::<(String, String)>(),
                    )
                })
                .and_then(|n| self.charge.checked_add(n))
                .ok_or_else(resource)?;
            if charge > config.allocation_limit {
                return Err(resource());
            }
            self.entries
                .push((key.to_ascii_uppercase(), value.to_owned()));
            self.charge = charge;
        }
        Ok(())
    }
    pub(crate) fn collect(self, collector: &mut Collector) {
        for (key, value) in self.entries {
            collector.add("vorbis", &key, crate::js::Js::String(value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{js::Js, parser::Budget};
    use lofty::file::FileType;
    use std::io::Cursor;
    use tokio_util::sync::CancellationToken;
    fn block(fields: &[&str]) -> Vec<u8> {
        let mut out = vec![0; 4]; // empty vendor
        out.extend((fields.len() as u32).to_le_bytes());
        for value in fields {
            out.extend((value.len() as u32).to_le_bytes());
            out.extend(value.as_bytes());
        }
        out
    }
    fn container(blocks: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = b"fLaC".to_vec();
        for (i, body) in blocks.iter().enumerate() {
            bytes.push(4 | if i == blocks.len() - 1 { 128 } else { 0 });
            bytes.extend(&((body.len() as u32).to_be_bytes())[1..]);
            bytes.extend(body);
        }
        bytes
    }
    fn plan(blocks: &[Vec<u8>], config: &AudioConfig) -> Result<Plan, AudioError> {
        crate::preflight::format(&mut Cursor::new(container(blocks)), FileType::Flac, config)
    }
    #[test]
    fn physical_blocks_items_and_original_collector_priority_are_retained() {
        let p = plan(
            &[
                block(&["TITLE=earlier", "ARTIST=A", "DATE=invalid"]),
                block(&["title=later", "ARTIST=B", "ARTIST=A", "ALBUM=last"]),
            ],
            &AudioConfig::default(),
        )
        .unwrap();
        assert_eq!(p.entries.len(), 7);
        let mut collector = Collector::new(32);
        collector.add("id3", "TIT2", "id3".into());
        collector.add("id3", "TALB", "id3 album".into());
        p.collect(&mut collector);
        assert!(!collector.exceeded);
        assert_eq!(collector.common.get("title").string(), "later");
        assert_eq!(collector.common.get("album").string(), "last");
        assert_eq!(collector.common.get("date").string(), "invalid");
        assert!(matches!(collector.common.get("year"), Js::Undefined));
        assert_eq!(
            collector.common.get("artists").json(),
            serde_json::json!(["A", "B"])
        );
    }
    #[test]
    fn aggregate_counts_and_owned_bytes_reject_across_separate_small_blocks() {
        let few = AudioConfig {
            tag_limit: 1,
            ..AudioConfig::default()
        };
        assert!(matches!(
            plan(&[block(&["TITLE=one"]), block(&["ALBUM=two"])], &few),
            Err(AudioError::Resource(_))
        ));
        let small = AudioConfig {
            allocation_limit: 512,
            ..AudioConfig::default()
        };
        let field = format!("UNKNOWN={}", "x".repeat(200));
        assert!(plan(&[block(&[&field])], &small).is_ok());
        assert!(matches!(
            plan(&[block(&[&field]), block(&[&field])], &small),
            Err(AudioError::Resource(_))
        ));
    }
    #[test]
    fn structurally_bad_or_huge_late_blocks_never_yield_a_partial_plan() {
        let mut bad = block(&["ALBUM=late"]);
        bad[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            plan(&[block(&["TITLE=early"]), bad], &AudioConfig::default()),
            Err(AudioError::Resource(_))
        ));
        let mut truncated = block(&["ALBUM=late"]);
        truncated.pop();
        assert!(matches!(
            plan(
                &[block(&["TITLE=early"]), truncated],
                &AudioConfig::default()
            ),
            Err(AudioError::Input)
        ));
    }
    #[test]
    fn unsupported_source_key_and_utf8_variants_are_not_silently_reinterpreted() {
        for field in ["NO_SEPARATOR", "=value", "TÍTLE=value", "TITLE\0=value"] {
            assert!(matches!(
                plan(&[block(&[field])], &AudioConfig::default()),
                Err(AudioError::UnsupportedVariant(_))
            ));
        }
        let mut invalid = block(&["TITLE=x"]);
        *invalid.last_mut().unwrap() = 0xff;
        assert!(matches!(
            plan(&[invalid], &AudioConfig::default()),
            Err(AudioError::UnsupportedVariant(_))
        ));
    }
    #[test]
    fn reads_operations_and_cancellation_remain_on_the_original_budget() {
        let bytes = container(&[block(&["TITLE=early"]), block(&["ALBUM=late"])]);
        let config = AudioConfig {
            read_limit: 12,
            ..AudioConfig::default()
        };
        let mut reader = Budget::new(
            Cursor::new(bytes.clone()),
            &config,
            CancellationToken::new(),
        );
        assert!(crate::preflight::format(&mut reader, FileType::Flac, &config).is_err());
        assert!(reader.exceeded);
        assert_eq!(reader.bytes, 12);
        let config = AudioConfig {
            operation_limit: 3,
            ..AudioConfig::default()
        };
        let mut reader = Budget::new(
            Cursor::new(bytes.clone()),
            &config,
            CancellationToken::new(),
        );
        assert!(crate::preflight::format(&mut reader, FileType::Flac, &config).is_err());
        assert!(reader.exceeded);
        let token = CancellationToken::new();
        token.cancel();
        let config = AudioConfig::default();
        let mut reader = Budget::new(Cursor::new(bytes), &config, token);
        assert!(crate::preflight::format(&mut reader, FileType::Flac, &config).is_err());
        assert!(reader.cancelled);
    }
}

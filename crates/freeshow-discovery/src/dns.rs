use crate::{DiscoveryError, Result};
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, Ipv6Addr};
const MAX_PACKET: usize = 9000;
fn invalid(message: &str) -> DiscoveryError {
    DiscoveryError::new("invalid_discovery_packet", message)
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RecordType {
    A,
    PTR,
    TXT,
    AAAA,
    SRV,
    ANY,
    Unknown(u16),
}
impl RecordType {
    fn code(self) -> u16 {
        match self {
            Self::A => 1,
            Self::PTR => 12,
            Self::TXT => 16,
            Self::AAAA => 28,
            Self::SRV => 33,
            Self::ANY => 255,
            Self::Unknown(n) => n,
        }
    }
    fn from(n: u16) -> Self {
        match n {
            1 => Self::A,
            12 => Self::PTR,
            16 => Self::TXT,
            28 => Self::AAAA,
            33 => Self::SRV,
            255 => Self::ANY,
            n => Self::Unknown(n),
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RecordData {
    Name(String),
    Srv {
        port: u16,
        target: String,
        priority: u16,
        weight: u16,
    },
    Txt(Vec<Vec<u8>>),
    A(Ipv4Addr),
    Aaaa(Ipv6Addr),
    Unknown(Vec<u8>),
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub name: String,
    pub kind: RecordType,
    pub ttl: u32,
    pub data: RecordData,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub name: String,
    pub kind: RecordType,
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Packet {
    pub response: bool,
    pub questions: Vec<Question>,
    pub answers: Vec<Record>,
    pub authorities: Vec<Record>,
    pub additionals: Vec<Record>,
}
pub(crate) fn validate_name(name: &str) -> Result<()> {
    let name = name.strip_suffix('.').unwrap_or(name);
    if name.is_empty()
        || name.len() > 253
        || name
            .split('.')
            .any(|s| s.is_empty() || s.len() > 63 || s.contains('\0'))
    {
        return Err(invalid("Invalid or oversized DNS name"));
    }
    Ok(())
}
fn take<'a>(bytes: &'a [u8], offset: &mut usize, n: usize) -> Result<&'a [u8]> {
    let end = offset
        .checked_add(n)
        .ok_or_else(|| invalid("DNS offset overflow"))?;
    let result = bytes
        .get(*offset..end)
        .ok_or_else(|| invalid("Truncated DNS packet"))?;
    *offset = end;
    Ok(result)
}
fn u16_read(bytes: &[u8], offset: &mut usize) -> Result<u16> {
    Ok(u16::from_be_bytes(
        take(bytes, offset, 2)?.try_into().unwrap(),
    ))
}
fn name_read(bytes: &[u8], offset: &mut usize) -> Result<String> {
    let mut cursor = *offset;
    let mut jumped = false;
    let mut jumps = 0;
    let mut output = Vec::new();
    loop {
        let length = *bytes
            .get(cursor)
            .ok_or_else(|| invalid("Truncated DNS name"))?;
        cursor += 1;
        if length & 0xc0 == 0xc0 {
            let low = *bytes
                .get(cursor)
                .ok_or_else(|| invalid("Truncated DNS pointer"))?;
            cursor += 1;
            if !jumped {
                *offset = cursor;
                jumped = true;
            }
            cursor = (((length & 0x3f) as usize) << 8) | low as usize;
            jumps += 1;
            if jumps > 32 {
                return Err(invalid("DNS compression loop or jump limit"));
            }
            continue;
        }
        if length & 0xc0 != 0 {
            return Err(invalid("Unsupported DNS label tag"));
        }
        if length == 0 {
            if !jumped {
                *offset = cursor;
            }
            break;
        }
        if !output.is_empty() {
            output.push(b'.');
        }
        output.extend_from_slice(take(bytes, &mut cursor, length as usize)?);
        if output.len() > 253 {
            return Err(invalid("DNS expanded name exceeds limit"));
        }
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}
fn record_read(bytes: &[u8], offset: &mut usize) -> Result<Record> {
    let name = name_read(bytes, offset)?;
    let kind = RecordType::from(u16_read(bytes, offset)?);
    let _class = u16_read(bytes, offset)?;
    let ttl = u32::from_be_bytes(take(bytes, offset, 4)?.try_into().unwrap());
    let length = u16_read(bytes, offset)? as usize;
    let start = *offset;
    let end = start
        .checked_add(length)
        .ok_or_else(|| invalid("DNS record length overflow"))?;
    if end > bytes.len() {
        return Err(invalid("Truncated DNS record"));
    }
    let data = match kind {
        RecordType::A if length == 4 => RecordData::A(Ipv4Addr::from(
            <[u8; 4]>::try_from(take(bytes, offset, 4)?).unwrap(),
        )),
        RecordType::AAAA if length == 16 => RecordData::Aaaa(Ipv6Addr::from(
            <[u8; 16]>::try_from(take(bytes, offset, 16)?).unwrap(),
        )),
        RecordType::PTR => RecordData::Name(name_read(bytes, offset)?),
        RecordType::SRV => {
            let priority = u16_read(bytes, offset)?;
            let weight = u16_read(bytes, offset)?;
            let port = u16_read(bytes, offset)?;
            let target = name_read(bytes, offset)?;
            RecordData::Srv {
                priority,
                weight,
                port,
                target,
            }
        }
        RecordType::TXT => {
            let mut parts = Vec::new();
            while *offset < end {
                let n = take(bytes, offset, 1)?[0] as usize;
                let part = take(bytes, offset, n)?;
                if *offset > end {
                    return Err(invalid("TXT exceeds record boundary"));
                }
                parts.push(part.to_vec());
            }
            RecordData::Txt(parts)
        }
        _ => RecordData::Unknown(take(bytes, offset, length)?.to_vec()),
    };
    if *offset != end {
        return Err(invalid("DNS record boundary mismatch"));
    }
    Ok(Record {
        name,
        kind,
        ttl,
        data,
    })
}
pub fn decode(bytes: &[u8]) -> Result<Packet> {
    if bytes.len() > MAX_PACKET {
        return Err(invalid("DNS packet exceeds 9000 bytes"));
    }
    let mut offset = 0;
    let _id = u16_read(bytes, &mut offset)?;
    let flags = u16_read(bytes, &mut offset)?;
    let q = u16_read(bytes, &mut offset)? as usize;
    let a = u16_read(bytes, &mut offset)? as usize;
    let ns = u16_read(bytes, &mut offset)? as usize;
    let ar = u16_read(bytes, &mut offset)? as usize;
    if q > 16 || a + ns + ar > 64 {
        return Err(invalid("DNS question/record count limit"));
    }
    let mut output = Packet {
        response: flags & 0x8000 != 0,
        ..Packet::default()
    };
    for _ in 0..q {
        let name = name_read(bytes, &mut offset)?;
        let kind = RecordType::from(u16_read(bytes, &mut offset)?);
        let _class = u16_read(bytes, &mut offset)?;
        output.questions.push(Question { name, kind });
    }
    for (count, records) in [
        (a, &mut output.answers),
        (ns, &mut output.authorities),
        (ar, &mut output.additionals),
    ] {
        for _ in 0..count {
            records.push(record_read(bytes, &mut offset)?);
        }
    }
    if offset != bytes.len() {
        return Err(invalid("Trailing DNS packet bytes"));
    }
    Ok(output)
}
fn name_write(name: &str, bytes: &mut Vec<u8>) -> Result<()> {
    validate_name(name)?;
    for part in name.trim_end_matches('.').split('.') {
        bytes.push(part.len() as u8);
        bytes.extend_from_slice(part.as_bytes());
    }
    bytes.push(0);
    Ok(())
}
fn record_write(record: &Record, bytes: &mut Vec<u8>) -> Result<()> {
    name_write(&record.name, bytes)?;
    bytes.extend_from_slice(&record.kind.code().to_be_bytes());
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&record.ttl.to_be_bytes());
    let size = bytes.len();
    bytes.extend_from_slice(&[0, 0]);
    let start = bytes.len();
    match &record.data {
        RecordData::Name(name) => name_write(name, bytes)?,
        RecordData::Srv {
            port,
            target,
            priority,
            weight,
        } => {
            bytes.extend_from_slice(&priority.to_be_bytes());
            bytes.extend_from_slice(&weight.to_be_bytes());
            bytes.extend_from_slice(&port.to_be_bytes());
            name_write(target, bytes)?;
        }
        RecordData::Txt(parts) => {
            for part in parts {
                if part.len() > 255 {
                    return Err(invalid("TXT character string exceeds 255 bytes"));
                }
                if bytes.len() + 1 + part.len() > MAX_PACKET {
                    return Err(invalid("TXT aggregate exceeds packet limit"));
                }
                bytes.push(part.len() as u8);
                bytes.extend_from_slice(part);
            }
        }
        RecordData::A(ip) => bytes.extend_from_slice(&ip.octets()),
        RecordData::Aaaa(ip) => bytes.extend_from_slice(&ip.octets()),
        RecordData::Unknown(value) => {
            if value.len() > MAX_PACKET.saturating_sub(bytes.len()) {
                return Err(invalid("Unknown DNS data limit"));
            }
            bytes.extend_from_slice(value);
        }
    }
    let length =
        u16::try_from(bytes.len() - start).map_err(|_| invalid("DNS record encoding limit"))?;
    bytes[size..size + 2].copy_from_slice(&length.to_be_bytes());
    Ok(())
}
pub fn encode(packet: &Packet) -> Result<Vec<u8>> {
    if packet.questions.len() > 16
        || packet.answers.len() + packet.authorities.len() + packet.additionals.len() > 64
    {
        return Err(invalid("DNS question/record count limit"));
    }
    let mut bytes = vec![0, 0];
    bytes.extend_from_slice(&(if packet.response { 0x8400u16 } else { 0 }).to_be_bytes());
    for n in [
        packet.questions.len(),
        packet.answers.len(),
        packet.authorities.len(),
        packet.additionals.len(),
    ] {
        bytes.extend_from_slice(&(n as u16).to_be_bytes());
    }
    for question in &packet.questions {
        name_write(&question.name, &mut bytes)?;
        bytes.extend_from_slice(&question.kind.code().to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
    }
    for record in packet
        .answers
        .iter()
        .chain(&packet.authorities)
        .chain(&packet.additionals)
    {
        record_write(record, &mut bytes)?;
        if bytes.len() > MAX_PACKET {
            return Err(invalid("DNS packet encoding limit"));
        }
    }
    if bytes.len() > MAX_PACKET {
        return Err(invalid("DNS packet encoding limit"));
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compression_loops_counts_and_truncation_are_bounded() {
        let mut bytes = vec![0; 12];
        bytes[5] = 1;
        bytes.extend([0xc0, 12, 0, 1, 0, 1]);
        assert!(decode(&bytes).unwrap_err().message.contains("compression"));
        bytes[5] = 17;
        assert!(decode(&bytes).is_err());
        for n in 0..12 {
            assert!(decode(&vec![0; n]).is_err());
        }
    }
    #[test]
    fn compressed_query_and_record_round_trip() {
        let bytes = [
            0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 1, b'a', 0, 0, 1, 0, 1, 0xc0, 12, 0, 12, 0, 1,
        ];
        let value = decode(&bytes).unwrap();
        assert_eq!(value.questions[1].name, "a");
        assert_eq!(decode(&encode(&value).unwrap()).unwrap(), value);
    }
    #[test]
    fn record_boundaries_and_txt_limits() {
        let p = Packet {
            response: true,
            answers: vec![Record {
                name: "x.local".into(),
                kind: RecordType::TXT,
                ttl: 3,
                data: RecordData::Txt(vec![vec![1; 256]]),
            }],
            ..Packet::default()
        };
        assert!(encode(&p).is_err());
    }
}

#[cfg(test)]
mod adversarial_tests {
    use super::*;
    #[test]
    fn every_truncation_and_mutated_offset_of_real_record_stays_bounded() {
        let bytes = encode(&Packet {
            response: true,
            answers: vec![Record {
                name: "_freeshow._udp.local".into(),
                kind: RecordType::PTR,
                ttl: 28800,
                data: RecordData::Name("freeshow-REMOTE-abcdef._freeshow._udp.local".into()),
            }],
            ..Packet::default()
        })
        .unwrap();
        for end in 0..bytes.len() {
            assert!(decode(&bytes[..end]).is_err());
        }
        for offset in 0..bytes.len() {
            for mask in [1, 128, 255] {
                let mut mutated = bytes.clone();
                mutated[offset] ^= mask;
                let _ = decode(&mutated);
            }
        }
        assert!(decode(&vec![0; 9001]).is_err());
    }
}

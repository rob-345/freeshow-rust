use crate::{
    dns::validate_name, DiscoveryName, NetworkSnapshot, Record, RecordData, RecordType, Result,
};
pub const SERVICE_TYPE: &str = "_freeshow._udp.local";
pub fn get_local_ips(snapshot: &NetworkSnapshot) -> Result<Vec<String>> {
    snapshot.validate()?;
    let all: Vec<(&str, Vec<String>)> = snapshot
        .source_order()
        .into_iter()
        .map(|i| {
            (
                i.name.as_str(),
                i.addresses
                    .iter()
                    .filter(|a| !a.internal && a.is_v4())
                    .map(|a| a.address.clone())
                    .collect(),
            )
        })
        .collect();
    const INHERITED: &[&str] = &[
        "constructor",
        "__proto__",
        "__defineGetter__",
        "__defineSetter__",
        "hasOwnProperty",
        "__lookupGetter__",
        "__lookupSetter__",
        "isPrototypeOf",
        "propertyIsEnumerable",
        "toString",
        "valueOf",
        "toLocaleString",
    ];
    if all
        .iter()
        .any(|(name, values)| !values.is_empty() && INHERITED.contains(name))
    {
        return Err(crate::DiscoveryError::new(
            "discovery_source_shape",
            "Original getLocalIPs would access an inherited results property",
        ));
    }
    Ok(["en0", "eth0", "Wi-Fi"]
        .into_iter()
        .flat_map(|name| {
            all.iter()
                .filter(move |(n, _)| *n == name)
                .flat_map(|(_, v)| v.clone())
        })
        .chain(all.iter().flat_map(|(_, v)| v.clone()))
        .chain(["localhost".to_owned()])
        .collect())
}
pub(crate) fn instance_name(name: DiscoveryName, id: [u8; 3]) -> String {
    format!(
        "freeshow-{}-{:02x}{:02x}{:02x}",
        name.as_str(),
        id[0],
        id[1],
        id[2]
    )
}
pub fn dns_sd_args(
    snapshot: &NetworkSnapshot,
    name: DiscoveryName,
    port: u16,
    id: [u8; 3],
) -> Result<Vec<String>> {
    let ips = get_local_ips(snapshot)?;
    let args = dns_sd_args_with_ips(&ips, name, port, id);
    if args.iter().map(|v| v.len()).sum::<usize>() > 8192 {
        return Err(crate::DiscoveryError::new(
            "discovery_argument_limit",
            "dns-sd arguments exceed 8192 bytes",
        ));
    }
    Ok(args)
}
pub(crate) fn dns_sd_args_with_ips(
    ips: &[String],
    name: DiscoveryName,
    port: u16,
    id: [u8; 3],
) -> Vec<String> {
    vec![
        "-R".into(),
        instance_name(name, id),
        "_freeshow._udp".into(),
        "local".into(),
        port.to_string(),
        format!("ip={}", ips[0]),
        format!("ips={}", ips.join(",")),
    ]
}
pub fn service_records(
    snapshot: &NetworkSnapshot,
    name: DiscoveryName,
    port: u16,
    id: [u8; 3],
) -> Result<Vec<Record>> {
    let ips = get_local_ips(snapshot)?;
    service_records_with_ips(snapshot, &ips, name, port, id)
}
pub(crate) fn service_records_with_ips(
    snapshot: &NetworkSnapshot,
    ips: &[String],
    name: DiscoveryName,
    port: u16,
    id: [u8; 3],
) -> Result<Vec<Record>> {
    if port == 0 {
        return Err(crate::DiscoveryError::new(
            "invalid_discovery_port",
            "Publication requires a nonzero bound port",
        ));
    }
    snapshot.validate()?;
    let fullname = format!("{}.{}", instance_name(name, id), SERVICE_TYPE);
    validate_name(&fullname)?;
    let mut records = vec![
        Record {
            name: SERVICE_TYPE.into(),
            kind: RecordType::PTR,
            ttl: 28800,
            data: RecordData::Name(fullname.clone()),
        },
        Record {
            name: fullname.clone(),
            kind: RecordType::SRV,
            ttl: 120,
            data: RecordData::Srv {
                port,
                target: snapshot.hostname.clone(),
                priority: 0,
                weight: 0,
            },
        },
        Record {
            name: fullname,
            kind: RecordType::TXT,
            ttl: 4500,
            data: RecordData::Txt(vec![
                format!("ip={}", ips[0]).into_bytes(),
                format!("ips={}", ips.join(",")).into_bytes(),
            ]),
        },
    ];
    for address in snapshot
        .source_order()
        .into_iter()
        .flat_map(|i| &i.addresses)
        .filter(|a| !a.internal && a.mac != "00:00:00:00:00:00")
    {
        let (kind, data) = if address.family == "IPv4" {
            (
                RecordType::A,
                RecordData::A(address.address.parse().map_err(|_| {
                    crate::DiscoveryError::new("invalid_discovery_snapshot", "Invalid IPv4 record")
                })?),
            )
        } else if address.family == "IPv6" {
            (
                RecordType::AAAA,
                RecordData::Aaaa(address.address.parse().map_err(|_| {
                    crate::DiscoveryError::new("invalid_discovery_snapshot", "Invalid IPv6 record")
                })?),
            )
        } else {
            continue;
        };
        records.push(Record {
            name: snapshot.hostname.clone(),
            kind,
            ttl: 120,
            data,
        });
    }
    // Enforce all protocol/output limits before publication ownership/effects.
    crate::encode(&crate::Packet {
        response: true,
        answers: records.clone(),
        ..crate::Packet::default()
    })?;
    Ok(records)
}
pub(crate) fn matching_records(records: &[Record], question: &crate::Question) -> Vec<Record> {
    records
        .iter()
        .filter(|r| {
            (question.kind == RecordType::ANY || r.kind == question.kind)
                && if question.name.contains('.') {
                    r.name.eq_ignore_ascii_case(&question.name)
                } else {
                    r.name
                        .split('.')
                        .next()
                        .is_some_and(|v| v.eq_ignore_ascii_case(&question.name))
                }
        })
        .cloned()
        .collect()
}
pub(crate) fn query_response(records: &[Record], question: &crate::Question) -> crate::Packet {
    let answers = if question.kind == RecordType::ANY {
        let mut kinds = Vec::new();
        for record in records {
            if !kinds.contains(&record.kind) {
                kinds.push(record.kind);
            }
        }
        kinds
            .into_iter()
            .flat_map(|kind| {
                matching_records(
                    records,
                    &crate::Question {
                        name: question.name.clone(),
                        kind,
                    },
                )
            })
            .collect()
    } else {
        matching_records(records, question)
    };
    let mut additionals = Vec::new();
    if question.kind != RecordType::ANY {
        for a in answers.iter().filter(|r| r.kind == RecordType::PTR) {
            if let RecordData::Name(name) = &a.data {
                for kind in [RecordType::SRV, RecordType::TXT] {
                    additionals.extend(matching_records(
                        records,
                        &crate::Question {
                            name: name.clone(),
                            kind,
                        },
                    ));
                }
            }
        }
        let mut targets = Vec::new();
        for a in &additionals {
            if let RecordData::Srv { target, .. } = &a.data {
                if !targets.contains(target) {
                    targets.push(target.clone());
                }
            }
        }
        for name in targets {
            for kind in [RecordType::A, RecordType::AAAA] {
                additionals.extend(matching_records(
                    records,
                    &crate::Question {
                        name: name.clone(),
                        kind,
                    },
                ));
            }
        }
    }
    crate::Packet {
        response: true,
        answers,
        additionals,
        ..crate::Packet::default()
    }
}

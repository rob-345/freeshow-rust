use super::*;
use serde_json::json;
#[test]
fn page_boundaries_crc_and_zero_lace() {
    for size in [0, 1, 254, 255, 256, 510, 65024] {
        let mut s = OggState::new(0x88776655);
        let bytes = s.page(&vec![0xa5; size], 2, 960).unwrap();
        assert_eq!(&bytes[..4], b"OggS");
        assert_eq!(bytes[26] as usize, size / 255 + 1);
        assert_eq!(bytes[27 + size / 255], (size % 255) as u8);
        let expected = u32::from_le_bytes(bytes[22..26].try_into().unwrap());
        let mut cleared = bytes;
        cleared[22..26].fill(0);
        assert_eq!(crc32(&cleared), expected);
    }
    assert!(OggState::new(0).page(&vec![0; 65025], 0, 0).is_err());
}
#[test]
fn config_js_defaults_and_identity() {
    let l = Limits::default();
    assert!(Config::parse(Some(&json!(false)), &l).unwrap().is_none());
    let c = Config::parse(
        Some(&json!({"enabled":[],"port":" 0x1f40 ","password":42})),
        &l,
    )
    .unwrap()
    .unwrap();
    assert!(c.enabled);
    assert_eq!(c.port, 8000);
    assert_eq!(c.password, "42");
    assert_eq!(c.host, "localhost");
    let string = Config::parse(Some(&json!({"enabled":[],"port":8000,"password":"42"})), &l)
        .unwrap()
        .unwrap();
    assert_ne!(c, string);
    for v in [
        json!({"mount":true}),
        json!({"host":12}),
        json!({"password":{}}),
        json!({"port":1.5}),
        json!({"port":"Infinity"}),
        json!({"mount":"x\r\ny"}),
    ] {
        assert!(Config::parse(Some(&v), &l).is_err());
    }
}
#[test]
fn tags_preserve_whitespace_without_separator() {
    let tags = opus_tags("  only song  ").unwrap();
    assert!(tags.windows(17).any(|s| s == b"TITLE=  only song"));
    let tags = opus_tags(" \u{feff}Artist\u{a0} -  song - more  ").unwrap();
    assert!(tags
        .windows(b"TITLE=song - more".len())
        .any(|s| s == b"TITLE=song - more"));
    assert!(tags.ends_with(b"ARTIST=Artist"));
}
#[test]
fn hard_limits_reject_unbounded_settings() {
    let l = Limits {
        queue: 65,
        ..Limits::default()
    };
    assert!(l.validate().is_err());
}

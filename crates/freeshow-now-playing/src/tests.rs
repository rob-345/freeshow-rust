use super::*;
use serde_json::json;
fn context() -> Context {
    Context::from_host("native", 1, "/library").unwrap()
}
fn router(c: &Context) -> Router {
    Router::new(
        c.clone(),
        TrustedPaths::from_host(c, Platform::Posix, "/library/Audio").unwrap(),
    )
    .unwrap()
}
fn source() -> SourceData {
    SourceData::from_payload(json!({"filePath":"/media.wav","name":"N","unknownLang":[],"duration":61,"format":"{artist}:{title}"})).unwrap()
}
fn code<T>(r: Result<T>) -> &'static str {
    match r {
        Err(e) => e.code,
        Ok(_) => panic!("expected error"),
    }
}
fn receipt(router: &mut Router, issued: Issued, r: Receipt) -> Option<Issued> {
    let token = router.accept(issued.token).unwrap();
    router.complete(&token, r).unwrap()
}
#[test]
fn four_slots_include_ready_results_and_full_context_run_authority_is_native_only() {
    let c = context();
    let mut r = router(&c);
    let mut ids = vec![];
    for _ in 0..MAX_ACTIVE {
        let started = r.begin_media_path().unwrap();
        ids.push(started.operation);
        assert!(receipt(
            &mut r,
            started.effect.unwrap(),
            Receipt::Done { success: false }
        )
        .is_none())
    }
    assert_eq!(code(r.begin_open()), "now_playing_busy");
    assert_eq!(r.retained(), 4);
    let result = r.take_result(ids[0]).unwrap();
    assert_eq!(
        result.media_path.as_deref(),
        Some("/library/Audio/NowPlayingCover.png")
    );
    assert!(result.error.is_none());
    assert_eq!(r.retained(), 3);
    let foreign = context();
    assert_eq!(
        code(r.begin_set(
            source(),
            TrustedMediaPath::from_host(&foreign, "/media.wav").unwrap()
        )),
        "now_playing_authority"
    );
    assert_eq!(
        code(Router::new(
            c,
            TrustedPaths::from_host(&foreign, Platform::Posix, "/other").unwrap()
        )),
        "now_playing_context"
    );
}
#[test]
fn accepted_effect_is_single_use_wrong_receipt_retries_and_foreign_tokens_reject() {
    let c = context();
    let mut r = router(&c);
    let issued = r.begin_media_path().unwrap().effect.unwrap();
    let accepted = r.accept(issued.token).unwrap();
    assert_eq!(
        code(r.complete(&accepted, Receipt::Bytes(vec![]))),
        "now_playing_receipt"
    );
    assert_eq!(r.accepted(), 1);
    assert!(r
        .complete(&accepted, Receipt::Done { success: true })
        .unwrap()
        .is_none());
    assert_eq!(r.accepted(), 0);
    assert_eq!(
        code(r.complete(&accepted, Receipt::Done { success: true })),
        "now_playing_token"
    );
    let mut other = router(&c);
    let token = other.begin_media_path().unwrap().effect.unwrap().token;
    assert_eq!(code(r.accept(token)), "now_playing_token");
}
#[test]
fn cancellation_before_acceptance_rejects_stale_tokens_without_claiming_io() {
    let c = context();
    let mut r = router(&c);
    let s = r
        .begin_set(
            source(),
            TrustedMediaPath::from_host(&c, "/media.wav").unwrap(),
        )
        .unwrap();
    assert!(r.cancel(s.operation).unwrap().is_none());
    assert_eq!(code(r.accept(s.effect.unwrap().token)), "now_playing_token");
    assert_eq!(
        r.take_result(s.operation).unwrap().error.unwrap().code,
        "now_playing_cancelled"
    );
}
#[test]
fn cancelled_accepted_real_image_remains_owned_until_explicit_successful_disposal() {
    let c = context();
    let mut r = router(&c);
    let s = r
        .begin_set(
            source(),
            TrustedMediaPath::from_host(&c, "/media.wav").unwrap(),
        )
        .unwrap();
    let id = s.operation;
    let picture = Picture::from_host(json!({"format":"image/png"}), vec![1]).unwrap();
    let mut next = receipt(
        &mut r,
        s.effect.unwrap(),
        Receipt::Metadata(Some(
            Metadata::from_host(json!({}), Covers::Complete(vec![picture])).unwrap(),
        )),
    )
    .unwrap();
    while !matches!(next.effect, Effect::ImageCreate { .. }) {
        next = receipt(&mut r, next, Receipt::Done { success: true }).unwrap()
    }
    let accepted = r.accept(next.token).unwrap();
    r.cancel(id).unwrap();
    assert_eq!(r.accepted(), 1);
    assert_eq!(code(r.take_result(id)), "now_playing_pending");
    let release = r.complete(&accepted, Receipt::Image(7)).unwrap().unwrap();
    assert!(matches!(release.effect, Effect::ReleaseImage { image: 7 }));
    let token = r.accept(release.token).unwrap();
    assert_eq!(
        code(r.complete(&token, Receipt::Done { success: false })),
        "now_playing_cleanup_pending"
    );
    assert_eq!(r.accepted(), 1);
    assert_eq!(code(r.take_result(id)), "now_playing_pending");
    assert_eq!(
        code(r.complete(
            &token,
            Receipt::Failed(Error::native(
                "image_dispose_failed",
                "actual owner still live"
            ))
        )),
        "image_dispose_failed"
    );
    assert!(r
        .complete(&token, Receipt::Done { success: true })
        .unwrap()
        .is_none());
    let result = r.take_result(id).unwrap();
    assert_eq!(result.error.unwrap().code, "now_playing_cancelled");
}
#[test]
fn skipped_covers_in_existing_audio_metadata_require_a_real_cover_receipt() {
    let c = context();
    let mut r = router(&c);
    let s = r
        .begin_set(
            source(),
            TrustedMediaPath::from_host(&c, "/media.wav").unwrap(),
        )
        .unwrap();
    let audio = freeshow_audio::AudioReply {
        data: json!({"common":{"title":"T","unknown":{"keep":true}}}),
        diagnostics: vec![],
    };
    let next = receipt(
        &mut r,
        s.effect.unwrap(),
        Receipt::Metadata(Metadata::from_audio_reply(&audio, Covers::Unknown).unwrap()),
    )
    .unwrap();
    assert!(matches!(next.effect, Effect::ReadCovers { .. }));
    let next = receipt(&mut r, next, Receipt::Covers(vec![])).unwrap();
    let Effect::PublishMetadata {
        common, pictures, ..
    } = &next.effect
    else {
        panic!()
    };
    assert_eq!(common["unknown"], json!({"keep":true}));
    assert!(pictures.is_empty());
    assert!(matches!(
        next.effect,
        Effect::PublishMetadata {
            json_projection_only: true,
            ..
        }
    ));
    let ambiguous = freeshow_audio::AudioReply {
        data: json!({"common":{"year":null}}),
        diagnostics: vec![],
    };
    assert_eq!(
        code(Metadata::from_audio_reply(
            &ambiguous,
            Covers::Complete(vec![])
        )),
        "now_playing_ambiguous_year"
    );
    let unsupported = freeshow_audio::AudioReply {
        data: Value::Null,
        diagnostics: vec![freeshow_audio::AudioDiagnostic {
            code: "audio_unsupported_variant".into(),
            message: "Not certified as source missing metadata".into(),
        }],
    };
    assert_eq!(
        code(Metadata::from_audio_reply(&unsupported, Covers::Unknown)),
        "now_playing_metadata_unavailable"
    );
    let c2 = context();
    let mut r2 = router(&c2);
    let s = r2
        .begin_set(
            source(),
            TrustedMediaPath::from_host(&c2, "/media.wav").unwrap(),
        )
        .unwrap();
    let next = receipt(
        &mut r2,
        s.effect.unwrap(),
        Receipt::Metadata(Metadata::from_audio_reply(&audio, Covers::Unknown).unwrap()),
    )
    .unwrap();
    assert!(receipt(
        &mut r2,
        next,
        Receipt::Failed(Error::native(
            "native_cover_unavailable",
            "No complete cover evidence"
        ))
    )
    .is_none());
    assert_eq!(
        r2.take_result(s.operation).unwrap().error.unwrap().code,
        "native_cover_unavailable"
    );
}
#[test]
fn input_picture_output_and_amplification_limits_reject_before_unbounded_growth() {
    assert_eq!(
        code(cover_base64(Some(&vec![0; MAX_TEXT]))),
        "now_playing_text_limit"
    );
    assert_eq!(
        code(SourceData::from_payload(
            json!({"filePath":"x","format":"x".repeat(65536)})
        )),
        "now_playing_input_limit"
    );
    assert_eq!(
        code(Picture::from_host(json!({}), vec![0; MAX_COVER + 1])),
        "now_playing_picture"
    );
    assert_eq!(
        code(Metadata::from_host(
            json!({}),
            Covers::Complete(
                (0..9)
                    .map(|_| Picture {
                        fields: json!({}),
                        bytes: vec![]
                    })
                    .collect()
            )
        )),
        "now_playing_picture"
    );
    assert_eq!(
        code(Metadata::from_host(
            json!({}),
            Covers::Complete(vec![Picture {
                fields: json!({"data":[]}),
                bytes: vec![]
            }])
        )),
        "now_playing_picture"
    );
    assert_eq!(
        code(replace_all(
            &"{artist}".repeat(2048),
            "{artist}",
            &"x".repeat(2048)
        )),
        "now_playing_text_limit"
    );
    assert_eq!(
        code(replace_all("hello", "", "x")),
        "now_playing_text_limit"
    );
}
#[test]
fn trusted_path_join_preserves_native_platform_rules_and_posix_backslashes() {
    let c = context();
    for (platform, folder, expected) in [
        (
            Platform::Posix,
            "/a/../音/./Audio/",
            "/音/Audio/NowPlaying.txt",
        ),
        (Platform::Posix, "/", "/NowPlaying.txt"),
        (Platform::Posix, "/path\\", "/path\\/NowPlaying.txt"),
        (
            Platform::Windows,
            "C:/a/../音/Audio/",
            "C:\\音\\Audio\\NowPlaying.txt",
        ),
        (
            Platform::Windows,
            "\\\\server\\share\\Audio\\..",
            "\\\\server\\share\\NowPlaying.txt",
        ),
    ] {
        let p = TrustedPaths::from_host(&c, platform, folder).unwrap();
        assert_eq!(p.text, expected);
    }
    assert_eq!(
        code(TrustedPaths::from_host(&c, Platform::Posix, "relative")),
        "now_playing_path"
    );
}
#[test]
fn repeated_set_unset_are_not_deduplicated_and_image_gap_retains_partial_source_effects() {
    let c = context();
    let mut r = router(&c);
    for _ in 0..2 {
        let s = r.begin_unset().unwrap();
        assert!(
            matches!(s.effect.as_ref().unwrap().effect,Effect::IcecastMetadata{ref title}if title.is_empty())
        );
        r.cancel(s.operation).unwrap();
        r.take_result(s.operation).unwrap();
    }
    let s = r
        .begin_set(
            source(),
            TrustedMediaPath::from_host(&c, "/media.wav").unwrap(),
        )
        .unwrap();
    let p = Picture::from_host(json!({}), vec![1, 2]).unwrap();
    let mut next = receipt(
        &mut r,
        s.effect.unwrap(),
        Receipt::Metadata(Some(
            Metadata::from_host(json!({}), Covers::Complete(vec![p])).unwrap(),
        )),
    )
    .unwrap();
    let mut order = vec![];
    loop {
        if matches!(next.effect, Effect::ImageCreate { .. }) {
            assert!(receipt(
                &mut r,
                next,
                Receipt::Failed(Error::native("native_image_unavailable", "No parity codec"))
            )
            .is_none());
            break;
        }
        order.push(match next.effect {
            Effect::PublishMetadata { .. } => "metadata",
            Effect::IcecastMetadata { .. } => "icecast",
            Effect::EnsureAudioFolder { .. } => "folder",
            _ => panic!(),
        });
        next = receipt(&mut r, next, Receipt::Done { success: true }).unwrap();
    }
    assert_eq!(order, ["metadata", "icecast", "folder"]);
    assert_eq!(
        r.take_result(s.operation).unwrap().error.unwrap().code,
        "native_image_unavailable"
    );
}

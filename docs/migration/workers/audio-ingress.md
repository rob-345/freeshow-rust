# Native AUDIO ingress ownership

`src-tauri/src/audio_ingress.rs` adds one fixed Tokio FIFO pump and one fixed join supervisor around the existing `NativeAudioPipeline`. It owns raw-job lifetime across IPC caller drop, full-context retirement and application shutdown. It does not replace `processAudio`, reset its source-global DSP/Opus state, claim a hardware sink, or emit an original AUDIO success event.

This slice adds only the ingress module, `src-tauri/tests/audio_ingress.rs`, and this document. Main command/window authority, raw framing/announced-length validation, frontend behavior, shared manifests, native audio source behavior and CI wiring remain coordinator-owned.

## Native API

```rust,ignore
let ingress = AudioIngress::new(library.clone(), audio.clone())?;
let ticket = ingress.submit(
    &captured_full_context,
    raw_pcm_view,                 // borrowed <=32768 bytes
    &audio_metadata,              // original source-shaped typed metadata
    &native_sink_snapshot,        // trusted native registry/activity authority
    cancel,
)?;
let observation = ticket.finish().await?;
observation.inspect(|receipt| {
    // Bounded private native diagnostics only. Do not block or reenter ingress.
    // receipt.effects()/diagnostics()/failure() remain real pipeline evidence.
})?;
observation.into_void()?;
```

`submit` is synchronous: it validates fixed input limits/full runtime context, reserves one whole-job quota lease, then copies the exact byte slice and clones metadata/native sink snapshot. There is no per-frame spawn. `Ticket`, `Observation`, input groups and their guards cannot be deserialized or supplied by renderer IPC. `AudioMetadata` retains the pipeline's `JsInput` values and source coercions; `SinkAvailability` must come from the native registry, not renderer claims. The coordinator must validate the originating main window and raw framing before calling this API.

`Ticket::finish` returns a sealed private observation. It does not release the quota or group guard: completed/uncollected native results, including already-inspected observations, remain counted. The original `AudioReceipt` never escapes the wrapper. `Observation::into_void()` consumes/drops the actual receipt and guard; dropping the observation has the same ownership cleanup. A native caller can inspect diagnostics but must not serialize them into an invented source AUDIO ACK. A pipeline `AudioReceipt.failure()` is private source-processing evidence; `into_void` does not reinterpret that field as a new source reply or event.

`disconnect_icecast(&context, cancel)` returns the same ticket type and enters the ordinary FIFO. This is the source `CLOSE_PORT` behavior. It neither closes an input group nor retires global DSP/Opus state. Frontend source count/connection quirks remain outside this owner. Pipeline `status`, `context_changed`, `complete_status`, capability and trusted renderer status delivery remain directly callable; none is queued behind ingress.

One process ingress owner may exist at a time. Retain it across ordinary library switches; do not construct one per request/library/frame. The underlying audio host separately enforces its process owner. A duplicate ingress constructor fails explicitly. `with_timeout` is a native test/host constructor that can lower the default 15-second drain budget; accepted values are positive and at most 30 seconds.

## Context retirement and actual guards

An ingress group records the complete `RuntimeContext` (runtime ID, generation, data path), a cancellation token, terminal admission flag and actual outstanding guard count. The group is selected/admitted under the queue mutex. A matching generation with a foreign runtime/path is stale. A new full context is admitted only after the previous group was explicitly closed and its real guard count reached zero. The same closed context stays closed. At most the current and immediately previous group are retained for native cleanup/retry.

Library-switch order is:

1. Commit the library change and call `audio.context_changed()` to cancel obsolete status acceptance.
2. Call `ingress.close_context(&old_context)`. This is a synchronous method returning an owned async future: the call itself closes group admission, cancels its token, removes/drops queued raw packets, and actually disposes completed private observations. These actions happen even if the returned future is never polled or is dropped.
3. Await its real guard drain. Retain the same owner/group and retry on timeout. Only after this succeeds admit raw packets for the new full context.

Queue/registry storage is accessible to explicit retirement, so queued packets and ready observations can be removed while another accepted native call blocks the pump. Their PCM/metadata, actual `AudioReceipt`, native retained-result lease, ingress quota and group guard are actually dropped. A later collection returns explicit cancellation/stale context; it cannot recover a disposed receipt. This is disposal of already-completed private data, not proof of OS cancellation.

An active cell remains strongly owned by shared state. The pump owns and polls the actual `NativeAudioPipeline::pcm`/disconnect future through caller drop. Caller drop cancels queued jobs; an already-active operation is abandoned only as a recipient, and still runs to its real return. Explicit caller/group cancellation is forwarded into the existing native cancellation token, without dropping/selecting away the active future. The source host decides cancellation before its own effect acceptance; accepted source effects keep their original continuation behavior. The guard is released only after the actual native receipt returns and is consumed/disposed. A group drain cannot claim success from a canceled waiter, lost IPC reply, elapsed timeout or queued FIFO barrier.

The current caller never receives a canceled operation's stale native receipt. Private ready observations remain quota/guard owners until consume/drop or actual explicit retirement. Wrappers retained by a native caller after retirement contain only a revoked token/context/error and no raw input/receipt/IO guard; the service does not retain an unbounded history of those caller-owned wrappers. The coordinator promptly consumes/drops each returned observation.

## Shutdown and failure

`ingress.shutdown()` also closes/cancels synchronously at call time, then waits for real active/group drain and the fixed pump's actual join through its fixed supervisor. It does not abort either task. Its 15-second default timeout retains the same active future/group/supervisor for retry. Only an already-finished supervisor handle is taken for final collection, so dropping a shutdown waiter cannot lose live join ownership.

Main must then separately call `NativeAudioPipeline::shutdown()`, attempt all other owners even after an earlier error, and collect all results before returning an error or closing library write admission. A group close is not native audio shutdown. The real host Tokio runtime and service handle must remain alive until these joins complete. Last ingress-handle drop synchronously cancels/closes admission, while the fixed pump/supervisor retain their actual ownership through real completion; it is not a substitute for explicit shutdown/join evidence. No status publisher is captured by ingress. A coordinator callback that references ingress must use weak/bounded routing and must not create a publisher ownership cycle.

A pump panic is sticky private failure, not a fake void receipt. Its active cell/guard stays retained even after the failed task is joined, because dropping its waiter cannot prove that the independent native audio owner finished. The coordinator still attempts the separate native audio shutdown. Only the actual native `joined` status permits recovery/disposal of that failed active cell on ingress retry; sticky failure remains visible after cleanup and on later retries. If a failed ingress owner is incorrectly discarded while native work is outstanding, its process reservation remains closed rather than admitting a replacement on a false drain claim.

## Bounds and evidence

The process has one fixed pump plus at most one fixed join supervisor, four leases across queued/active/ready observations, and no per-frame task/thread. Each PCM snapshot is at most 32,768 bytes. Metadata is checked before cloning: at most 8,192 counted UTF-8/value bytes, 256 JSON nodes and depth 8. Each of the three native registration lists has at most 64 keys, each key at most 1,024 bytes and all lists together at most 8,192 UTF-8 bytes. Context runtime ID/path are capped at 256/32,768 respectively. Diagnostics are capped at 2,048 UTF-8 bytes. The queue, registry and retained groups are bounded; the existing native host supplies its own bounded downstream buffers/codec/transport owners. This layer adds at most four raw PCM copies and four bounded metadata/registration snapshots, plus fixed queue/result/guard state. It does not allocate a whole audio stream.

Nine focused tests include the real existing audio host rather than a fake pipeline. They establish:

- Four whole-job/ready-result leases, FIFO sequence order, duplicate process owner rejection and actual native receipt disposal during retirement.
- Synchronous close before awaiting, removal of queued/ready data despite an accepted blocked source host, dropped drain waiters, timeout retention and successful same-owner retry.
- Queued/explicit cancellation and IPC caller drop, with no queued canceled packet entering the source host and accepted caller-drop work retaining actual native quota until completion.
- Full runtime/generation/path identity, stale library rejection, real accepted guard before new-group admission, and metadata/sink/PCM input bounds.
- Ordinary Icecast disconnect preserving group admission and source FIFO order.
- Terminal shutdown and actual pump/supervisor join without falsely claiming that the separate native pipeline was shut down.
- Real system Opus plus a controlled loopback Icecast HTTP connection: PCM/metadata are snapshotted while queued behind a real accepted source call, then the caller mutates its originals. After the second call is accepted and its IPC caller drops, its original Icecast destination still receives an actual Ogg audio packet with valid CRC while independent source status completion continues. The original connect-trigger packet is consumed before this second packet, and the final wire has header/tags/audio/EOS pages.
- A controlled panic at the ingress owner boundary while an actual source-accepted host call is blocked: its guard cannot disappear before the real native owner joins; failure stays sticky after cleanup.

The absent-Opus fixture makes ownership/admission tests independent of machine codec availability. The byte-delivery test requires the actual system codec when `FREESHOW_REQUIRE_OPUS=1`; otherwise it reports explicit unavailability instead of claiming that path. This layer does not add codec/source-equivalence claims beyond the separately tested `processAudio`/Opus/Icecast host, hardware delivery, or timing performance measurements.

The focused test includes both owned modules by path and needs no GTK or desktop application build. This worker used a private `/tmp/freeshow-native-audio-ingress-check` manifest with existing core/audio-pipeline/Opus/Icecast path dependencies and serde/serde_json/tokio/tokio-util/uuid/tempfile. Tokio features are `sync,rt,net,time,macros,io-util`. After coordinator manifest integration:

```sh
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 FREESHOW_REQUIRE_OPUS=1 \
  cargo test --manifest-path src-tauri/Cargo.toml --test audio_ingress -- --nocapture
cargo clippy --manifest-path src-tauri/Cargo.toml --test audio_ingress -- -D warnings
rustfmt --edition 2021 --check src-tauri/src/audio_ingress.rs src-tauri/tests/audio_ingress.rs
```

The worker reuses its isolated `target/assets` directory and makes no shared manifest, native binary, UI, root worktree or CI changes. Windows/macOS library crosschecks validate compilation of this OS-independent ownership layer; real Opus transport execution in this slice was Linux only.

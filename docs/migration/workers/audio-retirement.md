# Native AUDIO stream retirement

This patch owns only `src-tauri/src/audio_pipeline.rs`, its focused native test and this document. It starts at integrated milestone `37ee0099ce9caea7705881efb4ea3fa0c017ec4a`. No ingress, main/IPC, bridge, manifests, GUI, codec implementation or release binary is changed. Coordinator integration must retain the newer root's narrow `metadata` dead-code annotation when applying the host diff.

The observed bug was an idle, connected Icecast TCP stream surviving library retirement: browser port detachment removes its close callback, ingress guard draining alone performs no socket disconnect, and the pipeline's synchronous `context_changed()` only revokes obsolete status delivery. Successful renderer/context cleanup could therefore leave the old stream sending silence indefinitely without another PCM packet. Actual source `receiveAudio.ts` calls `IcecastSender.disconnect()` for Icecast CLOSE_PORT/port-close, while global resampler, partial accumulator, latest config, Opus encoder and reconnect cooldown survive that disconnect. The immutable source is revision `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`.

## Host integration

Call `native_audio.retire_context(&old_full_context).await` **after the old ingress group's accepted guards drain**, including caller-abandoned/completed native effects. Continue calling synchronous `context_changed()` to revoke stale private status receipts. These are native ownership operations; neither produces an original AUDIO success ACK or a fabricated ICECAST_STATUS.

`retire_context` synchronously admits before returning its `Send + 'static` future. Its separate one-slot FIFO and one whole-control lease share the existing AUDIO owner: there is no added task, runtime or thread. The owner selects this control queue before the ordinary FIFO between already accepted jobs. A full ordinary queue or four retained packet receipts cannot consume this reserved lease. The control lease stays with the accepted job, then with its immutable native `AudioRetirementReceipt` in the reply channel or caller, until collected/dropped. The receipt exposes the requested full context, optional actual run and whether that matching stream was disconnected. Drop it after checking/recording the private result.

Only bounded contexts from the same native runtime, with a generation no newer than the current library, are accepted. Same-generation path mismatch is rejected. No renderer-provided endpoint/sink authority is added. Context/run ownership attaches to actual connecting/written Icecast runs. Metadata/no-op commands cannot retag a socket. An obsolete cleanup is a no-op when a genuinely newer run owns the stream.

New-group PCM can race the point between old guards reaching zero and control selection. Before its first Icecast effect, under the existing publication/registration gate, it disconnects any older-context socket before attempting a genuine new connect. It cannot reuse and relabel the old live run even when the endpoint/config is identical. The global source three-second reconnect cooldown still applies; a same-config attempt during that interval remains a cooldown drop, rather than a newly invented immediate connection. Source-global DSP/Opus/config/accumulator state is preserved. This explicit native context fencing is an intentional lifecycle difference from an unbounded source callback that may finish after library selection; it does not claim to revoke source effects already accepted by the owner.

`audio_retirement_busy` means a previous accepted or uncollected control is still retained. `audio_retirement_timeout` uses the configured shutdown budget (default 15 seconds) and leaves actual work owned. Dropping the future, even without polling it, does not cancel an accepted disconnect. Drop/collect the old receipt or wait for an abandoned job to finish, then retry. Neither timeout nor busy is cleanup proof. Root must bound its own lifecycle waiters and retain actual ingress/context retirement across caller Drop; this patch does not add public IPC admission.

Terminal shutdown closes both queues under their admission locks, drains accepted control and ordinary work, then joins the existing CPU pipeline, codec, Icecast owner, status task and status-delivery thread before the outer AUDIO thread join completes. A timeout retains these actual owners for retry. The existing first terminal failure remains sticky while all other owners still drain. A successful per-context retirement proves the matching socket disconnect call returned: bounded EOS write/shutdown was attempted and the TcpStream was dropped. It does not terminate the process-global codec/network service or prove an Icecast receiver accepted the stream. The existing fixed process-global DNS resolver may finish an already accepted, non-preemptible `getaddrinfo` after cancellation; no new DNS task is created by retirement. Final thread join can still include thread-local/destructor work.

## Focused evidence

The native target contains 18 tests, including four added retirement tests. Actual system libopus and controlled loopback TCP prove idle old-context EOS/EOF without any subsequent PCM or ordinary disconnect, cleanup with all four ordinary receipts retained, one retained control receipt, foreign/future/path/oversize rejection, a synchronously accepted but never-polled/dropped control future, a timed-out control behind an accepted PCM job, terminal timeout/retry and all four real owner-drain bits plus outer join.

The race regression uses **two actual TCP connections to the same listener/unchanged config** and a deterministic post-acceptance PCM gate. It lets the real three-second source cooldown expire, admits new-context PCM before old cleanup, checks raw Ogg CRC/EOS on the old connection, requires a genuinely greater new run and continuing codec sequence, then proves delayed old cleanup leaves the new socket open and writable. That peer uses two fixed test tasks, two connections and at most 1 MiB per connection with bounded reads. Ordinary fixtures remain controlled loopback; no LAN server, hardware capture/playback or GUI was used.

An existing caller-drop/context regression now expects old EOS plus a real new connection, rather than assuming a new library adopts the old live socket. It still proves the accepted old encode survives caller Drop and the codec sequence remains continuous. The unchanged immutable-source N-API oracle also runs against the original bundled addon/libopus 1.3.1 and checks actual encoded packets/HTTP/Ogg framing, with all raw CRCs validated before any timer-silence filtering. System codec tests use actual installed libopus; their success is not a same-version claim about the bundled addon.

Checks use a temporary standalone manifest that points at the owned native test/module and existing path crates; no shared manifest is changed. Reused target and environment:

```sh
export CARGO_TARGET_DIR="$PWD/crates/freeshow-remotes/target"
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export FREESHOW_REQUIRE_OPUS=1
export FREESHOW_NATIVE_AUDIO_NODE=/home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node
export FREESHOW_OPUS_ORACLE_DIR=/home/rob-admin/GitHub/freeshow-rust/target/opus-oracles
export FREESHOW_REQUIRE_NATIVE_AUDIO_ORACLE=1
cargo test --offline --manifest-path /tmp/freeshow-audio-retirement-check/Cargo.toml --test audio_pipeline -- --test-threads=1
cargo clippy --offline --manifest-path /tmp/freeshow-audio-retirement-check/Cargo.toml --all-targets -- -D warnings
rustfmt --edition 2021 --check src-tauri/src/audio_pipeline.rs src-tauri/tests/audio_pipeline.rs
npx prettier --config config/formatting/.prettierrc.yaml --check docs/migration/workers/audio-retirement.md
git diff --check
```

Linux real effects and source comparison are the focused evidence. Actual Windows/macOS execution, ingress integration and the retained WebKit/library-switch workflow remain coordinator checks; compilation does not establish those behaviors.

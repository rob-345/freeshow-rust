# Assets native-exit fixture fence

Actual Windows core run `39253267937`, job `118062105030`, passed15 of17
assets tests. Two fixtures released a native gate and immediately required the
next100ms shutdown observer to return success or sticky `assets_worker`. Real
thread teardown/panic unwinding had not always completed; the observed error was
the existing truthful `assets_shutdown_timeout`.

The correction adds a cfg(test)-only observation of the actual retained
`JoinHandle::is_finished` states and logical-body count. After releasing the
gate, those two fixtures wait at most5s for actual handle completion, without
repeated shutdown calls or a blind delay. The exit-pause case explicitly proves
`(logical bodies=0, handles finished=false)` while held. After the fence, handles
must still be retained/unjoined; the unchanged production shutdown then performs
the real joins and must return its strict success/sticky-panic result. Existing
pre-release timeout, lease retention, resource disposal, repeated panic and
terminal admission assertions remain intact. Tests remain parallel.

Production configuration,100ms fixture policy, pool drain, completion counters,
thread creation and public API are unchanged. This is a native-fixture ordering
correction, not evidence that release/drop/logical completion proves OS drain.
Genuine Windows execution of the corrected cases remains a CI obligation.

Focused Linux access cases pass10/10; the complete assets target passes17/17
plus the parity binary's empty test target. Linux and Windows GNU all-target
strict Clippy pass. These checks use Rust1.99.1 with jobs1, debug0 and incremental
disabled in the existing private external cache; no root/native application is
built. Cross lint compiles the actual Windows test bodies but does not execute
them. Rustfmt, Prettier and whitespace checks pass.

```sh
export CARGO_TARGET_DIR=/mnt/data/cache/freeshow-cover-native-portability
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo +1.99.1 test --offline --locked -p freeshow-assets --lib access_tests:: -- --nocapture
cargo +1.99.1 test --offline --locked -p freeshow-assets --all-targets
cargo +1.99.1 clippy --offline --locked -p freeshow-assets --all-targets -- -D warnings
cargo +1.99.1 clippy --offline --locked -p freeshow-assets --all-targets \
  --target x86_64-pc-windows-gnu -- -D warnings
```

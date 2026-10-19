# AI inventory fixture observes actual frame release

Based on `5434778f4e820345070bcbe8cf7793791cd40d5e`. Its Windows core CI job
`118201339533` (run `39293892950`) failed
`actual_budget_failures_are_explicit_and_reclaim_whole_slots` at the immediate
`retained == 0` assertion: actual 1, expected 0. The original log is preserved at
`/mnt/data/cache/freeshow-rust-ci/5434778f/windows-core.log`.

The native worker correctly sends an envelope containing a cloned whole-job lease,
then disposes its budget, root and original frame lease. A consumer can receive and
drop the envelope before that final native frame release. `retained == 1` in this
interval is actual ownership, not a leaked slot or a production bug.

The existing fixture now uses its existing `observed` helper to await
`service.status().retained == 0` before the unchanged exact zero assertion and
actual shutdown. The helper's existing three-second bound is unchanged. All five
resource-limit inputs, exact `ai_local_limit` errors and original lifetime/join
checks remain. No production release order, quota, timeout or service code changes.

A disposable copy under
`/mnt/data/cache/freeshow-rust-build/ai-local-release-fixture/negative` adds a test-only
MPSC gate immediately after actual reply publication and before actual native frame
disposal. Neither this gate nor its two controls is committed:

- With the genuine frame held, consuming the ready error leaves retained 1. The old
  immediate-zero expectation fails deterministically, left 1/right 0. The control
  releases the native gate and actually joins shutdown before its intentional
  failing assertion.
- The same actual-release observation remains pending while that frame is held.
  After release it observes zero, preserves the exact assertion and successfully
  joins the native worker.

The negative and positive logs are preserved there as
`private_old_immediate_zero_assertion.log` and
`private_actual_release_observation.log`.

Final checks use a separate private manifest pointing directly to the tracked
worktree's actual library, and a private copy of the existing dependency lock:

```sh
CARGO_TARGET_DIR=/mnt/data/cache/freeshow-rust-build/ai-local-release-fixture/target CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo +1.99.1 test --offline --manifest-path /mnt/data/cache/freeshow-rust-build/ai-local-release-fixture/check/Cargo.toml
CARGO_TARGET_DIR=/mnt/data/cache/freeshow-rust-build/ai-local-release-fixture/target CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo +1.99.1 clippy --offline --all-targets --manifest-path /mnt/data/cache/freeshow-rust-build/ai-local-release-fixture/check/Cargo.toml -- -D warnings
```

Linux: all 13 original Rust tests pass, strict all-target Clippy and formatting
pass, and the private negative/positive native-frame controls behave as described.
Actual Windows/macOS runtime acceptance requires a fresh CI run. This patch does
not alter the separately reported Darwin undecodable-filename source fixture or
provider Unicode driver diagnostics, and it does not make the failed historical
run green. No shared/root cache, manifest, lock, host, GUI or CI file is changed.

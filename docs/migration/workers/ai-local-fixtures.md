# AI local inventory native fixture correction

Base: `d3227bd7`. This patch changes only `crates/freeshow-ai-local/src/tests.rs`, `src-tauri/tests/ai_local.rs`, and this note. Production inventory/host code, source oracle, root/native manifests, caches used by the coordinator, and frozen normal binaries are untouched.

## Actual failure, cause, and correction

Published `983258f0` CI logs:

- `/tmp/freeshow-ci-983258f0-windows-core.log`: 11/13 passed. `exact_number_sum_and_native_root_limits_never_round_or_guess` failed at the `TrustedUserData::from_host(std::env::temp_dir()).unwrap()` setup; the genuine empty/nested inventory test expected a mixed-separator path.
- `/tmp/freeshow-ci-983258f0-macos-core.log`: 12/13 passed. The same capacity-test setup failed with `ai_local_unsupported` before checking capacity 5.
- `/tmp/freeshow-ci-983258f0-windows-native.log`: 3/6 passed. The global-root/library-switch/held-library-owner cases returned native `...\app\bin\nemotron`, while fixtures expected `...\app/bin/nemotron`.

The temporary-directory provider may return a spelling with a trailing separator. `TrustedUserData` intentionally rejects non-normal component spellings; this is not a production validation failure. The capacity test now allocates an actual `tempfile::TempDir`, validates its supported `path()` spelling, and asserts the exact intended `ai_local_invalid` error for capacity 5. The malformed-root rejection loop and all exact integer-sum assertions remain unchanged.

On Windows, `PathBuf::join("app/bin/nemotron")` retains the embedded forward slashes in its serialized text even though Windows file operations accept them. The actual Rust inventory and original Node component joins produce native separators. All 13 compound `join(...)` calls across the two test files now join one component at a time, including nested fixture file creation. Complete expected objects, path strings, own-key order, listener/context checks, and lifetime assertions are still compared exactly. There is no normalization of actual replies and no production path coercion.

## Focused verification

Private manifests import the actual edited test sources and actual production sources. The host check imports the unchanged native adapter and actual Core/AI dependencies, avoiding a Tauri/GUI build. Target: `/mnt/data/cache/freeshow-rust-build/ai-local-fixtures/target`; jobs 1, dev/test debug 0, incremental 0. No shared target or normal GUI artifact is used.

```sh
cargo +1.99.1 test --manifest-path /mnt/data/cache/freeshow-rust-build/ai-local-fixtures/core-check/Cargo.toml --lib --offline
cargo +1.99.1 test --manifest-path /mnt/data/cache/freeshow-rust-build/ai-local-fixtures/host-check/Cargo.toml --test ai_local --offline
cargo +1.99.1 clippy --manifest-path /mnt/data/cache/freeshow-rust-build/ai-local-fixtures/core-check/Cargo.toml --all-targets --offline -- -D warnings
cargo +1.99.1 clippy --manifest-path /mnt/data/cache/freeshow-rust-build/ai-local-fixtures/core-check/Cargo.toml --all-targets --target x86_64-pc-windows-gnu --offline -- -D warnings
cargo +1.99.1 clippy --manifest-path /mnt/data/cache/freeshow-rust-build/ai-local-fixtures/core-check/Cargo.toml --all-targets --target aarch64-apple-darwin --offline -- -D warnings
cargo +1.99.1 clippy --manifest-path /mnt/data/cache/freeshow-rust-build/ai-local-fixtures/host-check/Cargo.toml --all-targets --offline -- -D warnings
```

Linux focused results: 13/13 core tests and 6/6 native adapter tests passed. Strict all-target Clippy passed for core on Linux, Windows GNU and macOS ARM and for the private native adapter harness on Linux; rustfmt, worker-note Prettier and `git diff --check` passed. Existing counts remain 13 core tests and 6 native adapter tests; no redundant cases or production test hooks were added. Foreign-target strict lint is compiler evidence only. Actual corrected Windows/macOS fixture execution remains pending the coordinator's fresh CI run.

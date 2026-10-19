# Two-Tauri release ablation collector

`compare-releases.mjs --mode tauri-ablation` compares two existing, frozen Linux
Tauri releases with explicit before/after labels. Both use the same real Tauri
adapter. The default `electron-rust` mode retains its official-package checks
and Electron/Tauri adapters. The process parser, CPU/PSS implementation, private
profile seed, ordering generator and Python subreaper are unchanged. Historical
receipts and archived executables are untouched.

This commit prepares tooling only: no release build, GUI, CPU measurement or
optimization result. Choose the common application base after full-fork audio
acceptance is resolved. The collector's `d0e88f3b` source base is not a proposed
performance control. The isolated audio harness and diagnostic full-fork bundle
remain functional evidence; neither supplies measurement assets.

## Frozen build receipts

`captureTauriBuild({sourceRoot,binary,buildLog})` reads the actual clean tracked
checkout, full commit, manifests/locks/toolchain, Vite configuration/build script,
optional local Cargo/dotenv inputs, compiler/tool versions and selected build
environment. Vite environment values are hashed rather than disclosed. It hashes
the selected executable, normal production/release build log, complete frontend
tree and actual remotes resource tree. All three client indexes must exist.
Keep executable/resources in a physical `target/release` directory so Tauri's
resource lookup retains its tested layout. Keep both build worktrees/artifacts
frozen and available throughout verification.

These are trusted-builder declarations plus observed file provenance, not an
independent reconstruction/attestation of embedded binary contents. Log patterns
cannot prove a compilation occurred or rule out every instrumentation technique.
The builder must actually execute the declared normal commands under its frozen
native-library/build environment. Known full-fork hook markers are rejected in
frontend/client JS/HTML and build logs; actual WebView readiness also rejects the
known hook globals before settling and after sampling. This is not a generic
proof that arbitrary diagnostic code is absent.

Per tree: maximum32 directory levels,8192 entries,4096 ordinary files,4096 UTF-8
bytes per relative path,1MiB aggregate path bytes,128MiB per file and512MiB total.
Symlinks/special entries and empty trees reject. Executable limit is1GiB; each
build input/log is at most16MiB; receipt input is at most8MiB. Hashing streams
sequentially; filesystem calls do not have hard OS cancellation guarantees.

The ablation verifies both explicit executable SHA256 values, receipts and
artifacts before launch and after every run. Build inputs, recorded toolchain,
build environment and remote client bytes must agree across variants. Actual
Git runtime changes under source/native/crates/public/config/scripts/vendor and
build inputs must equal1–16 explicit `--allowed-source-change` paths. A caller
cannot silently label unrelated native changes as an analyser ablation. Docs
and test-only additions are outside that runtime-source comparison.

## Future execution recipe

After functional acceptance, create two clean immutable revisions with the same
feature scope and only the approved runtime delta. Reuse existing node_modules
without modifying it. The following is a template for **each** variant; use its
own worktree/target/log/receipt paths and the same trusted build environment:

```sh
set -euo pipefail
export NODE_ENV=production CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_RELEASE_DEBUG=0
export CARGO_TARGET_DIR="$AB_VARIANT_TARGET"
cd "$AB_VARIANT_SOURCE"
{
  "$ORACLE_NODE" scripts/migration/build-frontend.mjs
  cargo +1.99.1 build --release --manifest-path src-tauri/Cargo.toml \
    --features custom-protocol --locked
} 2>&1 | tee "$AB_BUILD_LOG"

"$ORACLE_NODE" --input-type=module - \
  "$AB_COLLECTOR" "$AB_VARIANT_SOURCE" "$AB_BINARY" \
  "$AB_BUILD_LOG" "$AB_BUILD_RECEIPT" <<'JS'
import fs from 'node:fs';
import {pathToFileURL} from 'node:url';
const [collector,sourceRoot,binary,buildLog,output]=process.argv.slice(2);
const {captureTauriBuild}=await import(pathToFileURL(collector));
fs.writeFileSync(output, JSON.stringify(await captureTauriBuild({sourceRoot,binary,buildLog}),null,2)+'\n');
JS
```

`AB_COLLECTOR` is the absolute path to this reviewed collector. Use Node22.21.1
and the established native dependency environment; the helper does not install
or find substitute libraries. Preserve the build logs and read the resulting
executable hashes into `AB_BEFORE_SHA`/`AB_AFTER_SHA`. Do not run the diagnostic
`tests/desktop/audio_context_fullfork/build.mjs`: the normal frontend command
above empties/rebuilds `dist-rust` without its injected AudioContext/IPC hooks.

Once all builds/GUI/profiling are stopped and an idle reservation is explicit:

```sh
AB_ARGS=(
  --mode tauri-ablation --source-root "$AB_GIT_ROOT"
  --before-bin "$AB_BEFORE_BIN" --after-bin "$AB_AFTER_BIN"
  --before-label eager --after-label lazy
  --before-build-receipt "$AB_BEFORE_RECEIPT"
  --after-build-receipt "$AB_AFTER_RECEIPT"
  --expect-before-sha256 "$AB_BEFORE_SHA" --expect-after-sha256 "$AB_AFTER_SHA"
  --allowed-source-change src/frontend/audio/audioAnalyser.ts
  --font-dir "$AB_FONT_DIR" --pairs 5 --warmups 1
  --settle 20 --seconds 10 --interval 250 --order alternating --seed 42
)
"$ORACLE_NODE" "$AB_COLLECTOR" "${AB_ARGS[@]}" --dry-run --output "$AB_RESULTS/preflight"
"$ORACLE_NODE" "$AB_COLLECTOR" "${AB_ARGS[@]}" --output "$AB_RESULTS/measured"
```

The two output directories must be new/empty. Supply the same existing
`WEBKIT_WEBDRIVER`, `TAURI_DRIVER`, native runtime/font environment and audio
provider policy. `AB_GIT_ROOT` must contain both immutable commit objects.
The collector creates one private software Xvfb and per-run private profiles,
XDG paths and session buses. It checks identical normalized seed, fonts and
1280×800/DPR1 visible empty-editor workload. Reserve roughly8–10 minutes; take
process inventories before/after. There are no automation calls or profiling
in the steady interval. Hashing and preflight warm OS caches; per-run application
caches remain fresh. This is not a cold-start or physical-hardware experiment.

Audio overrides (`PULSE_SERVER`, sink/source/latency, ALSA/GStreamer selections)
are recorded. The operator must separately freeze/record the actual provider;
unset overrides do not prove its availability or select a fabricated backend.
Externally activated/shared audio services remain outside app-tree accounting.
The LTC library override is recorded with other selected runtime paths.

Require report `completed`, all12 runs valid, profiles removed, support cleanup
complete, Xvfb closed, and final supervisor exit0/complete/no remaining adopted
children. Inspect exact membership/reasons and scan failures. Require full
steady-sample PSS coverage for the primary comparison; retain incomplete raw
receipts without substituting selected subsets. Hash report/supervisor/raw files
afterward. Report both paired CPU differences and run-level distributions.
PSS is proportional memory, summed RSS double-counts shared pages, and `/proc`
CPU remains an observed lower bound unless an already-delegated cgroup supplies
subtree counters. No delegation or privileged setup is invented. Process
termination/reaping is not proof of graceful native CLOSE; startup remains
diagnostic even with two Tauri adapters. No automatic speedup ratio is produced.

## Focused verification

Pinned Node22.21.1 passes14 tests: the original eight plus six new groups for
two-Tauri labels/order/warmups, bounded options, actual private Git/filesystem
receipts and tampering, normal build/resource/hook/dirty-source rejection,
source/tool/profile controls, symlink/sparse-size bounds and fail-closed live
hook state. Controlled artifact/log fixtures are never executed as apps or
represented as genuine builds. Existing real fork/setsid/reparenting/subreaper
cleanup still executes independently. CLI help, syntax, Prettier and whitespace
checks pass. No actual two-Tauri GUI/preflight-on-release/measurement has run.

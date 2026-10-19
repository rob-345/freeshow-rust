# Production AudioAnalyser idle ablation

On 2026-10-19 the reviewed collector ran two normal production Tauri releases on
this Linux x64 host. The lazy variant had lower observed app-tree CPU and PSS in
all five measured pairs for this one visible, empty-editor workload. This is an
archived experiment, not a rerun, startup result, full-application benchmark,
hardware claim or Electron comparison. The lazy implementation is not imported
by this evidence commit.

## Frozen controls

Both releases use base `a3cbb911872eb324aa8727eb56a90c52be1443ba` plus the separately
verified AudioPlayer CORS correction `abf5faed`. Clean eager source is
`1836291624877fbdbb23f46d470422e2e0e79968`; clean lazy source is
`a3d95c3377332cbb53c932806de17a24faa30c95`. Their complete Git diff is only
`src/frontend/audio/audioAnalyser.ts`: normalized eager SHA256 `ff000b9d…`, lazy
`20630ed8…`. No full-fork observation hook, proxy or extra diagnostic packet IPC
is in either production build. Actual WebView checks found no known hook globals
before or after any run.

Node22.21.1 and Rust1.99.1 built normal production frontends and locked
`custom-protocol` releases with identical inputs, native configuration and
features. Independent real node_modules and physical target/release directories
have no shared regular-file inodes. The lazy build reused a by-value copy of the
new eager compiler cache, then explicitly cleaned/rebuilt only the desktop
package and ran its actual normal frontend/native commands. Historical caches,
original Electron and root build outputs were untouched.

| Artifact                                                     | SHA256                                                             |
| ------------------------------------------------------------ | ------------------------------------------------------------------ |
| Eager executable, 29,798,176 bytes                           | `390f9a51daf0edf99fe5e035ad2924c4ba2c83ca8a3cb8b6ace8303703a2e61e` |
| Lazy executable, 29,794,464 bytes                            | `35e39dd0c3cf419b78fabaf23870e4ba8e907c4c9bd5022df041bb7a33a2bb41` |
| Identical actual remotes tree, 20 files/three client indexes | `d7ccf33b625a1b3c6477bb940e939153eef791450940a8a547c4665caa9da945` |
| Compact archived receipt                                     | `3c8870716a35962be65ba3c40d34ae50169c7351abe1a1d94a51ad42a08ec82c` |

The build receipts are trusted-builder observations of real commands, inputs,
logs and artifacts, not independent embedded-binary reconstruction. All locks,
tools, selected build/Vite environment and remotes bytes agree. The reviewed
collector is `db99d51425a90eb62f82a6f92d226b69f29fb787`; its runtime/metrics/adapters
are unchanged. Archived provenance contains the actual runner and operator
hashes, build manifests/receipts, tool versions and environment.

## Workload and measurements

One warmup pair precedes five measured pairs, alternating order with seed42.
Each fresh private app/library/XDG profile settles20 seconds, then samples10
seconds at250ms. Warmups remain archived but are excluded from these summaries.
Both sides use the same initialized English empty-library seed, visible retained
Svelte editor, `tauri://localhost`, 1280x800 CSS pixels/DPR1 and software-oriented
Xvfb. No media, projection or network server workload is enabled. There are no
automation calls during the steady interval. OS/file caches are warmed, not cold.

The host is Ubuntu24.04, kernel6.8.0-146-generic, Intel i5-8500T (six permitted
CPUs), WebKitGTK2.52.6. The142-font manifest is
`b67076a7bef8c27a396867a82d3a55e6d235760274867a5f006ab3574445aa1a`.
The actual native SDK/WebDriver/LTC overrides and static-file checks are retained.

Each of the12 runs has42 steady snapshots and four observed living processes.
All504 snapshots have complete PSS for that observed membership and zero process
scan errors. All measured CPU deltas use `/proc` counters at CLK_TCK=100 and the
recorded collection-end wall boundary. They remain an **observed lower bound**:
unseen/escaped helpers and CPU after an exiting helper's final observation can
be missed. Every steady scan also records10 unreadable markers for unmatched
same-UID processes; these are retained attribution limits, not silently declared
unrelated. No delegated exact cgroup accounting was available.

PSS is the proportional share of resident mappings among host sharers, not unique
physical RAM. Per-run PSS below is the collector's nearest-rank median of42
complete sample sums (the21st sorted value), not the average of the middle two.
Summed RSS remains in the receipt but double-counts shared mappings.

| Pair | Actual order | Eager observed CPU (% of one core) | Lazy observed CPU | CPU change (percentage points) | Relative observed CPU change | PSS change (MiB) |
| ---- | ------------ | ---------------------------------: | ----------------: | -----------------------------: | ---------------------------: | ---------------: |
| 1    | Lazy → eager |                           20.11636 |           8.63927 |                      −11.47709 |                   −57.05352% |     −5.091796875 |
| 2    | Eager → lazy |                           20.30081 |           8.51755 |                      −11.78326 |                   −58.04331% |     −7.878906250 |
| 3    | Lazy → eager |                           20.58788 |           8.63993 |                      −11.94794 |                   −58.03388% |     −6.962890625 |
| 4    | Eager → lazy |                           17.82211 |           8.81537 |                       −9.00673 |                   −50.53687% |    −11.629882813 |
| 5    | Lazy → eager |                           20.02473 |           8.46235 |                      −11.56238 |                   −57.74052% |     −5.443359375 |

All changes are lazy minus eager within the same pair. Median paired observed
CPU change is **−11.562381413726335 percentage points**, with median paired
relative change **−57.74051855606777%**. Median paired PSS change is
**−7,301,120 bytes (−6.962890625 MiB)**; median paired relative PSS change is
**−2.050193805136699%**. These describe the recorded workload/counters, not a
universal true-CPU saving or physical-memory reduction.

Separately, the median across five per-run CPU values is20.11635752916642% eager
versus8.639267121889885% lazy. The median across per-run PSS medians is
339.62109375MiB eager versus332.197265625MiB lazy. Differences of these side
medians are not the paired-median estimators above. No confidence interval or
population/general-hardware inference is asserted from five pairs on one host.

## Provider and real cleanup

A single actual Pulse16.1 process, PID3664254/startTicks48828440, owns the private
socket throughout all12 application lifetimes. It uses the functional-control
`module-native-protocol-unix` plus `module-null-sink` at48000Hz/two channels,
`PULSE_SINK=freeshow_fullfork`, with no forced sample-format/source/latency change.
Actual before/after PID/starttime/executable, socket inode/uid/SO_PEERCRED,
server/modules/sink/source JSON match the held process policy. No Pulse restart
occurs between variants. The external provider and any system-activated services
are excluded from app-tree CPU/PSS; this is not whole-system CPU accounting or
physical microphone/speaker evidence.

The operator retains and actually waits its direct collector subreaper and
Pulse handles. Both returned0 without forced operator kills. The fixed bounded
log thread observed EOF and joined, with901 bytes kept/no omission. Static145
file checks and executed-script hashes match before/after, the private socket
folder was removed after process exit, and operator failures are empty.

Every app profile was removed and per-run cleanup is complete. Some per-run
receipts retain transient zombies; the final actual subreaper receipt records
181 reaped children (the collector plus180 other descendants), exit0/complete
and no remaining adopted children.
This proves recorded process cleanup. It does **not** prove graceful native
CLOSE, accepted-work/service drains or individual native thread joins: the
collector adapter's teardown and process termination are not those contracts.
The earlier separate full-fork functional controls provide their own normal
CLOSE/audio evidence; they are not substituted for this release experiment.

## Archive and independent verification

`tests/desktop/audio_context_ablation/receipt.json` is a323,861-byte projection
of the actual completed run. It retains all12 raw steady sample inputs packed
under explicit column names, per-process CPU/PSS/RSS/state/identity data,
per-run process histories, visible states, cleanup, build/provider/operator
provenance, final reaps and unrounded summaries. A134-file SHA256 manifest covers
the full local raw artifacts (including hashes of screenshots); screenshot and
full JSONL/log payloads stay at
`/mnt/data/cache/freeshow-rust-build/audio-ablation-a3/results` and are not copied
into Git. No source/parser/audio values or measurements are synthesized.

Two independent implementations recomputed all12×42 sample sums, tick deltas,
wall denominators and nearest-rank medians: a direct Python raw pass and the
committed Node validator. Node22.21.1 also verifies the archive against all134
raw files and both actual binaries when `--raw` is selected. Four corruption
controls reject altered CPU totals, changed process PSS, false supervisor drain
and accidentally included warmup. The default is a portable archival replay;
it requires neither apps, codec packages nor the cache. It never starts a
provider, collects new samples or represents an absent cache as a fresh pass.

```sh
node tests/desktop/audio_context_ablation/validate.mjs --self-test
# Explicit original local raw-cache comparison (fails if absent/changed):
node tests/desktop/audio_context_ablation/validate.mjs --raw --self-test
```

These checks, syntax/formatting and whitespace validation pass. The evidence
commit contains no runtime implementation, collector mutation or new app run.

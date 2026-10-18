# Linux release empty-editor measurements

On 2026-10-18, the official FreeShow v1.6.7-beta.2 Electron package and the
immutable Rust release at `c9e62492` completed one warmup pair and five measured
pairs. The Rust build is a **partial migration** from the files milestone, before
system/API/import/browse host integration. The workload is the same visible,
empty-library editor, not the full application feature set.

The median of five per-run median process-tree PSS measurements was **517.38 MiB
for Electron and 319.92 MiB for Rust**. The median observed CPU measurement was
**11.49% and 17.63% of one logical core**, respectively. Thus this workload shows
lower proportional resident memory and higher observed idle CPU for this partial
Rust build. It does not show an overall application speedup. The CPU result is a
regression candidate to investigate in the retained WebKit rendering workload;
its sampled accounting remains a lower bound.

| Pair | Order | Electron PSS MiB | Rust PSS MiB | Electron observed CPU | Rust observed CPU |
| --- | --- | ---: | ---: | ---: | ---: |
| 1 | rust → electron | 517.04 | 319.54 | 10.61% | 17.63% |
| 2 | electron → rust | 518.10 | 319.92 | 11.49% | 17.93% |
| 3 | rust → electron | 518.69 | 319.64 | 11.50% | 17.33% |
| 4 | electron → rust | 517.38 | 324.01 | 10.67% | 17.32% |
| 5 | rust → electron | 516.02 | 322.29 | 11.59% | 20.02% |

All ten measured runs were valid, with PSS for every observed living process in
all **42 steady snapshots per run** (210 per runtime). Both warmups also had
complete coverage and are excluded from the table/summary. Application processes
include observed Chromium/WebKit helpers; automation, observer, private Xvfb and
session buses are outside that boundary. PSS is a proportional share among host
sharers, not a unique physical-memory total. Summed RSS counts shared pages more
than once and is retained separately in the receipt. Process attribution cannot
certify an unseen helper that escapes all selection mechanisms.

Both editors had 1280×800 CSS content at DPR 1, the same normalized private seed,
English UI and font inventory, no shows/media/projects, no projection and disabled
configurable network services. One private software-oriented Xvfb served all
runs. Each app settled for 20 seconds and was sampled for 10 seconds every 250 ms,
with seed-42 alternating paired order. Application/XDG caches were fresh on every
run; the OS page cache was uncontrolled/warmed. Goal workers, builds and other
goal GUI tests were paused; normal machine background services remained, and
host load was recorded. No automation requests or screenshots occurred in the
steady interval.

CPU uses each observed PID/start-time identity's own user/system counters, without
reaped-child counter double counting. Short-lived or exiting helpers can lose CPU
between polls. There was no delegated cgroup, so this is not exact process-tree
CPU accounting. Different Electron debugger and Tauri WebDriver launch paths make
startup diagnostic only. One host/idle workload does not certify production
hardware, sustained playback, frame timing, loaded libraries or feature parity.

The full run completed from `2026-10-18T17:35:51.253Z` to
`2026-10-18T17:42:33.863Z`. All twelve private profiles, observed app process trees,
session buses and Xvfb were cleaned. The outer subreaper exited successfully with
no remaining adopted children and no forced adopted-child kills. The exact Rust
binary was archived outside the build target before further work resumed:
`/home/rob-admin/.local/share/freeshow-rust-baselines/c9e62492/freeshow-desktop`,
SHA256 `b1924acd481ec61baf80105be0734c4f7805caf64213d82ae0c81eda73909fbf`.

[Committed metrics, per-run summaries, provenance and raw-file hashes](release-idle-editor-linux.json)
identify the binaries, tools, fonts, settings and local raw evidence. The ignored
raw directory is
`test-output-rust/release-comparison-full-20261018`. See the
[runner contract and reproduction command](../workers/release-comparison.md).
This records the measured result, including the unfavorable CPU observation;
no startup ratio, significance claim or universal performance target is inferred.

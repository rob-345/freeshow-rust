# Native system information

Owned slice: `crates/freeshow-system/**`, `tests/parity/system-info.test.mjs`, this document. The worker initially left shared integration files untouched and validated a standalone crate. Coordinator integration now uses the root workspace/shared locks and `src-tauri/src/system.rs`; host tests are in `src-tauri/tests/system.rs`.

## Host integration

```rust
use freeshow_system::{JobControl, Limits, SystemService};

let system = SystemService::start(Limits::default())?;
// Existing MAIN responses; serialize each returned value as the reply data.
let os = system.get_os(JobControl::default()).await?;                // GET_OS
let name = system.get_device_name(JobControl::default()).await?;    // GET_DEVICE_NAME
let ram = system.ram_usage(JobControl::default()).await?;           // CHECK_RAM_USAGE
system.shutdown().await?;
```

The cloneable service owns one dedicated native OS/proc thread. Default immediate admission is eight queued requests plus the active request; overload returns `system_busy`. Queue capacity has a hard maximum of 32. A `JobControl` supplies a per-request cancellation token and deadline; the default maximum job duration is two seconds, configurable up to ten seconds. Dropping an admitted request future cancels its token; normal completion and failed admission leave that token unchanged. Use separate tokens for requests whose cancellation should be independent.

Preserve the existing IPC envelope/context/role checks and renderer correlation. These global system reads do not require the ordered library owner or a backup/archive queue. The host should close/drain this service during its normal shutdown sequence; queued and active work settles with `system_shutdown`, and new admission is terminally rejected. Cancellation/deadline checks happen before and after the OS operation, including after an active operation completes, so a cancelled/stale result is discarded. Already-running OS APIs cannot be interrupted; a stuck provider/proc mount can delay the single worker and shutdown. The host can bound its shutdown wait without creating replacement threads. No OS/proc calls run on GTK, the Tokio IO reactor, its general blocking pool, or a library-owner lock. There is no Node dependency or process-per-request backend.

Public replies match `src/electron/IPC/responsesMain.ts` at source commit `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`:

- `OsInfo { platform, name, arch }`: actual native hostname, Node platform aliases (`linux`, `darwin`, `win32`) and aliases for the compiled process architecture (`x86_64→x64`, `x86→ia32`, `aarch64→arm64`, plus supported ARM/PPC64/s390x/RISC-V/LoongArch/MIPS aliases). Architecture is the binary's target, matching [Node's `os.arch()` contract](https://nodejs.org/download/release/v22.21.1/docs/api/os.html#osarch), not a kernel/uname CPU guess. Unsupported OS/architecture combinations return `system_unsupported` rather than invented aliases.
- Device name: the first literal `.local` occurrence is removed anywhere in the native hostname, case sensitively. This intentionally preserves the original behavior for `a.local.local→a.local` and `a.localhost→ahost`.
- `RamUsage { total, free, performanceMode }`: byte counts converted to JS-compatible f64 numbers. The decision retains the exact operation order `total/1024/1024/1024 <= 8 || (total-free)/total > 0.98`, with no rounding, clamping or changed threshold. NaN/infinite/zero-denominator behavior is covered only at the pure-function boundary. Actual failed OS APIs or zero native total memory return `system_os`; they do not fabricate zero-memory success or set optimized mode from an invalid snapshot.

`memory_sample(control)` additionally returns internal `MemorySample { total, free, totalSource, freeSource }`; `.usage()` produces the renderer reply. The source labels are evidence for diagnostics/tests and should not enter product UI. `Error` has `code` and `message`; other explicit errors are `system_invalid`, `system_cancelled`, `system_deadline`, `system_worker` and Windows malformed UTF-16 `system_encoding`.

## Native API semantics

The untouched official application reports Electron `37.10.3`, Node `22.21.1` and embedded libuv `1.51.0`. This slice follows that pinned backend rather than substituting a general system-monitor crate:

| OS      | Hostname                                      | RAM total                                                          | RAM free                                                           |
| ------- | --------------------------------------------- | ------------------------------------------------------------------ | ------------------------------------------------------------------ |
| Linux   | native `gethostname`, bounded 65-byte buffer  | `/proc/meminfo` `MemTotal`, fallback `sysinfo.totalram * mem_unit` | `MemAvailable`, fallback `sysinfo.freeram * mem_unit`              |
| macOS   | native `gethostname`, bounded 257-byte buffer | `sysctl(CTL_HW, HW_MEMSIZE)`                                       | `host_statistics(HOST_VM_INFO).free_count * sysconf(_SC_PAGESIZE)` |
| Windows | Winsock `GetHostNameW`, 256 UTF-16 units      | `GlobalMemoryStatusEx.ullTotalPhys`                                | `GlobalMemoryStatusEx.ullAvailPhys`                                |

Linux uses separate bounded reads for total/free, each at most 4095 bytes, and KiB fields become bytes. Missing, zero or unparseable proc values trigger the original `sysinfo` fallback. The proc helper retains unsigned conversion and substring lookup behavior; tests distinguish `MemAvailable` from `MemFree`. It does not substitute process RSS, cgroup-constrained memory or swap. Exact pinned implementation: [libuv Linux RAM](https://github.com/libuv/libuv/blob/v1.51.0/src/unix/linux.c#L1944), [bounded proc read](https://github.com/libuv/libuv/blob/v1.51.0/src/unix/core.c#L1075); the [kernel proc documentation](https://docs.kernel.org/filesystems/proc.html) describes these memory fields.

macOS free pages are deliberately not augmented with inactive/reclaimable pages. The Mach host handle uses the maintained `mach2` binding; statistics/sysctl use their native ABI. See the [pinned Darwin implementation](https://github.com/libuv/libuv/blob/v1.51.0/src/unix/darwin.c#L92) and [Apple VM statistics definitions](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/vm_statistics.h). Unix hostname buffer/termination behavior follows [libuv's native gethostname wrapper](https://github.com/libuv/libuv/blob/v1.51.0/src/unix/core.c#L1473).

Windows calls the exact wide Winsock API used by [libuv `uv_os_gethostname`](https://github.com/libuv/libuv/blob/v1.51.0/src/win/util.c#L1329), with balanced per-call `WSAStartup`/`WSACleanup` references. It does not replace that API with `GetComputerNameEx` or DNS-name guessing. [Microsoft's GetHostNameW contract](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-gethostnamew) allows a simple hostname or FQDN depending on namespace providers, with NetBIOS fallback and no IDN conversion. It may honor `CLUSTER_NETWORK_NAME` on cluster resources; preserving that native behavior is intentional. The application does not read `HOSTNAME`, `HOST`, or `COMPUTERNAME` as substitutes. Memory fields follow [pinned libuv](https://github.com/libuv/libuv/blob/v1.51.0/src/win/util.c#L263) and [GlobalMemoryStatusEx](https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-globalmemorystatusex); this is physical memory, excluding page-file totals. Total/free remain separate API samples, as in the source.

## Verification and remaining gaps

Linux real execution and actual packaged Electron source comparisons are verified. Windows x86-64 and macOS ARM64 pass cross-target `cargo check` and clippy with warnings denied, including tests/bin code; those OS backends are not linked or executed here. Native hostname-provider behavior, Windows cluster/IDN edge cases and actual macOS free-page behavior remain platform validation gaps. Other OS backends return explicit unsupported errors. No GUI interaction changes are necessary or claimed; Svelte's existing startup optimized-mode handler is retained for coordinator wiring.

The source differential suite extracts and executes the actual three upstream functions with only OS boundaries injected. It covers inclusive eight-GiB/strict 98% boundaries and neighboring f64 values, seeded memory pairs, pure nonfinite/invalid denominators, first replacement/Unicode names, target aliases, spoofed environment values, actual stable native snapshots and explicit cancelled/expired requests. Seven lifecycle/backend tests plus the native invalid-total test cover queue overload, event-loop progress, deadlines through queue wait, active/queued shutdown settlement, cancellation, hard configuration limits, proc semantics and real native provenance.

The packaged-app probe launches the unchanged official release under Xvfb with private disposable app/XDG directories. It evaluates the extracted source functions using the actual embedded `os` module, compares stable hostname/platform/architecture/total fields, and evaluates the original RAM decision again using the native sample's exact captured inputs. Independently changing free-memory readings are recorded without an equality assertion or tolerance. `test-output-rust/system-info/electron-system.json` records the source-function checksum, executable checksum, complete runtime versions and separate samples. Initial verified total was `16565227520` bytes; source/native free samples differed (`13189349376` versus `13188993024`) while both decisions were false. Comparisons assume the Node/Electron and Rust binaries target the same process architecture.

Focused commands in the isolated worktree:

```sh
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$PWD/target/system" cargo test --manifest-path crates/freeshow-system/Cargo.toml --locked
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$PWD/target/system" cargo clippy --manifest-path crates/freeshow-system/Cargo.toml --all-targets --locked -- -D warnings
CARGO_BUILD_JOBS=1 CARGO_TARGET_DIR="$PWD/target/system" cargo build --manifest-path crates/freeshow-system/Cargo.toml --locked
FREESHOW_SYSTEM_PARITY="$PWD/target/system/debug/freeshow-system-parity" node --test tests/parity/system-info.test.mjs
FREESHOW_SYSTEM_PARITY="$PWD/target/system/debug/freeshow-system-parity" xvfb-run -a node crates/freeshow-system/tools/probe-electron.mjs
```

Cross checks use the same isolated target with `--target x86_64-pc-windows-gnu` and `--target aarch64-apple-darwin`, `--all-targets --locked`; clippy adds `-- -D warnings`. After integration, the test/probe default is `target/debug/freeshow-system-parity` (`.exe` on Windows). `FREESHOW_ELECTRON_BIN` selects the extracted official executable and `FREESHOW_SYSTEM_ARTIFACTS` overrides receipt location. The packaged probe currently requires Linux/Xvfb. Shared CI/runtime integration remains coordinator-owned.

## Coordinator integration

The three native MAIN commands now use this service. The host validates the captured context before admission and again before publishing, retaining listener IDs and rejecting obsolete/missing contexts. Native closing drains this worker alongside other independent services. The prior GET_OS environment-variable fallback has been removed; hostname now comes from the native OS. Three host tests exercise replies while the ordered library owner is deliberately blocked, context rejection and terminal shutdown. The Linux GUI probe compares actual source OS/device/total-memory results despite a spoofed HOSTNAME environment, then re-evaluates the original RAM decision on the exact native captured total/free values during real ZIP compression. Free readings taken at different instants are never asserted equal. Startup keeps the existing Svelte optimized-mode handler. Combined local validation passed 100 Rust, 104 Node and 20 frontend tests (224), plus 18 native host tests, 97 editor steps, four GTK menu checks and drops at both scales. Linux/Windows GNU Clippy and macOS ARM64 system-crate Clippy passed with warnings denied. Native Windows/macOS execution remains pending in CI.

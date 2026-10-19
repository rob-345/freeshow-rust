# Native DEVICE_ID preparation

The immutable `MAIN.DEVICE_ID` response calls `getMachineId()` with no payload. Its global app-data `config.machineId` takes precedence whenever JavaScript considers the stored value truthy. This includes numbers, booleans, arrays and objects: the source does not coerce them to strings. Only a missing/falsy value invokes `node-machine-id` 1.1.12, followed by a real UUIDv4 if that legacy provider throws. A config load/get/set or entropy failure reaches the outer catch and returns the literal `fallback-machine-id`.

`freeshow_system::device_id` supplies synchronous native preparation and a typed persistence obligation. It starts no thread, task, process, timer or service and writes no file/registry/config. The coordinator must execute it on the existing retained, bounded, ordered global CONFIG owner. Do not call it on a Tokio executor thread, move it to an unbounded blocking task, or prepare concurrent candidates outside that owner. Do not relocate the identity to the selected library or reset it on a library switch.

```rust
use freeshow_system::{device_id::{self, ConfigSnapshot, WriteResult}, JobControl};

// Inside the real retained ordered native CONFIG operation, after context checks:
let snapshot = ConfigSnapshot::Read {
    value: actual_global_config_machine_id, // None for absent getter/key
    can_set: true,
};
let prepared = device_id::prepare(snapshot, &JobControl::default())?;
let write = match prepared.write_intent() {
    None => WriteResult::NotRequired,
    Some(intent) => {
        // The native store root/key and context remain guarded here.
        match actual_global_config_set(intent.key(), intent.value()) {
            Ok(()) => WriteResult::Committed,
            Err(_) => WriteResult::Failed,
        }
    }
};
let original_reply_data = prepared.finish(write)?;
// Publish original MAIN.DEVICE_ID/listener reply only after the real write.
```

The snippet's CONFIG accessors are host obligations, not new functions in this crate. `ConfigSnapshot::ReadFailed` represents an actual config require/get failure and yields the outer literal without invoking either provider. It must not swallow context rejection, capacity, cancellation or lifecycle errors. An absent source setter uses `can_set: false` and returns the acquired value without a write. An actual failed write finalizes to the outer literal even after a legacy/UUID candidate was prepared; it does not publish that candidate. Wrong/missing persistence-result categories return `device_id_commit` instead of inventing a receipt. `finish(Committed)` is a trusted host assertion, not filesystem proof.

Snapshot, prepared identity and write intent have no Deserialize, Serialize, Clone or Debug implementation. The write key is always `machineId`; no path or executable comes from the renderer. Safe origin categories are exposed separately. Stored JSON is preserved, including unknown nested keys and own `__proto__` data. CONFIG's existing parser remains unchanged; finite JSON semantics are the input boundary. No identifier or raw provider output is logged. Actual native/source comparisons return only a per-test randomly salted digest.

## Native legacy compatibility

The native provider retains the dependency's default SHA256 result, rather than returning its original raw GUID. Exact source output parsing is tested using the installed, checksum-verified 1.1.12 code, with OS boundaries supplied by fixtures.

- Linux preserves `(cat /var/lib/dbus/machine-id /etc/machine-id ... || hostname) | head -n 1 || :`: concatenate without inserting a newline, stop at the first real LF, and append hostname only after a genuine cat failure if no LF has already fixed the result. A first-file line wins even if the second file differs. Missing/failed first files, no-newline prefixes, directory failures, empty files and followed symlinks are covered with real temporary files and the original shell pipeline. The native implementation opens only the two fixed paths; it uses nonblocking open, then requires regular files (directories are actual cat failures). Hostname uses the existing bounded native system query. Real empty/empty or an actual initial empty line hashes the empty string, preserving the original forced-success quirk. A failed native hostname query is an explicit OS error, never fabricated empty-provider success.
- Windows reads `HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid` with `RegGetValueW`, bounded `REG_SZ` data and the matching process registry view. The 32-bit mixed-architecture `PROCESSOR_ARCHITEW6432` choice is frozen at first provider use, matching the dependency's module-level decision; x64 uses the native 64-bit view. The canonical query result is projected through the original first-`REG_SZ` split, exact whitespace removal and lowercase rule. Missing/denied/wrong-type values are certified legacy failures eligible for real UUID fallback. Oversize, noncanonical UTF-16/NUL data and unexpected native API statuses are explicit resource/capability/OS errors, not newly persisted identities. No registry writes or spawned REG.exe occur.
- macOS uses the public IOKit/CoreFoundation C ABI for `IOPlatformExpertDevice`'s `IOPlatformUUID`. The matching dictionary is consumed by `IOServiceGetMatchingServices`; the iterator, returned service, property and key are held with RAII and released on every path. At most two entries are inspected, and multiple matching platform experts are explicitly unsupported instead of guessing ioreg ordering. Canonical ASCII hex/hyphen CFString values are projected through the dependency's first `IOPlatformUUID` line, quote/equals/whitespace removal and lowercase. Missing service/property is eligible for UUID fallback. Other property types/representations are explicitly unsupported. Native execution remains required before claiming macOS support; compiler success alone is insufficient.

Primary Windows reference: [RegGetValueW](https://learn.microsoft.com/en-us/windows/win32/api/winreg/nf-winreg-reggetvaluew) defines the registry type filter, returned byte count and view flags.

The manually declared macOS ABI is checked against immutable Apple source headers, separately from compilation:

- [IOKitUser-100222.60.2 IOKitLib.h, lines 340–396](https://github.com/apple-oss-distributions/IOKitUser/blob/IOKitUser-100222.60.2/IOKitLib.h#L340) declares iterator validity as Mach `boolean_t`, matching-service lookup as `kern_return_t`, and documents that lookup consumes one matching-dictionary reference on every result. Returned services and iterators require `IOObjectRelease`; the implementation uses RAII on all paths.
- [XNU-11417.140.69 ARM boolean.h](https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/osfmk/mach/arm/boolean.h#L65) defines Mach `boolean_t` as signed `int`; [its i386 header](https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/osfmk/mach/i386/boolean.h#L65) defines the x86_64 user-space type as unsigned `int`. The Rust declaration uses the corresponding 32-bit signed/unsigned type, rather than Rust `bool`.
- [XNU IOTypes.h](https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/iokit/IOKit/IOTypes.h#L184) aliases service/iterator handles to Mach ports. [CF-1153.18 CFBase.h](https://github.com/apple-oss-distributions/CF/blob/CF-1153.18/CFBase.h#L85) and the included [XNU OSTypes.h](https://github.com/apple-oss-distributions/xnu/blob/xnu-11417.140.69/libkern/libkern/OSTypes.h) define CoreFoundation `Boolean` as unsigned char; [CFString.h](https://github.com/apple-oss-distributions/CF/blob/CF-1153.18/CFString.h) declares `CFStringGetCString` with that return type. The Rust return type is `u8`; CF index/type IDs use pointer-sized signed/unsigned types. UTF-16 lengths, fixed UTF-8 conversion and embedded-NUL checks reject unsupported representations. Created CF strings/properties use `CFRelease` RAII.

These are declaration/ownership certificates, not native runtime evidence or proof that the provider value matches a particular machine's ioreg output.

The native getters replace the dependency's local command execution; CLI availability/failure equivalence is not claimed when those original system tools are missing or altered. Ordinary canonical identity values are the compatibility target. Non-ASCII identity content and ill-formed UTF-8/UTF-16 are explicitly unsupported: JavaScript would lowercase/replace some of these spellings, and the native capability does not guess Unicode-table or command-encoding behavior. All source ECMAScript whitespace characters are handled exactly, including BOM, NBSP and U+2028/U+2029; they may surround otherwise ASCII tokens. macOS multiple-provider ordering is explicitly unsupported; 32-bit Windows execution is not runtime-certified by the Linux checks.

## Bounds and lifecycle obligations

Stored JSON is limited to 8192 serialized bytes, depth eight and 256 nodes including object keys, before cloning a truthy value. A counting writer avoids serializing an extra huge buffer. Provider data is bounded to 16384 bytes; Linux performs at most 64 bounded reads per fixed file, and Windows/macOS use fixed buffers. UUID generation requests 16 real OS-random bytes, masks RFC4122 version/variant bits and formats lowercase UUIDv4. A truthy stored value never invokes provider, entropy or persistence. No random UUID is generated after resource/capability/cancel rejection.

Preparation clips the supplied `JobControl` deadline to at most two seconds and checks cancellation/deadline around bounded CPU work and native calls. A blocked OS syscall cannot be preempted by that deadline. These are cooperative work bounds; the actual host owner must remain retained until the synchronous operation physically returns and its dependencies close. There is no independent async observer, ready-result lease, context/run owner or join proof in this module. Caller Drop/terminal/context safety and finite whole-request admission belong to the real ordered CONFIG host. Native registry, IOKit, filesystem, hostname and OS-random calls have no hard interruption guarantee; cancellation/deadline checks only observe their eventual return. Unsupported provider-output representations and uncertified API errors remain explicit errors rather than UUID or literal fallback. The module cannot prove a transferred write has happened, replace a failed join, or publish a new source success ACK.

Root integration requires the `freeshow-system` dependency additions (`sha2` 0.10.9 and `getrandom` 0.4.3 from the existing lock, plus `tempfile` for tests), the Windows `Win32_System_Registry` feature, and the narrow public module export. `serde_json/preserve_order` is already used by the shared runtime; making this crate's own requirement explicit preserves stored object order in standalone tests without enabling float parser/precision features. The root lock needs only the three dependency names added to `freeshow-system`; no package version changes are needed. Coordinator owns that lock update, native command routing, full-context/window checks, real config commit and original listener reply.

## Verification

Final focused checks passed: 16 Rust tests (eight existing system cases and eight new identity cases), three Node source tests, and strict all-target Clippy for Linux, Windows GNU and macOS ARM. The identity cases include an actual native Linux versus installed-source salted comparison, ten real private-file source pipeline fixtures, cancellation/deadline/resource rejection, real UUID generation, stored raw truthy bypass and actual source outer-failure behavior. Windows/macOS native source comparisons are committed for those CI jobs; they have not executed on this Linux machine. No GUI, native app, release build, performance measurement, real config write, external provider or cloud request is part of these tests.

The source oracle extracts the original helper and actual `MAIN.DEVICE_ID` property from immutable commit `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`, then executes the actual code in one realm with controlled store/crypto boundaries. It verifies declaration hashes and the installed package version/dist hash. Provider cases execute the actual pinned dependency, not a copied parser. The actual OS comparison never outputs the stable identifier or its unsalted hash. All three native CI platforms must run it; only genuinely Linux-specific filesystem fixtures are gated by OS.

```sh
export PATH=/path/to/node-v22.21.1/bin:$HOME/.cargo/bin:/usr/bin:/bin
export CARGO_TARGET_DIR=/mnt/data/cache/freeshow-cover-native-portability
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo +1.99.1 test --offline --locked -p freeshow-system --all-targets
cargo +1.99.1 clippy --offline --locked -p freeshow-system --all-targets -- -D warnings
cargo +1.99.1 clippy --offline --locked -p freeshow-system --all-targets --target x86_64-pc-windows-gnu -- -D warnings
cargo +1.99.1 clippy --offline --locked -p freeshow-system --all-targets --target aarch64-apple-darwin -- -D warnings
node --test tests/parity/device-id.test.mjs
```

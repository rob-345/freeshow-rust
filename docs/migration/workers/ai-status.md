# Explicit Nemotron integrity status foundation

`freeshow-ai-status` implements only original `MAIN.AI_GET_STATUS` with explicit
`engineId: "nemotron"` and absent/falsy `modelId`. It is not wired to native IPC by
this patch. Other engines, all-provider status, inference, model setup/download,
cloud credentials, Ollama health and truthy model selection remain unsupported.
No models were downloaded or executed.

The immutable oracle is commit
`2d94defcf98175e4f5c9504e7f4fd1d33c35c975`. The new source helper verifies normalized
SHA-256 for the entire `setup/status.ts`, `LocalModelManager.ts`,
`models/nemotron.ts` and `DownloadManager.ts`, and executes them with TypeScript
and a controlled VM. It also executes the original `AI_GET_STATUS` response entry
and `receiveMain`. Filesystem calls, SHA-256 and stream pipeline are real Node
22.21.1 operations. Electron app userData/platform and unrelated provider/network
boundaries are controlled; any unrelated provider/setup/network invocation fails
the fixture. This is original-source execution under Node, not a packaged Electron
application or native macOS/Windows runtime receipt.

## Native integration API

All methods are synchronous. Use the coordinator's retained filesystem owner;
never call `prepare()` or `apply()` on GTK/Tokio IO/UI threads. This crate creates
no threads, tasks or runtime, and makes no independent shutdown/join claim.

```rust,ignore
let checker = Checker::new(
    TrustedUserData::from_host(native_app_user_data)?,
    Limits::default(),
)?;
let query = NemotronNoModel::from_payload(&original_payload)?;
let accepted = checker.try_begin(query, Platform::native(), captured_control)?;

// Move accepted into the host's actual retained FS owner before awaiting it.
let prepared = accepted.prepare()?;
let read = match prepared {
    Prepared::Ready(read) => read,
    Prepared::Stamp(intent) => {
        // Move this sealed intent to the actual ordered library IO owner.
        // Revalidate full captured RuntimeContext and native app_data identity;
        // intent.user_data() must equal that native application root.
        // Context change must cancel the original captured Control token.
        intent.apply()?
    }
};
// Full-context validation belongs to host publication. Borrow while serializing,
// then retain this actual read in Event.native_lifetime through clones/delivery.
let source_data = serde_json::to_value(read.reply())?;
let private_write_diagnostic = read.write_diagnostic();
```

`TrustedUserData`, `NemotronNoModel`, `Job`, `StampIntent` and `StatusRead` have no
Deserialize/path-grant surface. `customPath` is ignored on this source branch.
The fixed path is native userData/bin/nemotron/models, **not** selected library
dataPath. The five filenames, revision and SHA-256 values are fixed to source.
The original `modelId` falsiness includes null, false, +0/-0, empty string and
NaN; native typed `ModelId` preserves that choice before JSON projection. Ordinary
JSON callers cannot reconstruct a prior NaN/undefined value; truthy numbers,
arrays/objects and model strings are unsupported, because that source branch can
return an own-undefined `ready` and is outside this foundation.

There are four process-wide whole-job slots across checker replacement. Admission
reserves before per-job paths/read buffers; ownership transfers unchanged through
`Job -> Prepared -> StampIntent/StatusRead`. `StatusRead` offers borrowed reply,
evidence and diagnostic only, with no Clone or consuming wire escape. The actual
read must remain retained through publication. Serializing a borrowed projection
does not by itself retain the lease. `StampIntent` exposes no ready result: only
`apply(self)` after the real write attempt can produce its ready `StatusRead`.
It retains its **original** token/deadline, with no replacement Control argument.

The host owns queued/active futures, caller-drop survival, full-context fencing,
admission closure, actual join/drain and retry. Dropping an observer is not IO
cancellation. Cancellation/deadline checks occur between bounded reads and
operations; a blocking filesystem syscall cannot be interrupted by this crate.
The host must keep the actual operation and its permit until it really returns.
No timeout-as-drain or thread-abort receipt is provided. A completed read remains
a completed observation, not live context or immutable-file authority; host
publication still checks captured context/cancellation.

## Source observations preserved

- Intel Mac returns the exact platform error before filesystem work. That is a
  controlled source/platform case here; no Intel Mac runtime was launched.
- The stamp shortcut checks revision, fixed hash strings, then actual stat size
  and `mtimeMs` for each entry, in source short-circuit order. It stats before
  rejecting wrongly typed size/time fields. Duplicate JSON keys use the last
  value. Unknown finite/overflowing numeric fields are ignored like JSON.parse.
- A genuine valid stamp over tiny/empty files (or even a stat-able directory)
  returns ready **without hashing**. This certifies the source shortcut, never
  usable models, real pinned contents, executable availability or inference.
- Without a valid stamp, all five real hashes run in fixed order. An earlier bad
  digest does not stop the loop; a later missing file returns missing. Exists-probe
  failures return missing on the certified POSIX path; later stat/open/read errors
  are real errors, not false missing/outdated replies.
- Metadata is observed before each hash, as source does. No descriptor/identity
  snapshot is invented. Concurrent replacement/content writes can change what is
  read; restored/coarse timestamps can defeat the source shortcut. This is not a
  media authorization or protected-root confinement API.
- Real successful hashes lead to a direct create/truncate/write attempt for
  `model.json`, fixed original property/file order, four-space JSON, JS number
  formatting, no final newline. There is no mkdir, rename or fsync. A real write
  failure still returns ready, with bounded private `WriteDiagnostic::Failed`,
  mirroring source's ignored write error. Cancellation after actual IO does not
  publish a source success. No public success/source ACK is added.
- POSIX stat milliseconds use separate f64 casts/multiply/divide/add on signed
  seconds/nanoseconds, not integer aggregation or SystemTime normalization. Raw
  stamp number tokens use correctly rounded f64 parsing before strict JS-style
  numeric comparison; normal serde_json decimal reparse is not the oracle. The
  original pre-JSON stat IEEE bits are compared, not tolerant decimal values.

## Hard resource and representation boundaries

| Resource                                                     | Hard bound                                                                                                        |
| ------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------- |
| Retained whole jobs/results/intents across checker instances | 4                                                                                                                 |
| Input selector JSON                                          | depth 16, 4,096 nodes, 64 KiB aggregate key/string bytes                                                          |
| Native root and each fixed full path                         | 32,768 UTF-8 bytes, absolute/scalar/no NUL; no lexical alias components                                           |
| Stamp read                                                   | 64 KiB input plus one bounded growth-detection byte                                                               |
| Stamp JSON preflight                                         | depth 16; 4,096 structural/string/separator tokens before object allocations                                      |
| Hash chunk                                                   | 64 KiB fixed stack buffer, sequential one file at a time                                                          |
| Actual aggregate model hash bytes                            | configurable up to 1 GiB; exact exhausted boundary rejects before an extra EOF read                               |
| Charged operations                                           | configurable up to 32,768; includes each Windows namespace component probe                                        |
| Accepted work deadline                                       | default 10 s, trusted configuration up to 60 s; admission takes minimum with original deadline                    |
| Native evidence                                              | at most 10 stat observations and 5 completed hash names                                                           |
| Sealed stamp/output                                          | five fixed entries, stamp below 4 KiB; reply/evidence serialization below 256 KiB even with escaped path controls |

Preflight bounds precede JSON object allocations. RawValue/object copies remain
bounded by the stamp cap; no untrusted metadata length sizes a buffer. Returned
source fields are fixed, except the bounded native model path. Error/diagnostic
strings are fixed and bounded. There is no directory enumeration or variable
model list. A smaller trusted read/step budget or deadline may reject a source
case; it never fabricates source missing/success.

Oversized/deep/token-heavy stamps, invalid UTF-8, lone UTF-16 surrogate escapes,
unsupported path representations and nonregular hash inputs are explicit native
capability/resource errors, even when unsafe original parsing might continue or
block. Well-formed surrogate pairs are supported. Unix reads use O_NONBLOCK and
reject held nonregular files before hashing, preventing a FIFO from trapping the
worker. Unsafe stamp targets are refused before truncate and yield the original
ignored-write diagnostic. Source-followed POSIX symlinks remain followed; no
anchored/confinement or race-free path check is claimed.

Windows scope is ordinary absolute drive paths (including ordinary verbatim
drive spelling), with device/ambiguous components, UNC/device namespaces and
observed reparse components rejected. Namespace probes are not atomic authority.
Windows failed existence API probes are unsupported rather than assumed missing;
ordinary observed absence still returns source missing.
The FILETIME projection follows the previously verified Node/libuv unsigned32
seconds interval, sec 0..u32::MAX, with 100 ns remainder converted separately;
exceptional dates are explicit unsupported. Windows/macOS compile checks pass;
these new source/FS fixtures still require actual native CI execution there.
macOS POSIX fields keep their signed native seconds/subseconds. Unknown OS
platforms are unsupported. No claim of network-filesystem syscall cancellation,
ACL equivalence outside exercised cases, hardware, codec or performance parity.

## Evidence and reproducible checks

15 Rust tests cover actual fixed-file hashing/missing precedence, genuine stamp
fast path, retained four-slot capacity across replacement/ready/prepared states,
original cancellation/deadline through apply and real chunk reads, operation/read/
JSON limits, denied reads/symlinks/nonregular files, source numeric boundaries,
real filesystem timestamp retention and real accepted worker survival after the
public observer drops. The worker fixture joins its actual thread; the production
crate still creates none. `StatusRead` and `StampIntent` are checked Send + Sync.

11 Node suites execute the original modules/MAIN dispatcher. They compare exact
source reply shapes/keys, original listener propagation, original stat/hash call
order and IEEE bits, Unicode paths, missing/empty/tiny/directory stamp fast cases,
stale/invalid/malformed/duplicate-key stamps, ignored overflowing unknown JSON
numbers, falsy model selection/customPath ignoring, denied IO and symlinks, and
explicit unsupported/resource cases alongside actual source results.

The **injected digest boundary** tests use real tiny files, real metadata and
real sequential streaming hashes and stamp writes, replacing only the resulting
digest strings with the fixed pinned hashes. `ai-status-fixture.json` is independently regenerated/checked by the
original source test and compared byte-for-byte by the Rust apply test. It also
records actual Node `msFromTimeSpec` hash and seven pre-JSON binary64 boundaries,
including the 499,841 ns one-ULP integer-aggregation negative control and negative
epochs. There are no genuine ~682 MB assets or actual successful pinned slow
hashes here. That remains an explicit evidence gap.

The injection is private `engine::prepare(job, Some(FILES.map(|f| f.1)))`, only
called by crate unit tests. Public `Job::prepare()` always uses actual hashing.
There is no public arbitrary digest override.

### Fixed native-composition harness (separate followup)

The standalone crate has `default = []`. Only explicit `test-harness` enables
`Job::prepare_fixed_tiny_fixture(self) -> Result<Prepared>`. Its only input is the
already admitted sealed Job. Each fixed leaf's test bytes are its own ASCII name,
with **no newline**: `encoder.int8.onnx`, `decoder.int8.onnx`, `joiner.int8.onnx`,
`tokens.txt`, `silero_vad.onnx` (75 bytes total). Actual bounded reads and SHA-256
always run. Only when a leaf's real digest equals the private constant SHA-256
of that leaf's exact filename bytes is its classification replaced by the source
pinned digest. Changed bytes, newline suffixes or a different leaf's fixture bytes
cannot receive the fixture classification. Genuine original pinned digests still
retain their ordinary actual classification.

This method deliberately **bypasses stamp reuse** so each fixture call really
hashes again, including after a previous fixture-created stamp. Ordinary
`Job::prepare()` keeps the original stamp shortcut and actual digest behavior,
even when the harness feature is enabled. In particular, original source can
accept changed bytes with restored size/time through a valid stamp; the forced
fixture method rejects them. This is an explicitly different test-control path,
not a new claim about source's shortcut or real model readiness. Enable only in
native composition tests with disposable trusted userData. Never enable/use it
against the actual application/model root.

All preparation/control/quota ownership and the sealed actual-only `apply()` path
are shared. Three added gated tests (18 total with the feature) establish real
hash byte counts, unchanged production prepare, changed bytes at every leaf and
wrong-leaf matching, real stamp bytes and borrowed-result quota retention,
ignored real write failure, accepted cancellation/deadline at apply, actual
hash-read limit and during-read cancellation. The 15 ordinary tests remain.

`crates/freeshow-ai-status/tests/fixed-tiny-source.json` records independently
executed immutable source controls: real tiny hashes without injection are
outdated; labelled classification injection writes the real original stamp;
restored metadata can hit source's unchanged shortcut; no-stamp changed bytes are
outdated; and actual stamp write failure still returns ready. Injection and its
limits are explicit in the receipt. These source controls do not claim the new
test-only forced-hash method is an original source API.

`tests/feature-controls.json` in the crate records the private normal driver
build with `--no-default-features`, resolved root features `[]`, exact artifact
path/length/SHA-256 and a passing compile-fail doctest proving the method is
absent. Its source hashes identify this local build; it is not a receipt for the
coordinator's eventual native app. That app must independently capture normal
resolved build features. The doctest intentionally fails if the method becomes
unconditionally available.

```sh
cargo +1.99.1 test --offline --manifest-path crates/freeshow-ai-status/Cargo.toml --no-default-features --all-targets
cargo +1.99.1 test --offline --manifest-path crates/freeshow-ai-status/Cargo.toml --no-default-features --doc
cargo +1.99.1 test --offline --manifest-path crates/freeshow-ai-status/Cargo.toml --features test-harness --all-targets
cargo +1.99.1 clippy --offline --manifest-path crates/freeshow-ai-status/Cargo.toml --features test-harness --all-targets -- -D warnings
```

Feature-on strict all-target checks also cover Windows GNU and macOS ARM; these
are compile controls, with actual platform runtime evidence still pending.

```sh
export CARGO_HOME=/mnt/data/cache/freeshow-rust-build/linux-deb-package/cargo-home
export CARGO_TARGET_DIR=/mnt/data/cache/freeshow-rust-build/provider-shows/target
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo +1.99.1 test --offline --manifest-path crates/freeshow-ai-status/Cargo.toml --all-targets
cargo +1.99.1 clippy --offline --manifest-path crates/freeshow-ai-status/Cargo.toml --all-targets -- -D warnings
cargo +1.99.1 clippy --offline --manifest-path crates/freeshow-ai-status/Cargo.toml --all-targets --target x86_64-pc-windows-gnu -- -D warnings
cargo +1.99.1 clippy --offline --manifest-path crates/freeshow-ai-status/Cargo.toml --all-targets --target aarch64-apple-darwin -- -D warnings
cargo +1.99.1 build --offline --manifest-path crates/freeshow-ai-status/Cargo.toml --bins
FREESHOW_AI_STATUS_BIN="$CARGO_TARGET_DIR/debug/freeshow-ai-status-parity" /home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin/node --test tests/parity/ai-status.test.mjs
cargo +1.99.1 fmt --manifest-path crates/freeshow-ai-status/Cargo.toml -- --check
npx prettier --config config/formatting/.prettierrc.yaml --check tests/parity/ai-status* docs/migration/workers/ai-status.md
```

The private driver defaults in tests to `target/debug/freeshow-ai-status-parity`
(`.exe` on Windows), with the shown environment override. It accepts bounded
fixture stdin/root/platform arguments and is not a production renderer service.
Standalone workspace/lock are temporary integration artifacts; the coordinator
owns root membership, shared lock, native host, event lifetime and CI wiring.

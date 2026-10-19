# macOS source fixture and subprocess diagnostics

Based on `5434778f`. This follow-up changes only the AI inventory and provider-show
Node harnesses. Production code, source comparisons, fixed subprocess limits,
workspace configuration, and the failed CI log remain unchanged.

The actual macOS core log at
`/mnt/data/cache/freeshow-rust-ci/5434778f/macos-core.log` contains two distinct
failures:

- AI inventory fails at raw `0xff` filename creation with `EILSEQ` on `open`,
  before either source inventory or native inventory runs. The harness now accepts
  only that exact observed Darwin creation rejection. It verifies the rejected
  creation left no entry, creates a real one-byte Unicode-root sibling, and still
  compares its complete source/native inventory. Its diagnostic explicitly says
  no invalid-byte entry was created or compared. Other platforms, error codes,
  and syscalls still fail. If raw-name creation succeeds, including on Linux, the
  original source-one-byte/native-unsupported comparison remains mandatory, with
  extra raw-byte directory/read assertions. No existing test is skipped anew.
- Provider Unicode context processing failed with `status: null`. The old assertion
  discarded `spawnSync.error` and `signal`, so this log **does not identify the
  cause**. Total suite duration is not evidence of a particular child timeout.
  The subprocess still has its original 15,000 ms timeout and 32 MiB output cap.
  Failures now report batch index/count/methods, exact status/signal/error fields,
  elapsed time, byte lengths, and a SHA-256 identifying the input batch. Stderr is
  capped to a 2,048-code-unit tail and error messages to 1,024 code units. No input
  content or giant stdout dump is printed. Raw stdout/stderr buffers preserve byte
  counts even if failure truncates a multibyte sequence; successful JSON decoding
  and complete typed/source comparisons remain unchanged. There are no retries,
  skipped context vectors, larger budgets, or production changes.

Portable negative controls reject unrelated filename-creation failures and verify
that timeout, signal-only, and output-limit diagnostic projections stay distinct
and bounded without exposing input fixture text.

## Local evidence

All output is private under
`/mnt/data/cache/freeshow-rust-build/macos-source-harness`. An isolated standalone
manifest/source link built only the current AI inventory example, using one Cargo
job with incremental and debug data disabled. The provider driver was copied by
value from the sealed `provider-shows-fixes` target; its SHA-256 is
`0969ed67d325ff9a60ee2d2e3e2f1ba8933629ba7bff3ab6741a8fa76d45bb72`.
Current provider crate source is unchanged from that sealed foundation.

Pinned Node 22.21.1 checks:

- `ai-local.test.mjs`: eight pass, one existing Windows-only skip. The real Linux
  fixture creates and reads the raw `0xff` entry; actual original inventory returns
  one byte while native inventory explicitly returns `ai_local_unsupported`.
- `provider-shows.test.mjs`: ten pass. All 159 whole-source comparisons, seven
  unsupported controls, five resource controls, 1,433 actual-Electron scalar
  mappings and 7,886 actual-Electron context vectors remain exercised.
- An independent private probe executes the native test adapter extracted
  from the actual test source across all 147 Unicode batches. All return status
  zero. Maximum captured input is 140,901 bytes; maximum stdout is 122,789 bytes;
  stderr is empty. This is Linux functional evidence, not the old Darwin failure's
  diagnosis and not a performance comparison.
- Real private Node child controls separately produce `ETIMEDOUT`/`SIGTERM`,
  signal-only `SIGKILL`, and `ENOBUFS`/`SIGTERM`. Their explicitly smaller control
  thresholds are recorded beside the diagnostic projection; they do not change
  the provider subprocess's production test limits.
- Node syntax, configured Prettier and `git diff --check` pass.

Receipts/logs: `ai-local-node.log`, `provider-shows-node.log`, and
`diagnostic-probes.json` in the private directory above. Actual macOS rerun remains
required: the filename rejection branch has not executed locally, and any repeated
provider failure must be diagnosed from the newly captured subprocess evidence.
No GUI, account, model download, root/shared build cache, or frozen binary was used
or modified.

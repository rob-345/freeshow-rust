# Published AI status and text-import CI observation

Run [39293892950](https://github.com/rob-345/freeshow-rust/actions/runs/39293892950)
executed published `5434778f4e820345070bcbe8cf7793791cd40d5e`. Three jobs
succeeded, two failed and one never acquired a runner. Linux core, Linux native
desktop and Windows native passed. Windows core failed a test's immediate
inventory-retention assertion after caller receipt but before actual worker-frame
disposal. The next fixture correction observes that disposal without changing
production ownership or increasing the existing deadline.

macOS core passed Rust, then failed two source cases: Darwin rejected creation of
the intended invalid-byte filename with `EILSEQ` before comparison, and a provider
context batch returned a null subprocess status with insufficient diagnostics.
The latter cause is unknown; a timeout is not established. The follow-up retains
the original subprocess limits and cases while reporting bounded exact failure
metadata. The invalid-filename case accepts only the actual Darwin `EILSEQ/open`
branch and still compares a real valid sibling; successful raw creation retains
the original unsupported-name comparison.

The macOS native job was canceled before execution. Its runner ID is zero, its
step list is empty, and GitHub's annotation says the hosted runner was not acquired
after repeated attempts. No sixth execution log exists. This was runner capacity,
not evidence of a native macOS application failure or an account-usage setting.

The [sealed observation](baselines/ai-status-imports-ci-observations.json) retains
all six job records, five execution logs, the cancellation annotation and the
downloaded 67-file Linux artifact audit. Its SHA-256 is
`99543ab22cb4c55c101172f3beed56763c082564d50dd10cf65120707b33c7f4`.
The Linux normal desktop completed 193 observations, four menu cases and both
drop scales. AI/Spotify/catalogue comparisons cover three real phases and the
expanded imports retain 15 actual saved files. Compiler/resource/binary digests
are recorded CI observations: the executable and frozen resource tree were not
uploaded, so their bytes cannot be independently rehashed from this artifact.
No current corrections, foreign GUI parity or whole-application speedup are
attributed to this earlier run.

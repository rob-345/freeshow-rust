# Native now-playing host foundations

`PreparationServices::prepare_retained` retains the actual cover Session on
failure in `PreparationFailure`. The fixed host must keep the actual preparation
future alive, consume `into_parts`, await `CoverImageService::retire` on the
optional Session, and only then release its whole-operation lease. An early
context/cancellation rejection before Session creation has no Session. This does
not reinterpret failure as null metadata. The legacy `prepare` wrapper retains
its emergency Drop behavior for existing callers; Drop is not a host drain
receipt. Assets use independently bounded anchored handles; this API does not
assert that every process-wide asset handle is closed after each operation.

`Issued::source_phase() -> SourcePhase` is a read-only view of the private
planner Stage. It distinguishes Preparation, PublishMetadata, PostMetadata,
Text, Unset, Open, and Cleanup. `TextFolder` is the first Text effect after the
source `await convertDynamicValues` boundary. Cleanup remains separate even
when failure after image creation requires release. The accessor grants no
acceptance/completion authority and does not let a host restore forged stages.

Verification: 15 actual preparation tests, including original pinned TypeScript
and real WAV/MP3/FLAC reads, plus all 13 planner tests pass. The new invalid-WAV
case retains a real Session after parser failure until explicit retirement;
unregistered authority failure is distinct. Linux focused all-target Clippy
with warnings denied passes. Tests use pinned Node22 and an explicit
FREESHOW_NOW_PLAYING_ORACLE_ROOT with private headless manifests. No GUI, new
thread, renderer IPC, or dependency shutdown behavior was added.

# AI inventory host admission and cancellation evidence

This follow-up to `1fe33e2a` changes no production behavior. The only host additions
are `cfg(test)` fields/methods and a `cfg(test)` gate immediately after actual
`NativeInventory::begin` returns its accepted foundation Ticket and registered
Caller, before `Ticket::finish` observes any result. It runs the real global-root
inventory owner; the hook does not synthesize inventory, pause its filesystem
worker, add a runtime owner, or create a production task/thread.

The gate exports a clone of the actual registered parent cancellation token.
Its single one-use release receiver holds the real Ticket and Caller in the host
future. The query can have finished on the filesystem worker while this observer
is held; the test proves accepted/ready whole-result retention, not interruption
inside a blocking stat/readdir syscall.

Three new native integration tests supplement the existing three:

- Abort after witnessed actual admission. The test awaits the Tokio observer's
  real cancellation join, verifies cancellation of the registered **parent**
  token and removal from the registry, then awaits AI shutdown with actual native
  worker join and zero retained capacity. Merely dropping the child Ticket would
  not satisfy the parent-token assertion.
- Perform a real `UPDATE_DATA_PATH`, then call `context_changed`. A matching
  context leaves the token live; the actual changed library cancels it. While the
  old observer remains gated, its Ticket remains charged and no old reply can
  complete. A new-context query returns the actual nonempty global app-data
  inventory and adds a second lease on the same owner. After gate release the
  old observer returns `ai_local_cancelled`, then actual terminal cleanup joins
  the same native owner with zero retained results.
- Hold an entered trusted `LibraryService::commit` closure on the actual
  `freeshow-library` IO thread using a controlled channel. While that owner and
  its active-job record remain held, AI inventory returns the actual seven-byte
  fixture from its independent filesystem owner and retains its publication
  lease. The test releases the closure only afterward and awaits its actual
  commit receipt and final ordered fence. This is a concurrency capability
  check, not a response-time or whole-application performance claim.

The new fixtures catch assertion panics so gate releases, observer joins and both
independent cleanup paths still run before the assertion is resumed. Dropping
the ordered-commit release sender unblocks the closure even on failure. Timeout
observations borrow their JoinHandle; failure aborts and actually awaits that
test observer rather than declaring it drained. Every successful new case checks
the AI native worker's real `worker_joined` and `retained == 0` after shutdown.

LibraryService's public `finish_writes` is the existing repeatable terminal IO
fence, not a native thread join API. These tests do not claim it joins the library
thread. The existing close test remains distinct: it proves four completed
native reads/Event clones remain charged **after** the AI worker has genuinely
joined, causing a shutdown timeout until those ready guards are disposed. It does
not prove a worker blocked inside filesystem IO.

Focused local evidence: six host integration tests pass, zero ignored, using
the exact `src-tauri/src/ai_local.rs` and `src-tauri/tests/ai_local.rs` paths with
the actual isolated-worktree core and AI crates. A private headless manifest
compiles the host as both its production library and integration-test module,
avoiding a full Tauri/GUI build. Warnings-denied all-target Clippy and rustfmt
checks run on the same focused sources. No root/provider-foundations target,
shared manifest, protocol, main, bridge, real profile or credential is modified.

```sh
export CARGO_TARGET_DIR=/private/target
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
# Coordinator's ordinary native integration command:
cargo test --manifest-path src-tauri/Cargo.toml --test ai_local --offline --locked
```

Local verification used Rust1.99.1/Linux. The tests use portable controlled
channels and actual temporary native filesystem operations; macOS/Windows native
execution remains a CI obligation. No source/backend readiness, inference,
physical playback, network, GUI or broad AI parity is claimed.

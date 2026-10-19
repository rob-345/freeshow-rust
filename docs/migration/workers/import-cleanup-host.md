# Actual import cleanup in the desktop host

This composes the [foundation cleanup lifecycle](import-cleanup-join.md) into
`NativeImports`. Shutdown closes host admission and cancels accepted work, calls
`close_preparation()`, drains the actual host registry and its queued prepared
plans, then calls terminal `shutdown()` to observe and join the retained cleanup
thread. The final join is attempted after a preparation failure as well; failure
is preserved rather than converted into successful drain.

Four host shutdown-observer slots include observers waiting for serialization.
A fifth returns `import_lifecycle_busy`. Dropping or timing out an observer
releases its observation slot while the same host plans, native workers and real
join handles remain owned. A later observer resumes that ownership. No detached
shutdown continuation, extra thread/runtime or larger deadline is introduced.

Eight native host tests pass. A new case holds the actual ordered library owner,
queues two real ZIP stages, abandons four shutdown observers, rejects a fifth,
times out a retry, then releases the library. Both requests cancel, the two real
stage directories disappear and terminal retry joins. Existing stage-capacity
coverage now polls the actual shutdown future while the library is held.

The first ordinary-fence counterfactual unexpectedly passed the prior immediate
`is_finished()` assertion: it had not ensured the shutdown task was scheduled.
That result is preserved in the initial control log. Replacing it with a bounded
poll of the real future made the genuine fence-only mutation fail at
`shutdown must own retained cleanup`. The source was restored and all eight host
tests passed. This records the corrected test's evidence without representing
the earlier weak control as a failure. Foundation coverage remains 28 passing
tests with actual held-exit and native-join controls.

Local logs use the `/tmp/freeshow-spotify-cleanup-` prefix:
`host-initial-instantaneous-check.log`, `host-negative-fence-only.log`,
`imports-host-restored.log`, `imports-core-tests.log` and `native-focused.log`.
Normal application verification and foreign native lifecycle execution remain
separate from this focused host composition evidence.

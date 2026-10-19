# Bounded timecode publication handoff

This fixes the frontend composition boundary between the existing native host and retained synchronous MAIN dispatch. It changes only `timecodeBridge.ts`, its focused tests and this note. Native/main/shared bridge/wire/codec code is unchanged.

## Observed race and ownership

`NativeTimecode.complete` consumes its private nonce by sending the native one-shot before the Tauri invocation response arrives in JavaScript. The native owner can immediately publish its next queued source notification. A valid next event can therefore reach `publish` while the previous completion Promise is still pending. The former helper rejected it as `timecode_completion_busy`, losing that notification and leaving its native nonce to hit the delivery timeout. A bounded pure-helper reproduction delivered the first VALUE but lost the subsequent STATUS under this response ordering; no app or Cargo run was needed to demonstrate the helper defect.

The adapter now retains at most **one** next publication across that handoff. The native host still permits only one uncompleted nonce, so no further genuine native event can follow until this staged notification is delivered and completed. The existing seven ordinary FIFO leases, one completion Promise, one cleanup continuation and one drain observer remain unchanged. There is no task per publication, unbounded queue or UUID history.

Admission validates the bounded UUID, full context, safe run and monotonic decimal sequence, then validates/hydrates the source message immediately. Only its owned context, nonce and prepared message remain staged. LTC bytes are copied; IEEE restoration becomes its actual whole Number; strings/scalars have no mutable metadata reference. Unknown source fields do not become callback authority. Mutation of the caller's event, hydration, byte storage, context or sequence after admission cannot change later delivery. Malformed well-identified messages retain only their bounded false-completion receipt. Excess and reentrant publications reject without acquiring another slot or consuming another sequence.

Once the previous invoke response settles, synchronous retained dispatch checks both current full contexts again, then sends exactly one private completion. These are internal transport receipts, never original TIMECODE success ACKs. Retained async playback/actions remain outside this synchronous fence.

## Retirement and failure

Context retirement or terminal close clears the staged notification synchronously and invokes the existing independent native cleanup. Native retirement must revoke its old nonce and await actual owner drain; clearing a JS slot alone is not that proof. The adapter does not retag or deliver staged old data after adopting another library. The last admitted sequence remains a replay fence even if that publication was retired before delivery.

An actual completion rejection remains sticky, starts terminal cleanup and discards the staged notification. Cleanup runs before waiting for the ordinary pump/completion Promises. A held/lost completion can still make the bounded drain observer time out; that timeout retains the real Promise/cleanup continuation for retry. It does not cancel native effects or manufacture success. No new completion timer supersedes the native delivery deadline or the existing bounded cleanup observer. Terminal cleanup failure/timeout behavior remains visible through the existing error/status interface; `status().nextPublication` reports the single staging slot.

## Focused checks

Pinned standalone Node **22.21.1**: all **19** existing/expanded portable timecode bridge tests pass. New regressions delay the previous invoke response after native-shaped publication admission, then verify exactly-once VALUE/STATUS, negative zero, NaN and copied LTC-byte delivery; private nonce completions; bounded excess/replay rejection; malformed/foreign-context false completion; context crossing; cleanup independent of a held completion; and sticky completion rejection through actual controlled cleanup wait/timeout/retry. Existing actual immutable TypeScript/worklet/MessagePort source tests remain intact.

```sh
node --test tests/parity/timecode-bridge.test.mjs
npx tsc --noEmit --target ES2022 --module ES2022 --moduleResolution node --strict --skipLibCheck --lib ES2022,DOM src/frontend/rust/timecodeBridge.ts
npx prettier --config config/formatting/.prettierrc.yaml --check src/frontend/rust/timecodeBridge.ts tests/parity/timecode-bridge.test.mjs docs/migration/workers/timecode-publication-handoff.md
```

This is deterministic JavaScript boundary evidence. It does not claim actual Tauri/WebKit event-response ordering was observed in an app, physical MTC/LTC device behavior or browser playback/capture. The coordinator owns production hookup, native regression tests, real WebKit acceptance and the separately reported native context-observer terminal-error correction.

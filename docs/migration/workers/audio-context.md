# Lazy frontend audio context

This is the bounded frontend follow-up to [the idle CPU investigation](idle-cpu-investigation.md). The diagnostic pair attributed about 10% of one logical core to six WebKit audio/queue threads across two web processes; it did not identify JavaScript constructor stacks or prove that this change reduces release CPU. The archived `c9e62492` executable, original baseline, profiling receipts and retained Svelte UI are unchanged.

## Change and intentional difference

Only `src/frontend/audio/audioAnalyser.ts` changes at runtime. Its private context is created on first actual use instead of at module evaluation. The same context remains owned for the page lifetime. There is no new suspend, close, replacement, timer, task, native command or audio transport.

The no-source paths for source-volume settings, volume ramps and channel-count configuration retain their values without allocating audio infrastructure. Later source attachment applies those values. Missing-source analyser/meter reads and routing-manager initialization leave the context uninitialized when the actual `AudioSender.shouldBeActive()` predicate is false. An explicit context, destination or hardware-capability query, actual source attachment, or genuinely active sender still creates the singleton. Channel-count changes on an initialized graph retain the original reconnection behavior.

The inactive-sender guard applies only before initialization. On an already initialized graph it preserves the original `getAudioContext()` routing synchronization/resume behavior, including when local playback exists without a network sender. The active path still uses the retained `AudioSender`, its actual target selection, worklet module, MessagePort request and cleanup logic. `AudioMeter` keeps polling real analyser data; samples can change without Svelte store updates. Nothing throttles a live microphone or infers inactivity from silence.

This intentionally removes eager construction, unused-context resume, and unused splitter/analyser creation from cold configuration paths. A constructor failure consequently occurs at genuine first use instead of import. Explicit capability queries still construct because they inspect the actual browser context. A source audit found no external direct access to the private `AudioAnalyser.ac` property; callers use its existing public methods, whose signatures remain unchanged.

## Focused evidence

`tests/parity/audio-context.test.mjs` extracts the actual immutable `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` TypeScript from Git, verifies the analyser's normalized source SHA256 `ff000b9ddcd06eb88069a210abe09fb293e75a34e116bc702de0045768146eff`, and compares it to the working implementation. It executes the actual analyser, routing manager, input capture, multichannel, sidechain, sender and meter polling functions. Web Audio/DOM/processor effects, application stores and a bounded scheduler are controlled boundaries; source algorithms are not rewritten as the oracle.

Eight tests cover the intentional zero-context idle difference; twenty no-source meter/config cycles; cold running/suspended first use; singleton and destination reuse; 2/4/8-channel source graphs; volume/ramp/pitch/tempo and source removal; changing analyser samples without store notifications; cold and initialized Icecast/worklet activation, deactivation and reactivation; an initialized inactive sender resuming its existing context; output/PDF idle roles; explicit capability queries; and constructor/source/resume/worklet failure paths. Successful paths reject unexpected swallowed diagnostics. Active effect traces match the original after excluding only the intentionally deferred constructor event where necessary. The idle test explicitly expects original one-context versus new zero-context behavior, rather than treating it as accidental parity.

Local verification uses pinned standalone Node **22.21.1**, TypeScript transpilation and actual Svelte stores. These are controlled graph/ownership tests, not real device, browser policy, worklet processing, SoundTouch DSP, PCM, playback or Opus evidence. The separate ten existing actual MessagePort/worklet/raw-payload tests also pass: **18/18** focused tests in total. Those existing tests validate actual transferred ports and worklet bytes independently; they do not establish WebKit cold first-use behavior for the lazy graph.

```sh
node --test tests/parity/audio-context.test.mjs tests/parity/audio-port-bridge.test.mjs
npx prettier --config config/formatting/.prettierrc.yaml --check src/frontend/audio/audioAnalyser.ts tests/parity/audio-context.test.mjs docs/migration/workers/audio-context.md
```

Formatting and `git diff --check` also pass. No root/native/frontend production build was run.

## Remaining validation

The coordinator must validate real WebKit cold/programmatic first use, resume policy, worklet/port startup, genuine playback/capture and routing/reactivation before shipping this optimization. In particular, first use without a new user gesture must remain functional. No fake context test certifies browser autoplay or device policy.

Any release CPU claim requires immutable before/after release binaries with identical feature scope, UI/profile/workload and environment, repeated alternating/randomized whole-process-tree measurements, and the existing cleanup/coverage rules. The earlier archived partial migration is not a valid current-feature before/after control. No new performance comparison, app or profiler run was performed for this patch.

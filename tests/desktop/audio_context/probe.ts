import { get } from "svelte/store"
import * as state from "./boundaries"
declare const __ORIGINAL__: boolean
const win = window as any,
    query = new URLSearchParams(location.search),
    scenario = query.get("scenario") || "local",
    gesture = query.get("gesture") === "1"
const contexts: AudioContext[] = [],
    errors: string[] = [],
    warnings: string[] = [],
    resumeCalls: unknown[] = [],
    messages = new Set<EventListenerOrEventListenerObject>(),
    stages: unknown[] = []
const NativeContext = window.AudioContext,
    add = window.addEventListener,
    remove = window.removeEventListener
window.AudioContext = new Proxy(NativeContext, {
    construct(target, args) {
        if (contexts.length >= 8) throw new Error("Audio context diagnostic limit")
        const context = Reflect.construct(target, args) as AudioContext
        contexts.push(context)
        const resume = context.resume.bind(context)
        context.resume = () => {
            if (resumeCalls.length < 128) resumeCalls.push({ state: context.state, trustedGesture: navigator.userActivation?.isActive ?? null })
            return resume()
        }
        return context
    }
})
window.addEventListener = function (type: string, listener: any, options: any) {
    if (type === "message") messages.add(listener)
    return add.call(window, type, listener, options)
} as any
window.removeEventListener = function (type: string, listener: any, options: any) {
    if (type === "message") messages.delete(listener)
    return remove.call(window, type, listener, options)
} as any
const diagnostic = (target: string[], value: string) => {
    if (target.length < 32) target.push(value.slice(0, 2048))
}
window.addEventListener("error", (event) => diagnostic(errors, String(event.message)))
window.addEventListener("unhandledrejection", (event) => diagnostic(errors, String(event.reason)))
const originalError = console.error,
    originalWarn = console.warn
console.error = (...args) => {
    diagnostic(errors, args.map(String).join(" "))
    originalError(...args)
}
console.warn = (...args) => {
    diagnostic(warnings, args.map(String).join(" "))
    originalWarn(...args)
}
const send = (data: unknown) => win.webkit.messageHandlers.receipt.postMessage(JSON.stringify(data))
const delay = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms))
const assert = (test: unknown, message: string) => {
    if (!test) throw new Error(message)
}
async function wait(predicate: () => unknown, description: string, timeout = 5000) {
    const end = performance.now() + timeout
    do {
        const result = predicate()
        if (result) return result
        await delay(20)
    } while (performance.now() < end)
    throw new Error("WebKit functional deadline: " + description)
}
const timeout = async <T>(promise: Promise<T>, description: string, ms = 6000): Promise<T> => {
    let timer: ReturnType<typeof setTimeout>
    try {
        return await Promise.race([
            promise,
            new Promise<never>((_, reject) => {
                timer = setTimeout(() => reject(new Error(description)), ms)
            })
        ])
    } finally {
        clearTimeout(timer!)
    }
}
async function bootstrap() {
    const [{ AudioAnalyser: analyser }, { AudioSender: sender }, { AudioRoutingManager: routing }, { AudioInputCapture: input }, { AudioProcessor: processor }, recording] = await Promise.all([
        import("../../../src/frontend/audio/audioAnalyser"),
        import("../../../src/frontend/audio/audioSender"),
        import("../../../src/frontend/audio/routing/audioRoutingManager"),
        import("../../../src/frontend/audio/routing/audioInputCapture"),
        import("../../../src/frontend/audio/audioProcessor"),
        import("../../../src/frontend/audio/audioChannelRecorder")
    ])
    const privateAnalyser = analyser as any,
        privateSender = sender as any,
        manager = routing.getInstance() as any
    const cold = { afterImport: contexts.length, afterConfiguration: -1, beforeFirstUse: -1 }
    for (let i = 0; i < 20; i++) {
        analyser.setSourceVolume("future", 0.4)
        analyser.rampSourceVolume("future", 0.3, 50)
        analyser.updateChannelCount(i % 2 ? 2 : 4)
        analyser.getChannelLiveVolume("main")
        analyser.getAnalysers()
        analyser.recorderActivate()
    }
    cold.afterConfiguration = contexts.length
    assert(cold.afterImport === (__ORIGINAL__ ? 1 : 0), "Unexpected import constructor count")
    assert(cold.afterConfiguration === (__ORIGINAL__ ? 1 : 0), "Cold configuration constructed an unused context")
    let audio: HTMLAudioElement | undefined, captured: MediaStream | undefined
    const result: any = {
        original: __ORIGINAL__,
        scenario,
        gesture,
        cold,
        origin: location.origin,
        secureContext: isSecureContext,
        browser: navigator.userAgent,
        errors,
        warnings,
        stages,
        physicalHardwareVerified: false,
        nativeAppIntegrationVerified: false,
        capabilities: { audioWorklet: typeof AudioWorkletNode === "function", mediaRecorderWebmOpus: typeof MediaRecorder === "function" && MediaRecorder.isTypeSupported("audio/webm; codecs=opus"), getUserMedia: typeof navigator.mediaDevices?.getUserMedia === "function" }
    }
    async function run(event?: MouseEvent) {
        result.trustedClick = event?.isTrusted ?? false
        if (gesture) assert(result.trustedClick, "First-use gesture must be actual trusted native input")
        cold.beforeFirstUse = contexts.length
        try {
            if (scenario === "idle") {
                state.currentWindow.set("output")
                analyser.recorderActivate()
                await delay(300)
                assert(contexts.length === (__ORIGINAL__ ? 1 : 0), "Idle output role constructed context")
                state.currentWindow.set(null)
                stages.push({ stage: "coldIdle", contexts: contexts.length })
            } else if (scenario === "explicit") {
                const ac = analyser.getAudioContext(),
                    start = ac.currentTime
                await wait(() => ac.state === "running" && ac.currentTime > start + 0.08, "programmatic first context resume/clock")
                assert(analyser.getAudioContext() === ac, "First-use singleton changed")
                stages.push({ stage: "explicitRunning", state: ac.state, clockAdvanced: ac.currentTime - start })
            } else if (scenario === "capture") {
                if (!result.capabilities.getUserMedia) throw new Error("Actual getUserMedia unavailable")
                captured = await timeout(navigator.mediaDevices.getUserMedia({ audio: true, video: false }), "Actual private PulseAudio capture timed out")
                const tracks = captured.getAudioTracks()
                assert(tracks.length > 0 && tracks.every((track) => track.readyState === "live"), "No actual live input track")
                assert(
                    tracks.every((track) => track.label === "FreeShowVirtualAudioFixture"),
                    "Capture escaped the private virtual OS fixture"
                )
                state.audioRouting.set({ channels: [{ id: "main", name: "Main" }], connections: [{ from: "mic_default", to: "main" }] })
                state.playingAudio.set({ mic_sub_default: { isMic: true, paused: false } })
                await analyser.attach("mic_sub_default", captured)
                await wait(() => analyser.getChannelLiveVolume("main") > -45, "real virtual OS capture samples")
                stages.push({ stage: "actualGetUserMedia", tracks: tracks.map((track) => ({ label: track.label, settings: track.getSettings() })), db: analyser.getChannelLiveVolume("main") })
                analyser.detach("mic_sub_default")
                tracks.forEach((track) => track.stop())
                assert(
                    tracks.every((track) => track.readyState === "ended"),
                    "Actual capture track did not stop"
                )
                await wait(() => analyser.getChannelLiveVolume("main") <= -55, "captured signal ceases after detach/track stop")
                stages.push({ stage: "captureStopped", active: analyser.getActiveSourceIds(), allTracksEnded: true, db: analyser.getChannelLiveVolume("main") })
            } else {
                analyser.setSourceVolume("fixture", 0.25)
                if (scenario === "network" || scenario === "deferred") {
                    state.native.hold = scenario === "deferred"
                    state.special.set({ icecast: { enabled: true } })
                    state.audioRouting.set({
                        channels: [{ id: "main", name: "Main" }],
                        connections: [
                            { from: "drawer_audio", to: "main" },
                            { from: "main", to: "icecast" }
                        ]
                    })
                    analyser.recorderActivate()
                    await wait(() => state.native.commands.some((value: any) => value.commands[0] === "INIT_PORT"), "actual AudioSender worklet/port request")
                    assert(contexts.length === 1, "Cold sender must own one actual context")
                    if (scenario === "deferred") {
                        assert(messages.size === 1, "Expected one actual pending port listener")
                        state.special.set({ icecast: { enabled: false } })
                        analyser.recorderDeactivate()
                        await wait(() => messages.size === 0 && privateSender.processors.size === 0 && privateSender.pendingPortCleanups.size === 0, "pending listener/worklet cleanup")
                        state.native.deferred.splice(0).forEach((respond) => respond())
                        await delay(100)
                        assert(state.native.totalPackets === 0, "Destroyed target consumed delayed port")
                        state.native.hold = false
                        state.special.set({ icecast: { enabled: true } })
                        analyser.recorderActivate()
                        await wait(() => state.native.commands.filter((value: any) => value.commands[0] === "INIT_PORT").length === 2, "sender restart after delayed response")
                    }
                }
                audio = new Audio("./fixture.wav")
                audio.loop = true
                audio.crossOrigin = "anonymous"
                document.body.append(audio)
                state.playingAudio.set({ fixture: { paused: false } })
                await timeout(analyser.attach("fixture", audio), "real media source attachment timed out")
                await timeout(audio.play(), "real browser media playback timed out")
                const ac = analyser.getAudioContext(),
                    started = ac.currentTime,
                    originalSource = privateAnalyser.sources.fixture
                await wait(() => ac.state === "running" && ac.currentTime > started + 0.08 && audio!.currentTime > 0.08, "media/context playback clocks")
                await wait(() => analyser.getChannelLiveVolume("main") > -45, "real routed analyser samples")
                assert(privateAnalyser.gainNodes.fixture.gain.value === 0.25, "Cold source volume was lost")
                stages.push({ stage: "realHtmlPlayback", state: ac.state, mediaTime: audio.currentTime, sampleRate: ac.sampleRate, db: analyser.getChannelLiveVolume("main"), initialVolume: privateAnalyser.gainNodes.fixture.gain.value })
                if (scenario === "local") {
                    analyser.rampSourceVolume("fixture", 0.5, 100)
                    analyser.setPitch("fixture", 3)
                    analyser.setTempo("fixture", 0.9)
                    await wait(() => processor.isRegistered(ac) && privateAnalyser.processors.fixture.soundTouch instanceof AudioWorkletNode, "actual SoundTouch worklet startup")
                    await wait(() => analyser.getChannelLiveVolume("main") > -45, "real wet worklet routed output")
                    const soundTouch = privateAnalyser.processors.fixture.soundTouch
                    assert(soundTouch.pitchSemitones.value === 3 && Math.abs(soundTouch.tempo.value - 0.9) < 0.0001, "Real SoundTouch parameters lost")
                    stages.push({ stage: "realSoundTouch", pitch: soundTouch.pitchSemitones.value, tempo: soundTouch.tempo.value, db: analyser.getChannelLiveVolume("main") })
                    if (result.capabilities.mediaRecorderWebmOpus) {
                        recording.startChannelRecording("main", "Functional WebKit")
                        const stream = manager.recorderDestinations.get("main").stream as MediaStream
                        await delay(400)
                        await timeout(recording.stopChannelRecording("main"), "Actual MediaRecorder stop/event timed out")
                        assert(state.native.saved.length === 1 && (state.native.saved[0] as any).bytes > 100, "Real recorded source bytes missing")
                        assert(
                            stream.getTracks().every((track) => track.readyState === "ended"),
                            "Source recorder tracks retained after stop"
                        )
                        assert(!recording.isChannelRecording("main") && manager.recorderDestinations.size === 0 && Object.keys(get(state.recordingChannels)).length === 0, "Source recorder cleanup incomplete")
                        stages.push({ stage: "realSourceRecording", saved: state.native.saved, stoppedTracks: true })
                    } else stages.push({ stage: "sourceRecordingUnsupported", mime: "audio/webm; codecs=opus" })
                    analyser.detach("fixture")
                    await analyser.attach("fixture", audio)
                    assert(privateAnalyser.sources.fixture === originalSource, "Reattach did not reuse native media element source")
                    await ac.suspend()
                    analyser.recorderActivate()
                    await wait(() => ac.state === "running", "initialized inactive sender retains original resume behavior")
                    stages.push({ stage: "reattachResume", singleton: analyser.getAudioContext() === ac, sameMediaSource: true })
                } else {
                    await wait(() => state.native.firstNonzero !== null, "real captured PCM MessagePort packets")
                    assert(messages.size === 0 && privateSender.pendingPortCleanups.size === 0, "Delivered port listener retained")
                    for (const packet of state.native.packets as any[]) assert(packet.bytes === Math.max(128, Math.round(ac.sampleRate * 0.02)) * 8, "Actual worklet PCM framing mismatch")
                    stages.push({ stage: "realPcmWorklet", packets: state.native.packets, firstNonzero: state.native.firstNonzero, listeners: messages.size })
                    state.special.set({ icecast: { enabled: false } })
                    analyser.recorderDeactivate()
                    await wait(() => privateSender.processors.size === 0 && privateSender.pendingPortCleanups.size === 0 && messages.size === 0, "actual sender/worklet cleanup")
                    await delay(150)
                    const stoppedCount = state.native.totalPackets
                    await delay(150)
                    assert(state.native.totalPackets === stoppedCount, "PCM delivery continued after actual sender cleanup")
                    stages.push({ stage: "senderStopped", noFurtherPackets: true, closeCommands: state.native.commands.filter((value: any) => value.commands[0] === "CLOSE_PORT").length })
                }
                audio.pause()
                analyser.detach("fixture")
                state.playingAudio.set({})
                input.getInstance().removeInput("fixture")
                assert(analyser.getActiveSourceIds().length === 0 && Object.keys(privateAnalyser.processors).length === 0 && Object.keys(privateAnalyser.gainNodes).length === 0, "Source graph cleanup retained live input")
            }
            assert(contexts.length === (scenario === "idle" && !__ORIGINAL__ ? 0 : 1), "Actual singleton/context count changed")
            assert(errors.length === 0, "Actual browser/module errors: " + errors.join(" | "))
            assert(warnings.length === 0, "Unexpected browser/module warnings: " + warnings.join(" | "))
            result.ok = true
        } catch (error) {
            result.ok = false
            result.error = String(error)
            result.stack = String((error as Error).stack).slice(0, 4096)
        } finally {
            audio?.pause()
            audio?.remove()
            captured?.getTracks().forEach((track) => track.stop())
            state.special.set({ icecast: { enabled: false } })
            sender.cleanupAll()
            state.native.ports.forEach((port) => port.close())
            result.contexts = contexts.map((context) => ({ state: context.state, sampleRate: context.sampleRate, currentTime: context.currentTime }))
            result.resumeCalls = resumeCalls
            result.commands = state.native.commands
            result.final = { messageListeners: messages.size, pendingPortCleanups: privateSender.pendingPortCleanups.size, senderProcessors: privateSender.processors.size, sourceIds: analyser.getActiveSourceIds() }
            // Harness teardown, not an application page-lifetime policy change.
            await Promise.all(contexts.map((context) => context.close()))
            result.harnessContextsActuallyClosed = contexts.every((context) => context.state === "closed")
            result.finished = true
            document.querySelector("#result")!.textContent = JSON.stringify(result)
            send(result)
        }
    }
    const button = document.querySelector<HTMLButtonElement>("#start")!
    button.addEventListener(
        "click",
        (event) => {
            button.disabled = true
            void run(event)
        },
        { once: true }
    )
    send({ stage: "ready", scenario, original: __ORIGINAL__, gesture })
    if (!gesture)
        setTimeout(() => {
            button.disabled = true
            void run()
        }, 350)
}
bootstrap().catch((error) => send({ finished: true, ok: false, error: String(error), stack: String(error.stack) }))

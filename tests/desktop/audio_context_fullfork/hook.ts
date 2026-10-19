// Test-only instrumentation. Every browser/native call delegates to the real
// implementation; production imports, stores and routing are not substituted.
const win = window as any,
    contexts: AudioContext[] = [],
    errors: unknown[] = [],
    packets: any[] = [],
    ports: unknown[] = [],
    statuses: unknown[] = [],
    worklets: unknown[] = [],
    sourceAcks: unknown[] = [],
    consoleAudio: unknown[] = []
const record = (values: unknown[], value: unknown, cap = 64) => {
    if (values.length < cap) values.push(value)
}
for (const kind of ["warn", "error"] as const) {
    const original = console[kind]
    console[kind] = (...args: unknown[]) => {
        record(consoleAudio, { kind, text: args.map(String).join(" ").slice(0, 2048) })
        return original(...args)
    }
}
const NativeContext = window.AudioContext
window.AudioContext = new Proxy(NativeContext, {
    construct(target, args) {
        const context = Reflect.construct(target, args) as AudioContext
        record(contexts, context, 8)
        const add = context.audioWorklet?.addModule.bind(context.audioWorklet)
        if (add)
            context.audioWorklet.addModule = (...args) => {
                record(worklets, { url: String(args[0]), origin: location.origin })
                return add(...args)
            }
        return context
    }
})
window.addEventListener("error", (event) => record(errors, { kind: "error", message: String(event.message).slice(0, 2048) }))
window.addEventListener("unhandledrejection", (event) => record(errors, { kind: "rejection", message: String(event.reason).slice(0, 2048) }))
window.addEventListener("freeshow:runtime-error", (event) => record(errors, { kind: "runtime", detail: (event as CustomEvent).detail }))
window.addEventListener("message", (event) => {
    if (event.data?.type === "AUDIO_PORT_RESPONSE") record(ports, { keys: Object.keys(event.data), target: event.data.targetId, portCount: event.ports.length, actualPort: event.ports[0] instanceof MessagePort })
})
const base64 = (bytes: Uint8Array) => {
    let value = ""
    for (const byte of bytes) value += String.fromCharCode(byte)
    return btoa(value)
}
let modules: Promise<any> | undefined,
    stopping = false,
    packetLimit = 12
const actual = () => (modules ??= Promise.all([import("../../../src/frontend/stores"), import("../../../src/frontend/audio/audioAnalyser"), import("../../../src/frontend/audio/audioPlayer"), import("../../../src/frontend/audio/audioSender"), import("svelte/store")]).then(([stores, analyser, player, sender, store]) => ({ stores, analyser: analyser.AudioAnalyser, player: player.AudioPlayer, sender: sender.AudioSender, get: store.get })))
async function stopSender() {
    if (stopping) return
    stopping = true
    const { stores, get } = await actual()
    stores.special.update((value: any) => ({ ...value, icecast: { ...value.icecast, enabled: false } }))
    const routing = get(stores.audioRouting)
    stores.audioRouting.set({ ...routing, connections: routing.connections.filter((item: any) => item.to !== "icecast") })
}
const invoke = win.__TAURI__.core.invoke
const observedInvoke = function (this: unknown, name: string, args: any, ...rest: any[]) {
    let packet: any
    if (name === "audio_pcm") {
        if (!(args instanceof Uint8Array) || packets.length >= 64) throw new Error("Full-fork PCM diagnostic bound")
        const length = new DataView(args.buffer, args.byteOffset, args.byteLength).getUint32(0, true),
            prefix = JSON.parse(new TextDecoder().decode(args.subarray(4, 4 + length))),
            bytes = args.slice(4 + length),
            floats = new Float32Array(bytes.buffer)
        let peak = 0
        for (const value of floats) {
            if (!Number.isFinite(value)) throw new Error("Nonfinite real worklet PCM")
            peak = Math.max(peak, Math.abs(value))
        }
        packet = { ...prefix, planarBase64: base64(bytes), byteLength: bytes.byteLength, peak, state: "entered" }
        packets.push(packet)
        if (packets.length === packetLimit) void stopSender()
    }
    const pending = invoke.call(this, name, args, ...rest)
    if (!packet) return pending
    // Observe the real completed private receipt before the original bounded
    // frontend pump advances. This adds diagnostic IPC latency, not DSP/sinks.
    return pending.then(
        async (value: unknown) => {
            packet.receipt = await invoke("audio_status")
            packet.state = "completed"
            return value
        },
        (error: unknown) => {
            packet.state = "failed"
            packet.error = error
            throw error
        }
    )
}
// Tauri freezes the core namespace. Replace only the mutable outer property
// with a delegating namespace; never write a frozen function descriptor.
win.__TAURI__.core = { ...win.__TAURI__.core, invoke: observedInvoke }
win.__audioFullFork = {
    async configure(folder: string) {
        const { stores } = await actual()
        stores.audioFolders.update((value: any) => ({ ...value, fullfork: { name: "Actual native WAV", path: folder } }))
        stores.drawerTabsData.update((value: any) => ({ ...value, audio: { ...value.audio, activeSubTab: "fullfork" } }))
        stores.activeDrawerTab.set("audio")
        stores.drawer.set({ height: 350, stored: null })
        win.api.receive(
            "MAIN",
            (message: any, listenerId: unknown) => {
                if (message.channel !== "ICECAST_STATUS") return
                const value = message.data
                record(statuses, { keys: Object.keys(value), errorOwn: Object.hasOwn(value, "error"), errorUndefined: value.error === undefined, listenerIdUndefined: listenerId === undefined, connected: value.connected, connecting: value.connecting, error: value.error })
            },
            "audio-fullfork-status"
        )
        win.api.receive("AUDIO", (message: unknown) => record(sourceAcks, message), "audio-fullfork-acks")
        return this.snapshot()
    },
    async recorderFailure() {
        if (typeof MediaRecorder === "undefined" || MediaRecorder.isTypeSupported("audio/webm; codecs=opus")) return { attempted: false, reason: "exact source MIME is supported or recorder unavailable; no successful recording claim" }
        const recorder = await import("../../../src/frontend/audio/audioChannelRecorder"),
            { AudioRoutingManager } = await import("../../../src/frontend/audio/routing/audioRoutingManager"),
            manager = AudioRoutingManager.getInstance() as any
        let error: { name: string; message: string } | undefined
        try {
            recorder.startChannelRecording("main", "actual source MIME failure")
        } catch (failure: any) {
            error = { name: String(failure.name), message: String(failure.message) }
        }
        const active = recorder.isChannelRecording("main"),
            destination = manager.recorderDestinations.get("main") as MediaStreamAudioDestinationNode | undefined
        // Source registers this before its throwing constructor. Dispose that
        // observed failed-test resource explicitly, without changing product.
        const tracks = destination?.stream.getTracks() ?? []
        manager.unregisterChannelRecorder("main", destination)
        tracks.forEach((track) => track.stop())
        return { attempted: true, mime: "audio/webm; codecs=opus", error, active, sourceRegisteredBeforeFailure: !!destination, fixtureDisposedTracks: tracks.map((track) => track.readyState) }
    },
    async sender(config: any, count = 12) {
        if (!config || config.host !== "127.0.0.1" || !Number.isInteger(config.port) || count < 4 || count > 32) throw new Error("Controlled loopback sender required")
        const { stores, get } = await actual()
        packetLimit = count
        stopping = false
        stores.special.update((value: any) => ({ ...value, icecast: config }))
        const routing = get(stores.audioRouting)
        stores.audioRouting.set({ ...routing, connections: [...routing.connections.filter((item: any) => item.to !== "icecast"), { from: "main", to: "icecast" }] })
    },
    async processing(path: string, pitch: number, tempo: number) {
        const { analyser } = await actual()
        analyser.setPitch(path, pitch)
        analyser.setTempo(path, tempo)
        return this.snapshot()
    },
    async anonymousReload(path: string) {
        const { stores, get } = await actual(),
            audio = get(stores.playingAudio)[path]?.audio
        if (!audio || !audio.src.startsWith("http://127.0.0.1:")) throw new Error("Actual authorized native URL required")
        const url = audio.src
        audio.pause()
        audio.removeAttribute("src")
        audio.load()
        audio.crossOrigin = "anonymous"
        audio.src = url
        await audio.play()
        return this.snapshot()
    },
    async reattach(path: string) {
        const { analyser, stores, get } = await actual(),
            audio = get(stores.playingAudio)[path]?.audio,
            source = (analyser as any).sources[path]
        if (!audio || !source) throw new Error("Real playing media source required")
        analyser.detach(path)
        await analyser.attach(path, audio)
        if ((analyser as any).sources[path] !== source) throw new Error("Actual media source was not reused")
        return this.snapshot()
    },
    async snapshot() {
        const { stores, analyser, sender, get } = await actual()
        return {
            href: location.href,
            origin: location.origin,
            secure: isSecureContext,
            context: win.api?.getRuntimeContext(),
            contexts: contexts.map((value) => ({ state: value.state, sampleRate: value.sampleRate, currentTime: value.currentTime })),
            playing: Object.entries(get(stores.playingAudio)).map(([id, value]: [string, any]) => ({ id, paused: value.paused, time: value.audio.currentTime, duration: value.audio.duration, src: value.audio.src, error: value.audio.error?.code ?? null, crossOrigin: value.audio.crossOrigin, readyState: value.audio.readyState, elementPaused: value.audio.paused })),
            sourceIds: analyser.getActiveSourceIds(),
            // A cold observation must not become an audio consumer itself.
            level: contexts.length ? analyser.getChannelLiveVolume("main") : -60,
            sourceLevels: contexts.length ? Object.keys(get(stores.playingAudio)).map((id) => ({ id, level: analyser.getChannelLiveVolume(id) })) : [],
            speakerLevel: contexts.length ? analyser.getChannelLiveVolume("speaker_default") : -60,
            routing: get(stores.audioRouting),
            channelData: get(stores.audioChannelsData),
            gains: Object.entries((analyser as any).gainNodes).map(([id, node]: [string, any]) => ({ id, gain: node.gain.value })),
            processors: [...(sender as any).processors.keys()],
            pendingPorts: (sender as any).pendingPortCleanups.size,
            effects: Object.entries((analyser as any).processors).map(([id, value]: [string, any]) => ({ id, pitch: value.pitch, tempo: value.tempo, actualSoundTouch: !!value.soundTouch })),
            recordingMimeSupported: typeof MediaRecorder !== "undefined" && MediaRecorder.isTypeSupported("audio/webm; codecs=opus"),
            packets,
            statuses,
            ports,
            worklets,
            errors,
            sourceAcks,
            consoleAudio
        }
    },
    async cleanup() {
        await stopSender()
        const { player, stores, get } = await actual()
        for (const id of Object.keys(get(stores.playingAudio))) player.stop(id)
        win.api.send("AUDIO", { channel: "CLOSE_PORT", data: { id: "icecast" } })
        win.api.removeListener("MAIN", "audio-fullfork-status")
        win.api.removeListener("AUDIO", "audio-fullfork-acks")
        return this.snapshot()
    }
}

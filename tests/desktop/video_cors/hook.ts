// Test-only early observation. All intercepted calls delegate to real browser
// and production implementations. No source/routing/native result is faked.
const win = window as any,
    records: any[] = [],
    errors: any[] = [],
    identities = new WeakMap<object, number>()
let identity = 0
function id(element: object) {
    let value = identities.get(element)
    if (!value) identities.set(element, (value = ++identity))
    return value
}
function record(value: any) {
    if (records.length >= 256) throw new Error("Video CORS observer record budget")
    records.push(value)
}
const NativeAudio = window.Audio
window.Audio = new Proxy(NativeAudio, {
    construct(target, args) {
        const audio = Reflect.construct(target, args) as HTMLAudioElement
        record({ kind: "construct", id: id(audio), args, cors: audio.crossOrigin, source: audio.src })
        return audio
    }
})
const sourceProperty = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, "src")!
Object.defineProperty(HTMLMediaElement.prototype, "src", {
    ...sourceProperty,
    set(value: string) {
        record({ kind: "src", id: id(this), source: value, cors: this.crossOrigin })
        return sourceProperty.set!.call(this, value)
    }
})
const setAttribute = Element.prototype.setAttribute
Element.prototype.setAttribute = function (name: string, value: string) {
    if (this instanceof HTMLAudioElement && name.toLowerCase() === "src") record({ kind: "attributeSrc", id: id(this), source: value, cors: this.crossOrigin })
    return setAttribute.call(this, name, value)
}
for (const kind of ["error", "unhandledrejection", "freeshow:runtime-error"])
    window.addEventListener(kind, (event: any) => {
        if (errors.length >= 64) throw new Error("Video CORS error observer budget")
        errors.push(kind === "freeshow:runtime-error" ? { kind, detail: event.detail } : { kind, value: String(event.message ?? event.reason).slice(0, 2048) })
    })
let modules: Promise<any> | undefined,
    fixturePath = "",
    output = "",
    swaps = 0,
    capturedContext: any
const actual = () =>
    (modules ??= Promise.all([import("../../../src/frontend/components/media/video/videoPlayer"), import("../../../src/frontend/audio/audioAnalyser"), import("../../../src/frontend/audio/routing/audioRoutingManager"), import("../../../src/frontend/stores"), import("svelte/store")]).then(([player, analyser, routing, stores, store]) => {
        const originalUpdate = analyser.AudioAnalyser.updateSource.bind(analyser.AudioAnalyser)
        analyser.AudioAnalyser.updateSource = (...args: Parameters<typeof originalUpdate>) => {
            swaps++
            return originalUpdate(...args)
        }
        return { player: player.VideoPlayer, analyser: analyser.AudioAnalyser, manager: routing.AudioRoutingManager.getInstance(), stores, get: store.get }
    }))
function same() {
    const context = win.api.getRuntimeContext()
    if (!capturedContext || context.runtimeId !== capturedContext.runtimeId || context.libraryGeneration !== capturedContext.libraryGeneration || context.dataPath !== capturedContext.dataPath) throw new Error("Video test context changed")
}
win.__videoCors = {
    async configure(path: string, folder: string) {
        const { stores, get } = await actual()
        fixturePath = path
        capturedContext = win.api.getRuntimeContext()
        output = Object.keys(get(stores.outputs))[0]
        if (!output) throw new Error("Actual retained output configuration is required")
        stores.outputs.update((value: any) => ({ ...value, [output]: { ...value[output], enabled: true, active: true } }))
        stores.transitionData.update((value: any) => ({ ...value, media: { ...value.media, duration: 0 } }))
        // Explicit fixture routing through the real production manager. Same
        // actual input/main/speaker graph is retained across every comparison.
        stores.audioRouting.set({
            channels: [{ id: "main", name: "Video fixture" }],
            connections: [
                { from: `output_win_sub_${output}`, to: "main" },
                { from: "main", to: "speaker_default" }
            ]
        })
        await new Promise((resolve, reject) => {
            const listener = "video-cors-grants",
                timer = setTimeout(() => reject(new Error("Real READ_FOLDER grant deadline")), 8000)
            win.api.receive(
                "MAIN",
                (message: any, listenerId: unknown) => {
                    if (message.channel !== "READ_FOLDER" || listenerId !== listener) return
                    clearTimeout(timer)
                    win.api.removeListener("MAIN", listener)
                    if (!message.data[path]) return reject(new Error("Actual file absent from real READ_FOLDER"))
                    resolve(message.data)
                },
                listener
            )
            win.api.send("MAIN", { channel: "READ_FOLDER", data: { path: folder } }, listener)
        })
        const url = win.api.resolveFilePath(path)
        if (!/^http:\/\/127\.0\.0\.1:\d+\/[a-f0-9]{64}\/[a-f0-9]{32}$/.test(url)) throw new Error("Native issued media capability required")
        return { context: capturedContext, output, url, directResolveUnchanged: win.api.resolveFilePath(url) === url }
    },
    async start() {
        same()
        const { player } = await actual(),
            duration = await player.getDuration(fixturePath)
        const started = await player.start(fixturePath, { type: "item", volume: 1, loop: false }, [output])
        if (!started) throw new Error("Actual production VideoPlayer.start rejected fixture")
        return { duration, started, snapshot: await this.snapshot() }
    },
    async diagnosticReload() {
        same()
        const { player } = await actual(),
            audio = player.getAudio(fixturePath) as HTMLAudioElement
        const source = audio.src
        audio.pause()
        audio.crossOrigin = "anonymous"
        audio.src = source
        audio.load()
        await audio.play()
        return { source, snapshot: await this.snapshot() }
    },
    async loop() {
        same()
        const { player, stores } = await actual()
        stores.media.update((value: any) => ({ ...value, [fixturePath]: { ...value[fixturePath], softLoop: 1.2, fromTime: 0.4, toTime: 8, loop: true, volume: 1 } }))
        stores.playingVideos.update((items: any[]) => {
            const video = items.find((video: any) => video.path === fixturePath)
            if (video) Object.assign(video, { softLoop: 1.2, fromTime: 0.4, toTime: 8, loop: true })
            return items
        })
        player.updateProperties(fixturePath)
        const audio = player.getAudio(fixturePath) as HTMLAudioElement
        audio.currentTime = 7.2
        await audio.play()
        return { baselineSwaps: swaps, snapshot: await this.snapshot() }
    },
    async disableLoop() {
        const { player, stores } = await actual()
        stores.media.update((value: any) => ({ ...value, [fixturePath]: { ...value[fixturePath], softLoop: 1.2, loop: false } }))
        stores.playingVideos.update((items: any[]) => {
            const video = items.find((video: any) => video.path === fixturePath)
            if (video) video.loop = false
            return items
        })
        player.updateProperties(fixturePath)
    },
    async replaceResource(url: string) {
        same()
        const { player } = await actual(),
            audio = player.getAudio(fixturePath) as HTMLAudioElement
        if (!/^http:\/\/127\.0\.0\.1:\d+\/[a-f0-9]{64}\/[a-f0-9]{32}$/.test(url) || url === audio.src || win.api.resolveFilePath(url) !== url) throw new Error("Different already-granted direct URL required")
        const before = id(audio)
        audio.pause()
        audio.src = url
        audio.load()
        await audio.play()
        return { id: id(audio), before, url, snapshot: await this.snapshot() }
    },
    async snapshot() {
        const { player, analyser, stores, get } = await actual(),
            video = player.getPlaying(fixturePath),
            audio = video?.audio as HTMLAudioElement | undefined,
            encode = (item?: HTMLAudioElement) => (item ? { id: id(item), cors: item.crossOrigin, source: item.src, time: item.currentTime, duration: item.duration, paused: item.paused, readyState: item.readyState, error: item.error?.code ?? null } : null),
            context = analyser.getAudioContext()
        return {
            origin: location.origin,
            context: win.api?.getRuntimeContext(),
            audio: encode(audio),
            crossfade: encode((video as any)?.crossfadeAudio),
            swaps,
            swapping: !!(video as any)?.isSwapping,
            contextClock: context.currentTime,
            contextState: context.state,
            sampleRate: context.sampleRate,
            sourceLevel: analyser.getChannelLiveVolume(`${fixturePath}_${output}`),
            mainLevel: analyser.getChannelLiveVolume("main"),
            speakerLevel: analyser.getChannelLiveVolume("speaker_default"),
            sources: analyser.getActiveSourceIds(),
            gains: Object.entries((analyser as any).gainNodes).map(([key, node]: [string, any]) => ({ key, value: node.gain.value })),
            routing: get(stores.audioRouting),
            records: [...records],
            errors: [...errors],
            playing: get(stores.playingVideos).length
        }
    },
    async stop() {
        const { player } = await actual()
        await player.stop(fixturePath, output)
        return this.snapshot()
    }
}

import { hydrateSpotify, type SpotifyRestoration } from "./spotify"
// Preserve the existing preload API while the runtime moves to Rust.
// This module has no dependency on Electron or on the Svelte app.
import { hydrateFileBrowse } from "./fileStats"
import { installNativeDrop } from "./nativeDrop"
import { createMediaUrlRegistry, type LocalAssetGrant } from "./mediaUrl"
import { createCaptureBridge, type CaptureTransport } from "./captureBridge"
import { hydrateExif, type ExifHydration } from "./exif"
import { hydrateMidi, type MidiHydration } from "./midi"
import { createAudioPortBridge, type AudioPortOptions } from "./audioPortBridge"
import { encodeAudioFrame } from "./audioRaw"
import { createTimecodeBridge, type TimecodeBridgeOptions, type TimecodePublication } from "./timecodeBridge"
import { encodeTimecodeCommand, encodeTimecodeLtc } from "./timecodeWire"
import { createNowPlayingBridge, type NowPlayingTransport, type NowPlayingPublication } from "./nowPlayingBridge"
import { createCoverBridge, type CoverPublication, type CoverBridgeOptions } from "./coverBridge"

type Receiver = (data: any, listenerId?: string) => void
export type RuntimeContext = {
    runtimeId: string
    libraryGeneration: number
    dataPath: string
}
export type RuntimeEvent = {
    channel: string
    message: any
    listenerId?: string
    context?: RuntimeContext
    nativeSpotify?: SpotifyRestoration
    nativeAssets?: LocalAssetGrant[]
    nativeCapture?: { id: string }
    nativeExif?: ExifHydration
    nativeMidi?: {
        hydration: MidiHydration
        delivery?: { id: string; context: RuntimeContext; run: string; listener: string }
    }
    nativeAudio?: { id: string; run: string; ownUndefinedError: boolean }
    nativeTimecode?: Omit<TimecodePublication, "event">
    nativeNowPlaying?: Omit<NowPlayingPublication, "event">
    nativeCover?: Omit<CoverPublication, "event">
    nativeRemote?: {
        deliveryId: string
        run: string
        server: string
        sourceSocket: string
        origin: "connection" | "client" | "disconnect"
        message: any
    }
}
export type Transport = (request: { channel: string; data?: any; listenerId?: string; context?: RuntimeContext }) => Promise<RuntimeEvent[]>
const mutations = new Set(["SAVE", "UPDATE_DATA_PATH", "SET_STORE_VALUE", "SET_MEDIA_FOLDER_PATH", "RESTORE", "DELETE_BACKUP"])
const outputLifecycle = new Set(["CREATE", "REMOVE", "TOGGLE_OUTPUTS", "UPDATE_BOUNDS", "SET_VALUE", "FOCUS", "TO_FRONT", "MOVE"])
const remoteChannels = new Set(["REMOTE", "STAGE", "CONTROLLER"])
const remoteCommands = new Set(["START", "STOP", "SERVER_DATA"])
const midiCommands = new Set(["GET_MIDI_INPUTS", "GET_MIDI_OUTPUTS", "SEND_MIDI", "RECEIVE_MIDI", "CLOSE_MIDI"])
const timecodeCommands = new Set(["TIMECODE_START", "TIMECODE_STOP", "TIMECODE_VALUE", "TIMECODE_STATUS", "TIMECODE_AUDIO_DATA"])
const nowPlayingCommands = new Set(["NOW_PLAYING", "NOW_PLAYING_UNSET", "OPEN_NOW_PLAYING"])
export type TimecodeTransport = Pick<TimecodeBridgeOptions, "command" | "audio" | "complete" | "retire">
export type RemoteCompletion = (id: string, accepted: boolean) => Promise<unknown>
export type CoverCompletion = CoverBridgeOptions["complete"]
export type MidiCompletion = (id: string, accepted: boolean) => Promise<unknown>
export type AudioTransport = Pick<AudioPortOptions, "postPort" | "transport" | "disconnect" | "closeContext"> & {
    completeStatus: (context: RuntimeContext, id: string, accepted: boolean) => Promise<unknown>
}

export function createRustApi(transport: Transport, reportError: (error: unknown, request: unknown) => void, initialContext?: RuntimeContext, captureTransport?: CaptureTransport, remoteCompletion?: RemoteCompletion, midiCompletion?: MidiCompletion, audioTransport?: AudioTransport, timecodeTransport?: TimecodeTransport, nowPlayingTransport?: NowPlayingTransport, coverCompletion?: CoverCompletion) {
    const receivers = new Map<string, Set<Receiver>>()
    const named = new Map<string, { channel: string; receiver: Receiver }>()
    let context = initialContext ? { ...initialContext } : undefined
    const audio = audioTransport ? createAudioPortBridge({ ...audioTransport, getContext: () => context, report: (error) => reportError(error, { channel: "AUDIO" }) }) : undefined
    const timecode = timecodeTransport ? createTimecodeBridge({ ...timecodeTransport, getContext: () => context, deliver: (message) => deliver("MAIN", message), report: (error) => reportError(error, { channel: "TIMECODE" }) }) : undefined
    const nowPlaying = nowPlayingTransport ? createNowPlayingBridge({ transport: nowPlayingTransport, getContext: () => context, deliver: (message) => deliver("MAIN", message), report: (error) => reportError(error, { channel: "NOW_PLAYING" }) }) : undefined
    const covers = coverCompletion ? createCoverBridge({ getContext: () => context, deliver: (message) => deliver("MAIN", message), report: (error) => reportError(error, { channel: "native-cover" }), complete: coverCompletion }) : undefined
    const mediaUrls = createMediaUrlRegistry(
        () => context,
        (error) => reportError(error, { channel: "native-assets" })
    )
    const captures = createCaptureBridge(
        () => context,
        captureTransport,
        (error) => reportError(error, { channel: "native-thumbnail" })
    )
    let mutationQueue = Promise.resolve()
    let mediaSettingsQueue = Promise.resolve()
    let outputQueue = Promise.resolve()
    let outputLifecycleQueue = Promise.resolve()
    let remoteQueue = Promise.resolve()
    const remoteConnections = new Map<string, RuntimeContext>()

    function deliver(channel: string, message: any, listenerId?: string) {
        for (const receiver of [...(receivers.get(channel) || [])]) {
            try {
                receiver(message, listenerId)
            } catch (error) {
                reportError(error, { channel, message, listenerId })
            }
        }
    }

    function remote(event: RuntimeEvent): boolean {
        const metadata = event.nativeRemote!
        const frozen = event.context
        if (!frozen || !remoteChannels.has(event.channel) || event.channel !== metadata.server || !/^native-remote-[0-9a-f]{32}$/.test(metadata.deliveryId) || !/^[0-9a-f]{32}$/.test(metadata.run) || typeof metadata.sourceSocket !== "string" || !metadata.sourceSocket || metadata.sourceSocket.length > 128 || !["connection", "client", "disconnect"].includes(metadata.origin)) {
            reportError({ code: "remote_publication", message: "Invalid native remote envelope" }, event)
            return false
        }
        const key = JSON.stringify([frozen.runtimeId, metadata.run, event.channel, metadata.sourceSocket])
        const owned = remoteConnections.get(key)
        const equal = (a: RuntimeContext | undefined, b: RuntimeContext) => !!a && a.runtimeId === b.runtimeId && a.libraryGeneration === b.libraryGeneration && a.dataPath === b.dataPath
        const cleanup = metadata.origin === "disconnect" && equal(owned, frozen)
        // Only an owned genuine disconnect crosses a library switch. It neither
        // changes context nor permits another old client action through.
        if (!cleanup && !equal(context, frozen)) return false
        if (metadata.origin !== "client") {
            if (metadata.message?.channel !== (metadata.origin === "connection" ? "CONNECTION" : "DISCONNECT") || metadata.message?.id !== metadata.sourceSocket) return false
            if (metadata.origin === "connection") {
                if (owned || remoteConnections.size >= 192) return false
                remoteConnections.set(key, { ...frozen })
            } else {
                if (!cleanup || typeof metadata.message !== "object" || Array.isArray(metadata.message) || Object.keys(metadata.message).length !== 2) return false
                remoteConnections.delete(key)
            }
        }
        deliver(event.channel, metadata.message, event.listenerId)
        return true
    }

    function dispatch(event: RuntimeEvent, origin?: { channel: string; data?: any; listenerId?: string }) {
        if (event.nativeCover) {
            if (!covers) {
                reportError({ code: "cover_unavailable", message: "Native cover notifications are unavailable in this window" }, event)
                return
            }
            const sourceEvent = { channel: event.channel, message: event.message, context: event.context, ...(event.listenerId === undefined ? {} : { listenerId: event.listenerId }) }
            covers.publish({ ...event.nativeCover, event: sourceEvent })
            return
        }
        if (event.nativeNowPlaying) {
            if (!nowPlaying) {
                reportError({ code: "now_playing_unavailable", message: "Native now-playing is unavailable in this window" }, event)
                return
            }
            const sourceEvent = { channel: event.channel, message: event.message, context: event.context, ...(event.listenerId === undefined ? {} : { listenerId: event.listenerId }) }
            nowPlaying.publish({ ...event.nativeNowPlaying, event: sourceEvent })
            return
        }
        if (event.nativeTimecode) {
            if (!timecode) {
                reportError({ code: "timecode_unavailable", message: "Native timecode is unavailable in this window" }, event)
                return
            }
            const sourceEvent = { channel: event.channel, message: event.message, context: event.context, ...(event.listenerId === undefined ? {} : { listenerId: event.listenerId }) }
            timecode.publish({ ...event.nativeTimecode, event: sourceEvent as TimecodePublication["event"] })
            return
        }
        if (event.nativeAudio) {
            const metadata = event.nativeAudio
            const equal = (a: RuntimeContext | undefined, b: RuntimeContext | undefined) => !!a && !!b && a.runtimeId === b.runtimeId && a.libraryGeneration === b.libraryGeneration && a.dataPath === b.dataPath
            const id = typeof metadata.id === "string" && /^native-audio-[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(metadata.id)
            let accepted = false
            try {
                if (!equal(context, event.context)) return
                const data = event.message?.data
                if (
                    !id ||
                    typeof metadata.run !== "string" ||
                    !/^[1-9][0-9]{0,19}$/.test(metadata.run) ||
                    BigInt(metadata.run) > 18446744073709551615n ||
                    typeof metadata.ownUndefinedError !== "boolean" ||
                    event.channel !== "MAIN" ||
                    event.message?.channel !== "ICECAST_STATUS" ||
                    event.listenerId !== undefined ||
                    !data ||
                    typeof data !== "object" ||
                    Array.isArray(data) ||
                    !Object.hasOwn(data, "connected") ||
                    typeof data.connected !== "boolean" ||
                    !Object.hasOwn(data, "connecting") ||
                    typeof data.connecting !== "boolean" ||
                    Object.keys(data).some((key) => !["connected", "connecting", "error"].includes(key)) ||
                    (metadata.ownUndefinedError ? Object.hasOwn(data, "error") : !Object.hasOwn(data, "error") || typeof data.error !== "string")
                )
                    throw new Error("Invalid native audio status")
                deliver("MAIN", { ...event.message, data: { connected: data.connected, connecting: data.connecting, error: metadata.ownUndefinedError ? undefined : data.error } })
                accepted = true
            } catch (error) {
                reportError({ code: "audio_publication", message: String(error) }, event)
            } finally {
                if (id && event.context && audioTransport)
                    audioTransport.completeStatus(event.context, metadata.id, accepted).catch((error) => {
                        if (equal(context, event.context)) reportError(error, { channel: "native-audio-completion" })
                    })
            }
            return
        }
        if (event.nativeSpotify) {
            // Global OS metadata keeps a process-lifetime native owner. The
            // reply still belongs only to its actual frozen request context.
            if (!context || !event.context || event.context.runtimeId !== context.runtimeId || event.context.libraryGeneration !== context.libraryGeneration || event.context.dataPath !== context.dataPath) return
            try {
                if (event.channel !== "MAIN" || event.message.channel !== "SPOTIFY_GET_STATE" || origin?.channel !== "MAIN" || origin.data?.channel !== "SPOTIFY_GET_STATE" || event.listenerId !== origin.listenerId) throw new Error("Unexpected native Spotify envelope")
                const data = hydrateSpotify(event.message.data, event.nativeSpotify)
                deliver("MAIN", { channel: "SPOTIFY_GET_STATE", data }, event.listenerId)
            } catch (error) {
                reportError({ code: "spotify_publication", message: String(error) }, event)
            }
            return
        }
        if (event.nativeMidi) {
            const metadata = event.nativeMidi
            const delivery = metadata.delivery
            const same = (left: RuntimeContext | undefined, right: RuntimeContext | undefined) => !!left && !!right && left.runtimeId === right.runtimeId && left.libraryGeneration === right.libraryGeneration && left.dataPath === right.dataPath
            const token = (value: string) => typeof value === "string" && /^[1-9][0-9]{0,15}$/.test(value) && BigInt(value) <= 9007199254740991n
            let accepted = false
            try {
                if (event.channel !== "MAIN" || !same(context, event.context)) return
                const callback = event.message?.channel === "RECEIVE_MIDI2"
                if (callback ? !delivery || !/^native-midi-[0-9a-f]{32}$/.test(delivery.id) || !same(event.context, delivery.context) || !token(delivery.run) || !token(delivery.listener) || event.listenerId !== undefined : delivery !== undefined || !["GET_MIDI_INPUTS", "GET_MIDI_OUTPUTS"].includes(event.message?.channel)) throw new Error("Invalid native MIDI publication")
                const data = hydrateMidi(event.message.channel, event.message.data, metadata.hydration)
                deliver(event.channel, { ...event.message, data }, event.listenerId)
                accepted = true
            } catch (error) {
                reportError({ code: "midi_publication", message: String(error) }, event)
            } finally {
                // This receipt bounds synchronous retained dispatch. MIDI send,
                // listen and close retain their original void reply contract.
                if (midiCompletion && delivery && /^native-midi-[0-9a-f]{32}$/.test(delivery.id))
                    midiCompletion(delivery.id, accepted).catch((error) => {
                        // Context retirement deliberately revokes pending nonces.
                        if (same(context, event.context)) reportError(error, { channel: "native-midi-completion" })
                    })
            }
            return
        }
        if (event.nativeRemote) {
            let accepted = false
            try {
                accepted = remote(event)
            } finally {
                // This private native completion only bounds synchronous UI
                // delivery. The original Socket.IO protocol has no success ACK.
                if (remoteCompletion && /^native-remote-[0-9a-f]{32}$/.test(event.nativeRemote.deliveryId)) {
                    remoteCompletion(event.nativeRemote.deliveryId, accepted).catch((error) => reportError(error, { channel: "native-remote-completion" }))
                }
            }
            return
        }
        if (event.channel === "MAIN" && event.message.channel === "SPOTIFY_GET_STATE") {
            reportError({ code: "spotify_publication", message: "Native Spotify reply lacks restoration" }, event)
            return
        }
        if (event.channel === "MAIN" && event.message.channel === "AI_GET_STATUS") {
            if (!context || !event.context || event.context.runtimeId !== context.runtimeId || event.context.libraryGeneration !== context.libraryGeneration || event.context.dataPath !== context.dataPath) return
            const data = event.message.data
            const status = data?.nemotron
            const keys = status?.ready === true ? ["ready", "localPath"] : ["ready", "error"]
            if (origin?.channel !== "MAIN" || origin.data?.channel !== "AI_GET_STATUS" || event.listenerId !== origin.listenerId || !data || JSON.stringify(Object.keys(data)) !== JSON.stringify(["nemotron"]) || !status || typeof status.ready !== "boolean" || JSON.stringify(Object.keys(status)) !== JSON.stringify(keys) || typeof status[keys[1]] !== "string" || !status[keys[1]] || status[keys[1]].length > 32768) {
                reportError({ code: "ai_status_publication", message: "Unexpected explicit native Nemotron status" }, event)
                return
            }
            deliver("MAIN", { channel: "AI_GET_STATUS", data }, event.listenerId)
            return
        }
        if (event.context) {
            if (context && (event.context.runtimeId !== context.runtimeId || event.context.libraryGeneration < context.libraryGeneration)) return
            // Only the completed root-selection response may advance a library.
            // A path query can finish earlier and must not retag old UI data.
            const rootSelection = origin?.channel === "MAIN" && origin.data?.channel === "UPDATE_DATA_PATH"
            if (context && event.context.libraryGeneration > context.libraryGeneration && !(rootSelection && event.channel === "MAIN" && event.message.channel === "DATA_PATH")) return
            const previous = context
            const changed = previous && (previous.libraryGeneration !== event.context.libraryGeneration || previous.dataPath !== event.context.dataPath)
            context = { ...event.context }
            if (changed && audio) audio.contextChanged().catch((error) => reportError(error, { channel: "native-audio-retirement" }))
            if (changed && timecode) timecode.contextChanged().catch((error) => reportError(error, { channel: "native-timecode-retirement" }))
            if (changed && nowPlaying) nowPlaying.contextChanged(previous).catch((error) => reportError(error, { channel: "native-now-playing-retirement" }))
            if (changed && covers) covers.retire(previous).catch((error) => reportError(error, { channel: "native-cover-retirement" }))
        }
        if (event.nativeAssets && (!event.context || !mediaUrls.publish(event.context, event.nativeAssets))) {
            reportError({ code: "assets_publication", message: "Native media grants could not be published" }, event)
        }
        if (event.nativeCapture && (event.channel !== "MAIN" || event.message.channel !== "CAPTURE_CANVAS" || event.nativeCapture.id !== event.message.data?.id || !event.context || !captures.register(event.context, event.nativeCapture.id, event.message.data?.output))) return
        if (event.nativeExif) {
            try {
                if (event.channel !== "MAIN" || event.message.channel !== "READ_EXIF" || !event.context) throw new Error("Unexpected native EXIF envelope")
                hydrateExif(event.message.data, event.nativeExif)
            } catch (error) {
                reportError({ code: "exif_publication", message: String(error) }, event)
                return
            }
        }
        if (event.channel === "MAIN" && event.message.channel === "READ_FOLDER") hydrateFileBrowse("READ_FOLDER", event.message.data)
        // Snapshot listeners: handlers may unregister themselves during dispatch.
        deliver(event.channel, event.message, event.listenerId)
    }

    function removeListener(channel: string, id: string) {
        const listener = named.get(id)
        if (!listener || listener.channel !== channel) return
        receivers.get(channel)?.delete(listener.receiver)
        if (!receivers.get(channel)?.size) receivers.delete(channel)
        named.delete(id)
    }

    return {
        dispatch,
        publishAssets: mediaUrls.publish,
        api: {
            send(channel: string, data?: any, listenerId?: string) {
                if (channel === "AUDIO") {
                    if (audio) audio.handle(data)
                    else reportError({ code: "audio_unavailable", message: "Native AUDIO input is unavailable in this window" }, { channel })
                    return
                }
                if (channel === "MAIN" && timecodeCommands.has(data?.channel)) {
                    if (timecode) timecode.handle(data)
                    else reportError({ code: "timecode_unavailable", message: "Native timecode is unavailable in this window" }, { channel: "TIMECODE" })
                    return
                }
                if (channel === "MAIN" && nowPlayingCommands.has(data?.channel)) {
                    if (nowPlaying) nowPlaying.handle(data)
                    else reportError({ code: "now_playing_unavailable", message: "Native now-playing is unavailable in this window" }, { channel: "NOW_PLAYING" })
                    return
                }
                if (channel === "MAIN" && data?.channel === "SAVE_IMAGE" && captures.complete(data.data)) return
                if (channel === "MAIN" && data?.channel === "AI_GET_STATUS") {
                    // Capture once using Electron's clone semantics, then
                    // decide the branch before JSON can turn truthy values null.
                    try {
                        data = structuredClone(data)
                    } catch {
                        reportError({ code: "ai_status_unsupported", message: "Model status payload cannot be cloned" }, { channel, data, listenerId })
                        return
                    }
                    const model = data.data?.modelId
                    const noModel = model === undefined || model === null || model === false || model === "" || (typeof model === "number" && (model === 0 || Number.isNaN(model)))
                    if (data.data?.engineId !== "nemotron" || !noModel) {
                        reportError({ code: "ai_status_unsupported", message: "Only explicit Nemotron status without a selected model is supported" }, { channel, data, listenerId })
                        return
                    }
                }
                const mediaCopy = channel === "MAIN" && ["BUNDLE_MEDIA_FILES", "MEDIA_FOLDER_COPY"].includes(data?.channel)
                // Electron serializes at send time. Output updates often reuse
                // mutable Svelte objects; queued cues must keep their snapshot.
                // Pure catalogue requests also retain the original command;
                // Spotify validates this origin after its native wait.
                if (mediaCopy || channel === "OUTPUT" || remoteChannels.has(channel) || (channel === "MAIN" && (["SPOTIFY_GET_STATE", "GET_CONTENT_PROVIDERS"].includes(data?.channel) || remoteCommands.has(data?.channel) || midiCommands.has(data?.channel)))) data = structuredClone(data)
                // SAVE snapshots its context before its deferred send. Other
                // requests belong to the library visible when send is called.
                const captured = channel === "MAIN" && data?.channel === "SAVE" ? data.data?.runtimeContext || context : context
                const request = {
                    channel,
                    data,
                    listenerId,
                    ...(captured ? { context: { ...captured } } : {})
                }
                // Native replies are asynchronous, like Electron's IPC. This
                // also lets requestMain register its receiver after send().
                const serialized = channel === "MAIN" && mutations.has(data?.channel)
                const mediaSettingsWrite = channel === "MAIN" && (data?.channel === "SET_MEDIA_FOLDER_PATH" || (data?.channel === "SET_STORE_VALUE" && data?.data?.file === "config"))
                const mediaSettingsQuery = channel === "MAIN" && data?.channel === "GET_MEDIA_FOLDER_PATH"
                const mediaSettingsRead = mediaCopy || mediaSettingsQuery || (channel === "MAIN" && data?.channel === "LOCATE_MEDIA_FILE")
                const outputMutation = channel === "OUTPUT" && outputLifecycle.has(data?.channel)
                const outputMessage = channel === "OUTPUT" && !outputMutation
                const remoteRequest = remoteChannels.has(channel) || (channel === "MAIN" && remoteCommands.has(data?.channel))
                const queue = serialized ? mutationQueue : outputMutation ? outputLifecycleQueue : outputMessage ? outputQueue : remoteRequest ? remoteQueue : Promise.resolve()
                // A reset immediately asks for the default media path. Wait for
                // its earlier setting write, while unrelated saves/backups do
                // not delay reads of already committed media configuration.
                const ready = mediaSettingsRead || mediaSettingsWrite ? Promise.all([queue, mediaSettingsQueue]) : queue
                const complete = ready
                    .then(async () => {
                        if (channel === "MAIN" && data?.channel === "CLOSE") {
                            await Promise.all([audio?.close().catch((error) => reportError(error, { channel: "native-audio-retirement" })), timecode?.close().catch((error) => reportError(error, { channel: "native-timecode-retirement" })), nowPlaying?.close().catch((error) => reportError(error, { channel: "native-now-playing-retirement" })), covers?.close().catch((error) => reportError(error, { channel: "native-cover-retirement" }))])
                            // Native CLOSE still attempts every independent owner
                            // after a frontend drain error, retaining actual retry.
                        }
                        // A library switch cancels obsolete output work. It
                        // must never replay the old snapshot into new windows.
                        if ((channel === "OUTPUT" || remoteChannels.has(channel)) && request.context && context && (request.context.runtimeId !== context.runtimeId || request.context.libraryGeneration !== context.libraryGeneration)) return []
                        return transport(request)
                    })
                    .then((events) => events.forEach((event) => dispatch(event, request)))
                    .catch((error) => {
                        if (channel === "OUTPUT" && (error as any)?.code === "stale_library") return
                        reportError(error, request)
                    })
                if (serialized) mutationQueue = complete.catch(() => {})
                if (mediaSettingsWrite || mediaSettingsQuery) mediaSettingsQueue = complete.catch(() => {})
                if (outputMutation) outputLifecycleQueue = complete.catch(() => {})
                if (outputMessage) outputQueue = complete.catch(() => {})
                if (remoteRequest) remoteQueue = complete.catch(() => {})
            },
            getRuntimeContext: () => (context ? { ...context } : undefined),
            resolveFilePath: mediaUrls.resolve,
            receive(channel: string, receiver: Receiver, id?: string) {
                if (id && named.has(id)) {
                    const previous = named.get(id)!
                    removeListener(previous.channel, id)
                }
                if (!receivers.has(channel)) receivers.set(channel, new Set())
                receivers.get(channel)!.add(receiver)
                if (id) named.set(id, { channel, receiver })
            },
            removeListener,
            getListeners: () => [...receivers].map(([channel, listeners]) => [channel, listeners.size]),
            showFilePath(file: File) {
                // Browser-created Files have real bytes but no native path.
                // The main-window installer replaces this with its registry
                // for native drop descriptors, retaining context validation.
                const nativePath = (file as File & { path?: string }).path
                return typeof nativePath === "string" ? nativePath : ""
            }
        }
    }
}

export async function installRustBridge(target: Window = window) {
    const tauri = (target as any).__TAURI__
    if (!tauri?.core?.invoke || !tauri?.event?.listen) throw new Error("The Rust desktop runtime is unavailable")
    const nativeWindow = tauri.webviewWindow?.getCurrentWebviewWindow?.()
    if (!nativeWindow?.label) throw new Error("The native window identity is unavailable")
    const context = await tauri.core.invoke("runtime_context")
    if (!context?.runtimeId || !Number.isSafeInteger(context.libraryGeneration) || typeof context.dataPath !== "string") throw new Error("The Rust runtime context is unavailable")
    const bridge = createRustApi(
        (request) => tauri.core.invoke("ipc_dispatch", { request }),
        (error, request) => {
            console.error("FreeShow runtime request failed", { request, error })
            target.dispatchEvent(
                new CustomEvent("freeshow:runtime-error", {
                    detail: { error, request }
                })
            )
        },
        context,
        ({ id, buffer, failed }) => tauri.core.invoke("thumbnail_complete", buffer, { headers: { "x-freeshow-capture": id, "x-freeshow-capture-failed": failed ? "1" : "0" } }),
        (id, accepted) => tauri.core.invoke("remote_delivery_complete", { id, accepted }),
        (id, accepted) => tauri.core.invoke("midi_delivery_complete", { id, accepted }),
        nativeWindow.label === "main"
            ? {
                  postPort: (message, origin, ports) => target.postMessage(message, origin, ports),
                  transport: (bytes, metadata, frozen, origin) => tauri.core.invoke("audio_pcm", encodeAudioFrame(bytes, metadata, frozen, origin)),
                  disconnect: (frozen) => tauri.core.invoke("audio_disconnect", { context: frozen }),
                  closeContext: (frozen) => tauri.core.invoke("audio_close_context", { context: frozen }),
                  completeStatus: (frozen, id, accepted) => tauri.core.invoke("audio_status_complete", { context: frozen, id, accepted })
              }
            : undefined,
        nativeWindow.label === "main"
            ? {
                  command: (message, frozen) => tauri.core.invoke("timecode_command", encodeTimecodeCommand(message, frozen)),
                  audio: (bytes, mode, frozen) => tauri.core.invoke("timecode_audio", encodeTimecodeLtc(bytes, mode, frozen)),
                  complete: (frozen, id, accepted) => tauri.core.invoke("timecode_complete", { context: frozen, id, accepted }),
                  retire: (frozen, terminal) => tauri.core.invoke("timecode_retire", { context: frozen, terminal })
              }
            : undefined,
        nativeWindow.label === "main"
            ? {
                  command: (body) => tauri.core.invoke("now_playing_command", body),
                  complete: (frozen, id, accepted) => tauri.core.invoke("now_playing_complete", { context: frozen, id, accepted }),
                  retire: (frozen, terminal) => tauri.core.invoke("now_playing_retire", { context: frozen, terminal })
              }
            : undefined,
        nativeWindow.label === "main" ? (frozen, id, accepted) => tauri.core.invoke("cover_complete", { context: frozen, id, accepted }) : undefined
    )
    await tauri.event.listen(
        "freeshow://ipc",
        (event: { payload: RuntimeEvent }) => bridge.dispatch(event.payload),
        // Global Any listeners also observe events addressed to other windows.
        { target: { kind: "WebviewWindow", label: nativeWindow.label } }
    )
    if (nativeWindow.label === "main") {
        const drop = await installNativeDrop(target, {
            getRuntimeContext: bridge.api.getRuntimeContext,
            publishAssets: bridge.publishAssets,
            reportError: (error) => {
                console.error("FreeShow native file drop failed", error)
                target.dispatchEvent(new CustomEvent("freeshow:runtime-error", { detail: { error, request: { channel: "native-drop" } } }))
            }
        })
        bridge.api.showFilePath = drop.showFilePath
    }
    ;(target as any).api = bridge.api
    return bridge
}

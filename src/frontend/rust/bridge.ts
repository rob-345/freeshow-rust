// Preserve the existing preload API while the runtime moves to Rust.
// This module has no dependency on Electron or on the Svelte app.
import { hydrateFileBrowse } from "./fileStats"
import { installNativeDrop } from "./nativeDrop"
import { createMediaUrlRegistry, type LocalAssetGrant } from "./mediaUrl"
import { createCaptureBridge, type CaptureTransport } from "./captureBridge"
import { hydrateExif, type ExifHydration } from "./exif"
import { hydrateMidi, type MidiHydration } from "./midi"

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
    nativeAssets?: LocalAssetGrant[]
    nativeCapture?: { id: string }
    nativeExif?: ExifHydration
    nativeMidi?: {
        hydration: MidiHydration
        delivery?: { id: string; context: RuntimeContext; run: string; listener: string }
    }
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
export type RemoteCompletion = (id: string, accepted: boolean) => Promise<unknown>
export type MidiCompletion = (id: string, accepted: boolean) => Promise<unknown>

export function createRustApi(transport: Transport, reportError: (error: unknown, request: unknown) => void, initialContext?: RuntimeContext, captureTransport?: CaptureTransport, remoteCompletion?: RemoteCompletion, midiCompletion?: MidiCompletion) {
    const receivers = new Map<string, Set<Receiver>>()
    const named = new Map<string, { channel: string; receiver: Receiver }>()
    let context = initialContext ? { ...initialContext } : undefined
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

    function dispatch(event: RuntimeEvent, origin?: { channel: string; data?: any }) {
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
        if (event.context) {
            if (context && (event.context.runtimeId !== context.runtimeId || event.context.libraryGeneration < context.libraryGeneration)) return
            // Only the completed root-selection response may advance a library.
            // A path query can finish earlier and must not retag old UI data.
            const rootSelection = origin?.channel === "MAIN" && origin.data?.channel === "UPDATE_DATA_PATH"
            if (context && event.context.libraryGeneration > context.libraryGeneration && !(rootSelection && event.channel === "MAIN" && event.message.channel === "DATA_PATH")) return
            context = { ...event.context }
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
                if (channel === "MAIN" && data?.channel === "SAVE_IMAGE" && captures.complete(data.data)) return
                const mediaCopy = channel === "MAIN" && ["BUNDLE_MEDIA_FILES", "MEDIA_FOLDER_COPY"].includes(data?.channel)
                // Electron serializes at send time. Output updates often reuse
                // mutable Svelte objects; queued cues must keep their snapshot.
                if (mediaCopy || channel === "OUTPUT" || remoteChannels.has(channel) || (channel === "MAIN" && (remoteCommands.has(data?.channel) || midiCommands.has(data?.channel)))) data = structuredClone(data)
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
                    .then(() => {
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
        (id, accepted) => tauri.core.invoke("midi_delivery_complete", { id, accepted })
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

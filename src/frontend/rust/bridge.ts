// Preserve the existing preload API while the runtime moves to Rust.
// This module has no dependency on Electron or on the Svelte app.
import { hydrateFileBrowse } from "./fileStats"
import { installNativeDrop } from "./nativeDrop"
import { createMediaUrlRegistry, type LocalAssetGrant } from "./mediaUrl"
import { createCaptureBridge, type CaptureTransport } from "./captureBridge"
import { hydrateExif, type ExifHydration } from "./exif"

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
}
export type Transport = (request: { channel: string; data?: any; listenerId?: string; context?: RuntimeContext }) => Promise<RuntimeEvent[]>
const mutations = new Set(["SAVE", "UPDATE_DATA_PATH", "SET_STORE_VALUE", "SET_MEDIA_FOLDER_PATH", "RESTORE", "DELETE_BACKUP"])
const outputLifecycle = new Set(["CREATE", "REMOVE", "TOGGLE_OUTPUTS", "UPDATE_BOUNDS", "SET_VALUE", "FOCUS", "TO_FRONT", "MOVE"])

export function createRustApi(transport: Transport, reportError: (error: unknown, request: unknown) => void, initialContext?: RuntimeContext, captureTransport?: CaptureTransport) {
    const receivers = new Map<string, Set<Receiver>>()
    const named = new Map<string, { channel: string; receiver: Receiver }>()
    let context = initialContext ? { ...initialContext } : undefined
    const mediaUrls = createMediaUrlRegistry(() => context, (error) => reportError(error, { channel: "native-assets" }))
    const captures = createCaptureBridge(() => context, captureTransport, (error) => reportError(error, { channel: "native-thumbnail" }))
    let mutationQueue = Promise.resolve()
    let mediaSettingsQueue = Promise.resolve()
    let outputQueue = Promise.resolve()
    let outputLifecycleQueue = Promise.resolve()

    function dispatch(event: RuntimeEvent, origin?: { channel: string; data?: any }) {
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
        for (const receiver of [...(receivers.get(event.channel) || [])]) {
            try {
                receiver(event.message, event.listenerId)
            } catch (error) {
                reportError(error, event)
            }
        }
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
                // Electron serializes at send time. Output updates often reuse
                // mutable Svelte objects; queued cues must keep their snapshot.
                if (channel === "OUTPUT") data = structuredClone(data)
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
                const mediaSettingsRead = mediaSettingsQuery || (channel === "MAIN" && data?.channel === "LOCATE_MEDIA_FILE")
                const outputMutation = channel === "OUTPUT" && outputLifecycle.has(data?.channel)
                const outputMessage = channel === "OUTPUT" && !outputMutation
                const queue = serialized ? mutationQueue : outputMutation ? outputLifecycleQueue : outputMessage ? outputQueue : Promise.resolve()
                // A reset immediately asks for the default media path. Wait for
                // its earlier setting write, while unrelated saves/backups do
                // not delay reads of already committed media configuration.
                const ready = mediaSettingsRead || mediaSettingsWrite ? Promise.all([queue, mediaSettingsQueue]) : queue
                const complete = ready
                    .then(() => {
                        // A library switch cancels obsolete output work. It
                        // must never replay the old snapshot into new windows.
                        if (channel === "OUTPUT" && request.context && context && (request.context.runtimeId !== context.runtimeId || request.context.libraryGeneration !== context.libraryGeneration)) return []
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
        ({ id, buffer, failed }) => tauri.core.invoke("thumbnail_complete", buffer, { headers: { "x-freeshow-capture": id, "x-freeshow-capture-failed": failed ? "1" : "0" } })
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

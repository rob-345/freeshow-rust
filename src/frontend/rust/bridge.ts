// Preserve the existing preload API while the runtime moves to Rust.
// This module has no dependency on Electron or on the Svelte app.
import { installNativeDrop } from "./nativeDrop"

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
}
export type Transport = (request: { channel: string; data?: any; listenerId?: string; context?: RuntimeContext }) => Promise<RuntimeEvent[]>
const mutations = new Set(["SAVE", "UPDATE_DATA_PATH", "SET_STORE_VALUE", "SET_MEDIA_FOLDER_PATH", "RESTORE", "DELETE_BACKUP"])
const outputLifecycle = new Set(["CREATE", "REMOVE", "TOGGLE_OUTPUTS", "UPDATE_BOUNDS", "SET_VALUE", "FOCUS", "TO_FRONT", "MOVE"])

export function createRustApi(transport: Transport, reportError: (error: unknown, request: unknown) => void, initialContext?: RuntimeContext) {
    const receivers = new Map<string, Set<Receiver>>()
    const named = new Map<string, { channel: string; receiver: Receiver }>()
    let context = initialContext ? { ...initialContext } : undefined
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
        api: {
            send(channel: string, data?: any, listenerId?: string) {
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
        context
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

// Preserve the existing preload API while the runtime moves to Rust.
// This module has no dependency on Electron or on the Svelte app.
type Receiver = (data: any, listenerId?: string) => void
export type RuntimeContext = { runtimeId: string; libraryGeneration: number; dataPath: string }
export type RuntimeEvent = { channel: string; message: any; listenerId?: string; context?: RuntimeContext }
export type Transport = (request: { channel: string; data?: any; listenerId?: string; context?: RuntimeContext }) => Promise<RuntimeEvent[]>
const mutations = new Set(["SAVE", "UPDATE_DATA_PATH", "SET_STORE_VALUE", "RESTORE", "DELETE_BACKUP"])

export function createRustApi(transport: Transport, reportError: (error: unknown, request: unknown) => void, initialContext?: RuntimeContext) {
    const receivers = new Map<string, Set<Receiver>>()
    const named = new Map<string, { channel: string; receiver: Receiver }>()
    let context = initialContext ? { ...initialContext } : undefined
    let mutationQueue = Promise.resolve()

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
                // SAVE snapshots its context before its deferred send. Other
                // requests belong to the library visible when send is called.
                const captured = channel === "MAIN" && data?.channel === "SAVE" ? data.data?.runtimeContext || context : context
                const request = { channel, data, listenerId, ...(captured ? { context: { ...captured } } : {}) }
                // Native replies are asynchronous, like Electron's IPC. This
                // also lets requestMain register its receiver after send().
                const serialized = channel === "MAIN" && mutations.has(data?.channel)
                const complete = (serialized ? mutationQueue : Promise.resolve())
                    .then(() => transport(request))
                    .then((events) => events.forEach((event) => dispatch(event, request)))
                    .catch((error) => reportError(error, request))
                if (serialized) mutationQueue = complete.catch(() => {})
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
                // Browser File deliberately does not expose native paths. Native
                // drag/drop integration is a separate pending parity item.
                const nativePath = (file as File & { path?: string }).path
                if (nativePath) return nativePath
                reportError({ code: "unsupported_command", message: "Native file drop is not yet ported" }, { channel: "showFilePath" })
                return ""
            }
        }
    }
}

export async function installRustBridge(target: Window = window) {
    const tauri = (target as any).__TAURI__
    if (!tauri?.core?.invoke || !tauri?.event?.listen) throw new Error("The Rust desktop runtime is unavailable")
    const context = await tauri.core.invoke("runtime_context")
    if (!context?.runtimeId || !Number.isSafeInteger(context.libraryGeneration) || typeof context.dataPath !== "string") throw new Error("The Rust runtime context is unavailable")
    const bridge = createRustApi(
        (request) => tauri.core.invoke("ipc_dispatch", { request }),
        (error, request) => {
            console.error("FreeShow runtime request failed", { request, error })
            target.dispatchEvent(new CustomEvent("freeshow:runtime-error", { detail: { error, request } }))
        },
        context
    )
    await tauri.event.listen("freeshow://ipc", (event: { payload: RuntimeEvent }) => bridge.dispatch(event.payload))
    ;(target as any).api = bridge.api
    return bridge
}

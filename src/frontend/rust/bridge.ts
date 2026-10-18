// Preserve the existing preload API while the runtime moves to Rust.
// This module has no dependency on Electron or on the Svelte app.
type Receiver = (data: any, listenerId?: string) => void
export type RuntimeEvent = { channel: string; message: any; listenerId?: string }
export type Transport = (request: { channel: string; data?: any; listenerId?: string }) => Promise<RuntimeEvent[]>

export function createRustApi(transport: Transport, reportError: (error: unknown, request: unknown) => void) {
    const receivers = new Map<string, Set<Receiver>>()
    const named = new Map<string, { channel: string; receiver: Receiver }>()

    function dispatch(event: RuntimeEvent) {
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
                const request = { channel, data, listenerId }
                // Native replies are asynchronous, like Electron's IPC. This
                // also lets requestMain register its receiver after send().
                Promise.resolve()
                    .then(() => transport(request))
                    .then((events) => events.forEach(dispatch))
                    .catch((error) => reportError(error, request))
            },
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
    const bridge = createRustApi(
        (request) => tauri.core.invoke("ipc_dispatch", { request }),
        (error, request) => {
            console.error("FreeShow runtime request failed", { request, error })
            target.dispatchEvent(new CustomEvent("freeshow:runtime-error", { detail: { error, request } }))
        }
    )
    await tauri.event.listen("freeshow://ipc", (event: { payload: RuntimeEvent }) => bridge.dispatch(event.payload))
    ;(target as any).api = bridge.api
    return bridge
}

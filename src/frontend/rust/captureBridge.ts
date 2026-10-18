import type { AssetContext } from "./mediaUrl"

export type CaptureTransport = (completion: { id: string; buffer: ArrayBuffer; failed: boolean }) => Promise<void>
const same = (a: AssetContext | undefined, b: AssetContext | undefined) => !!a && !!b && a.runtimeId === b.runtimeId && a.libraryGeneration === b.libraryGeneration && a.dataPath === b.dataPath
const nativeId = (id: unknown): id is string => typeof id === "string" && /^native-thumbnail-[0-9a-f]{32}$/.test(id)

// Private capture completions use raw IPC bytes. They never authorize an output
// path or turn arbitrary SAVE_IMAGE requests into filesystem writes.
export function createCaptureBridge(getContext: () => AssetContext | undefined, transport: CaptureTransport | undefined, report: (error: unknown) => void) {
    const pending = new Map<string, { context: AssetContext; output: string; expires: number }>()
    function prune() {
        for (const [id, value] of pending) if (!same(value.context, getContext()) || value.expires <= Date.now()) pending.delete(id)
    }
    function fail(message: string) {
        report({ code: "thumbnail_capture", message })
        return false
    }
    return {
        register(context: AssetContext, id: unknown, output: unknown) {
            prune()
            if (!transport || !same(context, getContext()) || !nativeId(id) || typeof output !== "string" || !output || output.length > 32768 || output.includes("\0") || pending.has(id) || pending.size >= 4) return fail("Invalid or excessive native canvas capture")
            pending.set(id, { context: { ...context }, output, expires: Date.now() + 15000 })
            return true
        },
        complete(data: any) {
            prune()
            if (!nativeId(data?.id)) return false
            const captured = pending.get(data.id)
            pending.delete(data.id)
            if (!captured || !same(captured.context, getContext()) || !transport) {
                fail("Canvas completion has no current native request")
                return true
            }
            const failed = data.buffer === undefined
            if (!failed && (!(data.buffer instanceof ArrayBuffer) || !data.buffer.byteLength || data.buffer.byteLength > 8 * 1024 * 1024 || data.path !== captured.output)) {
                fail("Canvas completion exceeds its byte limit or mismatches its private output")
                return true
            }
            // Match Electron's send-time byte snapshot without JSON expansion.
            const id = data.id
            const buffer = failed ? new ArrayBuffer(0) : data.buffer.slice(0)
            Promise.resolve().then(() => transport({ id, buffer, failed })).catch(report)
            return true
        }
    }
}

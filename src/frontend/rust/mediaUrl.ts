// Synchronous URL lookup for retained Svelte media helpers. File authorization
// is performed by trusted native host flows before these grants are published.
export type AssetContext = { runtimeId: string; libraryGeneration: number; dataPath: string }
export type LocalAssetGrant = { id: string; sourcePath: string; url: string; mimeType: string; length: number; context: AssetContext }
export type AssetUrlDiagnostic = { code: string; message: string; sourcePath?: string }
type Stored = { id: string; url: string; bytes: number }
const sameContext = (a: AssetContext | undefined, b: AssetContext | undefined) => !!a && !!b && a.runtimeId === b.runtimeId && a.libraryGeneration === b.libraryGeneration && a.dataPath === b.dataPath
const validContext = (context: AssetContext | undefined): context is AssetContext => !!context && typeof context.runtimeId === "string" && !!context.runtimeId && context.runtimeId.length <= 128 && !context.runtimeId.includes("\0") && Number.isSafeInteger(context.libraryGeneration) && context.libraryGeneration >= 0 && typeof context.dataPath === "string" && context.dataPath.length <= 32768 && !context.dataPath.includes("\0")

export function createMediaUrlRegistry(getContext: () => AssetContext | undefined, reportError: (error: AssetUrlDiagnostic) => void, maxEntries = 1024, maxBytes = 8 * 1024 * 1024) {
    if (!Number.isInteger(maxEntries) || maxEntries < 1 || maxEntries > 16384 || !Number.isInteger(maxBytes) || maxBytes < 1 || maxBytes > 64 * 1024 * 1024) throw new Error("Invalid media URL registry bounds")
    let entries = new Map<string, Stored>()
    let context: AssetContext | undefined
    let bytes = 0
    const reported = new Set<string>()
    const encoder = new TextEncoder()
    function syncContext() {
        const current = getContext()
        if (!sameContext(context, current)) {
            entries.clear()
            reported.clear()
            bytes = 0
            context = validContext(current) ? { ...current } : undefined
        }
        return context
    }
    function diagnostic(code: string, message: string, sourcePath?: string) {
        sourcePath = sourcePath?.slice(0, 512)
        const key = code + ":" + (sourcePath || "")
        if (reported.has(key)) return
        if (reported.size >= 256) reported.clear()
        reported.add(key)
        reportError({ code, message, sourcePath })
    }
    function publish(captured: AssetContext, grants: LocalAssetGrant[]): boolean {
        if (!sameContext(syncContext(), captured)) return false
        if (!Array.isArray(grants) || grants.length > maxEntries) {
            diagnostic("assets_limit", "Media URL publication exceeds its grant limit")
            return false
        }
        const updated = new Map(entries)
        let updatedBytes = bytes
        for (const grant of grants) {
            if (!grant || !sameContext(grant.context, captured) || typeof grant.sourcePath !== "string" || !grant.sourcePath || grant.sourcePath.length > 32768 || grant.sourcePath.includes("\0") || typeof grant.id !== "string" || !/^[0-9a-f]{32}$/.test(grant.id) || typeof grant.url !== "string" || !/^http:\/\/127\.0\.0\.1:[0-9]{1,5}\/[0-9a-f]{64}\/[0-9a-f]{32}$/.test(grant.url) || !grant.url.endsWith("/" + grant.id) || !Number.isSafeInteger(grant.length) || grant.length < 0) {
                diagnostic("assets_invalid", "Invalid host-issued media URL grant")
                return false
            }
            let url: URL
            try {
                url = new URL(grant.url)
            } catch {
                diagnostic("assets_invalid", "Invalid loopback media URL authority")
                return false
            }
            const sourceBytes = encoder.encode(grant.sourcePath).length
            if (!url.port || Number(url.port) < 1 || Number(url.port) > 65535 || sourceBytes > 32768) {
                diagnostic("assets_invalid", "Invalid loopback media URL authority")
                return false
            }
            const previous = updated.get(grant.sourcePath)
            if (previous) updatedBytes -= previous.bytes
            const size = sourceBytes + grant.url.length + grant.id.length
            updatedBytes += size
            updated.set(grant.sourcePath, { id: grant.id, url: grant.url, bytes: size })
            if (updated.size > maxEntries || updatedBytes > maxBytes) {
                diagnostic("assets_limit", "Media URL registry budget exhausted")
                return false
            }
        }
        entries = updated
        bytes = updatedBytes
        return true
    }
    function resolve(path: unknown): string | undefined {
        if (typeof path !== "string" || !path) return ""
        if (path.startsWith("http://") || path.startsWith("https://") || path.startsWith("data:") || path.startsWith("blob:")) return path
        if (path.startsWith("freeshow-protected://")) {
            diagnostic("assets_protected", "Protected media decryption and license serving are not implemented", path)
            return path
        }
        if (path.length > 32768) {
            diagnostic("assets_limit", "Local media source key exceeds its byte limit", path)
            return ""
        }
        const current = syncContext()
        const granted = entries.get(path)
        if (granted) return granted.url
        // Bundled relative resources continue through the original encoder.
        // Native absolute/file URLs require an exact trusted source key.
        if (!path.startsWith("/") && !path.startsWith("\\\\") && !path.startsWith("file://") && !/^[a-zA-Z]:[\\/]/.test(path)) return undefined
        diagnostic(current ? "assets_unregistered" : "assets_context", current ? "This local media path has no host-issued byte grant" : "The native media context is unavailable", path)
        return ""
    }
    return {
        publish,
        resolve,
        revoke(id: string) {
            for (const [path, entry] of entries)
                if (entry.id === id) {
                    bytes -= entry.bytes
                    entries.delete(path)
                }
        },
        clear() {
            entries.clear()
            reported.clear()
            bytes = 0
            context = undefined
        },
        status() {
            syncContext()
            return { entries: entries.size, bytes }
        }
    }
}

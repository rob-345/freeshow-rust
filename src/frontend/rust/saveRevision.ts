// Each WebView owns its revision. Runtime/library provenance is checked by the
// bridge; this token additionally identifies the latest local save snapshot.
let editRevision = 0n
let saveSequence = 0n
let saveEpoch: string | undefined
let suppressed = 0
let latest: { token: string; revision: bigint } | undefined

export function recordEdit() {
    if (!suppressed) editRevision++
}

export function isSaveMutationSuppressed() {
    return suppressed > 0
}

export function withoutSaveEdits<T>(work: () => T): T {
    suppressed++
    try {
        return work()
    } finally {
        suppressed--
    }
}

export function beginSave() {
    // Initialize lazily: Electron and HTTP clients can import this shared module
    // without using Rust's token protocol. A new module/window gets a new epoch.
    saveEpoch ||= crypto.randomUUID()
    const token = `${saveEpoch}:${++saveSequence}`
    latest = { token, revision: editRevision }
    return token
}

export function acknowledgeSave(token: unknown): "current" | "edited" | "superseded" {
    if (!latest || token !== latest.token) return "superseded"
    return latest.revision === editRevision ? "current" : "edited"
}

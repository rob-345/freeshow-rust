import type { RuntimeContext } from "./bridge"

export type CoverPublication = {
    event: { channel: "MAIN"; message: { channel: "ALERT"; data: string }; context: RuntimeContext; listenerId?: undefined }
    id: string
    sequence: string
    folder: string
}
export type CoverBridgeOptions = {
    getContext: () => RuntimeContext | undefined
    deliver: (message: { channel: "ALERT"; data: string }) => void
    report: (error: unknown) => void
    complete: (context: RuntimeContext, id: string, accepted: boolean) => Promise<unknown>
    timeoutMs?: number
}
export class CoverBridgeError extends Error {
    constructor(
        public readonly code: string,
        message: string
    ) {
        super(message)
        this.name = "CoverBridgeError"
    }
}
const PREFIX = "Error: Could not create folder at: "
const U64_MAX = 18446744073709551615n
const same = (a: RuntimeContext | undefined, b: RuntimeContext | undefined) => !!a && !!b && a.runtimeId === b.runtimeId && a.libraryGeneration === b.libraryGeneration && a.dataPath === b.dataPath
function fail(code: string, message: string): never {
    throw new CoverBridgeError(code, message)
}
function field(value: unknown, key: string): unknown {
    if (!value || typeof value !== "object") return undefined
    const descriptor = Object.getOwnPropertyDescriptor(value, key)
    if (descriptor && !("value" in descriptor)) fail("cover_publication", "Native data properties are required")
    return descriptor?.value
}
function shape(value: unknown, required: string[], optional: string[] = []): void {
    if (!value || typeof value !== "object" || ![Object.prototype, null].includes(Object.getPrototypeOf(value)) || Object.getOwnPropertySymbols(value).length) fail("cover_publication", "Plain native objects are required")
    const names = Object.getOwnPropertyNames(value)
    if (names.length > required.length + optional.length || required.some((key) => !Object.hasOwn(value, key))) fail("cover_publication", "Unexpected native object fields")
    for (const name of names) {
        const descriptor = Object.getOwnPropertyDescriptor(value, name)!
        if ((!required.includes(name) && !optional.includes(name)) || !("value" in descriptor) || !descriptor.enumerable) fail("cover_publication", "Unexpected native data property")
    }
}
function utf8(value: unknown, maximum: number): value is string {
    if (typeof value !== "string" || value.length > maximum) return false
    let bytes = 0
    for (let index = 0; index < value.length; index++) {
        const code = value.charCodeAt(index)
        if (code === 0) return false
        if (code < 128) bytes++
        else if (code < 2048) bytes += 2
        else if (code >= 0xd800 && code <= 0xdbff) {
            const low = value.charCodeAt(++index)
            if (!(low >= 0xdc00 && low <= 0xdfff)) return false
            bytes += 4
        } else if (code >= 0xdc00 && code <= 0xdfff) return false
        else bytes += 3
        if (bytes > maximum) return false
    }
    return true
}
function context(value: unknown): RuntimeContext {
    shape(value, ["runtimeId", "libraryGeneration", "dataPath"])
    const runtimeId = field(value, "runtimeId"),
        libraryGeneration = field(value, "libraryGeneration"),
        dataPath = field(value, "dataPath")
    if (!utf8(runtimeId, 256) || !runtimeId || !Number.isSafeInteger(libraryGeneration) || (libraryGeneration as number) < 0 || !utf8(dataPath, 32768) || !dataPath) fail("cover_context", "A bounded complete runtime context is required")
    return Object.freeze({ runtimeId, libraryGeneration: libraryGeneration as number, dataPath })
}
function serial(value: unknown): bigint {
    if (typeof value !== "string" || value.length > 20 || !/^[1-9][0-9]{0,19}$/.test(value)) fail("cover_publication", "A positive decimal u64 sequence is required")
    const next = BigInt(value)
    if (next > U64_MAX) fail("cover_publication", "Native publication sequence exceeds u64")
    return next
}

type Completion = { context: RuntimeContext; id: string; accepted: boolean; entered: boolean; work: Promise<void> }
type Wait = { context?: RuntimeContext; work: Promise<void> }
// WebView-module lifetime, including accidental helper replacement. Native
// process ownership, nonce/sequence authority and producer retirement are separate.
const pending = new Set<Completion>()
let sequence = 0n,
    terminal = false
let seal: RuntimeContext | undefined, scopedWait: Wait | undefined, terminalWait: Wait | undefined

export function createCoverBridge(options: CoverBridgeOptions) {
    const timeoutMs = options.timeoutMs ?? 20000
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 60000) fail("cover_timeout", "Observer timeout must be an integer in 1..60000ms")
    function report(error: unknown): void {
        try {
            options.report(error)
        } catch {
            // A diagnostic callback cannot discard an admitted completion.
        }
    }
    function current(): RuntimeContext | undefined {
        const value = options.getContext()
        if (value === undefined) return undefined
        const captured = context(value)
        if (seal && !same(seal, captured)) seal = undefined
        return captured
    }
    function reserve(captured: RuntimeContext, id: string): Completion | undefined {
        if (pending.size === 4) {
            report(new CoverBridgeError("cover_completion_busy", "Four native cover completion promises remain owned"))
            return undefined
        }
        const record: Completion = { context: captured, id, accepted: false, entered: false, work: Promise.resolve() }
        record.work = Promise.resolve()
            .then(() => {
                record.entered = true
                return options.complete(captured, id, record.accepted)
            })
            .then(
                () => {},
                (error) => report(error)
            )
            .finally(() => pending.delete(record))
        // Before validation reports or original callbacks, including reentrancy.
        pending.add(record)
        return record
    }
    function observe(captured: RuntimeContext | undefined): Promise<void> {
        const existing = captured ? scopedWait : terminalWait
        if (existing) return same(existing.context, captured) || (!existing.context && !captured) ? existing.work : Promise.reject(new CoverBridgeError("cover_wait_busy", "Another scoped cover observation remains pending"))
        const records = [...pending].filter((record) => !captured || same(record.context, captured))
        if (!records.length) return Promise.resolve()
        let timer: ReturnType<typeof setTimeout> | undefined
        const record: Wait = { context: captured, work: Promise.resolve() }
        const limit = new Promise<never>((_, reject) => {
            timer = setTimeout(() => reject(new CoverBridgeError("cover_retirement_timeout", "Native completion promises remain owned for retry")), timeoutMs)
        })
        record.work = Promise.race([Promise.all(records.map((entry) => entry.work)), limit])
            .then(() => {})
            .finally(() => {
                clearTimeout(timer)
                if (captured && scopedWait === record) scopedWait = undefined
                if (!captured && terminalWait === record) terminalWait = undefined
            })
        if (captured) scopedWait = record
        else terminalWait = record
        // A discarded observer must not create an unhandled rejection. The
        // returned promise still rejects for callers that observe the timeout.
        record.work.catch(() => {})
        return record.work
    }
    return {
        publish(publication: unknown): void {
            if (terminal) {
                report(new CoverBridgeError("cover_closed", "Cover publication admission is terminally closed"))
                return
            }
            let captured: RuntimeContext, id: string
            try {
                const active = current(),
                    event = field(publication, "event"),
                    issued = context(field(event, "context")),
                    nonce = field(publication, "id")
                if (!active || !same(active, issued)) fail("cover_context", "Foreign native cover context cannot be adopted")
                if (same(seal, active)) fail("cover_closed", "Current cover context admission is sealed")
                if (typeof nonce !== "string" || nonce.length !== 45 || !/^native-cover-[0-9a-f]{32}$/.test(nonce)) fail("cover_publication", "Invalid native cover publication id")
                captured = active
                id = nonce
            } catch (error) {
                report(error)
                return
            }
            if ([...pending].some((record) => record.id === id)) {
                report(new CoverBridgeError("cover_replay", "Cover id already has an owned completion"))
                return
            }
            let next: bigint
            try {
                next = serial(field(publication, "sequence"))
            } catch (error) {
                if (reserve(captured, id)) report(error)
                return
            }
            if (next <= sequence) {
                report(new CoverBridgeError("cover_replay", "Cover sequence was already consumed or reordered"))
                return
            }
            const completion = reserve(captured, id)
            if (!completion) return
            try {
                const event = field(publication, "event"),
                    message = field(event, "message"),
                    folder = field(publication, "folder"),
                    data = field(message, "data")
                shape(publication, ["event", "id", "sequence", "folder"])
                shape(event, ["channel", "message", "context"], ["listenerId"])
                shape(message, ["channel", "data"])
                if (field(event, "channel") !== "MAIN" || field(event, "listenerId") !== undefined || field(message, "channel") !== "ALERT") fail("cover_publication", "Only the original MAIN ALERT is supported")
                if (!utf8(folder, 32774) || !(folder.endsWith("/Audio") || folder.endsWith("\\Audio")) || !utf8(data, 32814) || data !== `${PREFIX}${folder}!`) fail("cover_publication", "Invalid native fixed-folder failure receipt")
                // Commit after full validation, before entering a potentially
                // reentrant source callback. A throwing callback may have effects.
                sequence = next
                options.deliver({ channel: "ALERT", data })
                completion.accepted = true
            } catch (error) {
                report(error)
            }
        },
        retire(previous: RuntimeContext): Promise<void> {
            try {
                const captured = context(previous)
                if (same(current(), captured)) seal = captured
                return observe(captured)
            } catch (error) {
                return Promise.reject(error)
            }
        },
        close(): Promise<void> {
            terminal = true
            return observe(undefined)
        },
        status() {
            return { completionSlots: pending.size, enteredCompletions: [...pending].filter((record) => record.entered).length, sequence: sequence.toString(), closed: terminal, sealedCurrent: !!seal, scopedWait: !!scopedWait, terminalWait: !!terminalWait }
        }
    }
}

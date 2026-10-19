import type { RuntimeContext } from "./bridge"

export type TimecodeMessage = { channel: "TIMECODE_START" | "TIMECODE_STOP" | "TIMECODE_VALUE" | "TIMECODE_STATUS"; data?: unknown }
export type TimecodePublication = {
    event: { channel: "MAIN"; message: { channel: string; data: unknown }; context: RuntimeContext; listenerId?: never }
    hydration: { undefinedPaths: string[]; numbers: { path: string; number: { kind: string } }[] }
    bytes?: Uint8Array | number[]
    delivery: { id: string; context: RuntimeContext; run: number; sequence: string }
}
export type TimecodeBridgeOptions = {
    getContext: () => RuntimeContext | undefined
    // These are private native callbacks, not JSON stringify boundaries. The
    // coordinator must encode own undefined and IEEE numbers without loss.
    command: (message: TimecodeMessage, context: RuntimeContext) => Promise<unknown>
    audio: (bytes: Uint8Array, mode: "LTC", context: RuntimeContext, announcedLength: number) => Promise<unknown>
    complete: (context: RuntimeContext, id: string, accepted: boolean) => Promise<unknown>
    retire: (oldContext: RuntimeContext, terminal: boolean) => Promise<unknown>
    deliver: (message: { channel: string; data: unknown }) => void
    report: (error: TimecodeBridgeError) => void
    drainTimeoutMs?: number
}
export class TimecodeBridgeError extends Error {
    constructor(
        public readonly code: string,
        message: string
    ) {
        super(message.slice(0, 2048))
        this.name = "TimecodeBridgeError"
    }
}
const encoder = new TextEncoder()
const typedPrototype = Object.getPrototypeOf(Uint8Array.prototype)
const typedLength = Object.getOwnPropertyDescriptor(typedPrototype, "length")!.get!
const typedBuffer = Object.getOwnPropertyDescriptor(typedPrototype, "buffer")!.get!
const arrayBufferLength = Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, "byteLength")!.get!
const viewBuffer = Object.getOwnPropertyDescriptor(DataView.prototype, "buffer")!.get!
const commands = new Set(["TIMECODE_START", "TIMECODE_STOP", "TIMECODE_VALUE", "TIMECODE_STATUS", "TIMECODE_AUDIO_DATA"])
const same = (a: RuntimeContext | undefined, b: RuntimeContext | undefined) => !!a && !!b && a.runtimeId === b.runtimeId && a.libraryGeneration === b.libraryGeneration && a.dataPath === b.dataPath
const own = (value: object, key: string) => Object.prototype.hasOwnProperty.call(value, key)
function fail(code: string, message: string): never {
    throw new TimecodeBridgeError(code, message)
}
function field(value: unknown, key: string): unknown {
    if (!value || typeof value !== "object") return undefined
    const descriptor = Object.getOwnPropertyDescriptor(value, key)
    if (descriptor && !("value" in descriptor)) fail("timecode_shape", "Accessors are unsupported")
    return descriptor?.value
}
function plain(value: unknown): value is Record<string, unknown> {
    return !!value && typeof value === "object" && [Object.prototype, null].includes(Object.getPrototypeOf(value))
}
function context(value: RuntimeContext | undefined): RuntimeContext {
    const runtimeId = field(value, "runtimeId"),
        libraryGeneration = field(value, "libraryGeneration"),
        dataPath = field(value, "dataPath")
    if (!plain(value) || typeof runtimeId !== "string" || !runtimeId || runtimeId.includes("\0") || runtimeId.length > 256 || encoder.encode(runtimeId).length > 256 || !Number.isSafeInteger(libraryGeneration) || (libraryGeneration as number) < 0 || typeof dataPath !== "string" || !dataPath || dataPath.includes("\0") || dataPath.length > 32768 || encoder.encode(dataPath).length > 32768) fail("timecode_context", "A bounded full runtime context is required")
    return Object.freeze({ runtimeId, libraryGeneration: libraryGeneration as number, dataPath })
}
// Electron clones at send time. Keep property presence, insertion order and raw
// IEEE values in this callback snapshot; no user coercions/accessors are invoked.
function snapshot(input: unknown): unknown {
    let nodes = 1024,
        bytes = 65536
    const active = new Set<object>()
    const charge = (text: string) => {
        if (text.length > bytes) fail("timecode_limit", "Source metadata byte bound exceeded")
        bytes -= encoder.encode(text).length
        if (bytes < 0) fail("timecode_limit", "Source metadata byte bound exceeded")
    }
    const visit = (value: unknown, depth: number): unknown => {
        if (--nodes < 0 || depth > 8) fail("timecode_limit", "Source metadata work/depth bound exceeded")
        if (value === null || value === undefined || typeof value === "boolean" || typeof value === "number") {
            bytes -= 8
            if (bytes < 0) fail("timecode_limit", "Source metadata byte bound exceeded")
            return value
        }
        if (typeof value === "string") {
            charge(value)
            return value
        }
        if ((!plain(value) && !Array.isArray(value)) || active.has(value)) fail("timecode_shape", "Only acyclic plain source data is supported")
        active.add(value)
        const output: Record<string, unknown> | unknown[] = Array.isArray(value) ? [] : {}
        if (Array.isArray(value) && value.length > nodes) fail("timecode_limit", "Source array bound exceeded")
        let count = 0
        for (const key in value) {
            if (!own(value, key)) continue
            if (++count > 1024) fail("timecode_limit", "Source property bound exceeded")
            charge(key)
            const descriptor = Object.getOwnPropertyDescriptor(value, key)!
            if (!("value" in descriptor)) fail("timecode_shape", "Source accessors are unsupported")
            Object.defineProperty(output, key, { value: visit(descriptor.value, depth + 1), enumerable: true, writable: true, configurable: true })
        }
        if (Array.isArray(value)) (output as unknown[]).length = value.length
        active.delete(value)
        return Object.freeze(output)
    }
    return visit(input, 0)
}
function sourceBytes(value: unknown): Uint8Array {
    // Buffer.from(typedArray) copies numeric elements, not its backing raw bytes.
    // DataView has no array length and therefore becomes an empty source Buffer.
    if (value instanceof ArrayBuffer) {
        if (arrayBufferLength.call(value) > 65536) fail("timecode_limit", "Source audio exceeds 65536 bytes")
        return new Uint8Array(value).slice()
    }
    if (ArrayBuffer.isView(value)) {
        const backing = (value instanceof DataView ? viewBuffer : typedBuffer).call(value)
        if (!(backing instanceof ArrayBuffer)) fail("timecode_shape", "Shared audio storage is unsupported")
        if (value instanceof DataView) return new Uint8Array()
        if (value instanceof BigInt64Array || value instanceof BigUint64Array) fail("timecode_shape", "BigInt source buffers are unsupported")
        const view = value as unknown as ArrayLike<number>
        const length = typedLength.call(value)
        if (length > 65536) fail("timecode_limit", "Source audio exceeds 65536 elements")
        const output = new Uint8Array(length)
        for (let index = 0; index < output.length; index++) output[index] = view[index]
        return output
    }
    if (plain(value) && field(value, "type") === "Buffer") value = field(value, "data")
    if (!Array.isArray(value) || value.length > 65536) fail("timecode_shape", "Source LTC requires bounded Buffer.from-compatible bytes")
    const output = new Uint8Array(value.length)
    for (let index = 0; index < value.length; index++) {
        const item = field(value, String(index))
        if (item !== null && item !== undefined && !["number", "boolean", "string"].includes(typeof item)) fail("timecode_shape", "Custom source byte coercion is unsupported")
        if (typeof item === "string" && item.length > 64) fail("timecode_limit", "Source byte scalar exceeds bound")
        output[index] = Number(item)
    }
    return output
}
type Job = { context: RuntimeContext } & ({ kind: "command"; message: TimecodeMessage } | { kind: "audio"; bytes: Uint8Array })

/** Main-window adapter only. Native authority, codecs, IO and source global
 * state remain in NativeTimecode. All callbacks must be trusted bounded sinks. */
export function createTimecodeBridge(options: TimecodeBridgeOptions) {
    const timeout = options.drainTimeoutMs ?? 5000
    if (!Number.isFinite(timeout) || timeout <= 0 || timeout > 5000) fail("timecode_config", "Drain timeout must be in (0,5000] ms")
    const initial = options.getContext()
    let current = initial ? context(initial) : undefined
    let queue: Job[] = [],
        accepted = 0,
        active = false,
        pump: Promise<void> | undefined
    let completion: Promise<void> | undefined,
        publishing = false,
        lastSequence = 0n
    let terminal = false,
        retiring = false,
        cleanupRunning = false,
        cleanup: Promise<void> | undefined,
        cleanupError: TimecodeBridgeError | undefined,
        terminalError: TimecodeBridgeError | undefined,
        waiter = false
    const report = (error: unknown, code = "timecode_transport") => {
        const controlled = error instanceof TimecodeBridgeError ? error : new TimecodeBridgeError(code, error instanceof Error ? error.message : "Native timecode operation failed")
        try {
            options.report(controlled)
        } catch {
            /* Diagnostics cannot abandon real accepted effects. */
        }
        return controlled
    }
    function available() {
        if (terminal || retiring) fail("timecode_closed", "Timecode is closed or still retiring")
        const frozen = context(options.getContext())
        if (!same(current, frozen)) fail("timecode_stale_context", "Retire the old full context before replacement")
        return frozen
    }
    function enqueue(job: Job) {
        accepted++
        queue.push(job)
        if (!pump)
            pump = Promise.resolve().then(async () => {
                while (queue.length) {
                    const job = queue.shift()!
                    active = true
                    try {
                        if (!same(current, job.context) || !same(options.getContext(), job.context)) fail("timecode_stale_context", "Queued timecode belongs to an obsolete context")
                        if (job.kind === "audio") await options.audio(job.bytes, "LTC", job.context, job.bytes.byteLength)
                        else await options.command(job.message, job.context)
                    } catch (error) {
                        report(error)
                    } finally {
                        active = false
                        accepted--
                    }
                }
                pump = undefined
            })
    }
    function beginRetirement(final: boolean) {
        terminal ||= final
        if (cleanupRunning || (terminal && retiring && cleanup && !cleanupError)) return
        retiring = true
        for (const _job of queue) {
            accepted--
            report(new TimecodeBridgeError("timecode_context_closed", "Queued timecode canceled by retirement"))
        }
        queue = []
        const old = current
        cleanupRunning = true
        cleanupError = undefined
        cleanup = Promise.resolve()
            .then(async () => {
                let failed = false
                const final = terminal
                try {
                    if (old) await options.retire(old, final)
                } catch (error) {
                    cleanupError = report(error, "timecode_cleanup")
                    failed = true
                }
                await pump
                await completion
                // A concurrent close upgrades an already-entered context drain
                // to actual terminal host shutdown before this cleanup settles.
                if (terminal && !final) {
                    try {
                        if (old) await options.retire(old, true)
                    } catch (error) {
                        cleanupError = report(error, "timecode_cleanup")
                        failed = true
                    }
                }
                if (failed) return
                if (!terminal) current = context(options.getContext())
                retiring = terminal
            })
            .catch((error) => {
                cleanupError = report(error, "timecode_cleanup")
            })
            .finally(() => {
                cleanupRunning = false
            })
    }
    async function waitCleanup() {
        if (waiter) fail("timecode_control_busy", "A drain waiter already exists")
        waiter = true
        let timer: ReturnType<typeof setTimeout> | undefined
        try {
            await Promise.race([
                cleanup,
                new Promise<never>((_, reject) => {
                    timer = setTimeout(() => reject(new TimecodeBridgeError("timecode_drain_timeout", "Real timecode callbacks remain owned; retry drain")), timeout)
                })
            ])
            if (cleanupError) throw cleanupError
            if (terminalError) throw terminalError
        } finally {
            clearTimeout(timer)
            waiter = false
        }
    }
    return {
        // Called only for a retained MAIN message. true means intercepted,
        // including an original source no-op/error; it is never a success ACK.
        handle(message: unknown): boolean {
            let channel: unknown
            try {
                channel = field(message, "channel")
            } catch (error) {
                report(error)
                return false
            }
            if (!commands.has(channel as string)) return false
            try {
                const frozen = available()
                if (accepted >= 7) fail("timecode_busy", "Timecode packet/control queue is full")
                if (!plain(message)) fail("timecode_shape", "MAIN timecode message must be plain")
                const data = field(message, "data")
                if (channel === "TIMECODE_AUDIO_DATA") {
                    if (data === null || data === undefined) fail("timecode_shape", "Original audio handler requires a non-null payload")
                    // The actual retained worklet posts raw Uint8Array, so mode
                    // is absent and original processAudioData performs no effect.
                    if (field(data, "mode") !== "LTC") return true
                    enqueue({ kind: "audio", bytes: sourceBytes(field(data, "buffer")), context: frozen })
                } else {
                    const copied = snapshot(message) as TimecodeMessage
                    if (channel === "TIMECODE_START" && (!plain(copied.data) || typeof field(copied.data, "type") !== "string" || typeof field(copied.data, "mode") !== "string")) fail("timecode_shape", "START requires original type/mode strings")
                    if (channel === "TIMECODE_VALUE" && typeof copied.data !== "number") fail("timecode_shape", "VALUE requires a source Number")
                    enqueue({ kind: "command", message: copied, context: frozen })
                }
            } catch (error) {
                report(error)
            }
            return true
        },
        publish(publication: TimecodePublication): void {
            // Native retirement revokes old private nonces. Do not accept new
            // callback work after cleanup has started or terminal drain finished.
            if (retiring || terminal) {
                report(new TimecodeBridgeError("timecode_closed", "Timecode publication arrived during retirement"))
                return
            }
            let frozen: RuntimeContext, id: string, sequence: bigint
            try {
                const delivery = field(publication, "delivery")
                id = field(delivery, "id") as string
                const serial = field(delivery, "sequence"),
                    run = field(delivery, "run")
                if (typeof id !== "string" || id.length !== 45 || !/^timecode-[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(id) || typeof serial !== "string" || serial.length > 20 || !/^[1-9][0-9]{0,19}$/.test(serial) || BigInt(serial) > 18446744073709551615n || !Number.isSafeInteger(run) || (run as number) <= 0) fail("timecode_publication", "Invalid private timecode delivery")
                sequence = BigInt(serial)
                if (sequence <= lastSequence) fail("timecode_replay", "Timecode publication was already consumed or reordered")
                frozen = context(field(delivery, "context") as RuntimeContext)
                if (completion || publishing) fail("timecode_completion_busy", "A private timecode completion is still owned")
                lastSequence = sequence
                publishing = true
            } catch (error) {
                report(error)
                return
            }
            let delivered = false
            try {
                const event = field(publication, "event"),
                    message = field(event, "message"),
                    hydration = field(publication, "hydration")
                if (!same(frozen, context(field(event, "context") as RuntimeContext)) || field(event, "channel") !== "MAIN" || field(event, "listenerId") !== undefined) fail("timecode_publication", "Foreign native timecode event")
                const undefinedPaths = field(hydration, "undefinedPaths"),
                    numbers = field(hydration, "numbers"),
                    channel = field(message, "channel")
                let data = field(message, "data")
                if (!Array.isArray(undefinedPaths) || undefinedPaths.length || !Array.isArray(numbers) || numbers.length > 1) fail("timecode_publication", "Unsupported native timecode hydration")
                if (channel === "TIMECODE_VALUE") {
                    if (numbers.length) {
                        const entry = numbers[0],
                            kind = field(field(entry, "number"), "kind")
                        if (field(entry, "path") !== "" || data !== null || !["NaN", "PositiveInfinity", "NegativeInfinity", "NegativeZero"].includes(kind as string)) fail("timecode_publication", "Invalid whole-value Number restoration")
                        data = ({ NaN, PositiveInfinity: Infinity, NegativeInfinity: -Infinity, NegativeZero: -0 } as Record<string, number>)[kind as string]
                    } else if (typeof data !== "number" || !Number.isFinite(data)) fail("timecode_publication", "Invalid native timecode Number")
                    if (field(publication, "bytes") !== undefined) fail("timecode_publication", "Unexpected timecode bytes")
                } else {
                    if (numbers.length) fail("timecode_publication", "Unexpected timecode Number restoration")
                    if (channel === "TIMECODE_AUDIO_DATA") {
                        if (data !== null) fail("timecode_publication", "Invalid native audio placeholder")
                        const bytes = field(publication, "bytes")
                        if (!(bytes instanceof Uint8Array) && !Array.isArray(bytes)) fail("timecode_publication", "Native audio bytes missing")
                        if (bytes.length > 65536) fail("timecode_limit", "Native timecode audio exceeds bound")
                        if (Array.isArray(bytes))
                            for (let i = 0; i < bytes.length; i++) {
                                const byte = field(bytes, String(i))
                                if (!Number.isInteger(byte) || (byte as number) < 0 || (byte as number) > 255) fail("timecode_publication", "Invalid native timecode byte")
                            }
                        data = sourceBytes(bytes)
                    } else if ((channel !== "TIMECODE_STATUS" && channel !== "ALERT") || typeof data !== "string" || data.length > 65536 || encoder.encode(data).length > 65536 || (channel === "TIMECODE_STATUS" && !["play", "pause", "stop"].includes(data))) fail("timecode_publication", "Unsupported native timecode event")
                    else if (field(publication, "bytes") !== undefined) fail("timecode_publication", "Unexpected native bytes")
                }
                if (!terminal && !retiring && same(current, frozen) && same(options.getContext(), frozen)) {
                    options.deliver({ channel: channel as string, data })
                    delivered = true
                }
            } catch (error) {
                report(error)
            }
            try {
                completion = Promise.resolve(options.complete(frozen, id, delivered))
                    .then(() => {})
                    .catch((error) => {
                        if (!retiring && same(current, frozen) && same(options.getContext(), frozen)) {
                            terminalError ??= report(error, "timecode_completion")
                            beginRetirement(true)
                        }
                    })
                    .finally(() => {
                        completion = undefined
                    })
            } catch (error) {
                if (!retiring && same(current, frozen) && same(options.getContext(), frozen)) {
                    terminalError ??= report(error, "timecode_completion")
                    beginRetirement(true)
                }
            } finally {
                publishing = false
            }
        },
        contextChanged() {
            if (!retiring && same(current, options.getContext())) return Promise.resolve()
            beginRetirement(false)
            return waitCleanup()
        },
        close() {
            beginRetirement(true)
            return waitCleanup()
        },
        status() {
            return { accepted: accepted + Number(cleanupRunning), queued: queue.length, active, completion: !!completion, retiring, terminal, cleanupRunning, cleanupError: cleanupError?.code, terminalError: terminalError?.code, lastSequence: lastSequence.toString() }
        }
    }
}

import type { RuntimeContext } from "./bridge"

export type AudioMetadata = { sampleRate?: unknown; id?: unknown; icecast?: unknown }
export type AudioOrigin = "port" | "PCM" | "CAPTURE"
export type AudioPortOptions = {
    getContext: () => RuntimeContext | undefined
    postPort: (message: { type: "AUDIO_PORT_RESPONSE"; targetId: unknown }, targetOrigin: "*", ports: MessagePort[]) => void
    transport: (bytes: Uint8Array, metadata: AudioMetadata, context: RuntimeContext, origin: AudioOrigin) => Promise<unknown>
    disconnect: (context: RuntimeContext) => Promise<unknown>
    closeContext: (context: RuntimeContext) => Promise<unknown>
    report: (error: AudioPortError) => void
    drainTimeoutMs?: number
}
export class AudioPortError extends Error {
    constructor(
        public readonly code: string,
        message: string
    ) {
        super(message.slice(0, 2048))
        this.name = "AudioPortError"
    }
}
type Port = { key: string; port: MessagePort; context: RuntimeContext; onClose: () => void }
type Job = { context: RuntimeContext } & ({ kind: "packet"; bytes: Uint8Array; metadata: AudioMetadata; origin: AudioOrigin } | { kind: "disconnect"; count: number })
const same = (a: RuntimeContext | undefined, b: RuntimeContext | undefined) => !!a && !!b && a.runtimeId === b.runtimeId && a.libraryGeneration === b.libraryGeneration && a.dataPath === b.dataPath
const own = (value: object, key: string) => Object.prototype.hasOwnProperty.call(value, key)
function fail(code: string, message: string): never {
    throw new AudioPortError(code, message)
}

// Copy only bounded structured JSON, preserving own undefined at the metadata
// boundary. PCM views use byte offsets, never JSON numeric-array expansion.
function copyJson(value: unknown, bytes = 8192, nodes = 256, bufferArray = false): unknown {
    const active = new Set<object>()
    const encoder = new TextEncoder()
    const charge = (text: string) => {
        if (text.length > bytes) fail("audio_metadata_limit", "Audio JSON exceeds its byte bound")
        bytes -= encoder.encode(text).length
        if (bytes < 0) fail("audio_metadata_limit", "Audio JSON exceeds its byte bound")
    }
    const visit = (input: unknown, depth: number): unknown => {
        if (--nodes < 0 || depth > 8) fail("audio_metadata_limit", "Audio JSON exceeds its work/depth bound")
        if (input === undefined && bufferArray) return undefined
        if (input === null || typeof input === "boolean") {
            charge(String(input))
            return input
        }
        if (typeof input === "string") {
            if (input.length > bytes) fail("audio_metadata_limit", "Audio string exceeds its byte bound")
            charge(JSON.stringify(input))
            return input
        }
        if (typeof input === "number") {
            if (!bufferArray && !Number.isFinite(input)) fail("audio_metadata_invalid", "Nonfinite audio metadata needs a separate wire contract")
            charge(String(input))
            return input
        }
        if (!input || typeof input !== "object" || active.has(input)) fail("audio_metadata_invalid", "Audio metadata must be finite acyclic JSON")
        const array = Array.isArray(input)
        if (!array && Object.getPrototypeOf(input) !== Object.prototype && Object.getPrototypeOf(input) !== null) fail("audio_metadata_invalid", "Audio metadata has an unsupported prototype")
        active.add(input)
        charge("[]")
        let result: unknown
        if (array) {
            const arrayValue = input as unknown[]
            if (arrayValue.length > nodes) fail("audio_metadata_limit", "Audio array exceeds its work bound")
            const values = []
            for (let i = 0; i < arrayValue.length; i++) {
                const field = Object.getOwnPropertyDescriptor(arrayValue, String(i))
                if ((!field && !bufferArray) || (field && !("value" in field))) fail("audio_metadata_invalid", "Audio arrays cannot contain accessors or holes")
                if (i) charge(",")
                values.push(visit(field?.value, depth + 1))
            }
            result = values
        } else {
            const values: Record<string, unknown> = {}
            let first = true
            for (const key in input) {
                if (!own(input, key)) continue
                const field = Object.getOwnPropertyDescriptor(input, key)!
                if (!("value" in field)) fail("audio_metadata_invalid", "Audio metadata cannot contain accessors")
                if (key.length > bytes) fail("audio_metadata_limit", "Audio key exceeds its byte bound")
                charge((first ? "" : ",") + JSON.stringify(key) + ":")
                first = false
                Object.defineProperty(values, key, { value: visit(field.value, depth + 1), enumerable: true, writable: true, configurable: true })
            }
            result = values
        }
        active.delete(input)
        return result
    }
    return visit(value, 0)
}
function context(value: RuntimeContext | undefined): RuntimeContext {
    if (!value || typeof value.runtimeId !== "string" || !value.runtimeId || value.runtimeId.length > 256 || !Number.isSafeInteger(value.libraryGeneration) || value.libraryGeneration < 0 || typeof value.dataPath !== "string" || value.dataPath.length > 32768) fail("audio_context_invalid", "Audio requires a bounded full native runtime context")
    return Object.freeze({ runtimeId: value.runtimeId, libraryGeneration: value.libraryGeneration, dataPath: value.dataPath })
}
function field(value: unknown, key: string): unknown {
    if (value === null || value === undefined) return undefined
    const descriptor = Object.getOwnPropertyDescriptor(Object(value), key)
    if (descriptor && !("value" in descriptor)) fail("audio_metadata_invalid", "Audio fields cannot contain accessors")
    // Source data?.buffer also observes the built-in view buffer getter when
    // the payload itself is a typed view (the entire backing storage there).
    if (!descriptor && key === "buffer" && ArrayBuffer.isView(value)) return value.buffer
    return descriptor?.value
}
function metadata(value: unknown): AudioMetadata {
    const fields: AudioMetadata = {}
    for (const key of ["sampleRate", "id", "icecast"] as const) {
        if (value !== null && value !== undefined && own(Object(value), key)) {
            const raw = field(value, key)
            fields[key] = raw === undefined ? undefined : copyJson(raw)
        }
    }
    // A single aggregate budget, in addition to individual traversal bounds.
    copyJson(Object.fromEntries(Object.entries(fields).filter(([, v]) => v !== undefined)))
    return fields
}
function pcm(value: unknown): Uint8Array | undefined {
    try {
        return copyPcm(value)
    } catch (error) {
        if (error instanceof AudioPortError) throw error
        fail("audio_packet_invalid", "Audio buffer cannot be snapshotted as bytes")
    }
}
function copyPcm(value: unknown): Uint8Array | undefined {
    if (!value) return
    let view: Uint8Array
    if (value instanceof ArrayBuffer) view = new Uint8Array(value)
    else if (ArrayBuffer.isView(value)) view = new Uint8Array(value.buffer, value.byteOffset, value.byteLength)
    else if (field(value, "type") === "Buffer" && Array.isArray(field(value, "data"))) {
        const array = field(value, "data") as unknown[]
        if (array.length > 32768) fail("audio_packet_limit", "Serialized audio exceeds 32768 bytes")
        view = new Uint8Array(copyJson(array, 131072, 65537, true) as number[])
    } else return
    if (view.byteLength > 32768) fail("audio_packet_limit", "Audio exceeds 32768 raw bytes")
    return view.byteLength ? view.slice() : undefined
}

/** Pure preload adapter. Native framing, window authority, PCM conversion and
 * latest Icecast settings belong to the injected native pipeline. */
export function createAudioPortBridge(options: AudioPortOptions) {
    const timeout = options.drainTimeoutMs ?? 5000
    if (!Number.isFinite(timeout) || timeout <= 0 || timeout > 5000) fail("audio_configuration", "Audio drain timeout must be in (0,5000] ms")
    const ports = new Map<string, Port>()
    const initial = options.getContext()
    let current = initial ? context(initial) : undefined
    let queue: Job[] = []
    let accepted = 0
    let active = false
    let pump: Promise<void> | undefined
    let terminal = false
    let retiring = false
    let cleanup: Promise<void> | undefined
    let cleanupRunning = false
    let cleanupError: AudioPortError | undefined
    let waiter = false
    const report = (error: unknown, code = "audio_transport") => {
        const controlled = error instanceof AudioPortError ? error : new AudioPortError(code, error instanceof Error ? error.message : "Audio operation failed")
        try {
            options.report(controlled)
        } catch {
            // Diagnostics must not strand the real accepted operation.
        }
        return controlled
    }
    function enqueue(job: Job) {
        // One of eight whole leases is reserved for independent context cleanup.
        if (accepted >= 7) fail("audio_busy", "Audio packet/control queue is full")
        accepted++
        queue.push(job)
        if (!pump) {
            pump = Promise.resolve().then(async () => {
                while (queue.length) {
                    const job = queue.shift()!
                    active = true
                    try {
                        if (job.kind === "packet") await options.transport(job.bytes, job.metadata, job.context, job.origin)
                        else
                            for (let n = 0; n < job.count; n++) {
                                try {
                                    await options.disconnect(job.context)
                                } catch (error) {
                                    report(error, "audio_disconnect")
                                }
                            }
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
    }
    function detach(entry: Port) {
        entry.port.onmessage = null
        entry.port.onmessageerror = null
        entry.port.removeEventListener("close", entry.onClose)
        entry.port.close()
        if (ports.get(entry.key) === entry) ports.delete(entry.key)
    }
    function available() {
        if (terminal || retiring) fail("audio_closed", "Audio context is closed or still retiring")
        const frozen = context(options.getContext())
        if (!same(current, frozen)) fail("audio_stale_context", "Audio requires explicit context retirement before replacement")
        return frozen
    }
    function packet(value: unknown, origin: AudioOrigin, frozen: RuntimeContext) {
        const bytes = pcm(field(value, "buffer"))
        if (!bytes) return
        enqueue({ kind: "packet", bytes, metadata: metadata(value), context: frozen, origin })
    }
    function beginRetirement(final: boolean) {
        terminal ||= final
        if (cleanupRunning) return
        if (terminal && retiring && cleanup && !cleanupError) return
        retiring = true
        for (const entry of ports.values()) detach(entry)
        // Explicit context/terminal cancellation, not a sender-time port epoch.
        // Already-entered transport is retained; queued old work is canceled.
        for (const _job of queue) {
            accepted--
            report(new AudioPortError("audio_context_closed", "Queued audio canceled by context retirement"))
        }
        queue = []
        const old = current
        cleanupRunning = true
        cleanupError = undefined
        cleanup = Promise.resolve()
            .then(async () => {
                // This reserved operation runs even if the ordinary pump stalls.
                let failed = false
                try {
                    if (old) await options.closeContext(old)
                } catch (error) {
                    cleanupError = report(error, "audio_cleanup")
                    failed = true
                }
                await pump
                if (failed) return
                if (!terminal) current = context(options.getContext())
                retiring = terminal
            })
            .catch((error) => {
                cleanupError = report(error, "audio_cleanup")
            })
            .finally(() => {
                cleanupRunning = false
            })
    }
    async function waitCleanup() {
        if (waiter) fail("audio_control_busy", "Audio drain already has a waiter")
        waiter = true
        let timer: ReturnType<typeof setTimeout> | undefined
        try {
            await Promise.race([
                cleanup,
                new Promise<never>((_, reject) => {
                    timer = setTimeout(() => reject(new AudioPortError("audio_drain_timeout", "Actual audio transport/cleanup is still owned; retry drain")), timeout)
                })
            ])
            if (cleanupError) throw cleanupError
        } finally {
            clearTimeout(timer)
            waiter = false
        }
    }
    return {
        handle(message: { channel?: unknown; data?: unknown }) {
            try {
                const channel = field(message, "channel")
                if (channel === "RESET_DECODER") return
                if (!["INIT_PORT", "CLOSE_PORT", "PCM", "CAPTURE"].includes(channel as string)) fail("audio_unknown_channel", "Unknown AUDIO channel")
                const frozen = available()
                const data = field(message, "data")
                if (channel === "PCM" || channel === "CAPTURE") return packet(data, channel, frozen)
                const rawId = field(data, "id")
                const id = rawId === undefined ? undefined : copyJson(rawId)
                const key = id ? String(id) : "default"
                const existing = ports.get(key)
                if (channel === "CLOSE_PORT") {
                    if (key === "icecast") enqueue({ kind: "disconnect", count: existing ? 2 : 1, context: frozen })
                    if (existing) detach(existing)
                    return
                }
                if (!existing && ports.size >= 16) fail("audio_port_limit", "Audio has sixteen active ports")
                if (existing && key === "icecast") enqueue({ kind: "disconnect", count: 1, context: frozen })
                if (existing) detach(existing)
                const { port1, port2 } = new MessageChannel()
                const entry: Port = {
                    key,
                    port: port1,
                    context: frozen,
                    onClose() {
                        if (ports.get(key) === entry) ports.delete(key)
                        if (key === "icecast") {
                            try {
                                enqueue({ kind: "disconnect", count: 1, context: frozen })
                            } catch (error) {
                                report(error)
                                beginRetirement(true)
                            }
                        }
                    }
                }
                port1.onmessage = (event) => {
                    try {
                        const now = available()
                        if (!same(entry.context, now)) fail("audio_stale_context", "Audio port belongs to an obsolete context")
                        const payload = field(event.data, "payload")
                        if (field(event.data, "channel") === "AUDIO" && payload) packet(payload, "port", now)
                    } catch (error) {
                        report(error)
                    }
                }
                port1.onmessageerror = () => report(new AudioPortError("audio_message_invalid", "Audio MessagePort could not deserialize a packet"))
                port1.addEventListener("close", entry.onClose)
                ports.set(key, entry)
                port1.start()
                try {
                    options.postPort({ type: "AUDIO_PORT_RESPONSE", targetId: id }, "*", [port2])
                } catch (error) {
                    detach(entry)
                    port2.close()
                    throw error
                }
            } catch (error) {
                report(error)
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
            return { ports: ports.size, queued: queue.length, active, accepted: accepted + Number(cleanupRunning), cleanupRunning, retiring, terminal, cleanupError: cleanupError?.code }
        }
    }
}

import type { RuntimeContext } from "./bridge"
import type { TimecodeMessage } from "./timecodeBridge"

const encoder = new TextEncoder()
const COMMAND_LIMIT = 262144,
    PREFIX_LIMIT = 49152,
    AUDIO_LIMIT = 65536
const typedPrototype = Object.getPrototypeOf(Uint8Array.prototype)
const typedBuffer = Object.getOwnPropertyDescriptor(typedPrototype, "buffer")!.get!
const typedLength = Object.getOwnPropertyDescriptor(typedPrototype, "length")!.get!
export class TimecodeWireError extends Error {
    constructor(
        public readonly code: string,
        message: string
    ) {
        super(message)
        this.name = "TimecodeWireError"
    }
}
function fail(code: string, message: string): never {
    throw new TimecodeWireError(code, message)
}
function field(value: unknown, key: string): unknown {
    if (!value || typeof value !== "object") return undefined
    const descriptor = Object.getOwnPropertyDescriptor(value, key)
    if (descriptor && !("value" in descriptor)) fail("timecode_wire_shape", "Accessors are unsupported")
    return descriptor?.value
}
function plain(value: unknown): value is Record<string, unknown> {
    return !!value && typeof value === "object" && [Object.prototype, null].includes(Object.getPrototypeOf(value))
}
function utf8(text: string, limit: number): number {
    if (text.length > limit) fail("timecode_wire_limit", "String exceeds bound")
    for (let index = 0; index < text.length; index++) {
        const code = text.charCodeAt(index)
        if (code >= 0xd800 && code <= 0xdbff) {
            const low = text.charCodeAt(++index)
            if (!(low >= 0xdc00 && low <= 0xdfff)) fail("timecode_wire_shape", "Lone UTF-16 surrogate is unsupported")
        } else if (code >= 0xdc00 && code <= 0xdfff) fail("timecode_wire_shape", "Lone UTF-16 surrogate is unsupported")
    }
    const length = encoder.encode(text).length
    if (length > limit) fail("timecode_wire_limit", "UTF-8 string exceeds bound")
    return length
}
function text(value: unknown, limit: number): string {
    if (typeof value !== "string") fail("timecode_wire_shape", "Expected a string")
    utf8(value, limit)
    return JSON.stringify(value)
}
function context(value: RuntimeContext): string {
    const runtimeId = field(value, "runtimeId"),
        generation = field(value, "libraryGeneration"),
        dataPath = field(value, "dataPath")
    if (!plain(value) || typeof runtimeId !== "string" || !runtimeId || runtimeId.includes("\0") || typeof dataPath !== "string" || !dataPath || dataPath.includes("\0") || !Number.isSafeInteger(generation) || (generation as number) < 0) fail("timecode_wire_context", "A bounded full context is required")
    return `{"runtimeId":${text(runtimeId, 256)},"libraryGeneration":${generation},"dataPath":${text(dataPath, 32768)}}`
}
function number(value: number): string {
    if (Number.isNaN(value)) return '{"kind":"NaN"}'
    if (value === Infinity) return '{"kind":"PositiveInfinity"}'
    if (value === -Infinity) return '{"kind":"NegativeInfinity"}'
    if (Object.is(value, -0)) return '{"kind":"NegativeZero"}'
    return `{"kind":"Finite","value":${String(value)}}`
}
function scalar(value: unknown): string {
    if (value === undefined) return '{"kind":"undefined"}'
    if (value === null) return '{"kind":"null"}'
    if (typeof value === "boolean") return `{"kind":"bool","value":${value}}`
    if (typeof value === "number") return `{"kind":"number","value":${number(value)}}`
    if (typeof value === "string") return `{"kind":"text","value":${text(value, 4096)}}`
    fail("timecode_wire_shape", "Framerate supports only source primitives")
}
function data(value: unknown): string {
    if (value === undefined) return '{"kind":"undefined"}'
    let nodes = 1024,
        bytes = 65536
    const active = new Set<object>()
    const charge = (value: string) => {
        bytes -= utf8(value, bytes)
    }
    const visit = (value: unknown, depth: number): string => {
        if (--nodes < 0 || depth > 8) fail("timecode_wire_limit", "Data exceeds 1024 nodes/depth8")
        if (typeof value === "string") {
            charge(value)
            return JSON.stringify(value)
        }
        if (value === null || typeof value === "boolean" || typeof value === "number") {
            bytes -= 16
            if (bytes < 0) fail("timecode_wire_limit", "Data exceeds host byte/work bound")
            if (typeof value === "number") {
                if (!Number.isFinite(value)) fail("timecode_wire_shape", "Nested nonfinite data is not route::Data::Json")
                return Object.is(value, -0) ? "-0.0" : String(value)
            }
            return String(value)
        }
        if ((!plain(value) && !Array.isArray(value)) || active.has(value)) fail("timecode_wire_shape", "Data requires finite acyclic plain JSON; nested undefined is unsupported")
        active.add(value)
        let output: string
        if (Array.isArray(value)) {
            if (value.length > nodes) fail("timecode_wire_limit", "Data array exceeds work bound")
            const items: string[] = []
            for (let index = 0; index < value.length; index++) {
                const descriptor = Object.getOwnPropertyDescriptor(value, String(index))
                if (!descriptor || !("value" in descriptor)) fail("timecode_wire_shape", "Array holes/accessors are unsupported")
                items.push(visit(descriptor.value, depth + 1))
            }
            output = `[${items.join(",")}]`
        } else {
            const items: string[] = []
            for (const key in value) {
                if (!Object.prototype.hasOwnProperty.call(value, key)) continue
                charge(key)
                const descriptor = Object.getOwnPropertyDescriptor(value, key)!
                if (!("value" in descriptor)) fail("timecode_wire_shape", "Data accessors are unsupported")
                items.push(`${JSON.stringify(key)}:${visit(descriptor.value, depth + 1)}`)
            }
            output = `{${items.join(",")}}`
        }
        active.delete(value)
        return output
    }
    return `{"kind":"json","value":${visit(value, 0)}}`
}
/** Private raw command body, not a source message or success acknowledgment. */
export function encodeTimecodeCommand(message: TimecodeMessage, fullContext: RuntimeContext): Uint8Array {
    if (!plain(message)) fail("timecode_wire_shape", "Expected original MAIN command object")
    const channel = field(message, "channel")
    let command: string
    switch (channel) {
        case "TIMECODE_START": {
            const value = field(message, "data")
            if (!plain(value)) fail("timecode_wire_shape", "START requires original object")
            command = `{"kind":"start","type":${text(field(value, "type"), 32)},"mode":${text(field(value, "mode"), 4096)},"framerate":${scalar(field(value, "framerate"))},"data":${data(field(value, "data"))}}`
            break
        }
        case "TIMECODE_STOP":
            command = '{"kind":"stop"}'
            break
        case "TIMECODE_VALUE": {
            const value = field(message, "data")
            if (typeof value !== "number") fail("timecode_wire_shape", "VALUE requires a Number")
            command = `{"kind":"value","value":${number(value)}}`
            break
        }
        case "TIMECODE_STATUS":
            command = `{"kind":"status","data":${data(field(message, "data"))}}`
            break
        default:
            fail("timecode_wire_unsupported", "Unsupported timecode command")
    }
    const bytes = encoder.encode(`{"version":1,"context":${context(fullContext)},"command":${command}}`)
    if (bytes.length > COMMAND_LIMIT) fail("timecode_wire_limit", "Command exceeds 262144 encoded bytes")
    return bytes
}
/** Independent 64KiB timecode framing. No AUDIO PCM format, coercion or codec. */
export function encodeTimecodeLtc(bytes: Uint8Array, mode: "LTC", fullContext: RuntimeContext): Uint8Array {
    if (!(bytes instanceof Uint8Array) || !(typedBuffer.call(bytes) instanceof ArrayBuffer)) fail("timecode_wire_shape", "LTC requires private Uint8Array storage")
    if (mode !== "LTC") fail("timecode_wire_unsupported", "Raw framing supports explicit LTC only")
    const length = typedLength.call(bytes)
    if (length > AUDIO_LIMIT) fail("timecode_wire_limit", "LTC exceeds 65536 bytes")
    const prefix = encoder.encode(`{"version":1,"context":${context(fullContext)},"mode":"LTC","byteLength":${length}}`)
    if (prefix.length > PREFIX_LIMIT) fail("timecode_wire_limit", "LTC prefix exceeds 49152 encoded bytes")
    const result = new Uint8Array(4 + prefix.length + length)
    new DataView(result.buffer).setUint32(0, prefix.length, true)
    result.set(prefix, 4)
    result.set(bytes, 4 + prefix.length)
    return result
}

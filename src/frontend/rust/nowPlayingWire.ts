import type { RuntimeContext } from "./bridge"

const encoder = new TextEncoder()
export class NowPlayingWireError extends Error {
    constructor(
        public readonly code: string,
        message: string
    ) {
        super(message)
        this.name = "NowPlayingWireError"
    }
}
function fail(code: string, message: string): never {
    throw new NowPlayingWireError(code, message)
}
function plain(value: unknown): value is Record<string, unknown> {
    return !!value && typeof value === "object" && [Object.prototype, null].includes(Object.getPrototypeOf(value)) && Object.getOwnPropertySymbols(value).length === 0
}
function field(value: unknown, key: string): unknown {
    if (!value || typeof value !== "object") return undefined
    const descriptor = Object.getOwnPropertyDescriptor(value, key)
    if (descriptor && !("value" in descriptor)) fail("now_playing_wire_shape", "Accessors are unsupported")
    return descriptor?.value
}
function utf8(value: string, max: number): number {
    if (value.length > max) fail("now_playing_wire_limit", "String exceeds bound")
    for (let index = 0; index < value.length; index++) {
        const code = value.charCodeAt(index)
        if (code >= 0xd800 && code <= 0xdbff) {
            const low = value.charCodeAt(++index)
            if (!(low >= 0xdc00 && low <= 0xdfff)) fail("now_playing_wire_shape", "Lone UTF16 surrogate is unsupported")
        } else if (code >= 0xdc00 && code <= 0xdfff) fail("now_playing_wire_shape", "Lone UTF16 surrogate is unsupported")
    }
    const length = encoder.encode(value).length
    if (length > max) fail("now_playing_wire_limit", "UTF8 string exceeds bound")
    return length
}
function fullContext(value: RuntimeContext): string {
    const runtime = field(value, "runtimeId"),
        generation = field(value, "libraryGeneration"),
        path = field(value, "dataPath")
    if (!plain(value) || typeof runtime !== "string" || !runtime || runtime.includes("\0") || typeof path !== "string" || !path || path.includes("\0") || !Number.isSafeInteger(generation) || (generation as number) < 0) fail("now_playing_wire_context", "A bounded full context is required")
    utf8(runtime, 256)
    utf8(path, 32768)
    return `{"runtimeId":${JSON.stringify(runtime)},"libraryGeneration":${generation},"dataPath":${JSON.stringify(path)}}`
}
function number(value: number): string {
    if (Number.isNaN(value)) return '{"kind":"NaN"}'
    if (value === Infinity) return '{"kind":"PositiveInfinity"}'
    if (value === -Infinity) return '{"kind":"NegativeInfinity"}'
    if (Object.is(value, -0)) return '{"kind":"NegativeZero"}'
    return `{"kind":"Finite","value":${String(value)}}`
}

/** Snapshots original MAIN input at send time; this is private native framing. */
export function encodeNowPlayingCommand(message: unknown, context: RuntimeContext): Uint8Array {
    if (!plain(message)) fail("now_playing_wire_shape", "Expected original MAIN object")
    const channel = field(message, "channel")
    let command: string
    if (channel === "NOW_PLAYING_UNSET" || channel === "OPEN_NOW_PLAYING") {
        command = `{"kind":"${channel === "NOW_PLAYING_UNSET" ? "unset" : "open"}"}`
    } else if (channel === "NOW_PLAYING") {
        const data = field(message, "data")
        if (!plain(data)) fail("now_playing_wire_shape", "NOW_PLAYING requires an object")
        const duration = Object.getOwnPropertyDescriptor(data, "duration")?.enumerable ? field(data, "duration") : undefined
        let nodes = 1024,
            bytes = 65536
        const active = new Set<object>()
        const visit = (value: unknown, depth: number): string => {
            if (--nodes < 0 || depth > 8) fail("now_playing_wire_limit", "Data exceeds1024 nodes/depth8")
            if (typeof value === "string") {
                bytes -= utf8(value, bytes)
                return JSON.stringify(value)
            }
            if (value === null || typeof value === "boolean" || typeof value === "number") {
                bytes -= 16
                if (bytes < 0) fail("now_playing_wire_limit", "Data byte/work bound exceeded")
                if (typeof value === "number") {
                    if (!Number.isFinite(value)) fail("now_playing_wire_shape", "Only duration has a private nonfinite Number tag")
                    return Object.is(value, -0) ? "-0.0" : String(value)
                }
                return String(value)
            }
            if ((!plain(value) && !Array.isArray(value)) || active.has(value)) fail("now_playing_wire_shape", "Data requires finite acyclic plain JSON")
            active.add(value)
            let text: string
            if (Array.isArray(value)) {
                if (Object.getPrototypeOf(value) !== Array.prototype || Object.getOwnPropertySymbols(value).length || Object.getOwnPropertyNames(value).length !== value.length + 1) fail("now_playing_wire_shape", "Dense plain JSON arrays are required")
                if (value.length > nodes) fail("now_playing_wire_limit", "Array exceeds work bound")
                const values: string[] = []
                for (let index = 0; index < value.length; index++) {
                    const descriptor = Object.getOwnPropertyDescriptor(value, String(index))
                    if (!descriptor?.enumerable || !("value" in descriptor)) fail("now_playing_wire_shape", "Array holes/accessors/non-enumerable indices are unsupported")
                    values.push(visit(descriptor.value, depth + 1))
                }
                text = `[${values.join(",")}]`
            } else {
                const names = Object.getOwnPropertyNames(value)
                if (names.length > nodes) fail("now_playing_wire_limit", "Object exceeds work bound")
                const values: string[] = []
                for (const key of names) {
                    const descriptor = Object.getOwnPropertyDescriptor(value, key)!
                    if (!descriptor.enumerable) continue
                    if (!("value" in descriptor)) fail("now_playing_wire_shape", "Data accessors are unsupported")
                    // An own undefined duration has the same source scalar as
                    // an absent duration. Other undefined values reject.
                    if (depth === 0 && key === "duration" && descriptor.value === undefined) continue
                    bytes -= utf8(key, bytes)
                    values.push(`${JSON.stringify(key)}:${visit(depth === 0 && key === "duration" && typeof duration === "number" ? null : descriptor.value, depth + 1)}`)
                }
                text = `{${values.join(",")}}`
            }
            active.delete(value)
            return text
        }
        const payload = visit(data, 0)
        if (encoder.encode(payload).length > 65536) fail("now_playing_wire_limit", "Data exceeds65536 encoded bytes")
        command = `{"kind":"set","data":${payload}${typeof duration === "number" ? `,"duration":${number(duration)}` : ""}}`
    } else fail("now_playing_wire_unsupported", "Unsupported now-playing command")
    const body = encoder.encode(`{"version":1,"context":${fullContext(context)},"command":${command}}`)
    if (body.length > 262144) fail("now_playing_wire_limit", "Command exceeds262144 encoded bytes")
    return body
}

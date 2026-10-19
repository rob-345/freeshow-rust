import type { AudioMetadata, AudioOrigin } from "./audioPortBridge"
import type { RuntimeContext } from "./bridge"

export function encodeAudioFrame(bytes: Uint8Array, metadata: AudioMetadata, context: RuntimeContext, origin: AudioOrigin): Uint8Array {
    function fail(message: string): never {
        throw new Error("Invalid raw AUDIO frame: " + message)
    }
    const encoder = new TextEncoder()
    if (!(bytes instanceof Uint8Array) || bytes.byteLength > 32768) fail("PCM byte bound")
    if (!["port", "PCM", "CAPTURE"].includes(origin)) fail("origin")
    if (!context || typeof context.runtimeId !== "string" || !context.runtimeId || context.runtimeId.includes("\0") || context.runtimeId.length > 256 || encoder.encode(context.runtimeId).length > 256 || !Number.isSafeInteger(context.libraryGeneration) || context.libraryGeneration < 0 || typeof context.dataPath !== "string" || !context.dataPath || context.dataPath.includes("\0") || context.dataPath.length > 32768 || encoder.encode(context.dataPath).length > 32768) fail("full context")
    if (!metadata || typeof metadata !== "object" || Array.isArray(metadata)) fail("metadata object")
    let nodes = 256,
        budget = 8192
    const active = new Set<object>(),
        negativeZeroPaths: string[] = []
    const charge = (text: string) => {
        if (text.length > budget) fail("metadata byte bound")
        budget -= encoder.encode(text).length
        if (budget < 0) fail("metadata byte bound")
    }
    const pointer = (path: string, key: string) => path + "/" + key.replace(/~/g, "~0").replace(/\//g, "~1")
    const copy = (value: unknown, path: string, depth: number): unknown => {
        if (--nodes < 0 || depth > 8) fail("metadata work bound")
        if (value === null || typeof value === "boolean") {
            charge(String(value))
            return value
        }
        if (typeof value === "number") {
            if (!Number.isFinite(value)) fail("nonfinite metadata")
            charge(String(value))
            if (Object.is(value, -0)) negativeZeroPaths.push(path)
            return value
        }
        if (typeof value === "string") {
            if (value.length > budget) fail("metadata byte bound")
            charge(JSON.stringify(value))
            return value
        }
        if (!value || typeof value !== "object" || active.has(value)) fail("metadata type/cycle")
        const array = Array.isArray(value)
        if (!array && Object.getPrototypeOf(value) !== Object.prototype && Object.getPrototypeOf(value) !== null) fail("metadata prototype")
        active.add(value)
        charge("[]")
        let result: unknown
        if (array) {
            const input = value as unknown[]
            if (input.length > nodes) fail("metadata array bound")
            const output = []
            for (let index = 0; index < input.length; index++) {
                const field = Object.getOwnPropertyDescriptor(input, String(index))
                if (!field || !("value" in field)) fail("metadata array accessor/hole")
                if (index) charge(",")
                output.push(copy(field.value, pointer(path, String(index)), depth + 1))
            }
            result = output
        } else {
            const output: Record<string, unknown> = Object.create(null)
            let first = true
            for (const key in value) {
                if (!Object.hasOwn(value, key)) continue
                const field = Object.getOwnPropertyDescriptor(value, key)!
                if (!("value" in field)) fail("metadata accessor")
                if (depth === 0 && !["sampleRate", "id", "icecast"].includes(key)) fail("metadata field")
                if (depth === 0 && field.value === undefined) continue
                if (key.length > budget) fail("metadata key bound")
                charge((first ? "" : ",") + JSON.stringify(key) + ":")
                first = false
                Object.defineProperty(output, key, { value: copy(field.value, pointer(path, key), depth + 1), enumerable: true })
            }
            result = output
        }
        active.delete(value)
        return result
    }
    const copied = copy(metadata, "", 0)
    if (encoder.encode(JSON.stringify(copied)).length > 8192 || negativeZeroPaths.some((path) => encoder.encode(path).length > 1024)) fail("metadata restoration bound")
    const prefix = encoder.encode(JSON.stringify({ version: 1, context: { runtimeId: context.runtimeId, libraryGeneration: context.libraryGeneration, dataPath: context.dataPath }, origin, metadata: copied, negativeZeroPaths }))
    if (!prefix.length || prefix.length > 49152) fail("prefix byte bound")
    const frame = new Uint8Array(4 + prefix.length + bytes.byteLength)
    new DataView(frame.buffer).setUint32(0, prefix.length, true)
    frame.set(prefix, 4)
    frame.set(bytes, 4 + prefix.length)
    return frame
}

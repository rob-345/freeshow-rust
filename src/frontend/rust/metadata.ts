// Called only after native AUDIO_METADATA channel/full-context acceptance.
export type AudioMetadataHydration = {
    undefinedPaths: string[]
    specialNumbers: { path: string; kind: "NaN" | "PositiveInfinity" | "NegativeInfinity" | "NegativeZero" }[]
    byteBuffers: string[]
    objectKeys?: { path: string; keys: string[] }[]
}

const MAX_NODES = 65536
const MAX_DEPTH = 16
const MAX_METADATA_BYTES = 8 * 1024 * 1024
const MAX_ENVELOPE_BYTES = 2 * 1024 * 1024
const MAX_STRING_BYTES = 6 * 1024 * 1024
const MAX_PATH_BYTES = 1024 * 1024
const MAX_KEY_BYTES = 1024 * 1024
const MAX_DECODED_BYTES = 4 * 1024 * 1024
const forbidden = new Set(["__proto__", "prototype", "constructor"])
function bad(): never {
    throw new Error("Invalid or excessive native audio metadata restoration")
}

// JSON-string UTF8 accounting without allocating a TextEncoder/escaped copy.
// Numbers are conservatively charged 32 bytes separately, not their short spelling.
function stringSize(value: string): { utf8: number; json: number } {
    let utf8 = 0,
        json = 2
    for (let i = 0; i < value.length; i++) {
        const c = value.charCodeAt(i)
        if (c < 128) {
            utf8++
            json += c === 34 || c === 92 || c === 8 || c === 9 || c === 10 || c === 12 || c === 13 ? 2 : c < 32 ? 6 : 1
        } else if (c < 2048) {
            utf8 += 2
            json += 2
        } else if (c >= 0xd800 && c <= 0xdbff && value.charCodeAt(i + 1) >= 0xdc00 && value.charCodeAt(i + 1) <= 0xdfff) {
            utf8 += 4
            json += 4
            i++
        } else {
            utf8 += 3
            json += c >= 0xd800 && c <= 0xdfff ? 6 : 3
        }
    }
    return { utf8, json }
}
function descriptor(object: any, key: string): PropertyDescriptor {
    const d = Object.getOwnPropertyDescriptor(object, key)
    if (!d || !Object.hasOwn(d, "value") || !d.enumerable || !d.configurable || !d.writable) bad()
    return d
}
function record(value: any): void {
    if (!value || typeof value !== "object" || Array.isArray(value)) bad()
    const prototype = Object.getPrototypeOf(value)
    if ((prototype !== Object.prototype && prototype !== null) || Object.getOwnPropertySymbols(value).length) bad()
}
function keys(object: any): string[] {
    const names = Object.getOwnPropertyNames(object)
    if (names.length > MAX_NODES) bad()
    return names
}
function shape(value: any, required: string[], optional: string[] = []): void {
    record(value)
    const names = keys(value)
    if (required.some((key) => !Object.hasOwn(value, key)) || names.some((key) => !required.includes(key) && !optional.includes(key))) bad()
    for (const key of names) descriptor(value, key)
}
function index(key: string): number | null {
    if (!/^(0|[1-9]\d*)$/.test(key)) return null
    const n = Number(key)
    return n < 0xffffffff && String(n) === key ? n : null
}
function sextet(c: number): number {
    if (c >= 65 && c <= 90) return c - 65
    if (c >= 97 && c <= 122) return c - 71
    if (c >= 48 && c <= 57) return c + 4
    return c === 43 ? 62 : c === 47 ? 63 : -1
}
function base64Length(value: string): number {
    const n = value.length
    if (n % 4 !== 0) bad()
    const padding = n && value[n - 1] === "=" ? (value[n - 2] === "=" ? 2 : 1) : 0
    for (let i = 0; i < n - padding; i++) if (sextet(value.charCodeAt(i)) < 0) bad()
    if (padding && (n < 4 || sextet(value.charCodeAt(n - padding - 1)) & (padding === 2 ? 15 : 3))) bad()
    return (n / 4) * 3 - padding
}
function decode(value: string, length: number): Uint8Array {
    const output = new Uint8Array(length)
    let p = 0
    for (let i = 0; i < value.length; i += 4) {
        const n = (sextet(value.charCodeAt(i)) << 18) | (sextet(value.charCodeAt(i + 1)) << 12) | ((sextet(value.charCodeAt(i + 2)) & 63) << 6) | (sextet(value.charCodeAt(i + 3)) & 63)
        if (p < length) output[p++] = (n >> 16) & 255
        if (p < length) output[p++] = (n >> 8) & 255
        if (p < length) output[p++] = n & 255
    }
    return output
}

export function hydrateAudioMetadata(data: any, hydration: AudioMetadataHydration): void {
    shape(data, ["filePath", "metadata"])
    const root = descriptor(data, "metadata"),
        filePath = descriptor(data, "filePath").value
    if (typeof filePath !== "string" || filePath.length > 32768 || stringSize(filePath).utf8 > 32768) bad()
    record(root.value)
    shape(hydration, ["undefinedPaths", "specialNumbers", "byteBuffers"], ["objectKeys"])

    let nodes = 2,
        keyBytes = 0,
        pathBytes = 0
    const seen = new Set<object>()
    const chargeKey = (key: string) => {
        if (key.length > 1024) bad()
        const size = stringSize(key).utf8
        keyBytes += size
        if (forbidden.has(key) || size > 1024 || keyBytes > MAX_KEY_BYTES) bad()
    }
    const validate = (value: any, depth: number, budget: { bytes: number }, maxString: number): void => {
        if (++nodes > MAX_NODES || depth > MAX_DEPTH) bad()
        if (value === null) budget.bytes -= 4
        else if (typeof value === "boolean") budget.bytes -= 5
        else if (typeof value === "number") {
            if (!Number.isFinite(value)) bad()
            budget.bytes -= 32
        } else if (typeof value === "string") {
            if (value.length > maxString) bad()
            const size = stringSize(value).json
            if (size > maxString) bad()
            budget.bytes -= size
        } else {
            if (!value || typeof value !== "object" || seen.has(value) || Object.getOwnPropertySymbols(value).length) bad()
            seen.add(value)
            if (Array.isArray(value)) {
                if (Object.getPrototypeOf(value) !== Array.prototype || value.length > MAX_NODES || keys(value).length !== value.length + 1) bad()
                const length = Object.getOwnPropertyDescriptor(value, "length")!
                if (length.enumerable || length.configurable || !length.writable) bad()
                budget.bytes -= 2 + Math.max(0, value.length - 1)
                for (let i = 0; i < value.length; i++) validate(descriptor(value, String(i)).value, depth + 1, budget, maxString)
            } else {
                record(value)
                const names = keys(value)
                budget.bytes -= 2 + Math.max(0, names.length - 1)
                for (const key of names) {
                    chargeKey(key)
                    budget.bytes -= stringSize(key).json + 1
                    validate(descriptor(value, key).value, depth + 1, budget, maxString)
                }
            }
        }
        if (budget.bytes < 0) bad()
    }
    validate(root.value, 0, { bytes: MAX_METADATA_BYTES }, MAX_STRING_BYTES)
    validate(hydration, 0, { bytes: MAX_ENVELOPE_BYTES }, 2048 * 6 + 2)
    if (!Array.isArray(hydration.undefinedPaths) || !Array.isArray(hydration.specialNumbers) || !Array.isArray(hydration.byteBuffers) || (hydration.objectKeys !== undefined && !Array.isArray(hydration.objectKeys))) bad()

    type Parts = string[]
    type Mutation = { parts: Parts; value: unknown; encoded?: string; length?: number }
    const operations: Mutation[] = [],
        orders: { parts: Parts; keys: string[] }[] = []
    const extraKeys = new Map<object, string[]>()
    const paths = new Set<string>(),
        prefixes = new Set<string>(),
        orderPaths = new Set<string>()
    const parse = (pointer: string, mutation: boolean): Parts => {
        if (typeof pointer !== "string" || pointer.length > 2048) bad()
        const size = stringSize(pointer).utf8
        pathBytes += size
        if (size > 2048 || pathBytes > MAX_PATH_BYTES || !pointer.startsWith("/metadata") || /~(?![01])/u.test(pointer)) bad()
        const encoded = pointer.slice(1).split("/")
        if (encoded[0] !== "metadata" || encoded.length > MAX_DEPTH + 1 || (mutation && encoded.length < 2)) bad()
        const parts = encoded.slice(1).map((part) => part.replace(/~1/g, "/").replace(/~0/g, "~"))
        if (parts.some((part) => forbidden.has(part) || stringSize(part).utf8 > 1024)) bad()
        if (mutation) {
            if (paths.has(pointer) || prefixes.has(pointer)) bad()
            let prefix = "/metadata"
            for (const part of encoded.slice(1, -1)) {
                prefix += "/" + part
                if (paths.has(prefix)) bad()
                prefixes.add(prefix)
            }
            paths.add(pointer)
        }
        return parts
    }
    const target = (metadata: any, parts: Parts, allowMissing = false): { parent: any; key: string } => {
        let parent = metadata
        for (const part of parts.slice(0, -1)) {
            if (!parent || typeof parent !== "object" || (Array.isArray(parent) && index(part) === null) || !Object.hasOwn(parent, part)) bad()
            parent = parent[part]
        }
        const key = parts[parts.length - 1]
        if (!parent || typeof parent !== "object" || (Array.isArray(parent) && (index(key) === null || Number(key) >= parent.length)) || (!Object.hasOwn(parent, key) && !allowMissing)) bad()
        return { parent, key }
    }
    for (const pointer of hydration.undefinedPaths) {
        const parts = parse(pointer, true),
            { parent, key } = target(root.value, parts, true)
        if (Object.hasOwn(parent, key) && parent[key] !== null) bad()
        if (!Object.hasOwn(parent, key)) {
            if (++nodes > MAX_NODES) bad()
            chargeKey(key)
            const extra = extraKeys.get(parent) || []
            extra.push(key)
            extraKeys.set(parent, extra)
        }
        operations.push({ parts, value: undefined })
    }
    const special = { NaN, PositiveInfinity: Infinity, NegativeInfinity: -Infinity, NegativeZero: -0 }
    for (const number of hydration.specialNumbers) {
        shape(number, ["path", "kind"])
        if (typeof number.kind !== "string" || !Object.hasOwn(special, number.kind)) bad()
        const parts = parse(number.path, true),
            { parent, key } = target(root.value, parts)
        if (parent[key] !== (number.kind === "NegativeZero" ? 0 : null)) bad()
        operations.push({ parts, value: special[number.kind] })
    }
    let decodedBytes = 0
    for (const pointer of hydration.byteBuffers) {
        const parts = parse(pointer, true)
        if (parts.length !== 3 || parts[0] !== "picture" || !/^[0-7]$/.test(parts[1]) || parts[2] !== "data") bad()
        const { parent, key } = target(root.value, parts),
            encoded = parent[key]
        if (typeof encoded !== "string") bad()
        const length = base64Length(encoded)
        decodedBytes += length
        if (decodedBytes > MAX_DECODED_BYTES) bad()
        operations.push({ parts, value: undefined, encoded, length })
    }
    if (Object.hasOwn(root.value, "picture")) {
        const pictures = root.value.picture
        if (!Array.isArray(pictures) || pictures.length > 8) bad()
        for (let i = 0; i < pictures.length; i++) {
            record(pictures[i])
            if (!Object.hasOwn(pictures[i], "data") || !paths.has(`/metadata/picture/${i}/data`) || typeof pictures[i].data !== "string") bad()
        }
        if (hydration.byteBuffers.length !== pictures.length) bad()
    } else if (hydration.byteBuffers.length) bad()

    for (const order of hydration.objectKeys || []) {
        shape(order, ["path", "keys"])
        if (!Array.isArray(order.keys) || orderPaths.has(order.path)) bad()
        const parts = parse(order.path, false)
        orderPaths.add(order.path)
        const slot = parts.length ? target(root.value, parts) : null
        const object = slot ? slot.parent[slot.key] : root.value
        record(object)
        const expected = new Set(keys(object))
        for (const key of extraKeys.get(object) || []) expected.add(key)
        if (order.keys.length !== expected.size) bad()
        let nonIndex = false,
            lastIndex = -1
        for (const key of order.keys) {
            if (typeof key !== "string" || !expected.delete(key)) bad()
            chargeKey(key)
            const n = index(key)
            if (n === null) nonIndex = true
            else if (nonIndex || n <= lastIndex) bad()
            else lastIndex = n
        }
        orders.push({ parts, keys: order.keys })
    }
    if (!operations.length && !orders.length) return

    // No original callback data changes before all validation and allocations finish.
    const clone = (value: any): any => {
        if (!value || typeof value !== "object") return value
        if (Array.isArray(value)) {
            const array = new Array(value.length)
            for (let i = 0; i < value.length; i++) array[i] = clone(value[i])
            return array
        }
        const output = Object.create(Object.getPrototypeOf(value))
        for (const key of keys(value)) output[key] = clone(value[key])
        return output
    }
    const metadata = clone(root.value)
    for (const operation of operations) {
        const slot = target(metadata, operation.parts, true)
        slot.parent[slot.key] = operation.encoded !== undefined ? decode(operation.encoded, operation.length!) : operation.value
    }
    for (const order of orders) {
        const slot = order.parts.length ? target(metadata, order.parts) : null
        const object = slot ? slot.parent[slot.key] : metadata
        const values = order.keys.map((key) => object[key])
        for (const key of order.keys) delete object[key]
        order.keys.forEach((key, i) => {
            object[key] = values[i]
        })
    }
    Object.defineProperty(data, "metadata", { ...root, value: metadata })
}

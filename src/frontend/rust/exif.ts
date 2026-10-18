// Restore only the generated READ_EXIF clone differences after context acceptance.
export type ExifHydration = {
    undefinedPaths: string[]
    specialNumbers: { path: string; kind: "NaN" | "PositiveInfinity" | "NegativeInfinity" | "NegativeZero" }[]
    byteBuffers: string[]
    objectKeys?: { path: string; keys: string[] }[]
}

export function hydrateExif(data: any, metadata: ExifHydration): void {
    if (!data || typeof data !== "object" || Array.isArray(data) || !metadata || !Array.isArray(metadata.undefinedPaths) || !Array.isArray(metadata.specialNumbers) || !Array.isArray(metadata.byteBuffers)) throw new Error("Invalid native EXIF restoration")
    const count = metadata.undefinedPaths.length + metadata.specialNumbers.length + metadata.byteBuffers.length
    if (count > 65536) throw new Error("Native EXIF restoration count exceeded")
    const operations: { parent: any; key: string; value: unknown }[] = []
    const paths = new Set<string>(), prefixes = new Set<string>()
    let pathBytes = 0, bufferBytes = 0
    const target = (pointer: string, allowMissing = false, record = true) => {
        if (typeof pointer !== "string" || pointer.length > 1024 || !pointer.startsWith("/exif") || /~(?![01])/u.test(pointer)) throw new Error("Invalid native EXIF path")
        pathBytes += pointer.length * 2
        if (pathBytes > 2 * 1024 * 1024 || (record && (paths.has(pointer) || prefixes.has(pointer)))) throw new Error("Excessive or overlapping native EXIF paths")
        const encoded = pointer.slice(1).split("/")
        if (encoded[0] !== "exif" || encoded.length > 16) throw new Error("Invalid native EXIF root")
        let prefix = ""
        for (const part of encoded.slice(0, -1)) {
            prefix += "/" + part
            if (record && paths.has(prefix)) throw new Error("Overlapping native EXIF paths")
            if (record) prefixes.add(prefix)
        }
        if (record) paths.add(pointer)
        const parts = encoded.map((part) => part.replace(/~1/g, "/").replace(/~0/g, "~"))
        if (parts.some((part) => ["__proto__", "constructor", "prototype"].includes(part))) throw new Error("Invalid native EXIF property")
        let parent = data
        for (const part of parts.slice(0, -1)) {
            if (!parent || typeof parent !== "object" || !Object.hasOwn(parent, part)) throw new Error("Missing native EXIF parent")
            parent = parent[part]
        }
        const key = parts.at(-1)!
        if (!parent || typeof parent !== "object" || (Array.isArray(parent) && (!/^(0|[1-9]\d*)$/.test(key) || Number(key) >= parent.length)) || (!Object.hasOwn(parent, key) && !allowMissing)) throw new Error("Missing native EXIF target")
        return { parent, key }
    }
    for (const pointer of metadata.undefinedPaths) {
        const slot = target(pointer, true)
        if (Object.hasOwn(slot.parent, slot.key) && slot.parent[slot.key] !== null) throw new Error("Invalid native EXIF undefined placeholder")
        operations.push({ ...slot, value: undefined })
    }
    for (const number of metadata.specialNumbers) {
        if (!number || typeof number !== "object") throw new Error("Invalid native EXIF number")
        const values = { NaN, PositiveInfinity: Infinity, NegativeInfinity: -Infinity, NegativeZero: -0 }
        if (!Object.hasOwn(values, number.kind)) throw new Error("Invalid native EXIF number kind")
        const slot = target(number.path)
        if (slot.parent[slot.key] !== (number.kind === "NegativeZero" ? 0 : null)) throw new Error("Invalid native EXIF number placeholder")
        operations.push({ ...slot, value: values[number.kind] })
    }
    for (const pointer of metadata.byteBuffers) {
        const slot = target(pointer), bytes = slot.parent[slot.key]
        if (!Array.isArray(bytes) || bytes.length > 65536 || bytes.some((value) => !Number.isInteger(value) || value < 0 || value > 255)) throw new Error("Invalid native EXIF byte buffer")
        bufferBytes += bytes.length
        if (bufferBytes > 2 * 1024 * 1024) throw new Error("Native EXIF byte budget exceeded")
        operations.push({ ...slot, value: new Uint8Array(bytes) })
    }
    const orders: { object: any; keys: string[] }[] = []
    if (metadata.objectKeys !== undefined) {
        if (!Array.isArray(metadata.objectKeys) || metadata.objectKeys.length > 8) throw new Error("Invalid native EXIF object order")
        const seen = new Set<object>()
        for (const order of metadata.objectKeys) {
            if (!order || typeof order.path !== "string" || !Array.isArray(order.keys) || order.keys.length > 1024) throw new Error("Invalid native EXIF key list")
            const slot = order.path === "" ? null : target(order.path, false, false)
            const object = slot ? slot.parent[slot.key] : data
            if (!object || typeof object !== "object" || Array.isArray(object) || seen.has(object)) throw new Error("Invalid native EXIF ordered object")
            seen.add(object)
            const expected = new Set(Object.keys(object))
            for (const operation of operations) if (operation.parent === object) expected.add(operation.key)
            if (order.keys.length !== expected.size || new Set(order.keys).size !== expected.size || order.keys.some((key) => typeof key !== "string" || !expected.has(key))) throw new Error("Native EXIF key list mismatches original properties")
            orders.push({ object, keys: order.keys })
        }
    }
    // Validate the entire envelope before changing any original callback data.
    for (const operation of operations) operation.parent[operation.key] = operation.value
    for (const { object, keys } of orders) {
        const values = keys.map((key) => object[key])
        for (const key of keys) delete object[key]
        keys.forEach((key, index) => { object[key] = values[index] })
    }
}

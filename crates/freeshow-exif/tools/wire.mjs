// Data-only observer of original JS values; not an EXIF parser.
export function wire(value) {
    const hydration = { undefinedPaths: [], specialNumbers: [], byteBuffers: [] }
    function visit(item, at, array = false) {
        if (item === undefined) {
            hydration.undefinedPaths.push(at)
            return array ? null : undefined
        }
        if (typeof item === "number") {
            const kind = Number.isNaN(item) ? "NaN" : item === Infinity ? "PositiveInfinity" : item === -Infinity ? "NegativeInfinity" : Object.is(item, -0) ? "NegativeZero" : null
            if (kind) hydration.specialNumbers.push({ path: at, kind })
            return !Number.isFinite(item) ? null : Object.is(item, -0) ? 0 : item
        }
        if ((typeof Buffer !== "undefined" && Buffer.isBuffer(item)) || ArrayBuffer.isView(item)) {
            hydration.byteBuffers.push(at)
            return [...item]
        }
        if (Array.isArray(item)) return item.map((value, index) => visit(value, at + "/" + index, true))
        if (item && typeof item === "object") {
            const result = {}
            for (const key of Object.keys(item)) {
                const resultValue = visit(item[key], at + "/" + key.replaceAll("~", "~0").replaceAll("/", "~1"))
                if (resultValue !== undefined) result[key] = resultValue
            }
            return result
        }
        return item
    }
    return { reply: visit(value, ""), hydration }
}

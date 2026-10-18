// Ordinary MIDI callbacks have a small fixed source schema. Rebuild that schema
// after native context acceptance so omitted undefined properties retain order.
export type MidiHydration = { undefinedPaths: string[]; numbers: { path: string; number: unknown }[] }

export function hydrateMidi(command: string, data: any, metadata: MidiHydration): any {
    if (!metadata || !Array.isArray(metadata.undefinedPaths) || !Array.isArray(metadata.numbers) || metadata.numbers.length || metadata.undefinedPaths.length > 256) throw new Error("Invalid native MIDI restoration")
    const paths = new Set(metadata.undefinedPaths)
    if (paths.size !== metadata.undefinedPaths.length || [...paths].some((path) => typeof path !== "string" || path.length > 64)) throw new Error("Invalid native MIDI paths")
    const object = (value: any) => value !== null && typeof value === "object" && !Array.isArray(value)
    const field = (value: any, key: string, path: string) => {
        const missing = !Object.hasOwn(value, key)
        if (missing !== paths.has(path)) throw new Error("Native MIDI property mismatches restoration")
        paths.delete(path)
        return missing ? undefined : value[key]
    }
    if (command === "GET_MIDI_INPUTS" || command === "GET_MIDI_OUTPUTS") {
        if (!Array.isArray(data) || data.length > 256) throw new Error("Invalid native MIDI devices")
        const result = data.map((device, index) => {
            if (!object(device) || Object.keys(device).some((key) => key !== "name")) throw new Error("Invalid native MIDI device")
            const name = field(device, "name", `/${index}/name`)
            if (name !== null && typeof name === "object") throw new Error("Invalid native MIDI name")
            return { name }
        })
        if (paths.size) throw new Error("Unexpected native MIDI path")
        return result
    }
    if (command !== "RECEIVE_MIDI2" || !object(data) || Object.keys(data).some((key) => !["id", "values", "type"].includes(key)) || !Object.hasOwn(data, "values") || !Object.hasOwn(data, "type") || !["noteon", "noteoff", "control"].includes(data.type) || !object(data.values)) throw new Error("Invalid native MIDI callback")
    const keys = data.type === "control" ? ["controller", "value"] : ["note", "velocity"]
    if (Object.keys(data.values).some((key) => ![...keys, "channel"].includes(key)) || !Object.hasOwn(data.values, "channel") || !Number.isInteger(data.values.channel) || data.values.channel < 1 || data.values.channel > 16) throw new Error("Invalid native MIDI values")
    const id = field(data, "id", "/id")
    const values: Record<string, unknown> = {}
    for (const key of keys) {
        const value = field(data.values, key, `/values/${key}`)
        if (value !== undefined && (!Number.isInteger(value) || value < 0 || value > 255)) throw new Error("Invalid native MIDI byte")
        values[key] = value
    }
    values.channel = data.values.channel
    if (paths.size) throw new Error("Unexpected native MIDI path")
    return { id, values, type: data.type }
}

// Only the native Linux Outcome::wire schema, after the bridge accepts context.
export type SpotifyNumberField = "positionSec" | "durationSec" | "volume"
export type SpotifyNumberKind = "NaN" | "PositiveInfinity" | "NegativeInfinity" | "NegativeZero"
export type SpotifyRestoration = {
    undefinedFields: "albumArt"[]
    specialNumbers: { field: SpotifyNumberField; kind: SpotifyNumberKind }[]
}
export type SpotifyState = {
    isPlaying: boolean
    title: string
    artist: string
    positionSec: number
    durationSec: number
    albumArt: string | undefined
    volume: number
    platform: "linux"
}

const stringLimit = 1024 * 1024
const stateKeys = ["isPlaying", "title", "artist", "positionSec", "durationSec", "albumArt", "volume", "platform"] as const
const numberFields = ["positionSec", "durationSec", "volume"] as const
const invalid = (): never => {
    throw new Error("Invalid native Spotify restoration")
}

// Native transport provides JSON records, never getters, prototypes or symbols.
function record(value: unknown, keys: readonly string[], ordered = false): Record<string, unknown> {
    if (!value || typeof value !== "object" || Array.isArray(value)) return invalid()
    const prototype = Object.getPrototypeOf(value)
    if (prototype !== null && prototype !== Object.prototype) return invalid()
    const own = Reflect.ownKeys(value)
    if (own.length !== keys.length) return invalid()
    for (let index = 0; index < own.length; index++) {
        const key = own[index]
        if (typeof key !== "string" || (ordered ? key !== keys[index] : !keys.includes(key))) return invalid()
        const descriptor = Object.getOwnPropertyDescriptor(value, key)!
        if (!descriptor.enumerable || !("value" in descriptor)) return invalid()
    }
    return value as Record<string, unknown>
}

function list(value: unknown, limit: number): unknown[] {
    if (!Array.isArray(value) || value.length > limit) return invalid()
    const keys = Reflect.ownKeys(value)
    if (keys.length !== value.length + 1 || keys.at(-1) !== "length") return invalid()
    for (let index = 0; index < value.length; index++) {
        if (keys[index] !== String(index)) return invalid()
        const descriptor = Object.getOwnPropertyDescriptor(value, keys[index])!
        if (!descriptor.enumerable || !("value" in descriptor)) return invalid()
    }
    return value
}

// Count scalar UTF-8 bytes without allocating encoded/replacement strings.
function text(value: unknown): string {
    if (typeof value !== "string" || !value.length || value.length > stringLimit) return invalid()
    let bytes = 0
    for (let index = 0; index < value.length; index++) {
        const unit = value.charCodeAt(index)
        if (unit >= 0xd800 && unit <= 0xdbff) {
            const next = value.charCodeAt(++index)
            if (!(next >= 0xdc00 && next <= 0xdfff)) return invalid()
            bytes += 4
        } else if (unit >= 0xdc00 && unit <= 0xdfff) return invalid()
        else bytes += unit < 0x80 ? 1 : unit < 0x800 ? 2 : 3
        if (bytes > stringLimit) return invalid()
    }
    return value
}

/** No context authority or source ACK: the caller owns those bridge boundaries. */
export function hydrateSpotify(data: unknown, restoration: unknown): SpotifyState | null {
    const metadata = record(restoration, ["undefinedFields", "specialNumbers"])
    const undefinedFields = list(metadata.undefinedFields, 1)
    const specialNumbers = list(metadata.specialNumbers, 3)
    if (data === null) {
        if (undefinedFields.length || specialNumbers.length) return invalid()
        return null
    }
    const input = record(data, stateKeys, true)
    if (typeof input.isPlaying !== "boolean" || input.platform !== "linux") return invalid()
    const title = text(input.title)
    const artist = text(input.artist)
    const absentArt = undefinedFields.length === 1
    if (absentArt && (undefinedFields[0] !== "albumArt" || input.albumArt !== null)) return invalid()
    const albumArt = absentArt ? undefined : text(input.albumArt)

    const numbers = new Map<SpotifyNumberField, number>()
    let previous = -1
    for (const entry of specialNumbers) {
        const special = record(entry, ["field", "kind"])
        const index = numberFields.indexOf(special.field as SpotifyNumberField)
        if (index <= previous || index === -1 || input[numberFields[index]] !== null) return invalid()
        previous = index
        let value: number
        switch (special.kind) {
            case "NaN":
                value = NaN
                break
            case "PositiveInfinity":
                value = Infinity
                break
            case "NegativeInfinity":
                value = -Infinity
                break
            case "NegativeZero":
                value = -0
                break
            default:
                return invalid()
        }
        numbers.set(numberFields[index], value)
    }
    for (const field of numberFields) {
        if (numbers.has(field)) continue
        const value = input[field]
        if (typeof value !== "number" || !Number.isFinite(value) || Object.is(value, -0)) return invalid()
        numbers.set(field, value)
    }
    // Fresh fixed-order DTO; no mutation or private envelope reaches callbacks.
    return { isPlaying: input.isPlaying, title, artist, positionSec: numbers.get("positionSec")!, durationSec: numbers.get("durationSec")!, albumArt, volume: numbers.get("volume")!, platform: "linux" }
}

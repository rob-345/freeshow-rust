// Test-harness classification of the exact effective source, never an env label.
import { createHash } from "node:crypto"
export const EAGER_SHA256 = "ff000b9ddcd06eb88069a210abe09fb293e75a34e116bc702de0045768146eff"
export const LAZY_SHA256 = "20630ed8e7f025ee664cb0bd7851e2daf5e80ba63c42fe10784d344168e361d1"
export function analyserIdentity(source) {
    if (typeof source !== "string" || Buffer.byteLength(source) > 1048576) throw new Error("Bounded analyser source required")
    const effectiveAnalyserSha256 = createHash("sha256").update(source.replace(/\r\n/g, "\n")).digest("hex")
    const analyserVariant = effectiveAnalyserSha256 === EAGER_SHA256 ? "immutable-eager-control" : effectiveAnalyserSha256 === LAZY_SHA256 ? "candidate-lazy" : undefined
    if (!analyserVariant) throw new Error(`Unsupported effective analyser source: ${effectiveAnalyserSha256}`)
    return { analyserVariant, effectiveAnalyserSha256 }
}

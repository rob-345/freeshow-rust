// Real pinned metadata/formatter oracle and deterministic private fixture writer.
// No Electron nativeImage is substituted: image encoding/pixels are not certified.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import ts from "typescript"
import { parseFile } from "music-metadata"
import { apic, flac, mp3, png } from "../../crates/freeshow-cover-images/tools/fixtures.mjs"

const root = fileURLToPath(new URL("../../", import.meta.url))
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const loaded = spawnSync("git", ["show", `${revision}:src/electron/audio/nowPlaying.ts`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 })
assert.equal(loaded.status, 0, loaded.stderr)
const sourceHash = createHash("sha256").update(loaded.stdout).digest("hex")
assert.equal(sourceHash, "0a203138d9c283b3e5fa37f0aa46cd33df596d8ee7183ac6cd73d3c59cf3bc7c")
const ast = ts.createSourceFile("nowPlaying.ts", loaded.stdout, ts.ScriptTarget.Latest, true)
const declarations = ast.statements.filter((statement) => (ts.isFunctionDeclaration(statement) && ["getAudioMetadata", "getArtist", "convertDynamicValues", "getNowPlayingMediaPath"].includes(statement.name?.text)) || (ts.isVariableStatement(statement) && statement.declarationList.declarations.some((declaration) => ["dynamicValues", "fileNameImage"].includes(declaration.name.getText(ast)))))
assert.equal(declarations.length, 6)
const code = ts.transpileModule(declarations.map((declaration) => declaration.getText(ast).replace(/^export /, "")).join("\n") + ";return {getAudioMetadata,convertDynamicValues}", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
const parseVersion = JSON.parse(fs.readFileSync(path.join(root, "node_modules/music-metadata/package.json"), "utf8")).version
assert.equal(parseVersion, "7.14.0")
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex")
function raw(value, depth = 0) {
    assert.ok(depth <= 16)
    if (value === undefined) return ["undefined"]
    if (value === null) return ["null"]
    if (ArrayBuffer.isView(value)) return ["bytes", Buffer.from(value.buffer, value.byteOffset, value.byteLength).toString("base64")]
    if (typeof value === "number") return ["number", Object.is(value, -0) ? "-0" : Number.isFinite(value) ? value : String(value)]
    if (Array.isArray(value)) return ["array", value.map((item) => raw(item, depth + 1))]
    if (typeof value === "object") return ["object", Object.keys(value).map((key) => [key, raw(value[key], depth + 1)])]
    return [typeof value, value]
}
const durationKinds = { NegativeZero: -0, NaN, PositiveInfinity: Infinity, NegativeInfinity: -Infinity }
export function restoreData(entry) {
    assert.ok(entry && typeof entry.path === "string" && path.isAbsolute(entry.path) && entry.path.length <= 32768)
    const stat = fs.statSync(entry.path)
    assert.ok(stat.isFile() && stat.size <= 16384, "Oracle reads only bounded controlled fixture files")
    const data = structuredClone(entry.data)
    data.filePath = entry.path
    if (entry.durationKind !== undefined) {
        assert.ok(Object.hasOwn(durationKinds, entry.durationKind))
        data.duration = durationKinds[entry.durationKind]
    }
    return data
}

export async function replay(script) {
    assert.ok(script && ["set", "unset"].includes(script.action))
    assert.ok(typeof script.audioFolder === "string" && path.isAbsolute(script.audioFolder) && script.audioFolder.length <= 32768)
    if (script.action === "unset") return { action: "unset", text: "", coverExpectedAbsent: true, sourceVoid: true, imageEncodingVerified: false }
    const entry = script.case,
        sent = [],
        parseErrors = []
    const source = new Function("parseFile", "sendToMain", "ToMain", "console", "getDataFolderPath", "join", code)(
        parseFile,
        (channel, data) => sent.push({ channel, data }),
        { AUDIO_METADATA: "AUDIO_METADATA" },
        { error: (...values) => parseErrors.push(values.map(String)) },
        (kind) => {
            assert.equal(kind, "audio")
            return script.audioFolder
        },
        path.join
    )
    const data = restoreData(entry)
    const common = await source.getAudioMetadata(data.filePath)
    if (common) {
        assert.equal(sent.length, 1)
        assert.equal(sent[0].channel, "AUDIO_METADATA")
        assert.equal(sent[0].data.metadata, common)
        assert.equal(sent[0].data.filePath, entry.path)
    } else assert.equal(sent.length, 0)
    const pictures = common?.picture || []
    const metadataRaw = common ? raw(common) : null
    const imageUnsupported = entry.imageCapability === "unsupported"
    // The genuine source formatter consumes the original first cover bytes for
    // the verified controlled small-PNG case. This does not execute nativeImage
    // or predict Electron PNG recompression. Unsupported images are metadata-only.
    const coverBuffer = entry.imageCapability === "small-png" ? pictures[0]?.data : undefined
    const text = imageUnsupported ? null : await source.convertDynamicValues(data, common, coverBuffer)
    return {
        action: "set",
        id: entry.id,
        filePath: entry.path,
        metadataRaw,
        metadataExpected: !!common,
        pictures: pictures.map((picture) => ({ keys: Object.keys(picture), undefinedFields: Object.keys(picture).filter((key) => picture[key] === undefined), base64: picture.data.toString("base64"), sha256: hash(picture.data) })),
        text,
        parseErrors,
        duration: raw(restoreData(entry).duration),
        inputSha256: hash(fs.readFileSync(entry.path)),
        coverExpectedAbsent: pictures.length === 0,
        imageCapability: entry.imageCapability || "no-cover",
        imageEncodingVerified: false,
        imagePixelsVerified: false,
        ...(imageUnsupported ? { formattingSkipped: "Native image capability failure must remain explicit after metadata; no fake empty cover/text" } : {})
    }
}

function taggedWav() {
    const u32 = (n) => {
        const out = Buffer.alloc(4)
        out.writeUInt32LE(n)
        return out
    }
    const chunk = (key, bytes) => Buffer.concat([Buffer.from(key), u32(bytes.length), bytes, ...(bytes.length % 2 ? [Buffer.alloc(1)] : [])])
    const tags = [
        ["INAM", "Snapshot title"],
        ["IART", "First artist"],
        ["IPRD", "Native album"],
        ["YEAR", "invalid"]
    ]
    const bytes = Buffer.concat([Buffer.from("WAVE"), chunk("fmt ", Buffer.from([1, 0, 1, 0, 64, 31, 0, 0, 128, 62, 0, 0, 2, 0, 16, 0])), chunk("LIST", Buffer.concat([Buffer.from("INFO"), ...tags.map(([key, value]) => chunk(key, Buffer.from(value + "\0")))])), chunk("data", Buffer.alloc(160))])
    return Buffer.concat([Buffer.from("RIFF"), u32(bytes.length), bytes])
}
export async function writeFixtures(directory) {
    assert.ok(path.isAbsolute(directory))
    fs.mkdirSync(directory, { recursive: true })
    const image = png(1, 1, { solid: true })
    const cases = [
        ["wav-nan", "tagged 音楽.wav", taggedWav(), { durationKind: "PositiveInfinity", expectedYearNaN: true, imageCapability: "no-cover" }],
        ["flac-empty", "no cover.flac", flac([]), { imageCapability: "no-cover" }],
        [
            "png-cover",
            "cover 🎹.mp3",
            mp3([apic(image, { type: 255, description: "Undefined kind" }), apic(image, { type: 4, description: "Back" })], 4, [
                ["TIT2", Buffer.from([3, ...Buffer.from("PNG title")])],
                ["TCON", Buffer.from([3, ...Buffer.from("9999")])]
            ]),
            { imageCapability: "small-png", expectedPictures: 2, expectedUndefinedPictureType: true }
        ],
        ["image-unsupported", "invalid image.mp3", mp3([apic(Buffer.from([1, 2, 3]), { type: 255 })]), { imageCapability: "unsupported", expectedPictures: 1, expectedUndefinedPictureType: true, expectErrorCodes: ["cover_unsupported"] }]
    ].map(([id, name, bytes, options]) => {
        assert.ok(bytes.length < 16384)
        const filePath = path.join(directory, name)
        fs.writeFileSync(filePath, bytes, { flag: "wx" })
        return { id, path: filePath, expectMetadata: true, data: { filePath, name: "Snapshot name", unknownLang: ["Fallback artist", "Fallback title", "Fallback album"], format: "{artist}|{title}|{album}|{year}|{duration}|{duration_s}|{artwork_path}|{artwork_base64}", duration: 61.25 }, ...options, inputSha256: hash(bytes) }
    })
    // Genuine parser evidence at fixture creation; unknown covers are never
    // synthesized. Native application grants remain the coordinator's job.
    for (const entry of cases) {
        const metadata = await parseFile(entry.path)
        if (entry.expectedYearNaN) assert.ok(Number.isNaN(metadata.common.year))
        if (entry.expectedPictures !== undefined) assert.equal(metadata.common.picture.length, entry.expectedPictures)
        if (entry.expectedUndefinedPictureType) assert.ok(Object.hasOwn(metadata.common.picture[0], "type") && metadata.common.picture[0].type === undefined)
    }
    return { cases, image: { width: 1, height: 1, rgba: [255, 0, 0, 255], originalBase64: image.toString("base64"), sha256: hash(image), nativeEncodedByteParityClaimed: false } }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    const deadline = setTimeout(() => process.exit(124), 15000)
    try {
        const [mode, argument] = process.argv.slice(2)
        let result
        if (mode === "--fixtures") result = await writeFixtures(path.resolve(argument))
        else if (mode === "--replay") {
            const stat = fs.statSync(argument)
            assert.ok(stat.size <= 131072)
            result = await replay(JSON.parse(fs.readFileSync(argument, "utf8")))
        } else throw new Error("Use --fixtures PRIVATE_DIRECTORY or --replay SCRIPT_JSON")
        process.stdout.write(JSON.stringify({ revision, sourceHash, parseVersion, node: process.version, ...result }) + "\n")
    } finally {
        clearTimeout(deadline)
    }
}

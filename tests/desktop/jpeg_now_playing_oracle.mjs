// CI replay uses a bounded actual packaged recording; only --record-original
// launches Electron. No nativeImage substitute or JPEG decoder is used in Node.
import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { fileURLToPath } from "node:url"
import { createHash } from "node:crypto"
import { execFileSync } from "node:child_process"
import { createRequire } from "node:module"
import { deflateSync, inflateSync } from "node:zlib"
import ts from "typescript"
import { parseFile } from "music-metadata"
import { apic, comments, crc, flac, mp3, picture, png } from "../../crates/freeshow-cover-images/tools/fixtures.mjs"

const root = fileURLToPath(new URL("../../", import.meta.url)),
    revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975",
    require = createRequire(import.meta.url),
    hash = (bytes) => createHash("sha256").update(bytes).digest("hex"),
    imageReceiptPath = path.join(root, "crates/freeshow-cover-images/fixtures/jpeg/electron-linux.json"),
    recordingPath = fileURLToPath(new URL("./jpeg_now_playing_original.json", import.meta.url)),
    source = execFileSync("git", ["show", `${revision}:src/electron/audio/nowPlaying.ts`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 }).replace(/\r\n/g, "\n"),
    sourceHash = hash(source),
    audioToken = "${AUDIO_FOLDER}/NowPlayingCover.png",
    parameters = ["nativeImage", "parseFile", "sendToMain", "ToMain", "console", "getDataFolderPath", "join", "IcecastSender", "doesPathExist", "deleteFile", "writeFile"],
    durationKinds = { NegativeZero: -0, NaN, PositiveInfinity: Infinity, NegativeInfinity: -Infinity }
assert.equal(sourceHash, "0a203138d9c283b3e5fa37f0aa46cd33df596d8ee7183ac6cd73d3c59cf3bc7c")
const ast = ts.createSourceFile("nowPlaying.ts", source, ts.ScriptTarget.Latest, true),
    functions = ["setPlayingState", "unsetPlayingAudio", "getAudioMetadata", "getArtist", "convertDynamicValues", "getNowPlayingMediaPath", "getNowPlayingTextPath"],
    variables = ["fileNameText", "fileNameImage", "dynamicValues"],
    declarations = ast.statements.filter((node) => (ts.isFunctionDeclaration(node) && functions.includes(node.name?.text)) || (ts.isVariableStatement(node) && node.declarationList.declarations.some((item) => variables.includes(item.name.getText(ast))))).map((node) => node.getText(ast)),
    declarationHash = hash(declarations.join("\n")),
    javascript = ts.transpileModule(declarations.map((text) => text.replace(/^export /, "")).join("\n") + ";return {setPlayingState,unsetPlayingAudio,getAudioMetadata,convertDynamicValues}", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
assert.equal(declarations.length, functions.length + variables.length)
const imageReceiptText = fs.readFileSync(imageReceiptPath, "utf8").replace(/\r\n/g, "\n"),
    imageReceipt = JSON.parse(imageReceiptText),
    parserDirectory = path.dirname(require.resolve("music-metadata/package.json")),
    parserVersion = JSON.parse(fs.readFileSync(path.join(parserDirectory, "package.json"))).version
assert.equal(parserVersion, "7.14.0")
assert.equal(imageReceipt.sourceCommit, revision)
assert.equal(imageReceipt.sourceSha256, sourceHash)
assert.equal(imageReceipt.executableSha256, "eb3263aad96ce7c35df3db4516bb04c67bdc6c2f30ae8ebc0cb5649783c82a15")

function packageHash() {
    const rows = []
    let aggregate = 0
    function visit(relative) {
        assert.ok(rows.length < 256)
        const file = path.join(parserDirectory, relative),
            stat = fs.lstatSync(file)
        assert.ok(!stat.isSymbolicLink())
        if (stat.isDirectory()) for (const name of fs.readdirSync(file).sort()) visit(path.posix.join(relative, name))
        else {
            assert.ok(stat.isFile())
            aggregate += stat.size
            assert.ok(aggregate <= 4 * 1024 * 1024)
            rows.push([relative, hash(fs.readFileSync(file))])
        }
    }
    visit("package.json")
    visit("lib")
    return hash(JSON.stringify(rows))
}
const parserHash = packageHash()
const provenance = () => ({ revision, sourceHash, declarationHash, parserVersion, parserHash, imageReceiptHash: hash(imageReceiptText) })

export function raw(value, depth = 0) {
    assert.ok(depth <= 16)
    if (value === undefined) return ["undefined"]
    if (value === null) return ["null"]
    if (ArrayBuffer.isView(value)) return ["bytes", Buffer.from(value.buffer, value.byteOffset, value.byteLength).toString("base64")]
    if (typeof value === "number") return ["number", Object.is(value, -0) ? "-0" : Number.isFinite(value) ? value : String(value)]
    if (Array.isArray(value)) return ["array", value.map((item) => raw(item, depth + 1))]
    if (typeof value === "object") return ["object", Object.keys(value).map((key) => [key, raw(value[key], depth + 1)])]
    assert.ok(["string", "boolean"].includes(typeof value))
    return [typeof value, value]
}
function selectedImage(name) {
    const entry = imageReceipt.fixtures.find((item) => item.name === name)
    assert.ok(entry)
    const bytes = Buffer.from(entry.input.base64, "base64"),
        rgba = inflateSync(Buffer.from(entry.source.rgba.deflateBase64, "base64"), { maxOutputLength: 16384 })
    assert.equal(bytes.length, entry.input.length)
    assert.equal(hash(bytes), entry.input.sha256)
    assert.equal(hash(rgba), entry.source.rgba.sha256)
    assert.equal(rgba.length, entry.source.size.width * entry.source.size.height * 4)
    assert.equal(entry.source.cover.original, true, "small originals only: no unverified resize")
    return { entry, bytes, rgba }
}
const texture = selectedImage("texture-17x11-no-icc"),
    gray = selectedImage("native-encoded-gray"),
    icc = selectedImage("texture-64x48")
const tagged = (value) => Buffer.concat([Buffer.from([3]), Buffer.from(value)])
function specs() {
    return [
        {
            id: "jpeg-apic",
            name: "JPEG APIC 音楽.mp3",
            bytes: mp3([apic(texture.bytes, { mime: "image/jpeg", type: 255, description: "Unknown kind 音楽 🎵" }), apic(gray.bytes, { mime: "image/jpeg", type: 4, description: "Back café" })], 4, [
                ["TIT2", tagged("JPEG title 敬拜")],
                ["TPE1", tagged(" Artist α \0 Artist β ")],
                ["TALB", tagged("JPEG album")],
                ["TDRC", tagged("invalid")]
            ]),
            image: texture,
            durationKind: "NegativeZero",
            expectedPictures: 2,
            expectedUndefinedPictureType: true
        },
        {
            id: "jpeg-flac",
            name: "JPEG PICTURE Ελληνικά.flac",
            bytes: flac(
                [picture(icc.bytes, { mime: "image/jpeg", type: 3, width: 64, height: 48 })],
                [
                    [
                        4,
                        comments([
                            ["ARTIST", " Artist α "],
                            ["ARTIST", " Artist β "],
                            ["ALBUM", "FLAC JPEG album"],
                            ["DATE", "invalid"]
                        ])
                    ]
                ]
            ),
            image: icc,
            durationKind: "PositiveInfinity",
            expectedPictures: 1
        }
    ]
}
function dataFor(filePath) {
    return { filePath, name: "Snapshot JPEG name", unknownLang: ["Fallback artist", "Fallback title", "Fallback album"], format: "{artist}|{title}|{album}|{year}|{duration}|{duration_s}|{artwork_path}|{artwork_base64}", duration: 61.25 }
}
function pathInput(filePath) {
    assert.ok(typeof filePath === "string" && path.isAbsolute(filePath) && Buffer.byteLength(filePath) <= 32768 && !filePath.includes("\0"))
    return filePath
}
function boundedFile(filePath, maximum) {
    pathInput(filePath)
    const stat = fs.lstatSync(filePath)
    assert.ok(stat.isFile() && stat.size <= maximum, "only bounded regular controlled files")
    return fs.readFileSync(filePath)
}
function restore(entry) {
    const spec = specs().find((item) => item.id === entry?.id)
    assert.ok(spec, "known fixture id")
    assert.equal(path.basename(pathInput(entry.path)), spec.name)
    const bytes = boundedFile(entry.path, 16384)
    assert.equal(hash(bytes), hash(spec.bytes), "exact deterministic tagged input")
    assert.equal(entry.inputSha256, hash(bytes))
    assert.deepEqual(entry.data, dataFor(entry.path), "only the recorded original command is supported")
    assert.equal(entry.durationKind, spec.durationKind)
    const data = structuredClone(entry.data)
    data.duration = durationKinds[entry.durationKind]
    return { data, spec }
}
export async function writeFixtures(directory) {
    pathInput(directory)
    fs.mkdirSync(directory, { recursive: true })
    const cases = []
    for (const spec of specs()) {
        const filePath = path.join(directory, spec.name)
        assert.ok(spec.bytes.length < 16384)
        fs.writeFileSync(filePath, spec.bytes, { flag: "wx" })
        const parsed = await parseFile(filePath)
        assert.equal(parsed.common.picture.length, spec.expectedPictures)
        assert.equal(hash(parsed.common.picture[0].data), spec.image.entry.input.sha256)
        if (spec.expectedUndefinedPictureType) assert.ok(Object.hasOwn(parsed.common.picture[0], "type") && parsed.common.picture[0].type === undefined)
        cases.push({
            id: spec.id,
            path: filePath,
            data: dataFor(filePath),
            durationKind: spec.durationKind,
            inputSha256: hash(spec.bytes),
            imageCapability: "small-jpeg",
            expectMetadata: true,
            expectedPictures: spec.expectedPictures,
            ...(spec.expectedUndefinedPictureType ? { expectedUndefinedPictureType: true } : {}),
            sourceImage: { fixture: spec.image.entry.name, width: spec.image.entry.source.size.width, height: spec.image.entry.source.size.height, rgbaSha256: spec.image.entry.source.rgba.sha256 }
        })
    }
    return { ...provenance(), cases, originalRecordingRequiredForReplay: true, rootPicturePositionCompared: false }
}

// Independent PNG framing/CRC/inflate/filter validation for the actual file
// written by the GUI. It does not decode JPEG or model nativeImage.
export function decodePng(bytes) {
    assert.ok(bytes.length <= 4 * 1024 * 1024)
    assert.deepEqual(bytes.subarray(0, 8), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))
    let offset = 8,
        width,
        height,
        channels,
        ended = false,
        idatEnded = false,
        count = 0
    const chunks = []
    while (offset < bytes.length) {
        assert.ok(++count <= 64 && offset + 12 <= bytes.length)
        const length = bytes.readUInt32BE(offset),
            end = offset + length + 12
        assert.ok(end <= bytes.length)
        const name = bytes.subarray(offset + 4, offset + 8).toString("ascii"),
            data = bytes.subarray(offset + 8, end - 4)
        assert.equal(crc(bytes.subarray(offset + 4, end - 4)), bytes.readUInt32BE(end - 4), "PNG CRC")
        if (name === "IHDR") {
            assert.equal(offset, 8)
            assert.equal(length, 13)
            width = data.readUInt32BE(0)
            height = data.readUInt32BE(4)
            assert.ok(width > 0 && width <= 64 && height > 0 && height <= 48, "desktop fixture dimensions only")
            assert.equal(data[8], 8)
            assert.ok(data[9] === 2 || data[9] === 6)
            assert.deepEqual(data.subarray(10), Buffer.alloc(3))
            channels = data[9] === 2 ? 3 : 4
        } else if (name === "IDAT") {
            assert.ok(width && !idatEnded)
            chunks.push(data)
        } else {
            if (chunks.length) idatEnded = true
            if (name === "IEND") {
                assert.equal(length, 0)
                ended = true
            } else assert.ok(["sRGB", "iCCP", "gAMA", "cHRM", "pHYs", "tEXt"].includes(name), "unsupported PNG chunk " + name)
        }
        offset = end
        if (ended) break
    }
    assert.ok(ended && offset === bytes.length && chunks.length > 0)
    const compressed = Buffer.concat(chunks),
        stride = width * channels,
        expected = (stride + 1) * height,
        decoded = inflateSync(compressed, { maxOutputLength: expected + 1, info: true })
    assert.equal(decoded.engine.bytesWritten, compressed.length)
    assert.equal(decoded.buffer.length, expected)
    const raster = Buffer.alloc(width * height * channels),
        rgba = Buffer.alloc(width * height * 4, 255)
    for (let y = 0; y < height; y++) {
        const filter = decoded.buffer[y * (stride + 1)]
        assert.ok(filter <= 4)
        for (let x = 0; x < stride; x++) {
            const p = y * stride + x,
                left = x < channels ? 0 : raster[p - channels],
                above = y === 0 ? 0 : raster[p - stride],
                corner = x < channels || y === 0 ? 0 : raster[p - stride - channels]
            let prediction = 0
            if (filter === 1) prediction = left
            if (filter === 2) prediction = above
            if (filter === 3) prediction = Math.floor((left + above) / 2)
            if (filter === 4) {
                const sum = left + above - corner,
                    a = Math.abs(sum - left),
                    b = Math.abs(sum - above),
                    c = Math.abs(sum - corner)
                prediction = a <= b && a <= c ? left : b <= c ? above : corner
            }
            raster[p] = (decoded.buffer[y * (stride + 1) + x + 1] + prediction) & 255
        }
    }
    for (let p = 0, q = 0; p < raster.length; p += channels, q += 4) raster.copy(rgba, q, p, p + channels)
    return { width, height, rgba }
}

function loadRecording() {
    const text = boundedFile(recordingPath, 262144).toString("utf8").replace(/\r\n/g, "\n"),
        recording = JSON.parse(text)
    assert.equal(recording.schema, 1)
    assert.deepEqual(recording.provenance, provenance())
    assert.equal(recording.recorderHash, hash(fs.readFileSync(fileURLToPath(import.meta.url), "utf8").replace(/\r\n/g, "\n")))
    assert.equal(recording.executableSha256, imageReceipt.executableSha256)
    assert.equal(recording.runtime.version, "1.6.7-beta.2")
    assert.equal(recording.runtime.packaged, true)
    assert.equal(recording.runtime.versions.electron, "37.10.3")
    assert.equal(recording.runtime.versions.node, "22.21.1")
    assert.equal(recording.platform, "linux")
    assert.equal(recording.arch, "x64")
    assert.equal(recording.cases.length, 2)
    for (const [index, spec] of specs().entries()) {
        const entry = recording.cases[index]
        assert.equal(entry.id, spec.id)
        assert.deepEqual(entry.input, { name: spec.name, length: spec.bytes.length, sha256: hash(spec.bytes) })
        assert.equal(entry.durationRaw[1], raw(durationKinds[spec.durationKind])[1])
        assert.equal(entry.set.sourceVoid, true)
        assert.equal(entry.unset.sourceVoid, true)
        assert.deepEqual(entry.unset.events, [])
        assert.equal(entry.unset.text, "")
        assert.equal(entry.unset.coverExists, false)
        const bytes = Buffer.from(entry.set.cover.png.base64, "base64"),
            decoded = decodePng(bytes)
        assert.equal(bytes.length, entry.set.cover.png.length)
        assert.equal(hash(bytes), entry.set.cover.png.sha256)
        assert.equal(hash(decoded.rgba), spec.image.entry.source.rgba.sha256)
        assert.equal(entry.set.cover.rgbaSha256, hash(decoded.rgba))
        assert.deepEqual([decoded.width, decoded.height], [spec.image.entry.source.size.width, spec.image.entry.source.size.height])
        assert.ok(entry.set.text.includes(audioToken) && entry.set.text.split(audioToken).length === 2)
        assert.ok(entry.set.text.endsWith("data:image/jpeg;base64," + spec.image.bytes.toString("base64")))
    }
    return { recording, recordingHash: hash(text) }
}
function validateScript(script) {
    assert.ok(script && ["set", "unset"].includes(script.action))
    pathInput(script.audioFolder)
    assert.equal(path.basename(script.audioFolder), "Audio")
}
function replayUnset(original) {
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-jpeg-unset-")),
        audioFolder = path.join(temporary, "Audio"),
        coverPath = path.join(audioFolder, "NowPlayingCover.png"),
        textPath = path.join(audioFolder, "NowPlaying.txt"),
        effects = [],
        events = []
    try {
        fs.mkdirSync(audioFolder)
        fs.writeFileSync(coverPath, "old cover")
        fs.writeFileSync(textPath, "old text")
        const target = (file) => {
            assert.ok(file === coverPath || file === textPath)
            return file === coverPath ? "cover" : "text"
        }
        const api = new Function(...parameters, javascript)(
            undefined,
            parseFile,
            (...args) => events.push(args),
            {},
            console,
            (kind) => {
                assert.equal(kind, "audio")
                effects.push({ kind: "audio-folder" })
                return audioFolder
            },
            path.join,
            { updateMetadata: (value) => effects.push({ kind: "title", value }) },
            (file) => {
                effects.push({ kind: "exists", target: target(file) })
                return fs.existsSync(file)
            },
            (file) => {
                effects.push({ kind: "delete", target: target(file) })
                fs.unlinkSync(file)
            },
            (file, bytes) => {
                effects.push({ kind: "write", target: target(file) })
                fs.writeFileSync(file, bytes)
            }
        )
        const returned = api.unsetPlayingAudio(),
            result = { sourceVoid: returned === undefined, events, effects, text: fs.readFileSync(textPath, "utf8"), coverExists: fs.existsSync(coverPath) }
        assert.deepEqual(result, original, "actual current source UNSET and real controlled files vs packaged source recording")
        return result
    } finally {
        fs.rmSync(temporary, { recursive: true, force: true })
    }
}
export async function replay(script) {
    validateScript(script)
    const { recording, recordingHash } = loadRecording()
    if (script.action === "unset") return { ...provenance(), action: "unset", text: "", coverExpectedAbsent: true, sourceVoid: true, sourceUnset: "actual current original function, real controlled files, checked against packaged recording", originalUnset: replayUnset(recording.cases[0].unset), recordingHash, imageEncodingVerified: false, imagePixelsVerified: false, nativeExecutionClaimed: false }
    const { data, spec } = restore(script.case),
        original = recording.cases.find((entry) => entry.id === spec.id),
        events = [],
        errors = []
    const api = new Function(...parameters, javascript)(
        undefined,
        parseFile,
        (channel, sent) => events.push({ channel, sent }),
        { AUDIO_METADATA: "AUDIO_METADATA" },
        { error: (...values) => errors.push(values.map(String)) },
        (kind) => {
            assert.equal(kind, "audio")
            return script.audioFolder
        },
        path.join
    )
    const metadata = await api.getAudioMetadata(data.filePath)
    assert.ok(metadata)
    assert.deepEqual(errors, [])
    assert.equal(events.length, 1)
    assert.equal(events[0].channel, "AUDIO_METADATA")
    assert.equal(events[0].sent.filePath, script.case.path)
    assert.equal(events[0].sent.metadata, metadata)
    assert.deepEqual(raw(metadata), original.metadataRaw, "current real original parser vs packaged source event")
    assert.deepEqual(raw(data.duration), original.durationRaw)
    // Recorded actual setPlayingState proves this bounded original first JPEG
    // is selected, unchanged, for artwork_base64. NativeImage is never mocked.
    assert.equal(hash(metadata.picture[0].data), spec.image.entry.input.sha256)
    const text = await api.convertDynamicValues(data, metadata, metadata.picture[0].data),
        expectedText = original.set.text.replace(audioToken, path.join(script.audioFolder, "NowPlayingCover.png"))
    assert.equal(text, expectedText, "actual source formatter vs canonical packaged setPlayingState result")
    return {
        ...provenance(),
        action: "set",
        id: spec.id,
        filePath: script.case.path,
        metadataRaw: raw(metadata),
        metadataExpected: true,
        pictures: metadata.picture.map((item) => ({ keys: Object.keys(item), undefinedFields: Object.keys(item).filter((key) => item[key] === undefined), base64: item.data.toString("base64"), sha256: hash(item.data) })),
        text,
        inputSha256: hash(spec.bytes),
        duration: original.durationRaw,
        coverExpectedAbsent: false,
        cover: { width: spec.image.entry.source.size.width, height: spec.image.entry.source.size.height, rgbaSha256: spec.image.entry.source.rgba.sha256, sourcePngSha256: original.set.cover.png.sha256 },
        originalSet: original.set,
        recordingHash,
        parserAndFormatterExecuted: true,
        sourceSetPlayingState: "actual packaged recording",
        imagePixelsEvidence: "actual packaged recording, checked against earlier immutable image receipt",
        nativeImageExecutedInReplay: false,
        rootPicturePositionCompared: false,
        imageEncodedParityClaimed: false,
        nativeExecutionClaimed: false
    }
}
export async function verifyFiles(script) {
    const expected = await replay(script),
        textPath = path.join(script.audioFolder, "NowPlaying.txt"),
        coverPath = path.join(script.audioFolder, "NowPlayingCover.png"),
        text = boundedFile(textPath, 65536).toString("utf8")
    assert.equal(text, expected.text)
    if (script.action === "unset") {
        assert.equal(fs.existsSync(coverPath), false)
        return { action: "unset", text: "", coverExists: false, actualFilesystemRead: true, recordingHash: expected.recordingHash }
    }
    const encoded = boundedFile(coverPath, 4 * 1024 * 1024),
        decoded = decodePng(encoded)
    assert.deepEqual([decoded.width, decoded.height], [expected.cover.width, expected.cover.height])
    assert.equal(hash(decoded.rgba), expected.cover.rgbaSha256, "actual filesystem PNG pixels vs actual original JPEG→PNG")
    return { action: "set", text, coverExists: true, width: decoded.width, height: decoded.height, rgbaSha256: hash(decoded.rgba), pngSha256: hash(encoded), pngBytes: encoded.length, actualFilesystemRead: true, exactSourceRgbaVerified: true, encodedPngBytesEqualObserved: hash(encoded) === expected.cover.sourcePngSha256, encodedPngParityClaimed: false, recordingHash: expected.recordingHash }
}

export async function recordOriginal(output) {
    assert.equal(process.platform, "linux")
    assert.equal(process.arch, "x64")
    assert.equal(process.versions.node, "22.21.1")
    pathInput(output)
    assert.ok(!fs.existsSync(output), "recording is create-only")
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-jpeg-recording-"))
    try {
        const fixtures = await writeFixtures(path.join(temporary, "inputs")),
            audioFolder = path.join(temporary, "Audio")
        fs.mkdirSync(audioFolder)
        const { withElectron } = await import("../../crates/freeshow-files/tools/electron.mjs")
        const result = await withElectron(async (app, runtime, executable) => {
            assert.equal(hash(fs.readFileSync(executable)), imageReceipt.executableSha256)
            const cases = await app.evaluate(
                async ({ nativeImage }, { fixtures, javascript, parameters, audioFolder, parserModule, parserPackage, rawSource, audioToken }) => {
                    const require = process.getBuiltinModule("module").createRequire(parserModule),
                        assert = require("node:assert/strict"),
                        fs = require("node:fs"),
                        path = require("node:path"),
                        sha = (bytes) => require("node:crypto").createHash("sha256").update(bytes).digest("hex"),
                        raw = new Function("assert", "return (" + rawSource + ")")(assert),
                        parseFile = require(parserModule).parseFile
                    assert.equal(require(parserPackage).version, "7.14.0")
                    const coverPath = path.join(audioFolder, "NowPlayingCover.png"),
                        textPath = path.join(audioFolder, "NowPlaying.txt"),
                        replies = [],
                        allowed = (file) => {
                            assert.ok(file === coverPath || file === textPath)
                            return file === coverPath ? "cover" : "text"
                        }
                    let effects = [],
                        events = [],
                        errors = []
                    const api = new Function(...parameters, javascript)(
                        nativeImage,
                        parseFile,
                        (channel, data) => {
                            effects.push({ kind: "metadata" })
                            events.push({ channel, dataKeys: Object.keys(data), filePath: data.filePath, metadataRaw: raw(data.metadata) })
                        },
                        { AUDIO_METADATA: "AUDIO_METADATA" },
                        { error: (...values) => errors.push(values.map(String)) },
                        (kind) => {
                            assert.equal(kind, "audio")
                            effects.push({ kind: "audio-folder" })
                            return audioFolder
                        },
                        path.join,
                        { updateMetadata: (value) => effects.push({ kind: "title", value }) },
                        (file) => {
                            effects.push({ kind: "exists", target: allowed(file) })
                            return fs.existsSync(file)
                        },
                        (file) => {
                            effects.push({ kind: "delete", target: allowed(file) })
                            fs.unlinkSync(file)
                        },
                        (file, bytes) => {
                            effects.push({ kind: "write", target: allowed(file) })
                            assert.ok(Buffer.isBuffer(bytes) || typeof bytes === "string")
                            assert.ok(Buffer.byteLength(bytes) <= 65536)
                            fs.writeFileSync(file, bytes)
                        }
                    )
                    for (const fixture of fixtures) {
                        effects = []
                        events = []
                        errors = []
                        fs.writeFileSync(coverPath, "old cover")
                        fs.writeFileSync(textPath, "old text to verify replacement and truncation".repeat(256))
                        const data = structuredClone(fixture.data)
                        data.duration = fixture.durationKind === "NegativeZero" ? -0 : Infinity
                        const durationRaw = raw(data.duration),
                            returned = await api.setPlayingState(data)
                        assert.equal(returned, undefined)
                        assert.deepEqual(errors, [])
                        assert.equal(events.length, 1)
                        assert.equal(events[0].channel, "AUDIO_METADATA")
                        assert.equal(events[0].filePath, fixture.path)
                        const encoded = fs.readFileSync(coverPath),
                            image = nativeImage.createFromBuffer(encoded),
                            size = image.getSize(),
                            bgra = image.toBitmap(),
                            rgba = Buffer.from(bgra),
                            text = fs.readFileSync(textPath, "utf8")
                        assert.equal(bgra.length, size.width * size.height * 4)
                        for (let p = 0; p < rgba.length; p += 4) {
                            rgba[p] = bgra[p + 2]
                            rgba[p + 2] = bgra[p]
                        }
                        assert.equal(text.split(coverPath).length, 2)
                        const set = { sourceVoid: returned === undefined, events: events.map(({ filePath, ...event }) => ({ ...event, filePathProjection: "input" })), effects, text: text.replace(coverPath, audioToken), cover: { width: size.width, height: size.height, rgbaSha256: sha(rgba), png: { base64: encoded.toString("base64"), length: encoded.length, sha256: sha(encoded) } } }
                        effects = []
                        events = []
                        errors = []
                        const unsetReturn = api.unsetPlayingAudio()
                        assert.equal(unsetReturn, undefined)
                        assert.deepEqual(errors, [])
                        replies.push({ id: fixture.id, metadataRaw: set.events[0].metadataRaw, durationRaw, set, unset: { sourceVoid: unsetReturn === undefined, events, effects, text: fs.readFileSync(textPath, "utf8"), coverExists: fs.existsSync(coverPath) } })
                    }
                    return replies
                },
                { fixtures: fixtures.cases, javascript, parameters, audioFolder, parserModule: require.resolve("music-metadata"), parserPackage: require.resolve("music-metadata/package.json"), rawSource: raw.toString(), audioToken }
            )
            return { runtime: { version: runtime.version, packaged: runtime.packaged, versions: runtime.versions }, executableSha256: hash(fs.readFileSync(executable)), cases }
        })
        result.cases.forEach((entry, index) => {
            const spec = specs()[index],
                decoded = decodePng(Buffer.from(entry.set.cover.png.base64, "base64"))
            assert.equal(entry.set.cover.rgbaSha256, spec.image.entry.source.rgba.sha256)
            assert.deepEqual(decoded.rgba, spec.image.rgba)
            entry.input = { name: spec.name, length: spec.bytes.length, sha256: hash(spec.bytes) }
        })
        const recording = { schema: 1, provenance: provenance(), recorderHash: hash(fs.readFileSync(fileURLToPath(import.meta.url), "utf8").replace(/\r\n/g, "\n")), platform: process.platform, arch: process.arch, ...result }
        fs.writeFileSync(output, JSON.stringify(recording, null, 2) + "\n", { flag: "wx" })
        return { recorded: output, bytes: fs.statSync(output).size, sha256: hash(fs.readFileSync(output)), cases: recording.cases.map((entry) => entry.id), actualPackagedSourceExecuted: true }
    } finally {
        fs.rmSync(temporary, { recursive: true, force: true })
    }
}

async function selfTest() {
    const temporary = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-jpeg-oracle-test-")),
        checks = []
    try {
        const fixtures = await writeFixtures(path.join(temporary, "inputs"))
        assert.equal(fixtures.cases.length, 2)
        checks.push("two bounded deterministic real MP3/FLAC containers and genuine music-metadata pictures")
        for (const entry of fixtures.cases) {
            const { data } = restore(entry)
            assert.ok(Object.is(data.duration, entry.durationKind === "NegativeZero" ? -0 : Infinity))
            assert.throws(() => restore({ ...entry, data: { ...entry.data, name: "unrecorded" } }))
            assert.throws(() => restore({ ...entry, inputSha256: "0".repeat(64) }))
        }
        checks.push("send-time data and exact input identity; IEEE duration retained, unrelated variants rejected")
        assert.throws(() => validateScript({ action: "open", audioFolder: path.join(temporary, "Audio") }))
        assert.throws(() => validateScript({ action: "set", audioFolder: temporary }))
        assert.throws(() => pathInput("/tmp/NUL\0Audio"))
        checks.push("explicit command/path bounds and no unsupported OPEN oracle")
        const encoded = png(2, 1),
            decoded = decodePng(encoded)
        assert.deepEqual(decoded.rgba, Buffer.from([0, 0, 0, 255, 17, 3, 7, 255]))
        const corrupt = Buffer.from(encoded)
        corrupt[corrupt.length - 1] ^= 1
        assert.throws(() => decodePng(corrupt), /PNG CRC/)
        assert.throws(() => decodePng(encoded.subarray(0, encoded.length - 1)))
        assert.throws(() => decodePng(Buffer.concat([encoded, Buffer.from([0])])))
        assert.throws(() => decodePng(png(65, 1)))
        checks.push("independent PNG CRC/framing/dimensions/inflate/filter checks on synthetic unit controls only")
        const recordingPresent = fs.existsSync(recordingPath)
        if (recordingPresent) {
            const { recording } = loadRecording(),
                audioFolder = path.join(temporary, "Audio")
            fs.mkdirSync(audioFolder)
            for (const entry of fixtures.cases) {
                const script = { action: "set", case: entry, audioFolder },
                    result = await replay(script),
                    archived = recording.cases.find((item) => item.id === entry.id)
                fs.writeFileSync(path.join(audioFolder, "NowPlaying.txt"), result.text)
                fs.writeFileSync(path.join(audioFolder, "NowPlayingCover.png"), Buffer.from(archived.set.cover.png.base64, "base64"))
                assert.equal((await verifyFiles(script)).exactSourceRgbaVerified, true)
                fs.writeFileSync(path.join(audioFolder, "NowPlayingCover.png"), png(1, 1, { solid: true }))
                await assert.rejects(verifyFiles(script))
                fs.writeFileSync(path.join(audioFolder, "NowPlayingCover.png"), Buffer.from(archived.set.cover.png.base64, "base64"))
                fs.writeFileSync(path.join(audioFolder, "NowPlaying.txt"), "tampered")
                await assert.rejects(verifyFiles(script))
            }
            fs.writeFileSync(path.join(audioFolder, "NowPlaying.txt"), "")
            fs.unlinkSync(path.join(audioFolder, "NowPlayingCover.png"))
            assert.equal((await verifyFiles({ action: "unset", audioFolder })).coverExists, false)
            checks.push("validated actual-source recording, real parser/formatter replay and file verifier using archived PNG (not GUI evidence)")
        }
        return { ...provenance(), checks, recordingPresent, originalRecordingExecuted: false, guiExecuted: false }
    } finally {
        fs.rmSync(temporary, { recursive: true, force: true })
    }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    const [mode, argument] = process.argv.slice(2)
    let result
    if (mode === "--fixtures") result = await writeFixtures(path.resolve(argument))
    else if (mode === "--record-original") result = await recordOriginal(path.resolve(argument))
    else if (mode === "--self-test") result = await selfTest()
    else if (["--replay", "--verify"].includes(mode)) {
        const script = JSON.parse(boundedFile(path.resolve(argument), 131072).toString("utf8"))
        result = mode === "--verify" ? await verifyFiles(script) : await replay(script)
    } else throw new Error("Use --fixtures PRIVATE_DIRECTORY, --replay SCRIPT_JSON, --verify SCRIPT_JSON, --self-test, or explicit --record-original NEW_RECEIPT_JSON")
    process.stdout.write(JSON.stringify(result) + "\n")
}

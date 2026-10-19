// Actual immutable module, same-realm promises. Only module/OS/image boundaries
// are controlled. These traces prove selected turns, not real IO or GUI timing.
import assert from "node:assert/strict"
import { createHash } from "node:crypto"
import { spawnSync } from "node:child_process"
import path from "node:path"
import { fileURLToPath } from "node:url"
import ts from "typescript"

const root = fileURLToPath(new URL("../../../", import.meta.url))
const revision = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
export const sourceHash = "0a203138d9c283b3e5fa37f0aa46cd33df596d8ee7183ac6cd73d3c59cf3bc7c"
const original = spawnSync("git", ["show", `${revision}:src/electron/audio/nowPlaying.ts`], { cwd: root, encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024 })
assert.equal(original.status, 0, original.stderr)
assert.equal(createHash("sha256").update(original.stdout).digest("hex"), sourceHash)
const compiled = ts.transpileModule(original.stdout, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText

function source({ imageFailure = false } = {}) {
    const phases = [],
        boundaries = [],
        pending = new Map(),
        failures = []
    let post = ""
    const trace = (boundary, ...args) => boundaries.push([boundary, ...args])
    const modules = {
        electron: {
            nativeImage: {
                createFromBuffer(data) {
                    trace("image.create", post, Array.from(data))
                    if (imageFailure) throw new Error("controlled native image failure")
                    return {
                        toPNG() {
                            trace("image.png", post)
                            return Buffer.from([137, 80, 78, 71])
                        },
                        getSize() {
                            trace("image.size", post)
                            return { width: 1, height: 1 }
                        }
                    }
                }
            }
        },
        "music-metadata": {
            parseFile(filePath) {
                trace("parse", filePath)
                assert.ok(!pending.has(filePath))
                return new Promise((resolve, reject) => pending.set(filePath, { resolve, reject }))
            }
        },
        path,
        "../../types/IPC/ToMain": { ToMain: { AUDIO_METADATA: "AUDIO_METADATA" } },
        "../IPC/main": {
            sendToMain(channel, data) {
                assert.equal(channel, "AUDIO_METADATA")
                phases.push([data.filePath, "PublishMetadata"])
                trace("metadata", data.filePath, data.metadata)
            }
        },
        "../utils/files": {
            getDataFolderPath(type) {
                assert.equal(type, "audio")
                trace("folder", post)
                return path.join(path.parse(process.cwd()).root, "fixture", "Audio")
            },
            doesPathExist(filePath) {
                trace("exists", path.basename(filePath))
                return false
            },
            deleteFile(filePath) {
                trace("delete", path.basename(filePath))
            },
            writeFile(filePath, content) {
                const file = path.basename(filePath)
                trace("write", file, typeof content === "string" ? content : Array.from(content))
                if (file === "NowPlaying.txt" && content !== "") phases.push([content, "Text"])
            },
            openInSystem(filePath) {
                trace("open", path.basename(filePath))
                phases.push(["O", "Open"])
            }
        },
        "./IcecastSender": {
            IcecastSender: {
                updateMetadata(title) {
                    trace("title", title)
                    post = title ? title.slice(title.lastIndexOf(" - ") + 3) : "U"
                    phases.push([post, title ? "PostMetadata" : "Unset"])
                }
            }
        }
    }
    const exports = {}
    new Function("require", "exports", "console", compiled)(
        (name) => {
            assert.ok(Object.hasOwn(modules, name), `Unexpected source dependency ${name}`)
            return modules[name]
        },
        exports,
        { error: (...args) => failures.push(args.map(String)) }
    )
    return {
        phases,
        boundaries,
        failures,
        start(label) {
            return exports.setPlayingState({ filePath: label, name: label, format: "{title}", unknownLang: [], duration: 0 })
        },
        ready(label, { common = { title: label }, reject = false } = {}) {
            const parser = pending.get(label)
            assert.ok(parser, `Source did not call parseFile(${label})`)
            pending.delete(label)
            if (reject) parser.reject(new Error("controlled parser rejection"))
            else parser.resolve({ common })
        },
        unset() {
            assert.equal(exports.unsetPlayingAudio(), undefined, "source void UNSET must not invent a reply")
        },
        open() {
            assert.equal(exports.openNowPlaying(), undefined, "source void OPEN must not invent a reply")
        }
    }
}

export async function runCases() {
    const cases = {}
    for (const [name, order] of [
        ["same_turn_b_a", ["B", "A"]],
        ["same_turn_a_b", ["A", "B"]],
        ["unset_before_yield", ["B", "A"]]
    ]) {
        const s = source(),
            a = s.start("A"),
            b = s.start("B")
        for (const label of order) s.ready(label)
        if (name === "unset_before_yield") s.unset()
        assert.deepEqual(await Promise.all([a, b]), [undefined, undefined])
        cases[name] = { phases: s.phases, boundaries: s.boundaries }
    }
    {
        const s = source(),
            a = s.start("A"),
            b = s.start("B")
        s.ready("B")
        await new Promise(setImmediate)
        s.ready("A")
        assert.deepEqual(await Promise.all([a, b]), [undefined, undefined])
        cases.separate_turn = { phases: s.phases, boundaries: s.boundaries }
    }
    {
        const s = source(),
            a = s.start("A")
        s.unset()
        s.open()
        s.ready("A")
        assert.equal(await a, undefined)
        cases.unset_open_pending = { phases: s.phases, boundaries: s.boundaries }
    }
    {
        const s = source(),
            a = s.start("A")
        s.ready("A", { reject: true })
        assert.equal(await a, undefined)
        assert.equal(s.failures.length, 1)
        cases.parser_null = { phases: s.phases, boundaries: s.boundaries, failures: s.failures }
    }
    {
        const s = source(),
            a = s.start("A")
        s.ready("A", { common: null })
        assert.equal(await a, undefined)
        cases.missing_common = { phases: s.phases, boundaries: s.boundaries }
    }
    {
        const s = source({ imageFailure: true }),
            a = s.start("A")
        s.ready("A", { common: { title: "A", picture: [{ data: Buffer.from([1, 2, 3]) }] } })
        await assert.rejects(a, /controlled native image failure/)
        cases.post_failure = { phases: s.phases, boundaries: s.boundaries }
    }
    return { revision, sourceHash, node: process.version, cases }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
    const deadline = setTimeout(() => process.exit(124), 10000)
    try {
        process.stdout.write(JSON.stringify(await runCases()))
    } finally {
        clearTimeout(deadline)
    }
}

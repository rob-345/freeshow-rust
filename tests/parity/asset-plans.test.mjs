import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { spawnSync } from "node:child_process"
import { fileURLToPath, pathToFileURL } from "node:url"
import test from "node:test"
import ts from "typescript"
import { root, upstreamFunctions } from "./oracle.mjs"

const binary = process.env.FREESHOW_ASSET_PLANS_PARITY || path.join(root, "target/debug/freeshow-asset-plans-parity" + (process.platform === "win32" ? ".exe" : ""))
const context = { runtimeId: "plans-parity", libraryGeneration: 3, dataPath: "/library" }
const absolute = (...names) => path.resolve(path.sep, "oracle", ...names)
function run(cases) {
    const result = spawnSync(binary, [], { input: cases.map((input) => JSON.stringify({ context, ...input })).join("\n") + "\n", encoding: "utf8", timeout: 10000, maxBuffer: 8 * 1024 * 1024 })
    assert.equal(result.status, 0, result.stderr || result.error?.message)
    const replies = result.stdout
        .trim()
        .split("\n")
        .map((line) => JSON.parse(line))
    assert.equal(replies.length, cases.length)
    return replies
}
function success(input) {
    const reply = run([input])[0]
    assert.equal(reply.ok, true, JSON.stringify(reply))
    return reply.value
}
const sourceMedia = upstreamFunctions("src/frontend/components/helpers/media.ts", ["isLocalFile", "encodeFilePath", "splitPath", "joinPath"], {}, 'let pathJoiner = "";')
const sourcePaths = upstreamFunctions("src/electron/utils/files.ts", ["normalizeLocalPath"], { fileURLToPath })
function exportedPaths(shows) {
    let found = []
    const virtual = new Map(Object.entries(shows).map(([id, show]) => [path.join("/shows", id + ".show"), JSON.stringify([id, show])]))
    const oracle = upstreamFunctions(
        "src/electron/utils/files.ts",
        ["bundleMediaFiles", "parseShow", "parseJSON"],
        {
            path,
            getDataFolderPath: () => "/shows",
            readFolder: () => [...virtual.keys()].map((file) => path.basename(file)),
            readFile: (file) => virtual.get(file),
            getStore: () => ({}),
            addToMediaFolder: (files) => {
                found = [...files]
            },
            openInSystem: () => assert.fail("Source unexpectedly opened a folder")
        },
        "let currentlyBundling = false;"
    )
    oracle.bundleMediaFiles()
    return found
}
function expectedLocal(paths) {
    return [...new Set(paths)].filter((source) => sourceMedia.isLocalFile(source) && path.isAbsolute(sourcePaths.normalizeLocalPath(source))).map((source) => ({ sourcePath: source, path: sourcePaths.normalizeLocalPath(source), provenance: "CommittedLibrary", protection: "Plain" }))
}
function fixture(seed) {
    const choices = [absolute("日本語 é #%.png"), pathToFileURL(absolute("space name.png")).href, "https://example.test/a.mp4?x=é", "data:image/png;base64,AA==", "blob:opaque/1", "./assets/wood.wav", absolute("e\u0301.png"), absolute("é.png"), "freeshow-protected://encrypted-id"]
    const media = {}
    for (const [offset, id] of ["z", "10", "2", "a", "+1", "01"].entries()) media[id] = { path: (seed + offset) % 3 ? choices[(seed + offset) % choices.length] : "", id: choices[(seed + offset + 2) % choices.length], unknown: { path: absolute("never-grant-private.json") } }
    return {
        name: "Show " + seed,
        media,
        slides: {
            z: {
                items: [
                    { type: "text", src: absolute("ignore-text.png") },
                    { type: "media", src: choices[(seed + 1) % choices.length] },
                    { type: "media", src: choices[(seed + 2) % choices.length] }
                ],
                settings: {}
            },
            2: { items: [{ type: "media", src: choices[(seed + 3) % choices.length] }] }
        },
        layouts: {},
        unknown: { slides: { s: { items: [{ type: "media", src: absolute("never-follow.png") }] } } }
    }
}

test("generated full-show grant extraction matches actual source bundle traversal and media helpers", () => {
    const shows = Array.from({ length: 72 }, (_, seed) => fixture(seed))
    const plans = run(shows.map((data) => ({ op: "show", data })))
    shows.forEach((show, index) => {
        assert.equal(plans[index].ok, true)
        assert.deepEqual(plans[index].value.grants, expectedLocal(exportedPaths({ show })))
        assert.deepEqual(plans[index].value.context, context)
    })
})

test("actual source path-or-id truthiness never promotes a truthy malformed path to its fallback", () => {
    const values = [null, false, 0, "", true, 1, [], { path: absolute("hidden.png") }]
    const data = { name: "truthiness", media: Object.fromEntries(values.map((value, index) => [index, { path: value, id: absolute("fallback" + index + ".png") }])) }
    const plan = success({ op: "show", data })
    assert.deepEqual(plan.grants, expectedLocal(exportedPaths({ show: data })))
    assert.equal(plan.diagnostics.length, 4)
    assert.ok(plan.diagnostics.every((diagnostic) => diagnostic.code === "asset_plan_shape"))
})

function timelineOracle() {
    const filename = "src/frontend/components/timeline/TimelinePlayback.ts"
    const source = fs.readFileSync(path.join(root, filename), "utf8")
    const ast = ts.createSourceFile(filename, source, ts.ScriptTarget.Latest, true)
    const declaration = ast.statements.find((node) => ts.isClassDeclaration(node) && node.name.text === "TimelinePlayback")
    const method = declaration.members
        .find((node) => node.name?.getText(ast) === "resolveMediaPaths")
        .getText(ast)
        .replace(/^private\s+/, "")
    const found = []
    const javascript = ts.transpileModule("globalThis.oracle = ({" + method + "})", { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
    const context = {
        locateMediaFile: async (file) => {
            found.push(file)
            return null
        }
    }
    vm.runInNewContext(javascript, context, { timeout: 1000 })
    return { found, resolve: (actions) => context.oracle.resolveMediaPaths(actions) }
}
function backgroundOracle() {
    const filename = "src/frontend/components/helpers/showActions.ts"
    const source = fs.readFileSync(path.join(root, filename), "utf8")
    const ast = ts.createSourceFile(filename, source, ts.ScriptTarget.Latest, true)
    let block
    const visit = (node) => {
        if (ts.isIfStatement(node) && /^if \(!background\)/.test(node.getText(ast)) && node.getText(ast).includes("const showSlide: Slide")) block = node.getText(ast)
        ts.forEachChild(node, visit)
    }
    visit(ast)
    assert.ok(block)
    const javascript = ts.transpileModule(`globalThis.pick = function(slide) { let background=null, isSlideBg=false; const showId="s", layout=[{id:"s"}], index=0; const _show=()=>({slides:()=>({get:()=>[slide]})}); ${block}; return background; }`, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText
    const context = {}
    vm.runInNewContext(javascript, context, { timeout: 1000 })
    return context.pick
}

test("background images and both real audio/video timeline consumers extend the exporter without walking other actions", async () => {
    const audio = absolute("timeline.wav"),
        video = absolute("timeline.webm"),
        bg = absolute("slide background.png"),
        ignored = absolute("secret.json")
    const actions = [
        { type: "audio", data: { path: audio } },
        { type: "style", data: { path: ignored } },
        { type: "video", data: { path: video } },
        { type: "show", data: { path: ignored } },
        { type: "audio", data: { path: "" } }
    ]
    const data = { name: "extra consumers", media: {}, slides: { s: { settings: { backgroundImage: bg, path: ignored }, items: [], timeline: { actions } } }, layouts: { active: { timeline: { actions }, slides: [{ background: ignored, audio: [ignored] }] } } }
    const timeline = timelineOracle()
    await timeline.resolve(actions)
    assert.deepEqual(timeline.found, [audio, video])
    assert.equal(backgroundOracle()(data.slides.s), bg)
    assert.deepEqual(exportedPaths({ show: data }), [], "The source bundler omits these actual rendering consumers")
    const plan = success({ op: "show", data })
    assert.deepEqual(
        plan.grants.map((grant) => grant.sourcePath),
        [bg, ...timeline.found]
    )
    assert.equal(plan.stats.duplicates, 2)
})

test("full SHOW reply and committed SAVE.showsCache are explicit sources; trimmed SHOWS and unknown fields are untouched", () => {
    const show = fixture(7),
        extra = fixture(11)
    const full = success({ op: "show", data: show }).grants
    assert.deepEqual(success({ op: "show_reply", data: { id: "s", content: ["s", show] } }).grants, full)
    assert.deepEqual(success({ op: "save", data: { showsCache: { s: show }, SHOWS: { fake: extra }, other: extra } }).grants, full)
    assert.deepEqual(success({ op: "save", data: { SHOWS: { s: show }, unknown: show } }).grants, [])
    assert.equal(run([{ op: "show_reply", data: { error: "not_found", content: ["s", show] } }])[0].error.code, "asset_plan_shape")
    const serialized = JSON.stringify(["legacy", show])
    const legacy = success({ op: "show_reply", data: { id: "s", content: ["s", serialized] } })
    assert.deepEqual(legacy.grants, full)
    assert.equal(legacy.stats.legacyBytes, Buffer.byteLength(serialized))
    assert.equal(run([{ op: "show_reply", data: { id: "s", content: ["s", "corrupt embedded JSON"] } }])[0].error.code, "asset_plan_shape")
})

test("source file-URL normalization preserves every exact renderer source key and distinct Unicode spellings", () => {
    const sources = [absolute("a%20b.png"), pathToFileURL(absolute("a b.png")).href, pathToFileURL(absolute("a%20b.png")).href, absolute("bad%2G.png"), absolute("é.png"), absolute("e\u0301.png"), absolute("a%23b.png"), absolute("a%3Fb.png")]
    const data = { media: Object.fromEntries(sources.map((path, index) => [index, { path }])) }
    assert.deepEqual(success({ op: "show", data }).grants, expectedLocal(sources))
    for (const input of ["https://example.test/a%20b.mp4", "data:image/png;base64,AA==", "blob:test/123", "./assets/wood.wav"]) {
        const plan = success({ op: "show", data: { media: { x: { path: input } } } })
        assert.deepEqual(plan.grants, [])
        assert.equal(sourceMedia.encodeFilePath(input), input, "Original nonlocal/bundled encoder output remains unchanged")
    }
})

test("accepted native selections/drop/LOCATE/browse/import destinations retain explicit provenance without decoding actual filenames", () => {
    const file = absolute("literal%20name.png"),
        second = absolute("日本語.wav"),
        thumb = absolute("cache.png")
    const files = [
        { path: file, protection: "plain" },
        { path: second, protection: "plain", directory: true, isFolder: true, thumbnailPath: absolute("not-a-generated-folder-thumbnail.png") }
    ]
    for (const [op, expected, provenance] of [
        ["selection", [file, second], "NativeSelection"],
        ["drop", [file], "NativeDrop"],
        ["import", [file, second], "CommittedLibrary"],
        ["browse", [file], "NativeSelection"]
    ]) {
        const plan = success({ op, files })
        assert.deepEqual(
            plan.grants.map((grant) => grant.sourcePath),
            expected
        )
        assert.ok(plan.grants.every((grant) => grant.provenance === provenance && grant.path === grant.sourcePath))
    }
    const browsed = success({ op: "browse", files: [{ path: file, protection: "plain", thumbnailPath: thumb }] })
    assert.deepEqual(
        browsed.grants.map((grant) => [grant.sourcePath, grant.provenance]),
        [
            [file, "NativeSelection"],
            [thumb, "GeneratedThumbnail"]
        ]
    )
    assert.equal(success({ op: "locate", files: [files[0]] }).grants[0].provenance, "LocatedMedia")
    assert.deepEqual(success({ op: "locate", files: [] }).grants, [])
    assert.equal(success({ op: "thumbnail", files: [files[0]] }).grants[0].provenance, "GeneratedThumbnail")
})

test("classification, protected-URI override and conflicting source facts never create plaintext success", () => {
    const ordinary = absolute("ordinary.png"),
        opaque = absolute("provider-cache-hash"),
        unknown = absolute("unclassified.bin")
    const plan = success({ op: "show", data: { media: { a: { path: ordinary }, b: { path: opaque, encrypted: false }, c: { path: unknown, encrypted: false }, d: { path: "freeshow-protected://provider" } } }, protected: [opaque], unclassified: [unknown] })
    assert.deepEqual(
        plan.grants.map((grant) => grant.sourcePath),
        [ordinary]
    )
    assert.deepEqual(
        plan.diagnostics.map((diagnostic) => diagnostic.code),
        ["asset_plan_protected", "asset_plan_unclassified", "asset_plan_protected"]
    )
    const files = [
        { sourcePath: ordinary, path: ordinary, protection: "plain" },
        { sourcePath: ordinary, path: ordinary, protection: "protected" }
    ]
    assert.equal(run([{ op: "selection", files }])[0].error.code, "asset_plan_conflict")
    assert.equal(run([{ op: "selection", files: [files[0], { ...files[0], path: unknown }] }])[0].error.code, "asset_plan_conflict")
})

test("all typed work/payload budgets and diagnostic omissions are explicit and bounded", () => {
    const data = { media: Object.fromEntries(Array.from({ length: 20 }, (_, index) => [index, { path: absolute("f" + index) }])) }
    for (const limits of [{ candidates: 2 }, { candidateBytes: 2 }, { inputPathBytes: 2 }, { pathBytes: 2 }, { operations: 2 }, { containerEntries: 2 }, { containers: 2 }, { references: 2 }]) assert.equal(run([{ op: "show", data, limits }])[0].error.code, "asset_plan_limit")
    const protectedData = { media: Object.fromEntries(Array.from({ length: 100 }, (_, index) => [index, { path: "freeshow-protected://" + "é".repeat(300) + index }])) }
    for (const limits of [{ diagnostics: 2 }, { diagnosticBytes: 600 }, { diagnostics: 0, diagnosticBytes: 0 }]) {
        const plan = success({ op: "show", data: protectedData, limits })
        assert.deepEqual(plan.grants, [])
        assert.equal(plan.diagnostics.length + plan.stats.diagnosticsOmitted, 100)
        assert.ok(plan.stats.diagnosticBytes <= (limits.diagnosticBytes ?? 65536))
        assert.ok(plan.diagnostics.length <= (limits.diagnostics ?? 64))
        assert.ok(plan.diagnostics.every((diagnostic) => Buffer.byteLength(diagnostic.sourcePath) <= 512 && !diagnostic.sourcePath.includes("�")))
    }
    assert.equal(run([{ op: "save", data: { showsCache: { a: fixture(1), b: fixture(2) } }, limits: { shows: 1 } }])[0].error.code, "asset_plan_limit")
})

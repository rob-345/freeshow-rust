import assert from "node:assert/strict"
import { test } from "node:test"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import Conf from "conf"
import { root, showOracle, fileOracle, fixtureShow, clean, createSaveOracle } from "./oracle.mjs"

const binary = process.env.FREESHOW_CORE_BIN || path.join(root, "target", "debug", process.platform === "win32" ? "freeshow-core.exe" : "freeshow-core")
export function rust(cases, args = []) {
    const process = spawnSync(binary, args, { input: cases.map((input) => JSON.stringify(input)).join("\n") + "\n", encoding: "utf8", maxBuffer: 64 * 1024 * 1024, timeout: 30000 })
    assert.equal(process.error, undefined)
    assert.equal(process.status, 0, process.stderr)
    return process.stdout
        .trim()
        .split("\n")
        .map((line) => JSON.parse(line))
}
export const request = (command, data, listenerId) => ({ op: "runtime", request: { channel: "MAIN", data: { channel: command, data }, listenerId } })

for (const [operation, expected] of [
    ["show.trim", showOracle.trimShow],
    ["show.text", showOracle.getShowTextContent],
    ["show.searchText", fileOracle.getTextCacheString],
    ["show.empty", (show) => !showOracle.showHasLayoutContent(show) && !showOracle.getShowTextContent(show).length]
]) {
    test(`${operation}: 256 generated shows match original TypeScript`, () => {
        const corpus = Array.from({ length: 256 }, (_, seed) => fixtureShow(seed))
        const [response] = rust([{ op: "batch", cases: corpus.map((value) => ({ op: operation, value })) }])
        assert.equal(response.ok, true, JSON.stringify(response.error))
        corpus.forEach((value, index) => assert.deepEqual(response.value[index], clean(expected(value)), `seed ${index}`))
    })
}

test("parser matches original on valid, recovered, truncated and non-show JSON", () => {
    const normal = JSON.stringify(["abc", fixtureShow(7)])
    const corpus = [normal, normal + "trailing garbage", normal.slice(0, -5), "", "null", "{}", "false", "[]", "[1,2]", '["id",{"slides":{}}]garbage', "not JSON", "日本語", '{"unicode":"🙌"}', "true", "0"]
    const [response] = rust([{ op: "batch", cases: corpus.map((value) => ({ op: "show.parse", value })) }])
    assert.equal(response.ok, true)
    corpus.forEach((value, index) => assert.deepEqual(response.value[index], clean(fileOracle.parseJSON(value)), value))
})

test("malformed optional arrays and numeric slide IDs preserve original text semantics", () => {
    const corpus = [{}, { slides: null }, { slides: { x: {} } }, { slides: { x: { items: null } } }, { slides: { x: { items: [{ lines: false }, { lines: [{ text: null }, { text: [{ value: false }, { value: 12 }] }] }] } } }]
    const [response] = rust([{ op: "batch", cases: corpus.map((value) => ({ op: "show.text", value })) }])
    assert.equal(response.ok, true)
    corpus.forEach((value, index) => assert.equal(response.value[index], showOracle.getShowTextContent(value)))
})

test("startup preserves main/output role and request listener ID", () => {
    const make = (window) => ({ op: "runtime", window, request: { channel: "STARTUP", data: { channel: "READY" }, listenerId: "startup" } })
    const [main, output] = rust([make("main"), make("output-1")])
    assert.deepEqual(main.value, [{ channel: "STARTUP", message: { channel: "TYPE", data: null, autoProfile: "" }, listenerId: "startup" }])
    assert.equal(output.value[0].message.data, "output")
})

test("unported commands fail explicitly and never send a success event", () => {
    for (const response of rust([request("AI_LISTEN_START", {}), { op: "runtime", request: { channel: "NDI", data: { channel: "START" } } }])) {
        assert.equal(response.ok, false)
        assert.equal(response.error.code, "unsupported_command")
        assert.equal(response.value, undefined)
    }
})

function snapshot(directory) {
    if (!fs.existsSync(directory)) return {}
    const result = {}
    for (const entry of fs.readdirSync(directory, { recursive: true, withFileTypes: true })) {
        if (!entry.isFile()) continue
        const file = path.join(entry.parentPath || entry.path, entry.name)
        result[path.relative(directory, file).split(path.sep).join("/")] = JSON.parse(fs.readFileSync(file, "utf8"))
    }
    return result
}

test("save, reload, rename and deletion protection match the original save function", async (t) => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-parity-"))
    t.after(() => fs.rmSync(temp, { recursive: true, force: true }))
    const args = ["--app-data", path.join(temp, "rust/app"), "--data-dir", path.join(temp, "rust/data")]
    const oracle = createSaveOracle(path.join(temp, "electron/app"), path.join(temp, "electron/data"))
    const show = fixtureShow(9)
    show.name = "Unicode 🙌 日本語"
    const payloads = [
        { SETTINGS: { initialized: true, special: { preserved: "future" } }, PROJECTS: { projects: { project1: { name: "Sunday", shows: [{ id: "song1" }] } }, folders: {}, projectTemplates: {} }, showsCache: { song1: show }, scripturesCache: { bible1: { name: "Test Bible", books: [], custom: true } }, closeWhenFinished: false, customTriggers: {} },
        { showsCache: { song1: { ...show, name: "Renamed" } }, renamedShows: [{ id: "song1", name: "Renamed", oldName: show.name }], deletedShows: [{ id: "song1", name: "Renamed" }], customTriggers: {} },
        { deletedShows: [{ id: "different-id", name: "Renamed" }], customTriggers: {} },
        { deletedShows: [{ id: "song1", name: "Renamed" }], customTriggers: {} }
    ]
    for (const payload of payloads) await oracle.save(clean(payload))
    const responses = rust(
        payloads.map((payload) => request("SAVE", payload)),
        args
    )
    assert.ok(
        responses.every((response) => response.ok),
        JSON.stringify(responses)
    )
    assert.deepEqual(
        responses.flatMap((response) => response.value),
        oracle.events
    )
    assert.deepEqual(snapshot(path.join(temp, "rust")), snapshot(path.join(temp, "electron")))
})

test("store values, literal MEDIA dots, portable paths and restart preserve data", (t) => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-store-"))
    t.after(() => fs.rmSync(temp, { recursive: true, force: true }))
    const app = path.join(temp, "app"),
        data = path.join(temp, "data")
    const args = ["--app-data", app, "--data-dir", data]
    const results = rust([request("SET_STORE_VALUE", { file: "config", key: "special.enabled", value: true }), request("GET_STORE_VALUE", { file: "config", key: "special.enabled" }, "reply-1"), request("SET_STORE_VALUE", { file: "MEDIA", key: "/music/song.mp3", value: { duration: 12 } }), request("GET_STORE_VALUE", { file: "MEDIA", key: "/music/song.mp3" }), request("SET_STORE_VALUE", { file: "PROJECTS", key: "projects.p1", value: { name: "Sunday" } })], args)
    assert.ok(results.every((result) => result.ok))
    assert.equal(results[1].value[0].listenerId, "reply-1")
    assert.equal(results[1].value[0].message.data, true)
    assert.deepEqual(results[3].value[0].message.data, { duration: 12 })
    assert.deepEqual(rust([request("PROJECTS")], args)[0].value[0].message.data.projects.p1, { name: "Sunday" })
    assert.ok(fs.existsSync(path.join(data, "Config/projects.json")))
    assert.ok(fs.existsSync(path.join(app, "media.json")))
    assert.ok(!fs.existsSync(path.join(app, "projects.json")))
})

test("load show preserves unknown fields and overrides stored ID as Electron does", (t) => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-load-"))
    t.after(() => fs.rmSync(temp, { recursive: true, force: true }))
    const value = fixtureShow(4)
    const responses = rust([request("SAVE", { showsCache: { stored: value } }), request("SHOW", { name: value.name, id: "requested" }), request("SHOW", { name: "missing", id: "absent" })], ["--app-data", path.join(temp, "app"), "--data-dir", path.join(temp, "data")])
    assert.ok(responses.every((response) => response.ok))
    assert.deepEqual(responses[1].value[0].message.data, { id: "requested", content: ["requested", value] })
    assert.deepEqual(responses[2].value[0].message.data, { id: "absent", error: "not_found" })
})

test("invalid paths and unsupported backup fail before any save mutation", (t) => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-invalid-"))
    t.after(() => fs.rmSync(temp, { recursive: true, force: true }))
    const responses = rust([request("SAVE", { SETTINGS: { initialized: true }, showsCache: { id: { name: "../escape" } } }), request("SAVE", { SETTINGS: { initialized: true }, customTriggers: { backup: true } }), request("SET_STORE_VALUE", { file: "config", key: "__proto__.polluted", value: true })], ["--app-data", path.join(temp, "app"), "--data-dir", path.join(temp, "data")])
    assert.deepEqual(
        responses.map((response) => response.error.code),
        ["invalid_request", "unsupported_command", "invalid_request"]
    )
    assert.equal(fs.existsSync(path.join(temp, "app/settings.json")), false)
})

test("nested arrays and escaped store keys match electron-store's actual Conf dependency", (t) => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-conf-"))
    t.after(() => fs.rmSync(temp, { recursive: true, force: true }))
    const original = new Conf({ cwd: path.join(temp, "oracle"), projectName: "freeshow-parity", name: "config" })
    const changes = [
        ["state", { list: [{ name: "First" }, { name: "Second" }] }],
        ["state.list.0.name", "Updated"],
        ["state.literal\\.key", { preserved: true }],
        ["state.folder\\\\name.value", "double backslash"],
        ["state.trailing\\\\", "trailing backslash"],
        ["state.empty..key", false]
    ]
    for (const [key, value] of changes) original.set(key, value)
    const responses = rust([...changes.map(([key, value]) => request("SET_STORE_VALUE", { file: "config", key, value })), request("GET_STORE_VALUE", { file: "config", key: "state" }), request("GET_STORE_VALUE", { file: "config", key: "state.list.1.name" })], ["--app-data", path.join(temp, "app"), "--data-dir", path.join(temp, "data")])
    assert.ok(
        responses.every((response) => response.ok),
        JSON.stringify(responses)
    )
    assert.deepEqual(responses.at(-2).value[0].message.data, clean(original.get("state")))
    assert.deepEqual(responses.at(-1).value[0].message.data, original.get("state.list.1.name"))
})

import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { createRequire } from "node:module"
import { test } from "node:test"
import { root, transportOracle, frontendOracle } from "./imports-text-oracle.mjs"
const require = createRequire(import.meta.url),
    yazl = require("yazl")
const binary = process.env.FREESHOW_IMPORTS_BIN || path.join(root, "target/debug", process.platform === "win32" ? "freeshow-imports.exe" : "freeshow-imports")
const formats = ["openlp", "opensong", "mediashout", "quelea", "songbeamer", "easyslides", "verseview", "videopsalm"]
const samples = {
    openlp: '<song modifiedDate="2024-01-02"><properties><titles><title>Café 敬拜 🙌</title></titles><authors><author type="words">Writer</author></authors><verseOrder>v1 c1</verseOrder></properties><lyrics><verse name="v1"><lines>敬拜 🙌</lines></verse><verse name="c1"><lines>Hallelujah</lines></verse></lyrics></song>',
    opensong: "<song><title>Café 敬拜 🙌</title><author>Writer</author><lyrics>[V1]\n 敬拜 🙌\n[C1]\n Hallelujah</lyrics><presentation>V1 C1</presentation></song>",
    mediashout: "<MediaShout5_Document><Cues><Cue><SongID>source-song</SongID><Title>Café 敬拜 🙌</Title><Content><Element><Text>敬拜 🙌\nHallelujah</Text></Element></Content></Cue></Cues></MediaShout5_Document>",
    quelea: '<song><title>Café 敬拜 🙌</title><author>Writer</author><lyrics><section title="Verse"><lyrics>敬拜 🙌\nHallelujah</lyrics></section></lyrics></song>',
    songbeamer: "\ufeff#Title=Café 敬拜 🙌\r\n#Author=Writer\r\n#Comments=Tm90ZXMg5pWs5ouc\r\n---\r\nVerse 1\r\n敬拜 🙌\r\nHallelujah\r\n",
    easyslides: "<Easyslides><Item><Title1>Café 敬拜 🙌</Title1><Contents>[Verse 1]\n敬拜 🙌\nHallelujah</Contents><SongNumber>7</SongNumber></Item><Item><Title1>Second</Title1><Contents>Other words</Contents></Item></Easyslides>",
    verseview: "<songDB><song><name><![CDATA[Café 敬拜 🙌]]></name><category><![CDATA[Category]]></category><author><![CDATA[Writer]]></author><slide><![CDATA[敬拜 🙌<br>Hallelujah]]></slide></song></songDB>",
    videopsalm: JSON.stringify({ Text: "Album", Songs: [{ Guid: "source-song", Text: "Café 敬拜 🙌", Author: "Writer", Sequence: "V1 C1", Verses: [{ Text: "敬拜 🙌" }, { Text: "Hallelujah" }] }] })
}
function temporary(t) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-import-text-"))
    t.after(() => fs.rmSync(dir, { recursive: true, force: true }))
    return dir
}
function write(dir, name, bytes) {
    const file = path.join(dir, name)
    fs.mkdirSync(path.dirname(file), { recursive: true })
    fs.writeFileSync(file, bytes)
    return file
}
function request(dir, id, paths) {
    return { command: "IMPORT_FILES", importsRoot: path.join(dir, "unused-imports"), data: { id, paths } }
}
function rust(cases) {
    const result = spawnSync(binary, [], { input: cases.map(JSON.stringify).join("\n") + "\n", encoding: "utf8", maxBuffer: 32 * 1024 * 1024, timeout: 30000 })
    assert.equal(result.error, undefined)
    assert.equal(result.status, 0, result.stderr)
    return result.stdout.trim().split("\n").map(JSON.parse)
}
function canonical(events) {
    return events.map((event) => (event.message.channel === "IMPORT2" ? { ...event, message: { ...event.message, data: { ...event.message.data, data: [...event.message.data.data].sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b))) } } } : event))
}
async function compare(cases, multiple = false) {
    const results = rust(cases)
    for (const [index, item] of cases.entries()) {
        const source = transportOracle()
        await source.dispatch(item.data.id, item.data.paths)
        assert.equal(results[index].error, undefined, JSON.stringify(results[index].error))
        assert.deepEqual(multiple ? canonical(results[index].events) : results[index].events, multiple ? canonical(source.events) : source.events)
        const nativeEvents = multiple ? canonical(results[index].events) : results[index].events
        const sourceEvents = multiple ? canonical(source.events) : source.events
        for (const [eventIndex, event] of nativeEvents.entries()) {
            const original = sourceEvents[eventIndex]
            assert.deepEqual(Object.keys(event), Object.keys(original))
            assert.deepEqual(Object.keys(event.message), Object.keys(original.message))
            if (event.message.channel === "IMPORT2") {
                assert.deepEqual(Object.keys(event.message.data), Object.keys(original.message.data))
                for (const [fileIndex, file] of event.message.data.data.entries()) assert.deepEqual(Object.keys(file), Object.keys(original.message.data.data[fileIndex]))
            }
        }
        assert.deepEqual(results[index].media, [])
        assert.deepEqual(results[index].plan.directories, [])
        assert.equal(fs.existsSync(item.importsRoot), false, "Text preparation must not create a final import folder")
        assert.deepEqual(source.calls.special, [])
    }
    return results
}
async function zip(dir, name, entries) {
    const file = path.join(dir, name),
        archive = new yazl.ZipFile(),
        output = fs.createWriteStream(file)
    const done = new Promise((resolve, reject) => {
        output.on("close", resolve)
        output.on("error", reject)
        archive.on("error", reject)
    })
    archive.outputStream.pipe(output)
    for (const [name, content] of entries) archive.addBuffer(Buffer.from(content), name)
    archive.end()
    await done
    return file
}

test("eight retained text IDs emit exact immutable importShow callbacks on real Unicode, BOM and extensionless files", async (t) => {
    const dir = temporary(t)
    const cases = formats.map((id) => request(dir, id, [write(dir, id + "/敬拜 🙌 song" + (id === "opensong" ? "" : id === "songbeamer" ? ".SNG" : id === "videopsalm" ? ".JSON" : ".XML"), samples[id])]))
    const results = await compare(cases)
    assert.equal(results.length, 8, "All newly admitted formats must actually produce data")
    for (const result of results) {
        assert.equal(result.events.length, 1)
        assert.equal(result.events[0].message.data.data.length, 1)
    }
    const chordpro = request(dir, "chordpro", [write(dir, "Café.PRO", "{title: Café}\n[C]敬拜 🙌")])
    await compare([chordpro, request(dir, "mediashout", [write(dir, "Cues.SSC", samples.mediashout)])])
    const astral = write(dir, "ending 🙌", "ordinary source text")
    const source = transportOracle()
    await source.dispatch("opensong", [astral])
    const name = source.events[0].message.data.data[0].name
    assert.equal(name.charCodeAt(name.length - 1), 0xd83d, "Source slices one UTF-16 unit from extensionless names")
    assert.equal(rust([request(dir, "opensong", [astral])])[0].error.code, "unsupported_format", "A lone-surrogate source filename is an explicit native representability gap")
})

test("missing/directory/empty/malformed UTF-8 remain source fallback data, preserving BOM, duplicate count and unknown text", async (t) => {
    const dir = temporary(t),
        missing = path.join(dir, "missing.xml"),
        malformed = write(dir, "损坏.xml", Buffer.from([0xef, 0xbb, 0xbf, 0xff, 0xe2, 0x82, 0x41, 0, 0xed, 0xa0, 0x80]))
    const empty = write(dir, "empty.xml", ""),
        unknown = write(dir, "opaque.unlisted", "not valid XML & stays raw")
    await compare(formats.flatMap((id) => [missing, dir, malformed, empty, unknown].map((file) => request(dir, id, [file]))))
    await compare(formats.flatMap((id) => [[], null].map((paths) => request(dir, id, paths))))
    // Source asyncPool completion ordering is nondeterministic. Compare the
    // complete multiset here; one-file replies above are compared without sort.
    const results = await compare(
        formats.map((id) => request(dir, id, [malformed, missing, malformed])),
        true
    )
    for (const result of results) assert.equal(result.events[0].message.data.data.length, 3)
})

test("actual retained IMPORT2 converters consume native text into real ShowObj/history with controlled UI/DOM boundaries", async (t) => {
    const dir = temporary(t),
        cases = formats.map((id) => request(dir, id, [write(dir, id + (id === "songbeamer" ? ".sng" : id === "videopsalm" ? ".json" : ".xml"), samples[id])]))
    const results = await compare(cases)
    for (const [index, id] of formats.entries()) {
        const files = results[index].events[0].message.data.data
        const actual = frontendOracle(id),
            source = frontendOracle(id),
            original = transportOracle()
        await original.dispatch(id, cases[index].data.paths)
        actual.consume(files)
        source.consume(original.events[0].message.data.data)
        assert.deepEqual(actual.converted, source.converted, id)
        assert.deepEqual(actual.histories, source.histories, id)
        assert.ok(actual.converted.length > 0, id + " must create a real nonempty show")
        assert.equal(actual.converted[0].show.name, "Café 敬拜 🙌", id)
        const values = actual.converted.flatMap(({ show }) => Object.values(show.slides).flatMap((slide) => slide.items.flatMap((item) => item.lines.flatMap((line) => line.text.map((run) => run.value)))))
        assert.ok(
            values.some((value) => value.includes("敬拜 🙌")),
            id + " must retain actual lyric signal"
        )
        assert.ok(
            actual.histories.some((entry) => entry.location.page === "show"),
            id
        )
        const corrupted = frontendOracle(id)
        corrupted.consume(files.map((file) => ({ ...file, content: file.content.replaceAll("敬拜 🙌", "CHANGED") })))
        assert.notDeepEqual(corrupted.converted, actual.converted, "Actual converter must observe text, not return a constant fixture")
    }
})

test("ZIP text entries retain exact source metadata, ignore selected nonarchives, and leave third-party/binary bundles explicit", async (t) => {
    const dir = temporary(t),
        ignored = write(dir, "ignored.xml", "ignored")
    const cases = []
    for (const id of formats) {
        const extension = id === "opensong" ? "" : id === "songbeamer" ? ".SNG" : id === "videopsalm" ? ".JSON" : ".XML"
        const archive = await zip(dir, id + ".ZiP", [["folder/敬拜 🙌" + extension, samples[id]]])
        cases.push(request(dir, id, [ignored, archive]))
    }
    cases.push(request(dir, "mediashout", [await zip(dir, "cues.zip", [["folder/敬拜.ssc", samples.mediashout]])]))
    cases.push(request(dir, "openlp", [write(dir, "broken.zip", "broken")]), request(dir, "opensong", [await zip(dir, "empty.zip", [])]))
    await compare(cases)
    const third = await zip(dir, "third.qsp", [["song.xml", samples.quelea]])
    const vpc = await zip(dir, "third.vpc", [["songs.json", samples.videopsalm]])
    const probundle = await zip(dir, "third.proBundle", [["songs.json", "{}"]])
    const binaryEntry = await zip(dir, "binary.zip", [["song.sqlite", "SQLite format 3\0binary"]])
    const proEntry = await zip(dir, "proto.zip", [["song.pro", "protobuf bytes"]])
    const rejected = rust([request(dir, "quelea", [third]), request(dir, "videopsalm", [vpc]), request(dir, "opensong", [probundle]), request(dir, "openlp", [binaryEntry]), request(dir, "chordpro", [proEntry])])
    for (const result of rejected) {
        assert.equal(result.error.code, "unsupported_format")
        assert.equal(Object.hasOwn(result, "events"), false)
    }
    // Source really takes these differently: third-party qsp is ZIP text;
    // binary entries are strings, but .pro always invokes protobuf decoding.
    const qsp = transportOracle()
    await qsp.dispatch("quelea", [third])
    assert.equal(qsp.events[0].message.channel, "IMPORT2")
    const proto = transportOracle()
    await assert.rejects(proto.dispatch("chordpro", [proEntry]), /unsupported source operation/)
    assert.equal(proto.calls.special[0].kind, "protobuf")
})

test("source case-sensitive SQLite/MDB selections never silently fall back to valid text or archive success", async (t) => {
    const dir = temporary(t),
        xml = write(dir, "valid.xml", samples.openlp),
        sqlite = write(dir, "songs.sqlite", "SQLite format 3\0"),
        mdb = write(dir, "songs.mdb", "MDB"),
        archive = await zip(dir, "songs.zip", [["song.xml", samples.openlp]])
    const cases = [request(dir, "openlp", [xml, sqlite]), request(dir, "openlp", [archive, sqlite]), request(dir, "mediashout", [xml, mdb]), request(dir, "mediashout", [archive, mdb])]
    for (const result of rust(cases)) {
        assert.equal(result.error.code, "unsupported_format")
        assert.equal(Object.hasOwn(result, "events"), false)
        assert.deepEqual(result.error.priorEvents, [])
    }
    const sql = transportOracle()
    await sql.dispatch("openlp", [xml, sqlite, archive])
    assert.deepEqual(sql.calls.special, [{ kind: "sqlite", path: sqlite }])
    assert.deepEqual(sql.calls.reads, [])
    assert.deepEqual(sql.events, [])
    const access = transportOracle()
    await assert.rejects(access.dispatch("mediashout", [xml, mdb, archive]), /unsupported source operation/)
    assert.deepEqual(access.calls.reads, [mdb])
    assert.equal(access.calls.special[0].kind, "mdb")
    for (const [id, extension] of [
        ["openlp", "SQLITE"],
        ["mediashout", "MDB"]
    ]) {
        const upper = write(dir, "uppercase." + extension, "ordinary source text")
        const source = transportOracle()
        await source.dispatch(id, [xml, upper])
        assert.deepEqual(source.calls.special, [])
        assert.equal(source.events[0].message.data.data.length, 2)
        assert.equal(rust([request(dir, id, [xml, upper])])[0].error.code, "unsupported_format", "Uncertified uppercase binary suffix stays explicit")
    }
    for (const id of ["word", "powerpoint", "pdf", "powerkey", "easyworship", "softprojector", "propresenter", "lessons", "BIBLE"]) assert.equal(rust([request(dir, id, [xml])])[0].error.code, "unsupported_format", id)
})

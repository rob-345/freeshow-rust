// Source-only fixture preparation. Native app/converter execution remains smoke.
import assert from "node:assert/strict"
import fs from "node:fs"
import path from "node:path"
import vm from "node:vm"
import { execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import ts from "typescript"
import { root, original, provenance, transportOracle, frontendOracle } from "../parity/imports-text-oracle.mjs"
assert.equal(process.versions.node, "22.21.1")
const [mode, directory, ...extra] = process.argv.slice(2)
assert.ok(mode === "--fixtures" && directory && path.isAbsolute(directory) && extra.length === 0, "Expected --fixtures ABSOLUTE_DIRECTORY")
fs.mkdirSync(directory, { recursive: true })
assert.deepEqual(fs.readdirSync(directory), [], "Use a fresh isolated fixture directory")
const names = { openlp: "OpenLP", opensong: "OpenSong", mediashout: "MediaShout", quelea: "Quelea", songbeamer: "SongBeamer", easyslides: "EasySlides", verseview: "VerseVIEW", videopsalm: "VideoPsalm", chordpro: "ChordPro PRO" }
const title = (id) => `Native ${names[id]} 敬拜`
const lyric = (id) => `Lyric ${names[id]} 敬拜 🙌`
// Same source-shaped corpus as imports-text.test.mjs, with independent signals.
const samples = {
    openlp: `<song modifiedDate="2024-01-02"><properties><titles><title>${title("openlp")}</title></titles><authors><author type="words">Writer</author></authors><verseOrder>v1 c1</verseOrder></properties><lyrics><verse name="v1"><lines>${lyric("openlp")}</lines></verse><verse name="c1"><lines>Hallelujah</lines></verse></lyrics></song>`,
    opensong: `<song><title>${title("opensong")}</title><author>Writer</author><lyrics>[V1]\n ${lyric("opensong")}\n[C1]\n Hallelujah</lyrics><presentation>V1 C1</presentation></song>`,
    mediashout: `<MediaShout5_Document><Cues><Cue><SongID>native-mediashout-text-fixture</SongID><Title>${title("mediashout")}</Title><Content><Element><Text>${lyric("mediashout")}\nHallelujah</Text></Element></Content></Cue></Cues></MediaShout5_Document>`,
    quelea: `<song><title>${title("quelea")}</title><author>Writer</author><lyrics><section title="Verse"><lyrics>${lyric("quelea")}\nHallelujah</lyrics></section></lyrics></song>`,
    songbeamer: `\ufeff#Title=${title("songbeamer")}\r\n#Author=Writer\r\n#Comments=Tm90ZXMg5pWs5ouc\r\n---\r\nVerse 1\r\n${lyric("songbeamer")}\r\nHallelujah\r\n`,
    easyslides: `<Easyslides><Item><Title1>${title("easyslides")}</Title1><Contents>[Verse 1]\n${lyric("easyslides")}\nHallelujah</Contents><SongNumber>7</SongNumber></Item><Item><Title1>Native EasySlides Second 敬拜</Title1><Contents>Second EasySlides lyric 敬拜 🙌</Contents></Item></Easyslides>`,
    verseview: `<songDB><song><name><![CDATA[${title("verseview")}]]></name><category><![CDATA[Category]]></category><author><![CDATA[Writer]]></author><slide><![CDATA[${lyric("verseview")}<br>Hallelujah]]></slide></song></songDB>`,
    videopsalm: JSON.stringify({ Text: "Album", Songs: [{ Guid: "native-videopsalm-text-fixture", Text: title("videopsalm"), Author: "Writer", Sequence: "V1 C1", Verses: [{ Text: lyric("videopsalm") }, { Text: "Hallelujah" }] }] }),
    chordpro: `{title: ${title("chordpro")}}\n{artist: Native Artist}\n[C]${lyric("chordpro")}\n`
}
const chordFile = "src/frontend/converters/chordpro.ts"
const chordSource = execFileSync("git", ["show", `${provenance.oracle}:${chordFile}`], { cwd: root, encoding: "utf8", timeout: 5000, maxBuffer: 1024 * 1024 }).replaceAll("\r\n", "\n")
const chordHash = createHash("sha256").update(chordSource).digest("hex")
assert.equal(chordHash, "760d1e8966349505ad4215f484303b86ff45353073ec24abd0f7739830c571e7")
function chordOracle(files) {
    const converted = [],
        stores = { activePopup: { set() {} }, alertMessage: { set() {} }, activeShow: { value: { id: "default" } }, templates: { value: {} }, drawerTabsData: { value: { shows: { activeSubTab: "all" } } } }
    let count = 0
    const ast = (file, text) => ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true)
    const source = ast(chordFile, chordSource)
    const declarations = source.statements.filter((n) => ts.isFunctionDeclaration(n) || (ts.isVariableStatement(n) && n.declarationList.declarations.some((d) => ["metaKeys", "chorus", "defaultSlide"].includes(d.name.getText(source))))).map((n) => n.getText(source).replace(/^export\s+/, ""))
    const showAst = ast("Show.ts", original("src/frontend/classes/Show.ts"))
    declarations.unshift(
        showAst.statements
            .find((n) => ts.isClassDeclaration(n) && n.name?.text === "ShowObj")
            .getText(showAst)
            .replace(/^export\s+/, "")
    )
    const context = vm.createContext({ ...stores, uid: () => `fixture-${++count}`, get: (s) => s.value, clone: structuredClone, checkName: (n) => n, translateText: (n) => n, DEFAULT_ITEM_STYLE: "controlled-source-style", setTimeout: (fn) => fn(), setTempShows: (shows) => converted.push(...JSON.parse(JSON.stringify(shows))) })
    const main = ast("responsesMain.ts", original("src/frontend/IPC/responsesMain.ts"))
    let handler
    function visit(n) {
        if (ts.isPropertyAssignment(n) && n.name.getText(main) === "[ToMain.IMPORT2]") handler = n.initializer.getText(main)
        ts.forEachChild(n, visit)
    }
    visit(main)
    const code = declarations.join("\n") + `\nglobalThis.consume=${handler}`
    vm.runInContext(ts.transpileModule(code, { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText, context, { timeout: 1000 })
    context.consume({ channel: "chordpro", data: files })
    return converted
}
const showText = (show) =>
    Object.values(show.slides || {})
        .flatMap((slide) => (slide.items || []).flatMap((item) => (item.lines || []).flatMap((line) => (line.text || []).map((run) => run.value || ""))))
        .join("\n")
const requests = [],
    shows = [],
    sourceProof = []
for (const [id, sample] of Object.entries(samples)) {
    const extension = id === "opensong" ? "" : id === "songbeamer" ? ".SNG" : id === "videopsalm" ? ".JSON" : id === "chordpro" ? ".pro" : ".XML"
    const file = path.join(directory, `Native ${names[id]} fixture` + extension)
    fs.writeFileSync(file, sample)
    const transport = transportOracle()
    await transport.dispatch(id, [file])
    assert.equal(transport.events.length, 1)
    assert.deepEqual(transport.calls.special, [], "No special/binary handlers permitted")
    const message = transport.events[0].message
    assert.deepEqual(Object.keys(message), ["channel", "data"])
    assert.deepEqual(Object.keys(message.data), ["channel", "data"])
    assert.equal(message.channel, "IMPORT2")
    assert.equal(message.data.channel, id)
    const raw = message.data.data
    assert.equal(raw.length, 1)
    assert.deepEqual(Object.keys(raw[0]), ["content", "path", "name", "extension"])
    const converted = id === "chordpro" ? chordOracle(raw) : frontendOracle(id).consume(raw)
    const expected = [{ name: title(id), text: lyric(id) }, ...(id === "easyslides" ? [{ name: "Native EasySlides Second 敬拜", text: "Second EasySlides lyric 敬拜 🙌" }] : [])]
    assert.equal(converted.length, expected.length, id)
    for (const item of expected) {
        const actual = converted.find(({ show }) => show.name === item.name)
        assert.ok(actual && showText(actual.show).includes(item.text), id + " must produce actual unique show/lyric")
    }
    const changed = raw.map((item) => ({ ...item, content: item.content.replaceAll(lyric(id), "CHANGED SOURCE CONTROL") }))
    const different = id === "chordpro" ? chordOracle(changed) : frontendOracle(id).consume(changed)
    assert.ok(
        different.every(({ show }) => !showText(show).includes(lyric(id))),
        "Source converter must observe actual lyric bytes"
    )
    requests.push({ id, paths: [file], expectedRaw: raw, expectedFileKeys: raw.map(Object.keys), expectedShows: expected.map((show) => show.name) })
    shows.push(...expected)
    sourceProof.push({ id, file, bytes: Buffer.byteLength(sample), sha256: createHash("sha256").update(sample).digest("hex"), fileKeys: Object.keys(raw[0]), converted: converted.map(({ show }) => ({ name: show.name, text: showText(show) })), changedTextObserved: true })
}
assert.equal(requests.length, 9)
assert.equal(shows.length, 10)
assert.equal(new Set(shows.map((show) => show.name)).size, 10)
process.stdout.write(JSON.stringify({ requests, shows, sourceProof, source: { pin: provenance.oracle, sourceOnly: true, xmlDom: "controlled xmldom adaptation; actual browser still required", chordproSha256: chordHash, provenance: provenance.sources } }) + "\n")

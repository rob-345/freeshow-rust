// Immutable original handler and real private GIO peer; no Spotify account.
import assert from "node:assert/strict"
import { execFileSync } from "node:child_process"
import { createHash } from "node:crypto"
import { fileURLToPath } from "node:url"
import ts from "typescript"
import { commandBus } from "../parity/spotify-command-oracle.mjs"

assert.equal(process.versions.node, "22.21.1")
assert.equal(process.platform, "linux")
const [phase, ...extra] = process.argv.slice(2)
assert.equal(extra.length, 0)
assert.ok(["ordinary", "held-switch"].includes(phase))
const sourceCommit = "2d94defcf98175e4f5c9504e7f4fd1d33c35c975"
const hashes = {
    "src/electron/utils/spotify.ts": "75d1ceab68e46c2a21dfc5a61f4e667f1d7d130e7b34b377938b7a97082918fd",
    "src/electron/IPC/responsesMain.ts": "70372b48b698f74f239b82f92e48dde69880a7c4d78af1a9f0e1723313fb57a3",
    "src/electron/IPC/main.ts": "11afb38cdc1ca36134b48b7457da2cd27078dedd8b6d7a54cf24e0ecc0608609"
}
const root = fileURLToPath(new URL("../../", import.meta.url))
let responses
for (const [file, hash] of Object.entries(hashes)) {
    const text = execFileSync("git", ["show", `${sourceCommit}:${file}`], { cwd: root, encoding: "utf8", timeout: 5000, maxBuffer: 1024 * 1024 }).replaceAll("\r\n", "\n")
    assert.equal(createHash("sha256").update(text).digest("hex"), hash)
    if (file.endsWith("responsesMain.ts")) responses = text
}
const fromBits = (hex) => Buffer.from(hex, "hex").readDoubleBE()
const commands = phase === "ordinary" ? [{ command: "playpause" }, { command: "next" }, { command: "prev" }, { command: "pause" }, { command: "setVolume", valueBits: "3fe0000000000001" }, { command: "seek", valueBits: "40080000431bde83" }] : [{ command: "seek", valueBits: "40080000431bde83" }]
assert.equal(fromBits(commands.at(-1).valueBits), 3.0000005)
const bus = await commandBus("hold")
const observed = { phase, commands, replies: [], effects: [], scheduled: [], sourceErrors: 0, earlyBeforeSeekEffect: false, update: null, cleanup: null, provenance: { sourceCommit, hashes, platform: "linux", node: process.versions.node } }
try {
    for (const input of commands) {
        const listener = `spotify-command-probe-${phase}-${input.command}`
        const payload = { command: input.command, ...(input.valueBits ? { value: fromBits(input.valueBits) } : {}) }
        const before = bus.events().length
        assert.equal(await bus.original.send(payload, listener), undefined)
        const sourceReply = bus.original.calls.replies.at(-1)
        assert.deepEqual(sourceReply, ["MAIN", { channel: "SPOTIFY_COMMAND", data: true }, listener])
        const messageKeys = Object.keys(sourceReply[1])
        const args = sourceReply.slice(1).length
        assert.deepEqual(messageKeys, ["channel", "data"])
        assert.equal(args, 2)
        observed.replies.push({ message: sourceReply[1], messageKeys, args, listener: sourceReply[2], sendUndefined: true })
        await bus.peer.wait(() => bus.events().length >= before + 1)
        if (input.command === "seek") {
            assert.equal(bus.events().at(-1).name, "Get")
            assert.equal(
                bus.events().some((e) => e.name === "SetPosition"),
                false
            )
            observed.earlyBeforeSeekEffect = true
            if (phase === "held-switch") {
                const parsed = ts.createSourceFile("responsesMain.ts", responses, ts.ScriptTarget.Latest, true)
                let handler
                const visit = (n) => {
                    if (ts.isPropertyAssignment(n) && n.name.getText(parsed) === "[Main.UPDATE_DATA_PATH]") handler = n.initializer.getText(parsed)
                    ts.forEachChild(n, visit)
                }
                visit(parsed)
                assert.ok(handler)
                const effects = []
                const update = new Function("config", "createStores", `return (${handler})`)({ set: (...args) => effects.push(["config.set", ...args]) }, (old) => effects.push(["createStores", old]))
                assert.equal(update({ oldPath: "/fixture/old", newPath: "/fixture/new" }), undefined)
                assert.deepEqual(effects, [
                    ["config.set", "dataPath", "/fixture/new"],
                    ["createStores", "/fixture/old"]
                ])
                observed.update = { handlerSha256: createHash("sha256").update(handler).digest("hex"), effects, returnsUndefined: true, whileSeekMetadataHeld: true }
            }
            bus.peer.child.stdin.write("release\n")
            await bus.peer.wait(() => bus.events().length === before + 2)
            assert.equal(bus.events().at(-1).name, "SetPosition")
        }
        await bus.settle()
    }
    observed.effects = bus.events()
    observed.scheduled = bus.original.calls.commands
    observed.sourceErrors = bus.original.calls.errors
    assert.equal(observed.sourceErrors, 0)
    assert.equal(observed.effects.length, phase === "ordinary" ? 7 : 2)
    assert.deepEqual(observed.effects.at(-1), { name: "SetPosition", args: ["/fixture/track_01", 3000001] })
} finally {
    await bus.close()
    observed.cleanup = { sourceChildrenSettled: true, peerAndPrivateBusWaited: true }
}
process.stdout.write(JSON.stringify(observed) + "\n")

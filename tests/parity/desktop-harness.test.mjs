import assert from "node:assert/strict"
import fs from "node:fs"
import vm from "node:vm"
import { test } from "node:test"
import { root } from "./oracle.mjs"

const source = fs.readFileSync(`${root}/tests/desktop/smoke.py`, "utf8")
const wrapper = source.match(/wrapped = """([\s\S]*?)"""\.replace\("KEY"/)[1]

function execute(context, key, script) {
    const code = wrapper.replace("KEY", JSON.stringify(key)).replace("SOURCE", script)
    return new vm.Script(`(function(){${code}})()`).runInContext(context)
}

test("desktop script receipt recovers a lost response without repeating the original side effect", () => {
    const context = vm.createContext({ sends: 0 })
    context.window = context
    const script = "sends++; return {sent:sends}"
    // Discard the first result, as when the HTTP proxy drops its response.
    execute(context, "request-1", script)
    assert.equal(execute(context, "request-1", script).sent, 1)
    assert.equal(context.sends, 1)
    assert.equal(execute(context, "request-2", script).sent, 2)
    // Bound retained references without discarding the latest receipt.
    for (let index = 0; index < 200; index++) execute(context, `later-${index}`, script)
    assert.equal(context.__desktopScriptReceipts.size, 128)
    const sends = context.sends
    execute(context, "later-199", script)
    assert.equal(context.sends, sends)
})

test("desktop script receipt retains failures after partial work and isolates WebViews", () => {
    const context = vm.createContext({ sends: 0 })
    context.window = context
    const script = "sends++; throw new Error('after send')"
    assert.throws(() => execute(context, "failed", script), /after send/)
    assert.throws(() => execute(context, "failed", script), /after send/)
    assert.equal(context.sends, 1)
    const other = vm.createContext({ sends: 0 })
    other.window = other
    execute(other, "failed", "sends++")
    assert.equal(other.sends, 1)
    assert.equal(context.sends, 1)
})

test("desktop script receipt reuses an asynchronous invocation and its completed result", async () => {
    const context = vm.createContext({ sends: 0 })
    context.window = context
    const source = "sends++;return Promise.resolve({value:sends})"
    const first = execute(context, "async", source)
    const repeated = execute(context, "async", source)
    assert.equal(first, repeated)
    assert.equal((await repeated).value, 1)
    assert.equal((await execute(context, "async", source)).value, 1)
    assert.equal(context.sends, 1)
})

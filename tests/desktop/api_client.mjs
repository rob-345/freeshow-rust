// Real external clients; actions and dynamic values run in the retained Svelte UI.
import assert from "node:assert/strict"
import { performance } from "node:perf_hooks"
import { io } from "socket.io-client"

let input = ""
for await (const chunk of process.stdin) input += chunk
const { port, password, showId, expectedText } = JSON.parse(input)
const replies = [],
    started = performance.now()
function check(reply) {
    assert.equal(reply.action, "get_show")
    assert.equal(reply.id, showId)
    assert.equal(reply.returnId, undefined)
    assert.equal(typeof reply.data, "object")
    assert.ok(JSON.stringify(reply.data).includes(expectedText), `Missing current show text: ${expectedText}`)
    return { action: reply.action, id: reply.id, data: reply.data }
}
for (const target of [port, port + 1]) {
    const response = await fetch(`http://127.0.0.1:${target}/`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ action: "get_show", id: showId, returnId: "native-reused-id", token: password }),
        signal: AbortSignal.timeout(5000)
    })
    assert.equal(response.status, 200)
    replies.push({ transport: "REST", port: target, reply: check(await response.json()) })
}
const denied = await fetch(`http://127.0.0.1:${port + 1}/?action=get_show`, { signal: AbortSignal.timeout(3000) })
assert.equal(denied.status, 401)
for (const transport of ["polling", "websocket"]) {
    const socket = io(`http://127.0.0.1:${port}`, { transports: [transport], auth: { password }, reconnection: false, timeout: 3000, forceNew: true })
    try {
        await new Promise((resolve, reject) => {
            const timer = setTimeout(() => reject(new Error(`Native ${transport} connect timeout`)), 4000)
            socket.once("connect", () => {
                clearTimeout(timer)
                resolve()
            })
            socket.once("connect_error", (error) => {
                clearTimeout(timer)
                reject(error)
            })
        })
        const request = (data) =>
            new Promise((resolve, reject) => {
                const timer = setTimeout(() => reject(new Error(`Native ${transport} reply timeout`)), 4000)
                socket.once("data", (reply) => {
                    clearTimeout(timer)
                    resolve(reply)
                })
                socket.emit("data", data)
            })
        const action = check(await request({ action: "get_show", id: showId, returnId: "native-reused-id" }))
        const variables = await request({ isVariable: true, keys: ["date_year"] })
        assert.equal(variables.isVariable, true)
        assert.equal(typeof variables.values, "object")
        assert.equal(typeof variables.values.date_year, "string")
        assert.ok(["Stopped", "Paused", "Playing"].includes(variables.values.timer_status))
        replies.push({ transport, reply: action, variables })
    } finally {
        socket.disconnect()
    }
}
process.stdout.write(JSON.stringify({ replies, unauthorizedStatus: denied.status, elapsedMs: performance.now() - started }) + "\n")

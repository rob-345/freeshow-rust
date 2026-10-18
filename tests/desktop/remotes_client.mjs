// Real external clients reach the unchanged main-window authentication/handlers.
import assert from "node:assert/strict"
import { io } from "socket.io-client"
let input = ""
for await (const chunk of process.stdin) input += chunk
const { endpoints, password, stagePassword, showId, expectedText } = JSON.parse(input)
const receipts = []
for (const transport of ["polling", "websocket"]) {
    for (const kind of ["REMOTE", "STAGE", "CONTROLLER"]) {
        const socket = io("http://" + endpoints[kind], { transports: [transport], forceNew: true, reconnection: false, timeout: 3000 })
        try {
            await new Promise((resolve, reject) => {
                const timer = setTimeout(() => reject(new Error(kind + " connect deadline")), 4000)
                socket.once("connect", () => {
                    clearTimeout(timer)
                    resolve()
                })
                socket.once("connect_error", (error) => {
                    clearTimeout(timer)
                    reject(error)
                })
            })
            const replies = []
            socket.on(kind, (message) => replies.push(message))
            const request = (channel, data, predicate = (message) => message.channel === channel) =>
                new Promise((resolve, reject) => {
                    const timer = setTimeout(() => {
                        socket.off(kind, receive)
                        reject(new Error(kind + " " + channel + " reply deadline: " + JSON.stringify(replies)))
                    }, 4000)
                    const receive = (message) => {
                        if (!predicate(message)) return
                        clearTimeout(timer)
                        socket.off(kind, receive)
                        resolve(message)
                    }
                    socket.on(kind, receive)
                    socket.emit(kind, { channel, data, id: socket.id })
                })
            if (kind === "REMOTE") {
                const challenge = await request("PASSWORD", null)
                assert.equal(challenge.data.password, true)
                const denied = await request("ACCESS", "wrong-native-password", (message) => message.channel === "ERROR")
                assert.equal(denied.data, "wrongPass")
                await request("ACCESS", password)
                const show = await request("API:get_show", { id: showId }, (message) => message.channel === "API:get_show")
                assert.ok(JSON.stringify(show.data).includes(expectedText), JSON.stringify(show))
                assert.equal(show.id, socket.id)
                receipts.push({ kind, transport, challenge: true, denied: denied.data, show: show.data })
            } else if (kind === "STAGE") {
                const layouts = await request("LAYOUTS", null)
                assert.ok(layouts.data.some((layout) => layout.id === "native-stage-test" && layout.password === true))
                const denied = await request("LAYOUT", { id: "native-stage-test", password: "incorrect" }, (message) => message.channel === "ERROR")
                assert.equal(denied.data, "wrongPass")
                const layout = await request("LAYOUT", { id: "native-stage-test", password: stagePassword })
                assert.equal(layout.data.name, "Rust native Stage 敬拜")
                assert.equal(Object.hasOwn(layout.data, "password"), false)
                assert.deepEqual(layout.data.items, {})
                receipts.push({ kind, transport, denied: denied.data, layout: layout.data })
            } else {
                const output = await request("GET_OUTPUT_ID", null)
                assert.equal(typeof output.data, "string")
                assert.ok(output.data.length)
                receipts.push({ kind, transport, outputId: output.data })
            }
        } finally {
            socket.disconnect()
        }
    }
}
process.stdout.write(JSON.stringify({ receipts }) + "\n")

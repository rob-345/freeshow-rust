// Execute in the real main WebView after onboarding. The driver supplies
// {requests:[{id,paths}], shows:[{name,text?,unknownKey?,unknownValue?}]}.
// When readyToSave becomes true, the driver sends its real Ctrl+S shortcut,
// then sets window.__importsProbe.saveRequested=true. No renderer stores or
// conversion functions are replaced; imports traverse window.api end to end.
const fixture = arguments[0]
const invoke = window.__TAURI__.core.invoke
const context = window.api.getRuntimeContext()
const proof = (window.__importsProbe = { context, events: [], requests: [], saved: [], readyToSave: false, saveRequested: false, done: false })
const listener = "native-imports-probe"
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
const until = async (condition, description, timeout = 15000) => {
    const started = performance.now()
    while (performance.now() - started < timeout) {
        const value = await condition()
        if (value) return value
        await pause(25)
    }
    throw new Error(`Import probe timed out: ${description}`)
}
window.api.receive(
    "MAIN",
    (message, listenerId) => {
        if (["IMPORT2", "ALERT"].includes(message.channel)) proof.events.push({ message: structuredClone(message), listenerId: listenerId ?? null })
    },
    listener
)
;(async () => {
    try {
        for (const request of fixture.requests) {
            const before = proof.events.length
            window.api.send("MAIN", { channel: "IMPORT_FILES", data: request })
            const event = await until(() => proof.events.slice(before).find((event) => event.message.channel === "IMPORT2" && event.message.data.channel === request.id), request.id)
            if (event.listenerId !== null) throw new Error("Original IMPORT2 transport has no listener ID")
            if (!Array.isArray(event.message.data.data) || !event.message.data.data.length) throw new Error("Import produced no data")
            proof.requests.push({ id: request.id, raw: event.message.data.data })
        }
        // Text converters defer their real conversion/history operation by 10–50ms.
        await pause(150)
        for (const expected of fixture.shows) {
            await until(() => document.body.innerText.includes(expected.name), `converted show visible: ${expected.name}`)
        }
        proof.readyToSave = true
        await until(() => proof.saveRequested, "driver Ctrl+S", 30000)
        for (const expected of fixture.shows) {
            const reply = await until(async () => {
                const events = await invoke("ipc_dispatch", { request: { channel: "MAIN", context, data: { channel: "SHOW", data: { name: expected.name } } } })
                const data = events.find((event) => event.message.channel === "SHOW")?.message.data
                return data?.content ? data : null
            }, `converted show saved: ${expected.name}`)
            const show = reply.content[1]
            const text = Object.values(show.slides || {})
                .flatMap((slide) => (slide.items || []).flatMap((item) => (item.lines || []).flatMap((line) => (line.text || []).map((span) => span.value || ""))))
                .join("\n")
            if (show.name !== expected.name) throw new Error(`Saved imported name differs: ${show.name}`)
            if (expected.text && !text.includes(expected.text)) throw new Error(`Saved imported text differs: ${expected.name}`)
            if (expected.unknownKey && JSON.stringify(show[expected.unknownKey]) !== JSON.stringify(expected.unknownValue)) throw new Error(`Unknown imported field lost: ${expected.unknownKey}`)
            proof.saved.push({ name: show.name, id: reply.content[0], text, unknown: expected.unknownKey ? show[expected.unknownKey] : null })
        }
        if (JSON.stringify(window.api.getRuntimeContext()) !== JSON.stringify(context)) throw new Error("Import probe context changed")
    } catch (error) {
        proof.error = String(error)
    } finally {
        proof.done = true
        window.api.removeListener("MAIN", listener)
    }
})()

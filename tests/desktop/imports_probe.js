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
        if (proof.error) throw new Error(proof.error)
        const value = await condition()
        if (value) return value
        await pause(25)
    }
    throw new Error(`Import probe timed out: ${description}`)
}
const visibleShows = async (names) => {
    const remaining = new Set(names)
    const viewport = document.querySelector(".drawer svelte-virtual-list-viewport")
    const initialTop = viewport?.scrollTop || 0
    try {
        // Shows uses a real VirtualList. Scan its ordinary viewport, rather
        // than requiring every newly imported row to be mounted at once.
        for (let step = 0; step < 64; step++) {
            const text = document.body.innerText
            for (const name of remaining) if (text.includes(name)) remaining.delete(name)
            if (!remaining.size) return true
            if (!viewport || viewport.clientHeight <= 0) return false
            const top = Math.min(step * Math.max(30, viewport.clientHeight * 0.75), Math.max(0, viewport.scrollHeight - viewport.clientHeight))
            viewport.scrollTop = top
            viewport.dispatchEvent(new Event("scroll"))
            await pause(25)
            if (top >= viewport.scrollHeight - viewport.clientHeight && step > 0) {
                for (const name of remaining) if (document.body.innerText.includes(name)) remaining.delete(name)
                return remaining.size === 0
            }
        }
        return false
    } finally {
        if (viewport) {
            viewport.scrollTop = initialTop
            viewport.dispatchEvent(new Event("scroll"))
        }
    }
}
window.api.receive(
    "MAIN",
    (...args) => {
        const [message, listenerId] = args
        if (!message || !["IMPORT2", "ALERT", "IMPORT_FILES"].includes(message.channel)) return
        if (message.channel === "IMPORT_FILES") {
            proof.error = "Unexpected original import success acknowledgement"
            return
        }
        if (args.length !== 2 || listenerId !== undefined || JSON.stringify(Object.keys(message)) !== JSON.stringify(["channel", "data"])) {
            proof.error = "Original import callback shape/private fields changed"
            return
        }
        if (proof.events.length >= 64) {
            proof.error = "Unexpected excess import events"
            return
        }
        if (message.channel === "IMPORT2" && message.data.channel === "freeshow_project") {
            const path = JSON.parse(message.data.data[0].content).files[0]
            proof.importUrlAtCallback = window.api.resolveFilePath?.(path)
        }
        if (["IMPORT2", "ALERT"].includes(message.channel)) proof.events.push({ message: structuredClone(message), listenerId: listenerId ?? null })
    },
    listener
)
;(async () => {
    try {
        for (const request of fixture.requests) {
            const before = proof.events.length
            const sent = window.api.send("MAIN", { channel: "IMPORT_FILES", data: { id: request.id, paths: request.paths } })
            if (sent !== undefined) throw new Error("Original import send must return undefined")
            const event = await until(() => proof.events.slice(before).find((event) => event.message.channel === "IMPORT2" && event.message.data.channel === request.id), request.id)
            if (event.listenerId !== null) throw new Error("Original IMPORT2 transport has no listener ID")
            if (!Array.isArray(event.message.data.data) || !event.message.data.data.length) throw new Error("Import produced no data")
            if (request.expectedRaw) {
                const rows = event.message.data.data
                if (rows.length !== request.expectedRaw.length) throw new Error(`Native raw import count differs: ${request.id}`)
                for (let index = 0; index < rows.length; index++) {
                    const keys = request.expectedFileKeys[index]
                    // WebDriver may reorder object members in fixture args.
                    // Compare captured key arrays and exact field values apart.
                    if (JSON.stringify(Object.keys(rows[index])) !== JSON.stringify(keys) || keys.some((key) => rows[index][key] !== request.expectedRaw[index][key])) throw new Error(`Native raw import differs from immutable source: ${request.id}`)
                }
            }
            proof.requests.push({ id: request.id, raw: event.message.data.data, fileKeys: event.message.data.data.map(Object.keys), sendUndefined: true, rawMatchesSource: request.expectedRaw ? true : null })
            if (request.expectedShows) await until(() => visibleShows(request.expectedShows), `retained ${request.id} converter`)
        }
        // Text converters defer their real conversion/history operation by 10–50ms.
        await pause(150)
        for (const request of fixture.requests.filter((request) => request.expectedRaw)) {
            const replies = proof.events.filter((event) => event.message.channel === "IMPORT2" && event.message.data.channel === request.id && event.message.data.data?.[0]?.path === request.expectedRaw[0].path)
            if (replies.length !== 1) throw new Error(`Duplicate or missing original import reply: ${request.id}`)
        }
        await until(() => visibleShows(fixture.shows.map((show) => show.name)), "all converted shows visible in actual virtual list")
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

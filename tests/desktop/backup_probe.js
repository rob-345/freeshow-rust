// Executed in the main WebView. Send real cues through window.api while an
// actual ZIP job runs; both output WebViews report visible renderer progress.
const templates = arguments[0]
const texts = arguments[1]
const ids = Object.keys(templates)
const invoke = window.__TAURI__.core.invoke
const context = window.api.getRuntimeContext()
const observed = (window.__responsiveness = { samples: [], done: false, probesComplete: false, context, outputIds: ids })
const waiting = new Map()
window.api.receive(
    "OUTPUT",
    (message) => {
        if (message.channel !== "MAIN_PROBE_ACK") return
        const ack = message.data
        const pending = waiting.get(ack.probeId)
        if (!pending || !ids.includes(ack.outputId)) return
        if (ack.phase === "received") {
            pending.receipts[ack.outputId] = performance.now() - pending.started
            return
        }
        pending.acks[ack.outputId] = { ...ack, transportRoundTripMs: pending.receipts[ack.outputId], roundTripMs: performance.now() - pending.started }
        if (ids.every((id) => pending.acks[id])) {
            clearTimeout(pending.timeout)
            waiting.delete(ack.probeId)
            pending.resolve(pending.acks)
        }
    },
    "native-projection-ack"
)
const started = performance.now()
invoke("ipc_dispatch", { request: { channel: "MAIN", listenerId: "backup-responsiveness", context, data: { channel: "SAVE", data: { customTriggers: { backup: true, isAutoBackup: true }, closeWhenFinished: false } } } })
    .then((events) => {
        observed.backup = events.find((event) => event.message.channel === "BACKUP" && event.message.data.finished)?.message.data
        observed.backupMs = performance.now() - started
        observed.done = true
    })
    .catch((error) => {
        observed.error = error
        observed.done = true
    })
;(async () => {
    let sequence = 0
    try {
        while (!observed.done && performance.now() - started < 90000 && observed.samples.length < 1024) {
            const before = await invoke("library_status")
            if (before.activeJob?.listenerId === "backup-responsiveness") {
                const probeId = `projection-${sequence}`
                const cueData = {}
                ids.forEach((id, index) => {
                    const slideIndex = (sequence + index) % 2
                    const cue = structuredClone(templates[id])
                    cue.out.slide.index = slideIndex
                    cue.__nativeProbe = { probeId, expectedText: texts[slideIndex], forbiddenText: texts[1 - slideIndex] }
                    cueData[id] = cue
                })
                const cueStarted = performance.now()
                const rendered = new Promise((resolve, reject) => {
                    const timeout = setTimeout(() => {
                        waiting.delete(probeId)
                        reject(new Error(`Projection acknowledgments timed out: ${probeId}`))
                    }, 3500)
                    waiting.set(probeId, { resolve, acks: {}, receipts: {}, timeout, started: cueStarted })
                })
                window.api.send("OUTPUT", { channel: "OUTPUTS", data: cueData })
                const probeStart = performance.now()
                const controls = Promise.all([invoke("runtime_context"), invoke("ipc_dispatch", { request: { channel: "MAIN", data: { channel: "MAXIMIZED" } } }), invoke("ipc_dispatch", { request: { channel: "MAIN", data: { channel: "GET_OS" } } })]).then(([current, maximized, os]) => ({ current, maximized, os, elapsedMs: performance.now() - probeStart }))
                const [control, acks] = await Promise.all([controls, rendered])
                const after = await invoke("library_status")
                if (after.activeJob?.listenerId === "backup-responsiveness" && !observed.done) {
                    observed.samples.push({ ...control, acks, sequence, sinceBackupStartMs: performance.now() - started, queueCapacity: after.queueCapacity })
                }
                sequence++
            }
            await new Promise((resolve) => setTimeout(resolve, 100))
        }
    } catch (error) {
        observed.probeError = String(error)
    }
    observed.probesComplete = true
    window.api.removeListener("OUTPUT", "native-projection-ack")
})()

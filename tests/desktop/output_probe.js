// Executed in a real output WebView by WebDriver. Observe the existing Svelte
// renderer; no store, DOM or rendering implementation is replaced by the probe.
const outputId = arguments[0]
const probeState = (window.__projectionProbe = { received: 0, completed: 0, errors: [] })

function visibleText(text) {
    return [...document.querySelectorAll(".textContainer")].some((node) => {
        if (!node.textContent.includes(text)) return false
        const rect = node.getBoundingClientRect()
        if (rect.width <= 0 || rect.height <= 0) return false
        let opacity = 1
        for (let element = node; element; element = element.parentElement) {
            const style = getComputedStyle(element)
            if (style.display === "none" || style.visibility === "hidden") return false
            opacity *= Number(style.opacity)
        }
        return opacity >= 0.98
    })
}

window.api.receive(
    "OUTPUT",
    (message) => {
        if (message.channel !== "OUTPUTS") return
        const probe = message.data?.[outputId]?.__nativeProbe
        if (!probe?.probeId) return
        probeState.received++
        const started = performance.now()
        const context = window.api.getRuntimeContext()
        window.api.send("OUTPUT", { channel: "MAIN_PROBE_ACK", data: { probeId: probe.probeId, outputId, phase: "received" } })
        let finished = false
        const reply = async (error) => {
            if (finished) return
            finished = true
            try {
                // Exercise the actual native handshake on this output while
                // the library worker is busy. It must not join the IO queue.
                const ready = await window.__TAURI__.core.invoke("ipc_dispatch", { request: { channel: "STARTUP", data: { channel: "READY" }, context } })
                window.api.send("OUTPUT", {
                    channel: "MAIN_PROBE_ACK",
                    data: {
                        probeId: probe.probeId,
                        outputId,
                        phase: "rendered",
                        context,
                        role: ready[0]?.message.data,
                        renderMs: performance.now() - started,
                        text: document.body.innerText,
                        visibility: document.visibilityState,
                        ...(error ? { error } : {})
                    }
                })
                probeState.completed++
            } catch (failure) {
                probeState.errors.push(String(failure))
            }
        }
        function frame() {
            if (visibleText(probe.expectedText) && !visibleText(probe.forbiddenText)) {
                // Cross another frame boundary after the visible state. This
                // observes DOM/CSS/RAF progress, not physical display scanout.
                requestAnimationFrame(() => reply())
            } else if (performance.now() - started > 2000) {
                reply("Projection text did not finish its transition")
            } else {
                requestAnimationFrame(frame)
            }
        }
        requestAnimationFrame(frame)
    },
    "native-projection-probe"
)

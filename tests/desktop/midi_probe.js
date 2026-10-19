return (async () => {
    const context = window.api.getRuntimeContext(),
        results = []
    for (const command of ["GET_MIDI_INPUTS", "GET_MIDI_OUTPUTS"]) {
        const id = "native-midi-getter-" + crypto.randomUUID()
        results.push(
            await new Promise((resolve, reject) => {
                const cleanup = () => {
                    clearTimeout(timer)
                    window.api.removeListener("MAIN", id)
                    window.removeEventListener("freeshow:runtime-error", failed)
                }
                const failed = (event) => {
                    if (event.detail?.request?.listenerId !== id) return
                    cleanup()
                    resolve({ command, unavailable: event.detail.error })
                }
                const timer = setTimeout(() => {
                    cleanup()
                    reject(new Error("Native MIDI getter deadline elapsed"))
                }, 4000)
                window.addEventListener("freeshow:runtime-error", failed)
                window.api.receive(
                    "MAIN",
                    (message, listenerId) => {
                        if (listenerId !== id || message.channel !== command) return
                        cleanup()
                        resolve({ command, message, listenerId, expectedListenerId: id })
                    },
                    id
                )
                window.api.send("MAIN", { channel: command, data: { ignored: true, backend: "spoofed", names: ["not a device"] } }, id)
            })
        )
    }
    const closed = await window.__TAURI__.core.invoke("ipc_dispatch", { request: { channel: "MAIN", data: { channel: "CLOSE_MIDI", data: { id: "not-an-open-listener" } }, listenerId: "void-close", context } })
    const status = await window.__TAURI__.core.invoke("midi_status")
    return { context, current: window.api.getRuntimeContext(), results, closed, status, hardwareDeliveryVerified: false }
})()

// Real window.api listener/correlation, independent native encoder worker.
return (async () => {
    const context = window.api.getRuntimeContext(),
        invoke = window.__TAURI__.core.invoke
    const before = await invoke("encoders_status")
    const id = "native-encoder-" + crypto.randomUUID(),
        started = performance.now()
    const reply = await new Promise((resolve, reject) => {
        const cleanup = () => {
            clearTimeout(timer)
            window.api.removeListener("MAIN", id)
            window.removeEventListener("freeshow:runtime-error", failed)
        }
        const failed = (event) => {
            if (event.detail?.request?.listenerId !== id) return
            cleanup()
            reject(new Error(JSON.stringify(event.detail.error)))
        }
        const timer = setTimeout(() => {
            cleanup()
            reject(new Error("Native encoder detection deadline"))
        }, 38000)
        window.addEventListener("freeshow:runtime-error", failed)
        window.api.receive(
            "MAIN",
            (message, listenerId) => {
                if (listenerId !== id || message.channel !== "ENCODER_DETECT") return
                cleanup()
                resolve({ message, listenerId })
            },
            id
        )
        window.api.send("MAIN", { channel: "ENCODER_DETECT", data: arguments[0] }, id)
    })
    if (JSON.stringify(window.api.getRuntimeContext()) !== JSON.stringify(context)) throw new Error("Encoder context changed")
    return {
        context,
        before,
        reply,
        elapsedMs: performance.now() - started,
        status: await invoke("encoders_status")
    }
})()

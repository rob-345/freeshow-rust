return (async () => {
    const context = window.api.getRuntimeContext(),
        started = performance.now(),
        invoke = window.__TAURI__.core.invoke
    const request = (payload) =>
        new Promise((resolve, reject) => {
            const id = "native-scripture-" + crypto.randomUUID()
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
                reject(new Error("Scripture callback deadline"))
            }, 15000)
            window.addEventListener("freeshow:runtime-error", failed)
            window.api.receive(
                "MAIN",
                (message, listenerId) => {
                    if (listenerId !== id || message.channel !== "BIBLE") return
                    cleanup()
                    resolve({ data: message.data, listenerId, expectedListenerId: id })
                },
                id
            )
            window.api.send("MAIN", { channel: "BIBLE", data: payload }, id)
        })
    const payloads = arguments[0]
    // Two whole host leases; the third request follows collection of that pair.
    const replies = await Promise.all(payloads.slice(0, 2).map(request))
    for (const payload of payloads.slice(2)) replies.push(await request(payload))
    return { context, current: window.api.getRuntimeContext(), replies, status: await invoke("scriptures_status"), elapsedMs: performance.now() - started }
})()

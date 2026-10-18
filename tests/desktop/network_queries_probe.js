return (async () => {
    const started = performance.now(),
        context = window.api.getRuntimeContext()
    const before = await window.__TAURI__.core.invoke("network_queries_status")
    const replies = []
    for (const data of [null, false, 17, "ignored", [], {}, { interfaces: { invented: [{ address: "203.0.113.99", family: "IPv4" }] }, hostname: "spoofed", port: 1234, path: "/not/read" }]) {
        const id = "native-network-query-" + crypto.randomUUID()
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
                reject(new Error("Native IP callback deadline elapsed"))
            }, 2500)
            window.addEventListener("freeshow:runtime-error", failed)
            window.api.receive(
                "MAIN",
                (message, listenerId) => {
                    if (listenerId !== id || message.channel !== "IP") return
                    cleanup()
                    resolve({ message, listenerId, expectedListenerId: id })
                },
                id
            )
            window.api.send("MAIN", { channel: "IP", data }, id)
        })
        replies.push(reply)
    }
    return { context, current: window.api.getRuntimeContext(), before, status: await window.__TAURI__.core.invoke("network_queries_status"), replies, elapsedMs: performance.now() - started }
})()

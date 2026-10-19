return (async () => {
    const context = window.api.getRuntimeContext(),
        progress = [],
        id = "native-ffmpeg-download-" + crypto.randomUUID()
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
            reject(new Error("Native download callback deadline elapsed"))
        }, 5000)
        window.addEventListener("freeshow:runtime-error", failed)
        window.api.receive(
            "MAIN",
            (message, listenerId) => {
                if (message.channel === "MEDIA_DOWNLOAD_PROGRESS" && message.data?.url === "ffmpeg") progress.push(message.data)
                if (message.channel !== "FFMPEG_DOWNLOAD" || listenerId !== id) return
                cleanup()
                resolve({ message, listenerId, expectedListenerId: id })
            },
            id
        )
        window.api.send("MAIN", { channel: "FFMPEG_DOWNLOAD", data: { url: "https://untrusted.invalid/override", path: "/not-authorized", future: true } }, id)
    })
    return { context, current: window.api.getRuntimeContext(), reply, progress, status: await window.__TAURI__.core.invoke("ffmpeg_download_status") }
})()

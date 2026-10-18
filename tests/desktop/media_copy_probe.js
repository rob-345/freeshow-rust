return (async () => {
    const fixture = arguments[0], context = window.api.getRuntimeContext(), invoke = window.__TAURI__.core.invoke
    window.__mediaCopyObservations ||= {}
    const observations = { alerts: [], replies: [], context }
    const id = "native-media-copy-" + crypto.randomUUID()
    window.__mediaCopyObservations[id] = observations
    if (fixture.action === "collect") {
        delete window.__mediaCopyObservations[id]
        const retained = window.__mediaCopyObservations[fixture.id]
        window.api.removeListener("MAIN", fixture.id)
        delete window.__mediaCopyObservations[fixture.id]
        return { ...retained, status: await invoke("media_copy_status"), current: window.api.getRuntimeContext() }
    }
    window.api.receive("MAIN", (message, listenerId) => {
        if (message.channel === "ALERT") observations.alerts.push(message.data)
        if (listenerId === id) observations.replies.push({ channel: message.channel, data: message.data, listenerId })
    }, id)
    if (fixture.command === "BUNDLE_MEDIA_FILES") {
        if (fixture.setting !== undefined) window.api.send("MAIN", { channel: "SET_MEDIA_FOLDER_PATH", data: fixture.setting })
        window.api.send("MAIN", { channel: fixture.command, data: fixture.data }, id)
        return { id, context }
    }
    return await new Promise((resolve, reject) => {
        const cleanup = () => {
            clearTimeout(timer)
            window.api.removeListener("MAIN", id)
            window.api.removeListener("MAIN", id + "-reply")
            window.removeEventListener("freeshow:runtime-error", failed)
            delete window.__mediaCopyObservations[id]
        }
        const failed = (event) => {
            if (event.detail?.request?.listenerId !== id) return
            cleanup()
            reject(new Error(JSON.stringify(event.detail.error)))
        }
        const timer = setTimeout(() => { cleanup(); reject(new Error("Media copy callback deadline")) }, 15000)
        window.addEventListener("freeshow:runtime-error", failed)
        // Register before send, keeping the observation listener independent.
        window.api.receive("MAIN", async (message, listenerId) => {
            if (listenerId !== id || message.channel !== fixture.command) return
            const result = { ...observations, reply: message.data, listenerId, expectedListenerId: id, status: await invoke("media_copy_status"), current: window.api.getRuntimeContext() }
            window.api.removeListener("MAIN", id + "-reply")
            cleanup()
            resolve(result)
        }, id + "-reply")
        if (fixture.setting !== undefined) window.api.send("MAIN", { channel: "SET_MEDIA_FOLDER_PATH", data: fixture.setting })
        window.api.send("MAIN", { channel: fixture.command, data: fixture.data }, id)
    })
})()

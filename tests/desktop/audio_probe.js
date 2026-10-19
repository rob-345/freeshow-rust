// Controlled PCM through the retained preload API in the actual WebKit view.
return (async () => {
    const fixture = arguments[0],
        invoke = window.__TAURI__.core.invoke,
        context = window.api.getRuntimeContext(),
        observer = "native-audio-probe-" + crypto.randomUUID(),
        config = fixture?.config,
        statuses = [],
        errors = [],
        sourceAcks = [],
        script = { phase: fixture?.phase, config, title: "", packets: [] }
    if (!config || config.host !== "127.0.0.1" || !Number.isInteger(config.port) || config.port < 1024 || config.port > 65535 || !config.enabled || typeof config.mount !== "string" || typeof config.password !== "string") throw new Error("AUDIO probe requires a controlled loopback peer")
    const capability = await invoke("audio_capability")
    if (!capability?.Available) throw new Error("Actual native Opus unavailable: " + JSON.stringify(capability))
    if (capability.Available.version !== "libopus 1.4") throw new Error("Desktop exact-byte oracle requires actual libopus 1.4")
    const before = await invoke("audio_status")
    if (!Number.isSafeInteger(before.ingress?.completedPackets) || !Number.isSafeInteger(before.ingress?.acceptedPackets)) throw new Error("AUDIO needs actual ingress packet completion counters")
    const same = () => {
        const now = window.api.getRuntimeContext()
        return now?.runtimeId === context.runtimeId && now.libraryGeneration === context.libraryGeneration && now.dataPath === context.dataPath
    }
    const wait = async (test, description, ms = 6000) => {
        const deadline = performance.now() + ms
        for (;;) {
            if (!same()) throw new Error("AUDIO probe library context changed")
            if (errors.length) throw new Error("AUDIO runtime error: " + JSON.stringify(errors))
            const result = await test()
            if (result) return result
            if (performance.now() >= deadline) throw new Error("AUDIO deadline: " + description)
            await new Promise((resolve) => setTimeout(resolve, 10))
        }
    }
    const drained = (status) => status.ingress?.retained === 0 && status.ingress.queued === 0 && status.ingress.active === false && status.ingress.groupOutstanding === 0 && status.host?.retained === 0
    const failed = (event) => {
        const detail = event.detail
        if (detail?.request?.channel === "AUDIO" || String(detail?.error?.code || "").startsWith("audio_")) {
            if (errors.length >= 16) throw new Error("AUDIO error receipt bound")
            errors.push({ code: detail.error?.code, message: String(detail.error?.message || "").slice(0, 2048) })
        }
    }
    window.addEventListener("freeshow:runtime-error", failed)
    window.api.receive(
        "AUDIO",
        (message, listenerId) => {
            if (sourceAcks.length >= 16) throw new Error("AUDIO ACK receipt bound")
            sourceAcks.push({ message, listenerId })
        },
        observer + "-audio"
    )
    window.api.receive(
        "MAIN",
        (message, listenerId) => {
            if (listenerId === observer) {
                if (sourceAcks.length >= 16) throw new Error("AUDIO ACK receipt bound")
                sourceAcks.push({ message, listenerId })
            }
            if (message.channel !== "ICECAST_STATUS") return
            if (statuses.length >= 16) throw new Error("AUDIO status receipt bound")
            const data = message.data
            statuses.push({
                channel: message.channel,
                listenerIdUndefined: listenerId === undefined,
                keys: Object.keys(data),
                errorOwn: Object.prototype.hasOwnProperty.call(data, "error"),
                errorUndefined: data.error === undefined,
                connected: data.connected,
                connecting: data.connecting,
                ...(data.error === undefined ? {} : { error: data.error })
            })
        },
        observer + "-main"
    )
    let port,
        response,
        responseHandler,
        timer,
        closeSent = false
    try {
        const handshake = new Promise((resolve, reject) => {
            timer = setTimeout(() => reject(new Error("AUDIO transferred port deadline")), 5000)
            responseHandler = (event) => {
                if (event.source !== window || event.data?.type !== "AUDIO_PORT_RESPONSE" || event.data.targetId !== "icecast") return
                if (event.ports.length !== 1 || !(event.ports[0] instanceof MessagePort)) return reject(new Error("AUDIO handshake lacks one actual MessagePort"))
                response = { data: event.data, keys: Object.keys(event.data), portCount: event.ports.length, actualMessagePort: true }
                port = event.ports[0]
                port.start()
                resolve()
            }
            window.addEventListener("message", responseHandler)
        })
        window.api.send("AUDIO", { channel: "INIT_PORT", data: { id: "icecast" } }, observer)
        await handshake
        clearTimeout(timer)
        window.removeEventListener("message", responseHandler)
        if (JSON.stringify(response.keys) !== JSON.stringify(["type", "targetId"])) throw new Error("AUDIO handshake own-key order mismatch")
        await wait(async () => drained(await invoke("audio_status")), "pre-packet ingress drain")
        for (const [index, origin] of ["port", "port", "PCM", "CAPTURE"].entries()) {
            const baseline = await invoke("audio_status")
            const storage = new ArrayBuffer(16 + 960 * 2 * 4 + 16)
            new Uint8Array(storage).fill(0xa7)
            const planar = new Float32Array(storage, 16, 1920)
            let seed = ((fixture.seed ?? 12345) + index) >>> 0
            for (let n = 0; n < planar.length; n++) {
                seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0
                planar[n] = ((seed % 200001) - 100000) / 80000
            }
            const exact = new Uint8Array(storage, 16, 7680)
            let binary = ""
            for (const byte of exact) binary += String.fromCharCode(byte)
            script.packets.push({ origin, planarBase64: btoa(binary), sampleRate: 48000, id: "icecast", connectDropped: index === 0 })
            const buffer = index === 1 ? new DataView(storage, 16, 7680) : index === 2 ? planar : exact
            const payload = { id: "icecast", buffer, sampleRate: 48000, icecast: config }
            if (origin === "port") port.postMessage({ channel: "AUDIO", payload })
            else window.api.send("AUDIO", { channel: origin, data: payload }, observer)
            // The adapter must snapshot the exact range before reuse. Port
            // structured clone also snapshots before this mutation.
            new Uint8Array(storage).fill(0x19)
            await wait(
                async () => {
                    const status = await invoke("audio_status")
                    return status.ingress.completedPackets >= baseline.ingress.completedPackets + 1 && drained(status) && status
                },
                "actual " + origin + " packet completed"
            )
            if (index === 0) await wait(() => statuses.some((status) => status.connected === true && status.connecting === false), "retained connected status")
        }
        const closeAfter = statuses.length
        window.api.send("AUDIO", { channel: "CLOSE_PORT", data: { id: "icecast" } }, observer)
        closeSent = true
        await wait(() => statuses.slice(closeAfter).some((status) => status.connected === false && status.connecting === false), "retained disconnected status")
        const after = await wait(async () => {
            const status = await invoke("audio_status")
            return drained(status) && status
        }, "post-close actual ingress drain")
        if (after.ingress.completedPackets !== before.ingress.completedPackets + 4 || after.ingress.acceptedPackets !== before.ingress.acceptedPackets + 4) throw new Error("Controlled AUDIO phase had an unexpected extra/missing packet")
        if (sourceAcks.length) throw new Error("AUDIO void command acquired a fabricated source ACK")
        for (const status of statuses) if (!status.listenerIdUndefined || !status.errorOwn || !status.errorUndefined || JSON.stringify(status.keys) !== JSON.stringify(["connected", "connecting", "error"])) throw new Error("ICECAST_STATUS undefined/own-key/source envelope mismatch: " + JSON.stringify(status))
        return { phase: fixture.phase, context, current: window.api.getRuntimeContext(), capability, before, response, statuses, sourceAcks, script, after, errors, workletCapability: { AudioWorkletNode: typeof AudioWorkletNode === "function", controlledPortInput: true, actualWorkletCaptureExercised: false }, physicalCaptureVerified: false, playbackVerified: false }
    } finally {
        clearTimeout(timer)
        if (responseHandler) window.removeEventListener("message", responseHandler)
        if (!closeSent) window.api.send("AUDIO", { channel: "CLOSE_PORT", data: { id: "icecast" } }, observer)
        port?.close()
        window.api.removeListener("AUDIO", observer + "-audio")
        window.api.removeListener("MAIN", observer + "-main")
        window.removeEventListener("freeshow:runtime-error", failed)
    }
})()

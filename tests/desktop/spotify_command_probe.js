return (async () => {
    const { phase, command, valueBits, action = "command" } = arguments[0]
    const context = window.api.getRuntimeContext()
    if (!context || typeof context.runtimeId !== "string" || !context.runtimeId || !Number.isSafeInteger(context.libraryGeneration) || context.libraryGeneration < 0 || typeof context.dataPath !== "string" || JSON.stringify(Object.keys(context).sort()) !== JSON.stringify(["dataPath", "libraryGeneration", "runtimeId"])) throw Error("Spotify command requires actual full context")
    if (!["command", "routes"].includes(action) || typeof phase !== "string" || !/^[a-z-]{1,32}$/.test(phase)) throw Error("Unknown bounded fixture action/phase")
    const frozen = Object.freeze({ ...context })
    const same = (v) => !!v && v.runtimeId === frozen.runtimeId && v.libraryGeneration === frozen.libraryGeneration && v.dataPath === frozen.dataPath
    const listener = `spotify-command-probe-${phase}-${command || "routes"}`
    let timer, settle, failure, callback, runtimeError
    const result = { phase, context: frozen, action, callbacks: [], sendUndefined: null }
    try {
        window.api.receive(
            "MAIN",
            (...args) => {
                const [message, id] = args
                if (!message || message.channel !== "SPOTIFY_COMMAND" || (action !== "routes" && id !== listener)) return
                try {
                    if (action === "routes") throw Error("Rejected native route fabricated an original callback")
                    if (args.length !== 2 || JSON.stringify(Object.keys(message)) !== JSON.stringify(["channel", "data"]) || message.data !== true || !same(window.api.getRuntimeContext())) throw Error("Original Spotify callback shape/context differs")
                    if (result.callbacks.length) throw Error("Duplicate Spotify command callback")
                    result.callbacks.push({ message: structuredClone(message), messageKeys: Object.keys(message), args: args.length, listener: id, callbackContext: window.api.getRuntimeContext() })
                    settle = setTimeout(() => callback(), 25)
                } catch (error) {
                    failure ||= error
                    callback?.(error)
                }
            },
            listener
        )
        if (action === "routes") {
            const invoke = window.__TAURI__.core.invoke,
                request = { channel: "MAIN", context: frozen, listenerId: listener, data: { channel: "SPOTIFY_COMMAND", data: { command: "pause" } } }
            result.rejections = []
            for (const [route, payload] of [
                ["ipc_dispatch", { request }],
                ["spotify_command", { request }]
            ]) {
                try {
                    await invoke(route, payload)
                    throw Error("Nonraw Spotify route unexpectedly accepted")
                } catch (error) {
                    if (error?.code !== "spotify_raw_body") throw error
                    result.rejections.push({ route, code: error.code })
                }
            }
            await new Promise((resolve) => setTimeout(resolve, 25))
            if (failure) throw failure
        } else {
            if (!["playpause", "next", "prev", "pause", "setVolume", "seek"].includes(command)) throw Error("Unknown fixture command")
            const payload = { command }
            if (["seek", "setVolume"].includes(command)) {
                if (!/^[0-9a-f]{16}$/.test(valueBits)) throw Error("Expected exact IEEE fixture bits")
                const view = new DataView(new ArrayBuffer(8))
                view.setBigUint64(0, BigInt("0x" + valueBits))
                payload.value = view.getFloat64(0)
                if (!Number.isFinite(payload.value)) throw Error("Fixture number must be finite")
            }
            result.sent = { command, ...(valueBits ? { valueBits } : {}) }
            await new Promise((resolve, reject) => {
                callback = (error) => (error ? reject(error) : resolve())
                runtimeError = (event) => {
                    const request = event.detail?.request
                    if (request?.data?.channel === "SPOTIFY_COMMAND" || request?.message?.channel === "SPOTIFY_COMMAND" || request?.listenerId === listener) callback(Error("Spotify command runtime error: " + String(event.detail?.error?.code)))
                }
                window.addEventListener("freeshow:runtime-error", runtimeError)
                timer = setTimeout(() => callback(Error("Spotify original early reply deadline")), 5000)
                result.sendUndefined = window.api.send("MAIN", { channel: "SPOTIFY_COMMAND", data: payload }, listener) === undefined
                payload.command = "unknown"
                payload.value = 99
                if (!result.sendUndefined) callback(Error("Original command send must remain void"))
            })
            if (failure) throw failure
        }
        result.current = window.api.getRuntimeContext()
        if (!same(result.current)) throw Error("Command probe changed selected context")
        return result
    } finally {
        clearTimeout(timer)
        clearTimeout(settle)
        window.api.removeListener("MAIN", listener)
        if (runtimeError) window.removeEventListener("freeshow:runtime-error", runtimeError)
    }
})()

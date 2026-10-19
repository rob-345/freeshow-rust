return (async () => {
    const { phase, ignoredRoot } = arguments[0]
    const context = window.api.getRuntimeContext()
    if (!context || typeof context.runtimeId !== "string" || typeof context.libraryGeneration !== "number" || typeof context.dataPath !== "string") throw new Error("AI/Spotify requires real full context")
    const frozen = Object.freeze({ ...context })
    const same = (value) => !!value && value.runtimeId === frozen.runtimeId && value.libraryGeneration === frozen.libraryGeneration && value.dataPath === frozen.dataPath
    const channels = ["AI_GET_BIN", "SPOTIFY_GET_STATE"]
    const listener = "ai-spotify-probe-" + phase
    const ids = Object.fromEntries(channels.map((channel) => [channel, listener + "-" + channel]))
    const result = { phase, context: frozen, callbacks: [], sendUndefined: {}, rawIdentityArchived: false }
    let timer
    let failed
    try {
        await new Promise((resolve, reject) => {
            failed = (event) => {
                const request = event.detail?.request
                const channel = request?.data?.channel || request?.message?.channel
                if (!channels.includes(channel) && !Object.values(ids).includes(request?.listenerId)) return
                const code = String(event.detail?.error?.code || "unknown").slice(0, 96)
                reject(new Error("Unexpected AI/Spotify runtime error: " + code))
            }
            window.addEventListener("freeshow:runtime-error", failed)
            timer = setTimeout(() => reject(new Error("AI/Spotify retained callback deadline")), 6000)
            window.api.receive(
                "MAIN",
                (...args) => {
                    const [message, correlation] = args
                    if (correlation !== ids[message?.channel]) return
                    try {
                        if (args.length !== 2 || !same(window.api.getRuntimeContext())) throw new Error("AI/Spotify callback shape or context changed")
                        if (JSON.stringify(Object.keys(message)) !== JSON.stringify(["channel", "data"])) throw new Error("Private fields escaped the original callback payload")
                        if (result.callbacks.includes(message.channel)) throw new Error("Duplicate AI/Spotify callback")
                        const observed = { messageKeys: Object.keys(message), args: args.length, listener: correlation, callbackContext: window.api.getRuntimeContext(), data: message.data }
                        if (message.channel === "AI_GET_BIN") {
                            if (!Array.isArray(message.data) || message.data.length !== 1) throw new Error("Expected actual nonempty AI inventory")
                            observed.rowKeys = message.data.map((row) => Object.keys(row))
                            if (JSON.stringify(observed.rowKeys) !== JSON.stringify([["path", "name", "size"]])) throw new Error("Original AI row keys/order changed")
                            result.ai = observed
                        } else {
                            if (message.data !== null) throw new Error("Missing private Spotify service must return original null, never unsupported")
                            result.spotify = observed
                        }
                        result.callbacks.push(message.channel)
                        if (result.callbacks.length === channels.length) resolve()
                    } catch (error) {
                        reject(error)
                    }
                },
                listener
            )
            for (const channel of channels) {
                const sent = window.api.send("MAIN", { channel, data: { path: ignoredRoot, userData: ignoredRoot, engine: "ignored", platform: "spoofed", title: "not Spotify", connected: true } }, ids[channel])
                result.sendUndefined[channel] = sent === undefined
                if (sent !== undefined) reject(new Error("Original AI/Spotify send must return undefined"))
            }
        })
        result.current = window.api.getRuntimeContext()
        if (!same(result.current)) throw new Error("AI/Spotify probe crossed library context")
        return result
    } finally {
        clearTimeout(timer)
        window.api.removeListener("MAIN", listener)
        if (failed) window.removeEventListener("freeshow:runtime-error", failed)
    }
})()

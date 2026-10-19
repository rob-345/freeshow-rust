return (async () => {
    const { expectedIdentity, phase } = arguments[0]
    const context = window.api.getRuntimeContext()
    if (!context || typeof context.runtimeId !== "string" || !Number.isSafeInteger(context.libraryGeneration) || typeof context.dataPath !== "string") throw new Error("Device/provider probe requires full runtime context")
    const frozen = Object.freeze({ ...context })
    const same = (value) => !!value && value.runtimeId === frozen.runtimeId && value.libraryGeneration === frozen.libraryGeneration && value.dataPath === frozen.dataPath
    const result = { phase, context: frozen, callbacks: [], sendUndefined: {} }
    const listener = "device-provider-probe-" + phase
    const ids = {
        DEVICE_ID: listener + "-identity",
        PROVIDER_CONNECTIONS: listener + "-providers",
        GET_CONTENT_PROVIDERS: listener + "-catalogue"
    }
    const channels = Object.keys(ids)
    let timer
    let failed
    let failure
    try {
        await new Promise((resolve, reject) => {
            const fail = (error) => {
                failure ||= error
                reject(error)
            }
            failed = (event) => {
                const request = event.detail?.request
                const channel = request?.data?.channel || request?.message?.channel
                if (!Object.hasOwn(ids, channel) && !Object.values(ids).includes(request?.listenerId)) return
                const code = String(event.detail?.error?.code || "unknown").slice(0, 96)
                fail(new Error("Unexpected device/provider runtime error: " + code))
            }
            window.addEventListener("freeshow:runtime-error", failed)
            timer = setTimeout(() => reject(new Error("Device/provider retained callback deadline")), 4000)
            window.api.receive(
                "MAIN",
                (...args) => {
                    const [message, correlation] = args
                    // An unrelated callback without correlation must not match
                    // a missing ids entry (undefined === undefined).
                    if (!message || typeof message.channel !== "string" || !Object.hasOwn(ids, message.channel) || correlation !== ids[message.channel]) return
                    try {
                        if (result.callbacks.includes(message.channel)) throw new Error("Duplicate device/provider callback")
                        if (args.length !== 2 || !same(window.api.getRuntimeContext())) throw new Error("Device/provider callback shape or context changed")
                        const messageKeys = Object.keys(message)
                        if (JSON.stringify(messageKeys) !== JSON.stringify(["channel", "data"])) throw new Error("Private fields escaped the original device/provider callback")
                        const metadata = { messageKeys, args: args.length, listener: correlation, callbackContext: window.api.getRuntimeContext() }
                        if (message.channel === "DEVICE_ID") {
                            // Never copy native identity bytes into the durable receipt.
                            result.identity = {
                                ...metadata,
                                equalsPersisted: message.data === expectedIdentity,
                                type: typeof message.data,
                                nonempty: typeof message.data === "string" && !!message.data,
                                legacyFallback: message.data === "fallback-machine-id"
                            }
                        } else if (message.channel === "PROVIDER_CONNECTIONS") {
                            result.providers = { ...metadata, data: message.data, keys: Object.keys(message.data) }
                        } else {
                            if (!Array.isArray(message.data) || message.data.length !== 5) throw new Error("Expected the five declared provider rows")
                            const rowKeys = message.data.map((row) => Object.keys(row))
                            if (rowKeys.some((keys) => JSON.stringify(keys) !== JSON.stringify(["providerId", "displayName", "hasContentLibrary"]))) throw new Error("Declared provider row keys/order changed")
                            for (const row of message.data) if (typeof row.providerId !== "string" || typeof row.displayName !== "string" || typeof row.hasContentLibrary !== "boolean") throw new Error("Declared provider field types changed")
                            result.catalogue = { ...metadata, data: message.data, rowKeys }
                        }
                        result.callbacks.push(message.channel)
                        if (result.callbacks.length === channels.length) resolve()
                    } catch (error) {
                        fail(error)
                    }
                },
                listener
            )
            for (const channel of channels) {
                const sent = window.api.send(
                    "MAIN",
                    {
                        channel,
                        data: {
                            ignored: "payload cannot select a global store",
                            machineId: "untrusted",
                            contentProviders: { fake: true }
                        }
                    },
                    ids[channel]
                )
                result.sendUndefined[channel] = sent === undefined
                if (sent !== undefined) fail(new Error("Original device/provider send must return undefined"))
            }
        })
        // A synchronous duplicate after the final expected callback must still
        // reject, even if the promise was resolved earlier in that dispatch.
        if (failure) throw failure
        result.current = window.api.getRuntimeContext()
        if (!same(result.current)) throw new Error("Device/provider probe crossed runtime context")
        return result
    } finally {
        clearTimeout(timer)
        window.api.removeListener("MAIN", listener)
        if (failed) window.removeEventListener("freeshow:runtime-error", failed)
    }
})()

// Python owns the real regular-file blocker and its independent byte checks.
// This runs inside the retained main WebView; native invocation is status-only.
return (async () => {
    const fixture = arguments[0] || {},
        invoke = typeof arguments[1] === "function" ? arguments[1] : window.__TAURI__.core.invoke,
        context = structuredClone(window.api.getRuntimeContext()),
        { phase, action, audioFolder } = fixture,
        entry = fixture.case,
        observer = "native-now-playing-folder-" + crypto.randomUUID(),
        events = [],
        runtimeErrors = [],
        sourceAcks = [],
        browserErrors = [],
        deadline = performance.now() + 15000
    const utf8 = (value, maximum) => {
        if (typeof value !== "string" || value.length > maximum || value.includes("\0")) return false
        for (let i = 0; i < value.length; i++) {
            const code = value.charCodeAt(i)
            if (code >= 0xd800 && code <= 0xdbff) {
                const low = value.charCodeAt(++i)
                if (!(low >= 0xdc00 && low <= 0xdfff)) return false
            } else if (code >= 0xdc00 && code <= 0xdfff) return false
        }
        return new TextEncoder().encode(value).length <= maximum
    }
    if (!context || !utf8(context.runtimeId, 256) || !context.runtimeId || !Number.isSafeInteger(context.libraryGeneration) || context.libraryGeneration < 0 || !utf8(context.dataPath, 32768) || !context.dataPath) throw new Error("Actual full native context required")
    if (typeof phase !== "string" || !phase || phase.length > 64 || !["set", "unset", "open"].includes(action) || !utf8(audioFolder, 32774) || (!audioFolder.endsWith("/Audio") && !audioFolder.endsWith("\\Audio"))) throw new Error("Invalid bounded native folder phase")
    if (action === "set" && (!entry || typeof entry.id !== "string" || entry.id.length > 64 || !utf8(entry.path, 32768) || !/\.wav$/i.test(entry.path) || !entry.data || typeof entry.data !== "object" || Array.isArray(entry.data) || JSON.stringify(entry.data).length > 65536)) throw new Error("SET requires the actual already-granted bounded WAV fixture")
    const expectedFolderMessage = `Error: Could not create folder at: ${audioFolder}!`,
        expected = action === "set" ? ["AUDIO_METADATA", ...Array(4).fill(expectedFolderMessage)] : action === "unset" ? Array(2).fill(expectedFolderMessage) : [expectedFolderMessage, "This does not exist!"],
        sameContext = (value) => !!value && value.runtimeId === context.runtimeId && value.libraryGeneration === context.libraryGeneration && value.dataPath === context.dataPath,
        text = (value) => String(value?.message ?? value ?? "").slice(0, 2048)
    let reads = 0,
        callbackError
    const fail = (event) => {
        if (runtimeErrors.length < 8) runtimeErrors.push({ code: String(event.detail?.error?.code || "").slice(0, 128), message: text(event.detail?.error) })
    }
    const browserFail = (event) => {
        if (browserErrors.length < 8) browserErrors.push(text(event.error || event.reason || event.message))
    }
    const receive = (message, listenerId) => {
        try {
            if (listenerId === observer || ["NOW_PLAYING", "NOW_PLAYING_UNSET", "OPEN_NOW_PLAYING"].includes(message?.channel)) {
                if (sourceAcks.length < 4) sourceAcks.push({ channel: String(message?.channel || "").slice(0, 128), listenerId: typeof listenerId === "string" ? listenerId.slice(0, 128) : null })
                throw new Error("Original now-playing void command acquired a source ACK")
            }
            if (!["AUDIO_METADATA", "ALERT"].includes(message?.channel)) return
            if (events.length >= 8 || !sameContext(window.api.getRuntimeContext()) || listenerId !== undefined || JSON.stringify(Object.keys(message)) !== '["channel","data"]') throw new Error("Source callback context/listener/private-field envelope mismatch")
            const position = events.length,
                common = { channel: message.channel, listenerIdUndefined: true, messageKeys: Object.keys(message), contextMatched: true, privateFieldsStripped: true }
            if (message.channel === "AUDIO_METADATA") {
                const data = message.data
                if (expected[position] !== "AUDIO_METADATA" || action !== "set" || !data || JSON.stringify(Object.keys(data)) !== '["filePath","metadata"]' || data.filePath !== entry.path || !data.metadata || typeof data.metadata !== "object" || Array.isArray(data.metadata)) throw new Error("Foreign or unexpected metadata callback")
                const metadataKeys = Object.keys(data.metadata)
                if (metadataKeys.length > 128 || metadataKeys.some((key) => !utf8(key, 1024)) || (data.metadata.picture !== undefined && (!Array.isArray(data.metadata.picture) || data.metadata.picture.length !== 0))) throw new Error("Expected bounded WAV common metadata without covers")
                events.push({ ...common, dataKeys: Object.keys(data), filePath: data.filePath, metadataKeys })
            } else {
                if (typeof message.data !== "string" || message.data !== expected[position]) throw new Error("Original folder/missing-text ALERT count, order or exact message mismatch")
                events.push({ ...common, data: message.data })
            }
        } catch (error) {
            callbackError ||= text(error)
        }
    }
    const healthy = () => {
        if (!sameContext(window.api.getRuntimeContext())) throw new Error("Native folder probe context changed")
        if (callbackError || runtimeErrors.length || browserErrors.length || sourceAcks.length) throw new Error("Folder probe callback/runtime failure: " + JSON.stringify({ callbackError, runtimeErrors, browserErrors, sourceAcks }))
    }
    const counter = (value) => typeof value === "string" && /^(0|[1-9][0-9]{0,19})$/.test(value) && BigInt(value) <= 18446744073709551615n
    const status = async () => {
        healthy()
        if (++reads > 1502 || performance.now() >= deadline) throw new Error("Native folder probe15s/status-read bound")
        let timer
        try {
            const value = await Promise.race([
                invoke("now_playing_status"),
                new Promise((_, reject) => {
                    timer = setTimeout(() => reject(new Error("Native folder status deadline")), Math.max(1, deadline - performance.now()))
                })
            ])
            if (performance.now() >= deadline) throw new Error("Native folder status exceeded15s deadline")
            if (!value || typeof value.closed !== "boolean" || !Number.isSafeInteger(value.retained) || value.retained < 0 || value.retained > 4 || !Number.isSafeInteger(value.contextObservers) || value.contextObservers < 0 || value.contextObservers > 4 || (value.host !== null && (typeof value.host !== "object" || Array.isArray(value.host)))) throw new Error("Actual bounded native Owner status required")
            if (value.closed || value.terminalError || value.host?.error || value.host?.closing || !sameContext(value.readyContext)) throw new Error("Native folder owner unavailable or foreign")
            if (value.host) {
                if (!sameContext(value.host.context)) throw new Error("Foreign native host context")
                for (const key of ["retained", "activeOperations", "preparations", "cleanups", "lifecycleObservers", "publications"]) if (!Number.isSafeInteger(value.host[key]) || value.host[key] < 0 || value.host[key] > (key === "publications" ? 20 : 4)) throw new Error("Invalid native status bound: " + key)
                for (const key of ["admitted", "completed", "revealCalls"]) if (!counter(value.host[key])) throw new Error("Actual native counter required: " + key)
                if (typeof value.host.currentPhase !== "boolean") throw new Error("Actual native phase status required")
            }
            healthy()
            return value
        } finally {
            clearTimeout(timer)
        }
    }
    const idle = (value) => value.retained === 0 && value.contextObservers === 0 && (!value.host || (value.host.retained === 0 && value.host.activeOperations === 0 && value.host.publications === 0 && value.host.preparations === 0 && value.host.cleanups === 0 && value.host.lifecycleObservers === 0 && value.host.currentPhase === false))
    const wait = async (check) => {
        for (;;) {
            const value = await status()
            if (check(value)) return value
            if (performance.now() >= deadline) throw new Error("Actual folder effects/receipts did not settle within15s")
            await new Promise((resolve) => setTimeout(resolve, 10))
        }
    }
    try {
        window.addEventListener("freeshow:runtime-error", fail)
        window.addEventListener("error", browserFail)
        window.addEventListener("unhandledrejection", browserFail)
        window.api.receive("MAIN", receive, observer)
        const before = await wait(idle),
            admitted = BigInt(before.host?.admitted || "0"),
            completed = BigInt(before.host?.completed || "0"),
            reveals = BigInt(before.host?.revealCalls || "0")
        let data
        if (action === "set") {
            data = structuredClone(entry.data)
            data.filePath = entry.path
            if (entry.durationKind !== undefined) {
                const kinds = { NegativeZero: -0, NaN, PositiveInfinity: Infinity, NegativeInfinity: -Infinity }
                if (!Object.hasOwn(kinds, entry.durationKind)) throw new Error("Invalid controlled duration kind")
                data.duration = kinds[entry.durationKind]
            }
        }
        const command = action === "set" ? "NOW_PLAYING" : action === "unset" ? "NOW_PLAYING_UNSET" : "OPEN_NOW_PLAYING"
        if (window.api.send("MAIN", { channel: command, ...(action === "set" ? { data } : {}) }, observer) !== undefined) throw new Error("Original source command must return undefined")
        const after = await wait((value) => {
            const host = value.host
            if (!host) return false
            if (BigInt(host.admitted) > admitted + 1n || BigInt(host.completed) > completed + 1n) throw new Error("Concurrent command contaminated folder probe")
            if (BigInt(host.revealCalls) !== reveals) throw new Error("Blocked Audio path unexpectedly invoked native OS reveal callback")
            return BigInt(host.admitted) === admitted + 1n && BigInt(host.completed) === completed + 1n && idle(value) && events.length === expected.length
        })
        healthy()
        const counts = {
            admitted: (BigInt(after.host.admitted) - admitted).toString(),
            completed: (BigInt(after.host.completed) - completed).toString(),
            revealCalls: (BigInt(after.host.revealCalls) - reveals).toString(),
            metadata: events.filter((event) => event.channel === "AUDIO_METADATA").length,
            folderAlerts: events.filter((event) => event.data === expectedFolderMessage).length,
            missingTextAlerts: events.filter((event) => event.data === "This does not exist!").length,
            sourceAcks: sourceAcks.length,
            runtimeErrors: runtimeErrors.length,
            browserErrors: browserErrors.length,
            statusReads: reads
        }
        return { phase, action, context, current: window.api.getRuntimeContext(), audioFolder, expectedFolderMessage, before, after, events, counts, sourceAcks, runtimeErrors, browserErrors, sendReturnUndefined: true, ordinaryIdleVerified: true, actualCounterAdvanceVerified: true, noNowPlayingRevealCallbackInvoked: true, terminalJoinVerified: false, filesystemVerifiedByProbe: false }
    } finally {
        window.api.removeListener("MAIN", observer)
        window.removeEventListener("freeshow:runtime-error", fail)
        window.removeEventListener("error", browserFail)
        window.removeEventListener("unhandledrejection", browserFail)
    }
})()

// Coordinator executes this in the actual main WebView. One SET/UNSET per call
// deliberately leaves real output files intact for independent Python FS checks.
return (async () => {
    const fixture = arguments[0] || {},
        invoke = typeof arguments[1] === "function" ? arguments[1] : window.__TAURI__.core.invoke,
        context = window.api.getRuntimeContext(),
        observer = "native-now-playing-probe-" + crypto.randomUUID(),
        events = [],
        errors = [],
        sourceAcks = [],
        browserErrors = [],
        action = fixture.action || "set",
        entry = fixture.case,
        phase = fixture.phase
    if (!context || typeof phase !== "string" || !phase || phase.length > 64 || !["set", "unset"].includes(action)) throw new Error("Invalid bounded now-playing desktop phase")
    if (action === "set" && (!entry || typeof entry.id !== "string" || entry.id.length > 64 || typeof entry.path !== "string" || !entry.path || entry.path.length > 32768 || !entry.data || typeof entry.data !== "object" || Array.isArray(entry.data))) throw new Error("SET requires an actual already-granted private fixture")
    const sameContext = (value) => !!value && value.runtimeId === context.runtimeId && value.libraryGeneration === context.libraryGeneration && value.dataPath === context.dataPath
    const same = () => sameContext(window.api.getRuntimeContext())
    const expectedCodes = action === "set" ? entry.expectErrorCodes || [] : []
    if (!Array.isArray(expectedCodes) || expectedCodes.length > 8 || expectedCodes.some((code) => typeof code !== "string" || code.length > 128)) throw new Error("Invalid expected error-code corpus")
    const text = (value) => String(value?.message ?? value ?? "").slice(0, 2048)
    const failed = (event) => {
        const detail = event.detail,
            channel = detail?.request?.channel,
            nested = detail?.request?.data?.channel,
            code = String(detail?.error?.code || "")
        if (channel !== "NOW_PLAYING" && !["NOW_PLAYING", "NOW_PLAYING_UNSET"].includes(nested) && !code.startsWith("now_playing_") && !code.startsWith("cover_")) return
        if (errors.length >= 8) throw new Error("Now-playing error receipt bound")
        errors.push({ code: code.slice(0, 128), message: text(detail.error) })
    }
    const browserFailed = (event) => {
        if (browserErrors.length < 8) browserErrors.push(text(event.error || event.reason || event.message))
    }
    const status = async () => {
        const value = await invoke("now_playing_status")
        if (!value || typeof value.closed !== "boolean" || !Number.isSafeInteger(value.contextObservers) || (value.host !== null && typeof value.host !== "object")) throw new Error("Actual now-playing Owner status schema is required")
        if (value.host) {
            for (const key of ["retained", "activeOperations", "publications", "preparations", "cleanups", "lifecycleObservers"]) if (!Number.isSafeInteger(value.host[key]) || value.host[key] < 0 || value.host[key] > 4) throw new Error("Invalid bounded native status " + key)
            for (const key of ["admitted", "completed"]) if (typeof value.host[key] !== "string" || !/^(0|[1-9][0-9]{0,19})$/.test(value.host[key])) throw new Error("Actual native command counter required: " + key)
            if (typeof value.host.currentPhase !== "boolean" || typeof value.host.closing !== "boolean") throw new Error("Actual native phase status required")
        }
        return value
    }
    const idle = (host) => !!host && host.retained === 0 && host.activeOperations === 0 && host.publications === 0 && host.preparations === 0 && host.cleanups === 0 && host.currentPhase === false && host.lifecycleObservers === 0
    const wait = async (test, description) => {
        const end = performance.now() + 15000
        for (;;) {
            if (!same()) throw new Error("Now-playing probe context changed")
            if (browserErrors.length) throw new Error("Browser failure during now-playing probe: " + JSON.stringify(browserErrors))
            const unexpected = errors.find((error) => !expectedCodes.includes(error.code))
            if (unexpected) throw new Error("Unexpected native now-playing failure: " + JSON.stringify(unexpected))
            const value = await test()
            if (value) return value
            if (performance.now() >= end) throw new Error("Now-playing deadline: " + description + " errors=" + JSON.stringify(errors))
            await new Promise((resolve) => setTimeout(resolve, 10))
        }
    }
    const snapshot = (metadata) => {
        let nodes = 4096,
            bytes = 65536
        const undefinedPaths = [],
            specialNumbers = [],
            pictureViews = []
        const raw = (value, path, depth) => {
            if (--nodes < 0 || depth > 16) throw new Error("Probe metadata work bound")
            if (value === undefined) {
                undefinedPaths.push(path)
                return ["undefined"]
            }
            if (value === null) return ["null"]
            if (value instanceof Uint8Array) {
                if (value.byteLength > 4096) throw new Error("Controlled fixture cover byte bound")
                bytes -= value.byteLength
                if (bytes < 0) throw new Error("Probe metadata byte bound")
                let binary = ""
                for (const byte of value) binary += String.fromCharCode(byte)
                pictureViews.push({ path, uint8Array: true, constructor: value.constructor.name, byteOffset: value.byteOffset, byteLength: value.byteLength, backingByteLength: value.buffer.byteLength })
                return ["bytes", btoa(binary)]
            }
            if (typeof value === "number") {
                const encoded = Object.is(value, -0) ? "-0" : Number.isFinite(value) ? value : String(value)
                if (typeof encoded === "string") specialNumbers.push({ path, value: encoded })
                return ["number", encoded]
            }
            if (typeof value === "boolean") return ["boolean", value]
            if (typeof value === "string") {
                bytes -= new TextEncoder().encode(value).length
                if (bytes < 0) throw new Error("Probe metadata byte bound")
                return ["string", value]
            }
            if (!value || typeof value !== "object") throw new Error("Unexpected metadata source value")
            const pointer = (key) => path + "/" + key.replace(/~/g, "~0").replace(/\//g, "~1")
            if (Array.isArray(value)) {
                if (value.length > nodes) throw new Error("Probe metadata array bound")
                return ["array", value.map((item, index) => raw(item, pointer(String(index)), depth + 1))]
            }
            return ["object", Object.keys(value).map((key) => [key, raw(value[key], pointer(key), depth + 1)])]
        }
        return { raw: raw(metadata, "/metadata", 0), undefinedPaths, specialNumbers, pictureViews }
    }
    window.addEventListener("freeshow:runtime-error", failed)
    window.addEventListener("error", browserFailed)
    window.addEventListener("unhandledrejection", browserFailed)
    window.api.receive(
        "MAIN",
        (message, listenerId) => {
            try {
                if (listenerId === observer) {
                    if (sourceAcks.length >= 4) throw new Error("Now-playing source ACK receipt bound")
                    sourceAcks.push({ channel: message?.channel, listenerId })
                }
                if (message?.channel !== "AUDIO_METADATA") return
                if (events.length >= 4) throw new Error("Now-playing metadata callback receipt bound")
                const data = message.data
                if (listenerId !== undefined || JSON.stringify(Object.keys(message)) !== JSON.stringify(["channel", "data"]) || !data || JSON.stringify(Object.keys(data)) !== JSON.stringify(["filePath", "metadata"])) throw new Error("Original metadata envelope/private-field stripping mismatch")
                if (action !== "set" || data.filePath !== entry.path) throw new Error("Foreign/concurrent metadata contaminated controlled probe")
                const observed = snapshot(data.metadata),
                    pictures = data.metadata.picture || []
                if (!Array.isArray(pictures) || pictures.some((picture) => !(picture.data instanceof Uint8Array))) throw new Error("Cover callbacks must contain real Uint8Array bytes")
                events.push({ channel: message.channel, listenerIdUndefined: true, messageKeys: Object.keys(message), dataKeys: Object.keys(data), filePath: data.filePath, metadataKeys: Object.keys(data.metadata), pictureKeys: pictures.map((picture) => Object.keys(picture)), ...observed })
            } catch (error) {
                if (errors.length < 8) errors.push({ code: "now_playing_probe_callback", message: text(error) })
            }
        },
        observer
    )
    try {
        const before = await wait(async () => {
            const value = await status()
            if (value.closed || value.host?.error || value.host?.closing) throw new Error("Native now-playing owner unavailable: " + JSON.stringify(value))
            return sameContext(value.readyContext) && value.contextObservers === 0 && (!value.host || (sameContext(value.host.context) && idle(value.host))) && value
        }, "native full-context admission and initial idle")
        const admitted = BigInt(before.host?.admitted || "0"),
            completed = BigInt(before.host?.completed || "0")
        let payload, script
        if (action === "set") {
            payload = structuredClone(entry.data)
            payload.filePath = entry.path
            if (entry.durationKind !== undefined) {
                const kinds = { NegativeZero: -0, NaN, PositiveInfinity: Infinity, NegativeInfinity: -Infinity }
                if (!Object.hasOwn(kinds, entry.durationKind)) throw new Error("Invalid private controlled duration kind")
                payload.duration = kinds[entry.durationKind]
            }
            script = { phase, action, case: structuredClone(entry), audioFolder: context.dataPath.replace(/[\\/]$/, "") + "/Audio" }
        } else script = { phase, action, audioFolder: context.dataPath.replace(/[\\/]$/, "") + "/Audio" }
        const returned = window.api.send("MAIN", { channel: action === "set" ? "NOW_PLAYING" : "NOW_PLAYING_UNSET", ...(action === "set" ? { data: payload } : {}) }, observer)
        if (returned !== undefined) throw new Error("Original now-playing void command returned an invented source ACK")
        if (payload) {
            payload.name = "MUTATED AFTER SEND"
            payload.filePath += ".never-authorized"
            payload.format = "MUTATED AFTER SEND"
            payload.duration = -1
            if (Array.isArray(payload.unknownLang)) payload.unknownLang.fill("MUTATED AFTER SEND")
        }
        const after = await wait(async () => {
            const value = await status(),
                host = value.host
            if (!host || !sameContext(host.context)) return false
            if (host.error) throw new Error("Actual native owner fault: " + JSON.stringify(host.error))
            if (BigInt(host.admitted) > admitted + 1n || BigInt(host.completed) > completed + 1n) throw new Error("Concurrent now-playing command contaminated controlled phase")
            const metadataReady = action === "unset" || entry.expectMetadata === false ? events.length === 0 : events.length === 1
            const errorReady = expectedCodes.length === 0 ? errors.length === 0 : errors.some((error) => expectedCodes.includes(error.code))
            return BigInt(host.admitted) === admitted + 1n && BigInt(host.completed) === completed + 1n && idle(host) && metadataReady && errorReady && value
        }, "actual command effects, cleanup, private completion and result-consumption drain")
        if (sourceAcks.length) throw new Error("Original now-playing acquired a listener-specific success ACK")
        if (action === "set" && entry.expectedYearNaN && !events[0]?.specialNumbers.some((number) => number.path === "/metadata/year" && number.value === "NaN")) throw new Error("Actual raw year NaN was lost before retained callback")
        if (action === "set" && entry.expectedUndefinedPictureType && !events[0]?.undefinedPaths.includes("/metadata/picture/0/type")) throw new Error("Original own undefined picture type was lost")
        if (action === "set" && entry.expectedPictures !== undefined && events[0]?.pictureViews.length !== entry.expectedPictures) throw new Error("All real cover buffers were not retained")
        for (const event of events) for (const view of event.pictureViews) if (view.constructor !== "Uint8Array" || view.byteOffset !== 0 || view.backingByteLength !== view.byteLength) throw new Error("Hydrated picture view lacks exact owned byte range")
        return { phase, action, context, current: window.api.getRuntimeContext(), before, after, events, errors, sourceAcks, browserErrors, script, sendReturnUndefined: true, mutatedAfterSend: !!payload, initialHostAbsentIsNotJoinProof: !before.host, ordinaryIdleVerified: true, terminalJoinVerified: false, filesystemVerifiedByProbe: false, imageEncodedParityClaimed: false, imagePixelParityClaimed: false }
    } finally {
        window.api.removeListener("MAIN", observer)
        window.removeEventListener("freeshow:runtime-error", failed)
        window.removeEventListener("error", browserFailed)
        window.removeEventListener("unhandledrejection", browserFailed)
    }
})()

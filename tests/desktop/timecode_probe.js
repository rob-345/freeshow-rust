// Executed by the coordinator's WebDriver script runner in the actual main view.
// No device capture, output selection, fake MIDI backend or native codec stub.
return (async () => {
    const fixture = arguments[0] || {},
        invoke = window.__TAURI__.core.invoke,
        context = window.api.getRuntimeContext(),
        observer = "native-timecode-probe-" + crypto.randomUUID(),
        events = [],
        sourceAcks = [],
        errors = [],
        browserErrors = [],
        feedEvidence = [],
        phase = fixture.phase,
        fps = fixture.fps ?? 25,
        values = fixture.values || [0, 40, 80, 120, 1000, 1040, 1080, 1120, 400, 440, 480, 520],
        script = { phase, actions: [] }
    let omittedErrors = 0,
        omittedBrowserErrors = 0,
        stopped = false,
        stopCheckpoint,
        before
    if (typeof phase !== "string" || !phase || phase.length > 64 || ![25, 29.97].includes(fps) || !Array.isArray(values) || values.length < 4 || values.length > 16 || values.some((value) => !Number.isFinite(value) || value < 0 || value >= 86400000)) throw new Error("Invalid bounded desktop timecode fixture")
    const same = () => {
        const now = window.api.getRuntimeContext()
        return now?.runtimeId === context.runtimeId && now.libraryGeneration === context.libraryGeneration && now.dataPath === context.dataPath
    }
    const status = async () => {
        const result = await invoke("timecode_status")
        if (!result || typeof result.router !== "object" || !Number.isSafeInteger(result.retained) || !Number.isSafeInteger(result.queuedPublications)) throw new Error("timecode_status must expose actual NativeTimecode status")
        return result
    }
    const drained = (state) => state.retained === 0 && state.asyncEffects === 0 && state.syncEffect === false && state.queuedPublications === 0 && state.pendingDelivery === false && (!state.ltc || state.ltc.admitted === 0)
    const wait = async (test, description, ms = 6000) => {
        const end = performance.now() + ms
        for (;;) {
            if (!same()) throw new Error("Timecode phase context changed")
            if (errors.length) throw new Error("Timecode runtime error: " + JSON.stringify(errors))
            const value = await test()
            if (value) return value
            if (performance.now() >= end) throw new Error("Timecode deadline: " + description)
            await new Promise((resolve) => setTimeout(resolve, 10))
        }
    }
    const base64 = (bytes) => {
        if (!(bytes instanceof Uint8Array) || bytes.length > 65536) throw new Error("Timecode callback lacks bounded Uint8Array bytes")
        let binary = ""
        for (const byte of bytes) binary += String.fromCharCode(byte)
        return btoa(binary)
    }
    const describeError = (error) => ({ name: typeof error?.name === "string" ? error.name.slice(0, 128) : "Error", message: String(error?.message ?? error ?? "Browser error").slice(0, 2048) })
    const failed = (event) => {
        const detail = event.detail,
            channel = detail?.request?.data?.channel,
            code = String(detail?.error?.code || "")
        if (!String(channel || "").startsWith("TIMECODE_") && !code.startsWith("timecode_") && !code.startsWith("ltc_")) return
        if (errors.length >= 16) {
            omittedErrors++
            return
        }
        errors.push({ code: code.slice(0, 128), ...describeError(detail?.error) })
    }
    const browserFailed = (event) => {
        if (browserErrors.length >= 16) {
            omittedBrowserErrors++
            return
        }
        browserErrors.push(describeError(event.error || event.reason || event.message))
    }
    const send = (action, channel, data, ownData = true) => {
        if (script.actions.length >= 64) throw new Error("Timecode action receipt bound")
        script.actions.push(action)
        window.api.send("MAIN", { channel, ...(ownData ? { data } : {}) }, observer)
    }
    const stop = async () => {
        send({ op: "stop" }, "TIMECODE_STOP", undefined)
        const state = await wait(async () => {
            const value = await status()
            return drained(value) && value.router.encoder === false && value.router.decoder === false && value.router.ownedListeners === 0 && value.router.receivers.length === 0 && (!value.ltc || (!value.ltc.encoder && !value.ltc.decoder && value.ltc.encoderCreated === value.ltc.encoderFreed && value.ltc.decoderCreated === value.ltc.decoderFreed)) && value
        }, "STOP actual encoder/decoder free and source callback drain")
        stopped = true
        return state
    }
    window.addEventListener("freeshow:runtime-error", failed)
    window.addEventListener("error", browserFailed)
    window.addEventListener("unhandledrejection", browserFailed)
    window.api.receive(
        "MAIN",
        (message, listenerId) => {
            try {
                if (listenerId === observer) {
                    if (sourceAcks.length >= 16) throw new Error("Timecode source ACK receipt bound")
                    sourceAcks.push({ channel: message.channel, listenerId })
                }
                if (!["TIMECODE_AUDIO_DATA", "TIMECODE_VALUE", "TIMECODE_STATUS", "ALERT"].includes(message.channel)) return
                if (events.length >= 64) throw new Error("Timecode event receipt bound")
                const common = { channel: message.channel, listenerIdUndefined: listenerId === undefined, dataOwn: Object.prototype.hasOwnProperty.call(message, "data"), messageKeys: Object.keys(message) }
                if (message.channel === "TIMECODE_AUDIO_DATA") {
                    const bytes = message.data
                    events.push({ ...common, base64: base64(bytes), bytes: { uint8Array: bytes instanceof Uint8Array, constructor: bytes.constructor.name, byteOffset: bytes.byteOffset, byteLength: bytes.byteLength, backingByteLength: bytes.buffer.byteLength } })
                } else {
                    if (typeof message.data !== "number" && typeof message.data !== "string") throw new Error("Unsupported controlled timecode callback payload")
                    events.push({ ...common, data: message.data })
                }
            } catch (error) {
                if (errors.length < 16) errors.push({ code: "timecode_probe_callback", ...describeError(error) })
                else omittedErrors++
            }
        },
        observer
    )
    try {
        before = await wait(async () => {
            const value = await status()
            if (value.closing || value.terminal) throw new Error("Timecode owner already closed/failed: " + JSON.stringify(value))
            return value.context?.runtimeId === context.runtimeId && value.context.libraryGeneration === context.libraryGeneration && value.context.dataPath === context.dataPath && !value.retiring && drained(value) && value
        }, "full-context native run ready after actual library retirement")
        // This is a real source STOP, not a reset of process-global source state.
        await stop()
        const initialCreated = (await status()).ltc?.decoderCreated || 0
        send({ op: "start", data: { type: "receive", mode: "LTC", framerate: fps } }, "TIMECODE_START", { type: "receive", mode: "LTC", framerate: fps })
        stopped = false
        await wait(async () => {
            const value = await status()
            if (value.router.decoderFailed) throw new Error("Actual verified LTC decoder unavailable: " + JSON.stringify(value))
            return value.router.decoder && value.ltc?.loaded && value.ltc.decoder && value.ltc.decoderCreated === initialCreated + 1 && drained(value) && value
        }, "actual verified LTC decoder construction")
        send({ op: "start", data: { type: "send", mode: "LTC", framerate: fps } }, "TIMECODE_START", { type: "send", mode: "LTC", framerate: fps })
        // Actual worklet shape is a raw array, whose missing mode is a source no-op.
        send({ op: "rawNoop", base64: "AQID" }, "TIMECODE_AUDIO_DATA", new Uint8Array([1, 2, 3]))
        for (const time of values) {
            const count = events.filter((event) => event.channel === "TIMECODE_AUDIO_DATA").length
            send({ op: "value", value: time }, "TIMECODE_VALUE", time)
            const captured = await wait(() => {
                const audio = events.filter((event) => event.channel === "TIMECODE_AUDIO_DATA")
                if (audio.length > count + 1) throw new Error("Unexpected concurrent timecode send contaminated controlled phase")
                return audio.length === count + 1 && audio.at(-1)
            }, "actual encoded LTC Uint8Array callback")
            const binary = atob(captured.base64),
                backing = new Uint8Array(binary.length + 14)
            backing.fill(0xa7)
            for (let index = 0; index < binary.length; index++) backing[index + 7] = binary.charCodeAt(index)
            const exact = backing.subarray(7, 7 + binary.length),
                previous = await status(),
                samples = previous.ltc.samples
            feedEvidence.push({ byteOffset: exact.byteOffset, byteLength: exact.byteLength, backingByteLength: backing.byteLength, poisonedPrefix: true, reusedAfterSend: true, expectedBase64: captured.base64 })
            send({ op: "audio", base64: captured.base64 }, "TIMECODE_AUDIO_DATA", { mode: "LTC", buffer: exact })
            backing.fill(0x19)
            await wait(async () => {
                const value = await status()
                return value.ltc?.samples === samples + binary.length && drained(value) && value
            }, "actual borrowed raw LTC input decode and native callback completion")
        }
        send({ op: "status", ownUndefined: true }, "TIMECODE_STATUS", undefined)
        stopCheckpoint = await stop()
        // Original STOP leaves currentMode/framerate intact: VALUE reopens the
        // encoder without START while the stopped decoder remains absent.
        const audioBefore = events.filter((event) => event.channel === "TIMECODE_AUDIO_DATA").length,
            afterStopTime = 2080
        send({ op: "value", value: afterStopTime }, "TIMECODE_VALUE", afterStopTime)
        stopped = false
        await wait(() => events.filter((event) => event.channel === "TIMECODE_AUDIO_DATA").length === audioBefore + 1, "source post-STOP encoder reopen")
        const after = await stop()
        if (sourceAcks.length || events.some((event) => event.channel === "TIMECODE_STATUS" || event.channel === "ALERT")) throw new Error("Controlled LTC source acquired a fabricated ACK/status or constructor alert")
        if (!events.some((event) => event.channel === "TIMECODE_VALUE")) throw new Error("No real LTC decoder frame was observed")
        if (events.filter((event) => event.channel === "TIMECODE_AUDIO_DATA").length !== values.length + 1) throw new Error("Unexpected source audio callback count")
        for (const event of events) if (!event.listenerIdUndefined || !event.dataOwn || JSON.stringify(event.messageKeys) !== JSON.stringify(["channel", "data"]) || (event.bytes && (!event.bytes.uint8Array || event.bytes.constructor !== "Uint8Array" || event.bytes.byteOffset !== 0 || event.bytes.backingByteLength !== event.bytes.byteLength))) throw new Error("Retained timecode source envelope/typed byte identity mismatch")
        return {
            phase,
            context,
            current: window.api.getRuntimeContext(),
            fps,
            before,
            stopCheckpoint,
            after,
            script,
            events,
            feedEvidence,
            sourceAcks,
            errors,
            omittedErrors,
            browserErrors,
            omittedBrowserErrors,
            actualNativeLtc: { loaded: after.ltc?.loaded === true, encoderCreated: after.ltc?.encoderCreated, encoderFreed: after.ltc?.encoderFreed, decoderCreated: after.ltc?.decoderCreated, decoderFreed: after.ltc?.decoderFreed },
            mtc: { exercised: false, hardwareDeliveryVerified: false, reason: "Exact controlled LTC corpus does not open a physical MIDI port" },
            microphoneCaptureVerified: false,
            playbackVerified: false,
            terminalShutdownVerified: false
        }
    } finally {
        if (!stopped && before && same()) window.api.send("MAIN", { channel: "TIMECODE_STOP", data: undefined }, observer)
        window.api.removeListener("MAIN", observer)
        window.removeEventListener("freeshow:runtime-error", failed)
        window.removeEventListener("error", browserFailed)
        window.removeEventListener("unhandledrejection", browserFailed)
    }
})()

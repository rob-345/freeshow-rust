// Retained main-WebView observer only. Python makes the actual HTTP requests.
return (async () => {
    const fixture = arguments[0] || {},
        invoke = typeof arguments[1] === "function" ? arguments[1] : window.__TAURI__.core.invoke,
        slot = "__nativeRemoteCoverProbe",
        same = (left, right) => !!left && !!right && left.runtimeId === right.runtimeId && left.libraryGeneration === right.libraryGeneration && left.dataPath === right.dataPath,
        text = (value) => String(value?.message ?? value ?? "").slice(0, 2048),
        utf8 = (value, maximum) => {
            if (typeof value !== "string" || value.length > maximum || value.includes("\0")) return false
            for (let i = 0; i < value.length; i++) {
                const code = value.charCodeAt(i)
                if (code >= 0xd800 && code <= 0xdbff) {
                    const low = value.charCodeAt(++i)
                    if (!(low >= 0xdc00 && low <= 0xdfff)) return false
                } else if (code >= 0xdc00 && code <= 0xdfff) return false
            }
            return new TextEncoder().encode(value).length <= maximum
        },
        validContext = (value) => !!value && utf8(value.runtimeId, 256) && !!value.runtimeId && Number.isSafeInteger(value.libraryGeneration) && value.libraryGeneration >= 0 && utf8(value.dataPath, 32768) && !!value.dataPath,
        counter = (value) => typeof value === "string" && /^(0|[1-9][0-9]{0,19})$/.test(value) && BigInt(value) <= 18446744073709551615n
    if (!["start", "options", "finish", "abort"].includes(fixture.stage) || typeof fixture.phase !== "string" || !/^[a-zA-Z0-9_-]{1,64}$/.test(fixture.phase)) throw new Error("Bounded remote-cover stage/phase required")
    if (fixture.stage === "abort") {
        const pending = window[slot]
        if (pending && pending.phase !== fixture.phase) throw new Error("Cannot abort a different cover probe")
        pending?.cleanup()
        return { phase: fixture.phase, aborted: !!pending, listenerRemoved: true }
    }
    if (!validContext(fixture.context) || !same(window.api.getRuntimeContext(), fixture.context)) {
        const pending = window[slot]
        if (pending?.phase === fixture.phase && same(pending.context, fixture.context)) pending.cleanup()
        throw new Error("Remote-cover probe requires the actual full current context")
    }
    if (fixture.stage === "start") {
        if (window[slot]) throw new Error("A remote-cover probe is already held")
        if (typeof fixture.blocker !== "boolean") throw new Error("Explicit cover blocker fixture required")
        const context = Object.freeze(structuredClone(fixture.context)),
            observer = "native-remote-cover-" + crypto.randomUUID(),
            // This desktop fixture is Linux-only; this string conveys no path authority.
            expectedFolderMessage = `Error: Could not create folder at: ${context.dataPath}/Audio!`,
            state = { phase: fixture.phase, context, expectedFolderMessage, expectedAlerts: fixture.blocker ? 3 : 0, deadline: performance.now() + 15000, reads: 0, events: [], sourceAcks: [], runtimeErrors: [], browserErrors: [], ignoredMain: [], callbackError: undefined }
        if (!utf8(context.dataPath + "/Audio", 32774) || !utf8(expectedFolderMessage, 32814)) throw new Error("Linux fixture folder/message bound exceeded")
        const failure = (event) => {
            if (state.runtimeErrors.length < 8) state.runtimeErrors.push({ code: String(event.detail?.error?.code || "").slice(0, 128), message: text(event.detail?.error) })
        }
        const browserFailure = (event) => {
            if (state.browserErrors.length < 8) state.browserErrors.push(text(event.error || event.reason || event.message))
        }
        const receive = (message, listenerId) => {
            try {
                if (listenerId === observer || /cover|(^|_)ack($|_)/i.test(String(message?.channel || ""))) {
                    if (state.sourceAcks.length < 4) state.sourceAcks.push({ channel: String(message?.channel || "").slice(0, 128), listenerId: typeof listenerId === "string" ? listenerId.slice(0, 128) : null })
                    throw new Error("Remote cover acquired an original source ACK")
                }
                if (message?.channel !== "ALERT") {
                    if (state.ignoredMain.length >= 16) throw new Error("Unrelated MAIN traffic exceeded probe bound")
                    state.ignoredMain.push(String(message?.channel || "").slice(0, 128))
                    return
                }
                if (!same(window.api.getRuntimeContext(), context) || listenerId !== undefined || JSON.stringify(Object.keys(message)) !== '["channel","data"]' || message.data !== expectedFolderMessage || state.events.length >= state.expectedAlerts) throw new Error("Remote-cover original ALERT context, count, message or public envelope mismatch")
                state.events.push({ channel: message.channel, data: message.data, messageKeys: Object.keys(message), listenerIdUndefined: true, contextMatched: true, privateFieldsStripped: true })
            } catch (error) {
                state.callbackError ||= text(error)
            }
        }
        let timer,
            cleaned = false
        state.cleanup = () => {
            if (cleaned) return
            cleaned = true
            clearTimeout(timer)
            window.api.removeListener("MAIN", observer)
            window.removeEventListener("freeshow:runtime-error", failure)
            window.removeEventListener("error", browserFailure)
            window.removeEventListener("unhandledrejection", browserFailure)
            if (window[slot] === state) delete window[slot]
        }
        window[slot] = state
        try {
            window.addEventListener("freeshow:runtime-error", failure)
            window.addEventListener("error", browserFailure)
            window.addEventListener("unhandledrejection", browserFailure)
            window.api.receive("MAIN", receive, observer)
            timer = setTimeout(state.cleanup, 15000)
        } catch (error) {
            state.cleanup()
            throw error
        }
    }
    const state = window[slot]
    if (!state || state.phase !== fixture.phase || !same(state.context, fixture.context)) throw new Error("Matching held remote-cover observer required")
    const healthy = () => {
        if (!same(window.api.getRuntimeContext(), state.context)) throw new Error("Remote-cover full context changed")
        if (state.callbackError || state.runtimeErrors.length || state.browserErrors.length || state.sourceAcks.length) throw new Error("Remote-cover callback/runtime failure: " + JSON.stringify({ callbackError: state.callbackError, runtimeErrors: state.runtimeErrors, browserErrors: state.browserErrors, sourceAcks: state.sourceAcks }))
        if (performance.now() >= state.deadline || window[slot] !== state) throw new Error("Remote-cover15s observation deadline")
    }
    const status = async () => {
        healthy()
        if (++state.reads > 1502) throw new Error("Remote-cover status-read bound exceeded")
        let timer
        try {
            const [remote, nowPlaying] = await Promise.race([
                Promise.all([invoke("remotes_status"), invoke("now_playing_status")]),
                new Promise((_, reject) => {
                    timer = setTimeout(() => reject(new Error("Remote-cover status deadline")), Math.max(1, state.deadline - performance.now()))
                })
            ])
            const binding = remote?.binding,
                cover = remote?.cover,
                endpoints = binding?.bound?.endpoints,
                generation = binding?.bound?.generation,
                host = nowPlaying?.host
            if (typeof remote?.closing !== "boolean" || remote.closing || remote.failure || !same(binding?.context, state.context) || binding.retiring !== false || !endpoints || JSON.stringify(Object.keys(endpoints).sort()) !== '["CONTROLLER","REMOTE","STAGE"]' || Object.values(endpoints).some((value) => !utf8(value, 256)) || generation?.context !== state.context.libraryGeneration || !utf8(generation.run, 256) || !generation.run || !cover)
                throw new Error("Actual live three-port remote binding required")
            for (const key of ["filesRetained", "publicationsRetained"]) if (!Number.isSafeInteger(cover[key]) || cover[key] < 0 || cover[key] > 4) throw new Error("Actual bounded cover retention status required")
            if (!nowPlaying || nowPlaying.closed || nowPlaying.terminalError || !same(nowPlaying.readyContext, state.context) || (host !== null && (!host || host.closing || host.error || !same(host.context, state.context) || !counter(host.revealCalls)))) throw new Error("Actual current now-playing reveal status required")
            healthy()
            return { cover: { filesRetained: cover.filesRetained, publicationsRetained: cover.publicationsRetained }, endpoints, generation: { context: generation.context, run: generation.run }, bindingContextMatched: true, nowPlaying: { hasHost: host !== null, contextMatched: true, revealCalls: host?.revealCalls ?? "0" } }
        } finally {
            clearTimeout(timer)
        }
    }
    const wait = async (expectedAlerts) => {
        for (;;) {
            const value = await status()
            if (state.before && (value.nowPlaying.hasHost !== state.before.nowPlaying.hasHost || value.nowPlaying.revealCalls !== state.before.nowPlaying.revealCalls || JSON.stringify(value.endpoints) !== JSON.stringify(state.before.endpoints) || JSON.stringify(value.generation) !== JSON.stringify(state.before.generation))) throw new Error("HTTP probe changed now-playing reveal ownership/counter or remote binding")
            if (value.cover.filesRetained === 0 && value.cover.publicationsRetained === 0 && state.events.length === expectedAlerts) return value
            await new Promise((resolve) => setTimeout(resolve, 10))
        }
    }
    try {
        if (fixture.stage === "start") {
            state.before = await wait(0)
            return { phase: state.phase, context: state.context, expectedFolderMessage: state.expectedFolderMessage, before: state.before, listenerRegistered: true }
        }
        if (fixture.stage === "options") {
            if (state.events.length) throw new Error("OPTIONS unexpectedly published a source folder ALERT")
            const after = await wait(0)
            return { phase: state.phase, after, alerts: state.events.length, statusReads: state.reads, noAlertObserved: true }
        }
        const after = await wait(state.expectedAlerts)
        healthy()
        return {
            phase: state.phase,
            context: state.context,
            expectedFolderMessage: state.expectedFolderMessage,
            before: state.before,
            after,
            events: state.events,
            counts: { folderAlerts: state.events.length, sourceAcks: state.sourceAcks.length, runtimeErrors: state.runtimeErrors.length, browserErrors: state.browserErrors.length, statusReads: state.reads },
            sourceAcks: state.sourceAcks,
            runtimeErrors: state.runtimeErrors,
            browserErrors: state.browserErrors,
            ignoredMain: state.ignoredMain,
            publicProjectionVerified: true,
            noNowPlayingRevealCallbackInvoked: true,
            retainedZeroVerified: true,
            nativeIoJoinVerified: false
        }
    } catch (error) {
        state.cleanup()
        throw error
    } finally {
        if (fixture.stage === "finish") state.cleanup()
    }
})()

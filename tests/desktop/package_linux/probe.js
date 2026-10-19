// Production-page observation only; no module/store/IPC replacement.
return (async () => {
    const mode = arguments[0]
    if (mode === "install") {
        if (window.__packageReview) throw new Error("Package observer already installed")
        const browserErrors = [],
            runtimeErrors = [],
            subscriptions = []
        for (const kind of ["error", "unhandledrejection", "freeshow:runtime-error"]) {
            const callback = (event) => {
                const list = kind === "freeshow:runtime-error" ? runtimeErrors : browserErrors
                if (list.length >= 32) throw new Error("Package error observation budget")
                list.push(kind === "freeshow:runtime-error" ? event.detail : String(event.message ?? event.reason).slice(0, 2048))
            }
            window.addEventListener(kind, callback)
            subscriptions.push([kind, callback])
        }
        window.__packageReview = { browserErrors, runtimeErrors, cleanup: () => subscriptions.forEach(([kind, callback]) => window.removeEventListener(kind, callback)) }
    }
    const observer = window.__packageReview
    if (!observer) throw new Error("Package observer absent")
    let version
    if (mode === "version") {
        version = await new Promise((resolve, reject) => {
            const id = "package-version-" + crypto.randomUUID(),
                timer = setTimeout(() => {
                    window.api.removeListener("MAIN", id)
                    reject(new Error("Package VERSION deadline"))
                }, 5000)
            window.api.receive(
                "MAIN",
                (message, listener) => {
                    if (message.channel !== "VERSION" || listener !== id) return
                    clearTimeout(timer)
                    window.api.removeListener("MAIN", id)
                    resolve(message.data)
                },
                id
            )
            window.api.send("MAIN", { channel: "VERSION" }, id)
        })
    } else if (!["install", "snapshot", "finish"].includes(mode)) throw new Error("Invalid package observation")
    const result = { origin: location.origin, context: window.api?.getRuntimeContext(), version, title: document.title, editor: !!document.querySelector("main .left"), body: document.body.innerText.slice(0, 8192), diagnosticGlobalsAbsent: !window.__audioFullFork && !window.__videoCors, browserErrors: [...observer.browserErrors], runtimeErrors: [...observer.runtimeErrors] }
    if (mode === "finish") observer.cleanup()
    return result
})()

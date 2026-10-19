// Read-only DOM/error observation: no module, store, API or native result hooks.
const operation = arguments[0]
if (operation === "install") {
    if (window.__browseStartup) throw new Error("Startup observer already installed")
    const errors = [],
        clicks = [],
        subscriptions = []
    for (const kind of ["error", "unhandledrejection", "freeshow:runtime-error"]) {
        const listener = (event) => {
            if (errors.length >= 64) throw new Error("Startup error observation budget")
            errors.push(kind === "freeshow:runtime-error" ? { kind, detail: event.detail } : { kind, value: String(event.message ?? event.reason).slice(0, 2048) })
        }
        window.addEventListener(kind, listener)
        subscriptions.push([kind, listener])
    }
    const onClick = (event) => {
        const button = event.target.closest?.("section.drawer button")
        if (!button || (!button.closest(".top") && !button.closest(".tabSection"))) return
        if (clicks.length >= 16) throw new Error("Startup click observation budget")
        clicks.push({ group: button.closest(".top") ? "top" : "sub", id: button.id, label: button.querySelector("div[data-title]")?.getAttribute("data-title") ?? button.textContent.trim(), trusted: event.isTrusted })
    }
    document.addEventListener("click", onClick, true)
    window.__browseStartup = {
        errors,
        clicks,
        cleanup() {
            for (const [kind, listener] of subscriptions) window.removeEventListener(kind, listener)
            document.removeEventListener("click", onClick, true)
        }
    }
} else if (operation !== "snapshot" && operation !== "finish") throw new Error("Unknown startup observation")
const state = window.__browseStartup
if (!state) throw new Error("Missing startup observer")
const rows = [...document.querySelectorAll("section.drawer button")].filter((button) => button.classList.contains("#audio_button")).map((button) => ({ path: button.getAttribute("data-title"), name: button.querySelector("p")?.textContent, duration: button.querySelectorAll("p")[1]?.textContent }))
const tabs = [...document.querySelectorAll("section.drawer .tabSection button")].map((button) => ({ label: button.querySelector("div[data-title]")?.getAttribute("data-title"), count: Number(button.querySelector(".count")?.textContent ?? 0), active: button.classList.contains("isActive") })).filter((tab) => tab.label === "All" || tab.label?.startsWith("Startup audio"))
if (rows.length > 32 || tabs.length > 16) throw new Error("Startup DOM observation budget")
const result = {
    origin: location.origin,
    context: window.api?.getRuntimeContext(),
    topTab: document.querySelector("section.drawer .top button.isActive")?.id,
    rows,
    tabs,
    errors: [...state.errors],
    clicks: [...state.clicks],
    diagnosticGlobalsAbsent: !window.__audioFullFork && !window.__videoCors,
    body: document.body.innerText.slice(0, 8192)
}
if (operation === "finish") state.cleanup()
return result

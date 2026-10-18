import { installRustBridge } from "./bridge"
import { recoverFailedSave } from "./saveFailure"

async function start() {
    await installRustBridge()
    const stores = await import("../stores")
    window.addEventListener("freeshow:runtime-error", (event: Event) => {
        const { request, error } = (event as CustomEvent).detail || {}
        recoverFailedSave(request, stores, error)
    })
    // Install window.api before evaluating App.svelte and its startup code.
    await import("../main")
}

start().catch((error) => {
    console.error(error)
    document.body.textContent = "FreeShow could not start. Please see the application log."
})

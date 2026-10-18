import { installRustBridge } from "./bridge"

async function start() {
    await installRustBridge()
    // Install window.api before evaluating App.svelte and its startup code.
    await import("../main")
}

start().catch((error) => {
    console.error(error)
    document.body.textContent = "FreeShow could not start. Please see the application log."
})

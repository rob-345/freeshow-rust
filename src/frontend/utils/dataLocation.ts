import { get } from "svelte/store"
import { Main } from "../../types/IPC/Main"
import { requestMain, sendMain } from "../IPC/main"
import { activePopup, alertMessage, dataPath, saved, statusIndicator } from "../stores"
import { waitForSave } from "../rust/saveCompletion"
import { wait } from "./common"
import { save } from "./save"

export async function changeDataLocation(newPath: string) {
    if (!newPath || newPath === get(dataPath)) return
    if (!(window as any).__TAURI__) {
        if (!get(saved)) {
            save(false, { backup: true, isAutoBackup: true })
            await wait(1500)
        }
        sendMain(Main.UPDATE_DATA_PATH, { newPath, oldPath: get(dataPath) })
        dataPath.set(newPath)
        sendMain(Main.REFRESH_SHOWS)
        return
    }
    if (get(statusIndicator) === "saving") {
        alertMessage.set("Please wait for the current save to finish, then select the data folder again.")
        activePopup.set("alert")
        return
    }
    try {
        if (!get(saved)) {
            const operationId = `data-location-${crypto.randomUUID()}`
            await waitForSave(window.api, window, operationId, () => save(false, { backup: true, isAutoBackup: true, autosave: true }, operationId))
        }
        const result = await requestMain(Main.UPDATE_DATA_PATH, { newPath, oldPath: get(dataPath) }, undefined, 60000)
        if (!result) throw new Error("Data folder selection timed out")
        // DATA_PATH and portable-store events arrive only after the commit.
        if (result.success) sendMain(Main.SHOWS)
    } catch {
        alertMessage.set("Could not change the data folder. Save your changes and try again.")
        activePopup.set("alert")
    }
}

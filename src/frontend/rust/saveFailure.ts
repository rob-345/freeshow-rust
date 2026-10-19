import { get } from "svelte/store"
import { isBackupOnly } from "../utils/savePolicy"
import { cancelDeferredClosing } from "./saveRevision"

type SaveStores = Pick<typeof import("../stores"), "saved" | "statusIndicator" | "deletedShows" | "renamedShows" | "shows" | "alertMessage" | "activePopup">
type Failure = { code?: string; recoveryPath?: string; recoveryError?: string }

// If disk recovery fails, retain the complete snapshot while the window is open.
// These snapshots must never be merged into the newly selected library's queues.
export const retainedStaleSaves: { request: any; error: Failure }[] = []
const escapeHtml = (text: string) => text.replace(/[&<>"']/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]!)

// The shared frontend clears pending rename/delete operations before sending a
// SAVE. Restore those operations on a rejected native save so retry keeps them.
export function recoverFailedSave(request: any, stores: SaveStores, error: Failure = {}): boolean {
    if (request?.channel !== "MAIN" || request?.data?.channel !== "SAVE") return false
    const payload = request.data.data || {}
    if (error.code === "stale_library") {
        if (!error.recoveryPath) retainedStaleSaves.push({ request, error })
        const recovery = error.recoveryPath ? `Recovery file: <code>${escapeHtml(error.recoveryPath)}</code>` : "No recovery file could be written. Keep this window open; the unsaved snapshot remains in memory."
        stores.alertMessage.set(`A save from a previous library or session was kept separate from the current library.<br>${recovery}`)
        stores.activePopup.set("alert")
        return true
    }
    cancelDeferredClosing()
    // Backup-only operations never removed these queues before dispatch.
    if (isBackupOnly(payload.customTriggers, payload.closeWhenFinished)) {
        stores.statusIndicator.set("error")
        stores.alertMessage.set("Could not save backup settings. Check the data folder permissions and try again.")
        stores.activePopup.set("alert")
        return true
    }
    const failedDeletes = Array.isArray(payload.deletedShows) ? payload.deletedShows : []
    const failedRenames = Array.isArray(payload.renamedShows) ? payload.renamedShows : []
    const currentShows = get(stores.shows)
    const pendingDeletes = [...failedDeletes, ...get(stores.deletedShows)].filter((show) => !currentShows[show.id])
    const deletingIds = new Set(pendingDeletes.map((show) => show.id))
    // A rename may have reached disk before a later write failed. If the user
    // then deleted that show, include every possible filename; the backend's ID
    // check prevents deleting an unrelated show with the same name.
    for (const rename of failedRenames) {
        if (deletingIds.has(rename.id)) pendingDeletes.push({ id: rename.id, name: rename.oldName }, { id: rename.id, name: rename.name })
    }
    stores.deletedShows.set(pendingDeletes)
    // Preserve ordered rename chains, including an undo or another rename made
    // while the failed save was in flight.
    stores.renamedShows.set([...failedRenames, ...get(stores.renamedShows)])
    stores.saved.set(false)
    stores.statusIndicator.set("error")
    stores.alertMessage.set("Could not save all changes. Check the data folder permissions and try again.")
    stores.activePopup.set("alert")
    return true
}

export function recoverFailedClose(request: any, stores: Pick<typeof import("../stores"), "saved" | "statusIndicator" | "alertMessage" | "activePopup" | "errorHasOccurred">): boolean {
    if (request?.channel !== "MAIN" || request?.data?.channel !== "CLOSE") return false
    cancelDeferredClosing()
    stores.saved.set(false)
    stores.errorHasOccurred.set(true)
    stores.statusIndicator.set("error")
    stores.alertMessage.set("Could not finish pending file requests. Keep this window open to preserve unsaved work. You can explicitly quit without saving from the Quit dialog.")
    stores.activePopup.set("alert")
    return true
}

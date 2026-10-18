import { get } from "svelte/store"

type SaveStores = Pick<typeof import("../stores"), "saved" | "statusIndicator" | "deletedShows" | "renamedShows" | "shows" | "alertMessage" | "activePopup">

// The shared frontend clears pending rename/delete operations before sending a
// SAVE. Restore those operations on a rejected native save so retry keeps them.
export function recoverFailedSave(request: any, stores: SaveStores): boolean {
    if (request?.channel !== "MAIN" || request?.data?.channel !== "SAVE") return false
    const payload = request.data.data || {}
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

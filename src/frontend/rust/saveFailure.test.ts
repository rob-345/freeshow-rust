import { describe, expect, it } from "vitest"
import { get, writable } from "svelte/store"
import { recoverFailedSave, retainedStaleSaves } from "./saveFailure"

function state() {
    return { saved: writable(true), statusIndicator: writable("saving"), deletedShows: writable<any[]>([]), renamedShows: writable<any[]>([]), shows: writable<any>({}), alertMessage: writable(""), activePopup: writable<any>(null) }
}

describe("Rust save failure recovery", () => {
    it("keeps old-library operations out of current state and escapes the recovery path", () => {
        const stores = state()
        stores.deletedShows.set([{ id: "current", name: "Current library" }])
        const request = { channel: "MAIN", data: { channel: "SAVE", data: { deletedShows: [{ id: "old", name: "Old library" }] } } }
        recoverFailedSave(request, stores, { code: "stale_library", recoveryPath: '/app/Recovery/<unsafe>&".json' })
        expect(get(stores.deletedShows)).toEqual([{ id: "current", name: "Current library" }])
        expect(get(stores.saved)).toBe(true)
        expect(get(stores.statusIndicator)).toBe("saving")
        expect(get(stores.alertMessage)).toContain("&lt;unsafe&gt;&amp;&quot;")
        expect(get(stores.alertMessage)).not.toContain("<unsafe>")
    })

    it("retains the complete stale snapshot in memory if disk recovery failed", () => {
        retainedStaleSaves.length = 0
        const stores = state()
        const request = { channel: "MAIN", data: { channel: "SAVE", data: { showsCache: { song: { lyrics: "Keep these edits" } } } } }
        recoverFailedSave(request, stores, { code: "stale_library", recoveryError: "disk full" })
        expect(retainedStaleSaves[0].request).toBe(request)
        expect(get(stores.alertMessage)).toContain("Keep this window open")
        expect(get(stores.saved)).toBe(true)
        retainedStaleSaves.length = 0
    })
    it("retains failed operations and newer rename chains without dropping an undo", () => {
        const stores = state()
        stores.shows.set({ song: { name: "Newest" }, undeleted: { name: "Restored by undo" } })
        stores.renamedShows.set([{ id: "song", oldName: "Middle", name: "Newest" }])
        const failed = {
            channel: "MAIN",
            data: {
                channel: "SAVE",
                data: {
                    renamedShows: [{ id: "song", oldName: "Original", name: "Middle" }],
                    deletedShows: [
                        { id: "gone", name: "Gone" },
                        { id: "undeleted", name: "Restored by undo" }
                    ]
                }
            }
        }
        expect(recoverFailedSave(failed, stores)).toBe(true)
        expect(get(stores.renamedShows)).toEqual([
            { id: "song", oldName: "Original", name: "Middle" },
            { id: "song", oldName: "Middle", name: "Newest" }
        ])
        expect(get(stores.deletedShows)).toEqual([{ id: "gone", name: "Gone" }])
        expect(get(stores.saved)).toBe(false)
        expect(get(stores.statusIndicator)).toBe("error")
        expect(get(stores.activePopup)).toBe("alert")
    })

    it("retains possible on-disk names when a failed rename is followed by deletion", () => {
        const stores = state()
        stores.deletedShows.set([{ id: "song", name: "Newest" }])
        recoverFailedSave({ channel: "MAIN", data: { channel: "SAVE", data: { renamedShows: [{ id: "song", oldName: "Original", name: "Middle" }] } } }, stores)
        expect(get(stores.deletedShows)).toEqual([
            { id: "song", name: "Newest" },
            { id: "song", name: "Original" },
            { id: "song", name: "Middle" }
        ])
    })

    it("leaves unrelated runtime failures to their own feature handlers", () => {
        const stores = state()
        expect(recoverFailedSave({ channel: "MAIN", data: { channel: "START" } }, stores)).toBe(false)
        expect(get(stores.saved)).toBe(true)
        expect(get(stores.activePopup)).toBe(null)
    })

    it("does not duplicate pending operations or mark edits saved after a backup-only failure", () => {
        const stores = state()
        stores.saved.set(false)
        stores.deletedShows.set([{ id: "song", name: "Deleted" }])
        stores.renamedShows.set([{ id: "other", oldName: "Before", name: "After" }])
        recoverFailedSave({ channel: "MAIN", data: { channel: "SAVE", data: { deletedShows: get(stores.deletedShows), renamedShows: get(stores.renamedShows), customTriggers: { backup: true, isAutoBackup: true } } } }, stores)
        expect(get(stores.deletedShows)).toHaveLength(1)
        expect(get(stores.renamedShows)).toHaveLength(1)
        expect(get(stores.saved)).toBe(false)
    })

    it("shows a fixed message without inserting request contents into HTML", () => {
        const stores = state()
        recoverFailedSave({ channel: "MAIN", data: { channel: "SAVE", data: { secret: "private-value", error: '<img src=x onerror="bad()">' } } }, stores)
        expect(get(stores.alertMessage)).toContain("Could not save all changes")
        expect(get(stores.alertMessage)).not.toContain("private-value")
        expect(get(stores.alertMessage)).not.toContain("<img")
    })
})

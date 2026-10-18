import { describe, expect, it } from "vitest"
import { get, writable } from "svelte/store"
import { recoverFailedSave } from "./saveFailure"

function state() {
    return { saved: writable(true), statusIndicator: writable("saving"), deletedShows: writable<any[]>([]), renamedShows: writable<any[]>([]), shows: writable<any>({}), alertMessage: writable(""), activePopup: writable<any>(null) }
}

describe("Rust save failure recovery", () => {
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

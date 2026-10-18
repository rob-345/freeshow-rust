import { beforeEach, describe, expect, it, vi } from "vitest"
import { get } from "svelte/store"
import { Main } from "../../types/IPC/Main"
import { requestMain, sendMain } from "../IPC/main"
import { activePopup, alertMessage, dataPath, saved, statusIndicator } from "../stores"
import { save } from "../utils/save"
import { changeDataLocation } from "../utils/dataLocation"
import { waitForSave } from "./saveCompletion"

vi.mock("../stores", async () => {
    const { writable } = await import("svelte/store")
    return { activePopup: writable(null), alertMessage: writable(""), dataPath: writable("/old"), saved: writable(false), statusIndicator: writable("") }
})
vi.mock("../IPC/main", () => ({ requestMain: vi.fn(), sendMain: vi.fn() }))
vi.mock("../utils/save", () => ({ save: vi.fn(() => true) }))
vi.mock("../utils/common", () => ({ wait: vi.fn() }))
vi.mock("./saveCompletion", () => ({ waitForSave: vi.fn() }))

describe("changing the Rust data library", () => {
    beforeEach(() => {
        vi.clearAllMocks()
        vi.stubGlobal("window", { __TAURI__: {}, api: {} })
        dataPath.set("/old")
        saved.set(false)
        statusIndicator.set("")
        activePopup.set(null)
        vi.mocked(requestMain).mockResolvedValue({ success: true, path: "/new" } as any)
    })

    it("waits for the requested ordinary save before selecting another folder", async () => {
        let release = () => {}
        vi.mocked(waitForSave).mockImplementation(async (_api, _window, id, trigger) => {
            expect(id).toMatch(/^data-location-/)
            expect(trigger()).toBe(true)
            await new Promise<void>((resolve) => {
                release = resolve
            })
        })
        const changing = changeDataLocation("/new")
        expect(requestMain).not.toHaveBeenCalled()
        expect(save).toHaveBeenCalledWith(false, { backup: true, isAutoBackup: true, autosave: true }, expect.any(String))
        expect(get(dataPath)).toBe("/old")
        release()
        await changing
        expect(requestMain).toHaveBeenCalledWith(Main.UPDATE_DATA_PATH, { oldPath: "/old", newPath: "/new" }, undefined, 60000)
        expect(sendMain).toHaveBeenCalledWith(Main.SHOWS)
    })

    it("keeps the active library when saving fails", async () => {
        vi.mocked(waitForSave).mockRejectedValue(new Error("disk failure"))
        await changeDataLocation("/new")
        expect(requestMain).not.toHaveBeenCalled()
        expect(get(dataPath)).toBe("/old")
        expect(get(activePopup)).toBe("alert")
        expect(get(alertMessage)).toContain("Could not change")
    })

    it("keeps the displayed path when the backend rejects the destination", async () => {
        saved.set(true)
        vi.mocked(requestMain).mockResolvedValue({ success: false, path: "/old" } as any)
        await changeDataLocation("/new")
        expect(waitForSave).not.toHaveBeenCalled()
        expect(get(dataPath)).toBe("/old")
        expect(sendMain).not.toHaveBeenCalled()
    })

    it("waits for a busy save to finish before accepting another selection", async () => {
        statusIndicator.set("saving")
        await changeDataLocation("/new")
        expect(requestMain).not.toHaveBeenCalled()
        expect(get(dataPath)).toBe("/old")
        expect(get(alertMessage)).toContain("current save")
    })
})

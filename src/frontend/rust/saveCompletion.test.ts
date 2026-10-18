import { describe, expect, it, vi } from "vitest"
import { waitForSave } from "./saveCompletion"

function harness() {
    let receiver: (message: any, id?: string) => void = () => {}
    const api = {
        receive: vi.fn((_channel, fn) => {
            receiver = fn
        }),
        removeListener: vi.fn()
    } as any
    const target = new EventTarget()
    const failure = (id: string) => {
        const event = new Event("freeshow:runtime-error")
        Object.assign(event, { detail: { request: { channel: "MAIN", data: { channel: "SAVE" }, listenerId: id } } })
        target.dispatchEvent(event)
    }
    return { api, target, reply: (message: any, id?: string) => receiver(message, id), failure }
}

describe("save acknowledgment before a library switch", () => {
    it("ignores unrelated saves and backup-only acknowledgments", async () => {
        const h = harness()
        let settled = false
        const pending = waitForSave(h.api, h.target, "switch", () => true).then(() => {
            settled = true
        })
        h.reply({ channel: "SAVE2", data: {} }, "unrelated")
        h.reply({ channel: "SAVE2", data: { customTriggers: { backup: true, isAutoBackup: true } } }, "switch")
        h.failure("unrelated")
        await Promise.resolve()
        expect(settled).toBe(false)
        h.reply({ channel: "SAVE2", data: { customTriggers: { backup: true, isAutoBackup: true, autosave: true } } }, "switch")
        await pending
        expect(h.api.removeListener).toHaveBeenCalledWith("MAIN", "switch-completion")
    })

    it("rejects a correlated native failure and removes the listener", async () => {
        const h = harness()
        const pending = waitForSave(h.api, h.target, "switch", () => true)
        h.failure("switch")
        await expect(pending).rejects.toThrow("Save failed")
        expect(h.api.removeListener).toHaveBeenCalledTimes(1)
    })

    it("does not proceed when another save prevents this save from starting", async () => {
        const h = harness()
        await expect(waitForSave(h.api, h.target, "switch", () => false)).rejects.toThrow("Another save")
        expect(h.api.removeListener).toHaveBeenCalledTimes(1)
    })

    it("cleans up after a missing acknowledgment", async () => {
        vi.useFakeTimers()
        try {
            const h = harness()
            const pending = waitForSave(h.api, h.target, "switch", () => true, 100)
            const rejected = expect(pending).rejects.toThrow("Save timed out")
            await vi.advanceTimersByTimeAsync(100)
            await rejected
            expect(h.api.removeListener).toHaveBeenCalledTimes(1)
        } finally {
            vi.useRealTimers()
        }
    })
})

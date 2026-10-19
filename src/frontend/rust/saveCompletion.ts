import { isBackupOnly } from "../utils/savePolicy"

type Api = Pick<typeof window.api, "receive" | "removeListener">

// Install before starting the save. Only this operation's acknowledgment or
// failure can settle the wait; an unrelated automatic backup cannot release it.
export function waitForSave(api: Api, target: EventTarget, operationId: string, trigger: () => boolean, timeoutMs = 60000): Promise<void> {
    return new Promise((resolve, reject) => {
        const listener = `${operationId}-completion`
        const cleanup = () => {
            clearTimeout(timer)
            api.removeListener("MAIN", listener)
            target.removeEventListener("freeshow:runtime-error", failed)
        }
        const failed = (event: Event) => {
            const request = (event as CustomEvent).detail?.request
            if (request?.channel !== "MAIN" || request?.data?.channel !== "SAVE" || request?.listenerId !== operationId) return
            cleanup()
            reject(new Error("Save failed"))
        }
        const timer = setTimeout(() => {
            cleanup()
            reject(new Error("Save timed out"))
        }, timeoutMs)
        api.receive(
            "MAIN",
            (message, listenerId) => {
                if (message.channel !== "SAVE2" || listenerId !== operationId || isBackupOnly(message.data?.customTriggers, message.data?.closeWhenFinished)) return
                cleanup()
                resolve()
            },
            listener
        )
        target.addEventListener("freeshow:runtime-error", failed)
        try {
            if (!trigger()) throw new Error("Another save is in progress")
        } catch (error) {
            cleanup()
            reject(error)
        }
    })
}

import type { RuntimeContext } from "./bridge"
import { hydrateAudioMetadata, type AudioMetadataHydration } from "./metadata"
import { encodeNowPlayingCommand } from "./nowPlayingWire"

export type NowPlayingTransport = {
    command: (body: Uint8Array) => Promise<unknown>
    complete: (context: RuntimeContext, id: string, accepted: boolean) => Promise<unknown>
    retire: (context: RuntimeContext, terminal: boolean) => Promise<unknown>
}
export type NowPlayingPublication = {
    event: { channel: string; message: any; listenerId?: string; context?: RuntimeContext }
    id: string
    sequence: string
    hydration?: AudioMetadataHydration
    alertKind?: "missingText" | "folderCreationFailed"
    alertFolder?: string
}
type Options = {
    transport: NowPlayingTransport
    getContext: () => RuntimeContext | undefined
    deliver: (message: unknown) => void
    report: (error: unknown) => void
    timeoutMs?: number
}
const same = (a: RuntimeContext | undefined, b: RuntimeContext | undefined) => !!a && !!b && a.runtimeId === b.runtimeId && a.libraryGeneration === b.libraryGeneration && a.dataPath === b.dataPath
const failure = (code: string, message: string) => Object.assign(new Error(message), { code })
const FOLDER_PREFIX = "Error: Could not create folder at: "
const MAX_COMPLETIONS = 20

function boundedUtf8(value: unknown, maximum: number): value is string {
    if (typeof value !== "string" || value.length > maximum) return false
    let bytes = 0
    for (let i = 0; i < value.length; i++) {
        const code = value.charCodeAt(i)
        if (code === 0) return false
        if (code < 128) bytes++
        else if (code < 2048) bytes += 2
        else if (code >= 0xd800 && code <= 0xdbff) {
            const low = value.charCodeAt(++i)
            if (!(low >= 0xdc00 && low <= 0xdfff)) return false
            bytes += 4
        } else if (code >= 0xdc00 && code <= 0xdfff) return false
        else bytes += 3
        if (bytes > maximum) return false
    }
    return true
}
function originalAlert(publication: NowPlayingPublication, data: unknown): boolean {
    if (Object.hasOwn(publication, "hydration")) return false
    if (publication.alertKind === "missingText") return !Object.hasOwn(publication, "alertFolder") && data === "This does not exist!"
    // The native sealed EnsureFolder observation supplies this diagnostic path.
    // It is never filesystem authority and is not normalized/joined in the UI.
    const folder = publication.alertFolder
    return publication.alertKind === "folderCreationFailed" && boundedUtf8(folder, 32774) && (folder.endsWith("/Audio") || folder.endsWith("\\Audio")) && boundedUtf8(data, 32814) && data === `${FOLDER_PREFIX}${folder}!`
}

/** One UI owner; all entered native promises remain charged until settlement. */
export function createNowPlayingBridge(options: Options) {
    const commands = new Map<Promise<void>, RuntimeContext>(),
        completions = new Map<Promise<void>, RuntimeContext>()
    let completionSlots = 0,
        sequence = 0n,
        closed = false
    let lastContext: RuntimeContext | undefined
    let control: { context: RuntimeContext; terminal: boolean; work: Promise<unknown>; settled: boolean; failed: boolean; observation?: Promise<void> } | undefined
    const timeoutMs = options.timeoutMs ?? 20000
    const report = (error: unknown) => options.report(error)
    function bounded<T>(work: Promise<T>): Promise<T> {
        let timer: ReturnType<typeof setTimeout> | undefined
        const limit = new Promise<never>((_, reject) => {
            timer = setTimeout(() => reject(failure("now_playing_retirement_timeout", "Native now-playing work remains owned for retry")), timeoutMs)
        })
        return Promise.race([work, limit]).finally(() => clearTimeout(timer))
    }
    function handle(message: unknown): void {
        const captured = options.getContext()
        if (closed || !captured) {
            report(failure("now_playing_closed", "Now-playing admission is unavailable"))
            return
        }
        if (commands.size === 4) {
            report(failure("now_playing_busy", "Four native now-playing command promises remain owned"))
            return
        }
        try {
            // Encode synchronously before any deferred native call. The source
            // object/context may be reused as soon as send returns undefined.
            const frozen = { ...captured },
                bytes = encodeNowPlayingCommand(message, frozen)
            lastContext = frozen
            const owned = Promise.resolve()
                .then(() => options.transport.command(bytes))
                .then(
                    () => {},
                    (error) => {
                        if (same(options.getContext(), frozen) && !closed) report(error)
                    }
                )
                .finally(() => commands.delete(owned))
            commands.set(owned, frozen)
        } catch (error) {
            report(error)
        }
    }
    function publish(publication: NowPlayingPublication): void {
        const event = publication?.event,
            frozen = event?.context ? { ...event.context } : undefined
        if (!same(options.getContext(), frozen) || closed) return
        const id = typeof publication.id === "string" && /^native-now-playing-[0-9a-f]{32}$/.test(publication.id)
        let accepted = false,
            reserved = false
        try {
            if (!id || event.channel !== "MAIN" || event.listenerId !== undefined || !event.message || typeof event.message !== "object" || Array.isArray(event.message) || Object.keys(event.message).some((key) => !["channel", "data"].includes(key)) || !Object.hasOwn(event.message, "data")) throw failure("now_playing_publication", "Invalid native now-playing event")
            if (typeof publication.sequence !== "string" || !/^[1-9][0-9]{0,19}$/.test(publication.sequence) || BigInt(publication.sequence) > 18446744073709551615n) throw failure("now_playing_publication", "Invalid native publication sequence")
            const next = BigInt(publication.sequence)
            if (next <= sequence) return
            // Reserve before restoration/callbacks, including reentrant input.
            if (completionSlots === MAX_COMPLETIONS) throw failure("now_playing_completion_busy", "Twenty entered native completion promises remain owned")
            completionSlots++
            reserved = true
            lastContext = frozen
            if (event.message.channel === "AUDIO_METADATA") {
                if (!publication.hydration || Object.hasOwn(publication, "alertKind") || Object.hasOwn(publication, "alertFolder")) throw failure("now_playing_publication", "Invalid native metadata restoration")
                hydrateAudioMetadata(event.message.data, publication.hydration)
            } else if (event.message.channel === "ALERT") {
                if (!originalAlert(publication, event.message.data)) throw failure("now_playing_publication", "Invalid original now-playing alert")
            } else throw failure("now_playing_publication", "Unexpected original now-playing callback")
            // Commit before callback invocation so a reentrant replay cannot
            // deliver the same original message twice.
            sequence = next
            options.deliver(event.message)
            accepted = true
        } catch (error) {
            report(error)
        } finally {
            if (reserved && frozen) {
                const owned = Promise.resolve()
                    .then(() => options.transport.complete({ ...frozen }, publication.id, accepted))
                    .then(
                        () => {},
                        (error) => {
                            if (same(options.getContext(), frozen) && !closed) report(error)
                        }
                    )
                    .finally(() => {
                        completions.delete(owned)
                        completionSlots--
                    })
                completions.set(owned, frozen)
            }
        }
    }
    function retire(frozen: RuntimeContext, terminal: boolean): Promise<void> {
        // A returned native error is a settled observation. Retry the same
        // process owner after that receipt, while a lost response stays owned.
        if (control?.settled && control.failed && !control.observation) control = undefined
        if (control && (!same(control.context, frozen) || (terminal && !control.terminal))) return Promise.reject(failure("now_playing_control_busy", "An earlier native retirement and its promises remain owned"))
        if (!control) {
            const captured = { ...frozen }
            const record = { context: captured, terminal, work: Promise.resolve() as Promise<unknown>, settled: false, failed: false, observation: undefined as Promise<void> | undefined }
            const work = Promise.resolve()
                .then(() => options.transport.retire(captured, terminal))
                .then(
                    (value) => {
                        record.settled = true
                        return value
                    },
                    (error) => {
                        record.settled = true
                        record.failed = true
                        throw error
                    }
                )
            // An observer timeout never discards this entered promise.
            work.catch(() => {})
            record.work = work
            control = record
        }
        if (control.observation) return control.observation
        const owned = control
        const observation = bounded(owned.work).then(async () => {
            const pending = [...commands, ...completions].filter(([, context]) => terminal || same(context, frozen)).map(([promise]) => promise)
            await bounded(Promise.allSettled(pending))
            if (control === owned) control = undefined
        })
        owned.observation = observation
        observation
            .finally(() => {
                if (owned.observation === observation) owned.observation = undefined
            })
            .catch(() => {})
        return observation
    }
    return {
        handle,
        publish,
        contextChanged(previous: RuntimeContext) {
            return retire(previous, false)
        },
        close() {
            closed = true
            const captured = options.getContext() ?? control?.context ?? lastContext
            return captured ? retire(captured, true) : Promise.resolve()
        },
        status() {
            return { commands: commands.size, completionSlots, completions: completions.size, control: !!control, closed, sequence: sequence.toString() }
        }
    }
}

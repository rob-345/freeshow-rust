// Controlled application state and native sinks. Web Audio, media, worklets,
// routing, analyser, input-capture and processor implementations stay real.
import { get, writable } from "svelte/store"
export const currentWindow = writable<string | null>(null)
export const disabledServers = writable({ output_stream: true })
export const serverData = writable({})
export const outputs = writable({})
export const media = writable({})
export const playingAudio = writable({})
export const playingVideos = writable([])
export const special = writable({ icecast: { enabled: false } })
export const audioEffects = writable({})
export const activeAudioEffects = writable({})
export const audioChannelsData = writable({})
export const channelSidechainMultipliers = writable({})
export const recordingChannels = writable({})
export const audioRouting = writable({
    channels: [{ id: "main", name: "Main" }],
    connections: [
        { from: "drawer_audio", to: "main" },
        { from: "mic_default", to: "main" },
        { from: "main", to: "speaker_default" }
    ]
})
export const native = { commands: [] as unknown[], packets: [] as unknown[], saved: [] as unknown[], ports: [] as MessagePort[], totalPackets: 0, firstNonzero: null as unknown, hold: false, deferred: [] as (() => void)[] }
export const getFirstOutput = () => null
export const getFirstActiveOutput = () => null
export const AudioPlayer = { getPath: (id: string) => id, initCheckLoop() {} }
export const AudioMicrophone = { channelCountCache: new Map() }
export const translateText = (text: string) => text
export const newToast = (text: string) => text
export const getRecordingFileName = () => "native-webkit-controlled-recording.webm"
function bounded<T>(values: T[], value: T, maximum = 64) {
    if (values.length >= maximum) throw new Error("Controlled native receipt limit")
    values.push(value)
}
export function send(channel: string, commands: string[], data: any) {
    bounded(native.commands, { channel, commands, data })
    if (commands[0] !== "INIT_PORT") return
    const pair = new MessageChannel()
    native.ports.push(pair.port1)
    pair.port1.onmessage = (event) => {
        const message = event.data,
            payload = message?.payload
        if (message?.channel !== "AUDIO" || !(payload?.buffer instanceof Uint8Array) || payload.buffer.byteLength > 32768) throw new Error("Invalid real worklet packet")
        if (++native.totalPackets > 100000) throw new Error("Controlled packet operation limit")
        const bytes = payload.buffer,
            samples = new Float32Array(bytes.buffer, bytes.byteOffset, bytes.byteLength / 4)
        let peak = 0
        for (const sample of samples) {
            if (!Number.isFinite(sample)) throw new Error("Nonfinite worklet PCM")
            peak = Math.max(peak, Math.abs(sample))
        }
        const summary = { id: payload.id, sampleRate: payload.sampleRate, bytes: bytes.byteLength, peak }
        if (peak > 0.001 && !native.firstNonzero) native.firstNonzero = summary
        // Keep a fixed diagnostic subset; do not queue the ongoing stream.
        if (native.packets.length < 32) native.packets.push(summary)
    }
    pair.port1.start()
    const respond = () => window.postMessage({ type: "AUDIO_PORT_RESPONSE", targetId: data.id }, "*", [pair.port2])
    if (native.hold) native.deferred.push(respond)
    else setTimeout(respond, 0)
}
export function sendMain(command: string, data: any) {
    if (!(data.blob instanceof ArrayBuffer) || data.blob.byteLength > 4 * 1024 * 1024) throw new Error("Invalid real recorder bytes")
    bounded(native.saved, { command, bytes: data.blob.byteLength, name: data.name, path: data.path })
}
export const snapshotStores = () => ({ routing: get(audioRouting), recording: get(recordingChannels) })

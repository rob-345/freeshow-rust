// Actual retained CAPTURE_CANVAS -> raw upload -> guarded native cache -> URL.
const fixture = arguments[0]
const proof = window.__thumbnailsProbe = { done: false, captures: [], blobs: [] }
const original = HTMLCanvasElement.prototype.toBlob
HTMLCanvasElement.prototype.toBlob = function (callback, type, quality) {
    const dimensions = { width: this.width, height: this.height }
    return original.call(this, (blob) => {
        if (!blob) return callback(blob)
        blob.arrayBuffer().then((buffer) => {
            const bytes = new Uint8Array(buffer), chunks = []
            for (let offset = 8; offset + 12 <= bytes.length;) {
                const size = new DataView(buffer).getUint32(offset)
                chunks.push(String.fromCharCode(...bytes.slice(offset + 4, offset + 8)))
                offset += size + 12
            }
            proof.blobs.push({ ...dimensions, bytes: Array.from(bytes), chunks })
            callback(blob)
        }).catch((error) => { proof.blobError = String(error); callback(null) })
    }, type, quality)
}
window.api.receive("MAIN", (message) => {
    if (message.channel === "CAPTURE_CANVAS") proof.captures.push({ input: message.data.input, id: message.data.id, width: message.data.size.width })
}, "native-thumbnail-captures")
let sequence = 0
const request = (command, data) => new Promise((resolve, reject) => {
    const id = `native-thumbnail-probe-${++sequence}`
    const cleanup = () => { clearTimeout(timer); window.api.removeListener("MAIN", id); window.removeEventListener("freeshow:runtime-error", failed) }
    const failed = (event) => {
        if (event.detail?.request?.listenerId !== id) return
        cleanup(); reject(new Error(JSON.stringify(event.detail.error)))
    }
    const timer = setTimeout(() => { cleanup(); reject(new Error(`${command} timed out`)) }, 22000)
    window.addEventListener("freeshow:runtime-error", failed)
    window.api.receive("MAIN", (message, listenerId) => {
        if (message.channel !== command || listenerId !== id) return
        const urls = {}
        if (command === "GET_THUMBNAIL" && message.data.output) urls[message.data.output] = window.api.resolveFilePath(message.data.output)
        if (command === "READ_FOLDER") for (const record of Object.values(message.data)) if (record.thumbnailPath) urls[record.thumbnailPath] = window.api.resolveFilePath(record.thumbnailPath)
        cleanup(); resolve({ data: message.data, callbackUrls: urls })
    }, id)
    window.api.send("MAIN", { channel: command, data }, id)
})
const decode = async (result) => {
    const output = result.data.output, url = result.callbackUrls[output]
    if (!url?.startsWith("http://127.0.0.1:")) throw new Error("Thumbnail reply ran before cache byte grant")
    const image = new Image()
    image.crossOrigin = "anonymous"
    await new Promise((resolve, reject) => { image.onload = resolve; image.onerror = () => reject(new Error("Installed native thumbnail did not decode")); image.src = url })
    const canvas = document.createElement("canvas")
    canvas.width = image.naturalWidth; canvas.height = image.naturalHeight
    canvas.getContext("2d").drawImage(image, 0, 0)
    return { ...result, width: canvas.width, height: canvas.height, rgba: Array.from(canvas.getContext("2d").getImageData(0, 0, 1, 1).data) }
}
;(async () => {
    try {
        proof.image = await decode(await request("GET_THUMBNAIL", { input: fixture.image, size: 16, nativeFutureField: "preserved" }))
        const beforeHit = proof.captures.length
        proof.hit = await request("GET_THUMBNAIL", { input: fixture.image, size: 16, nativeFutureField: "preserved" })
        if (proof.captures.length !== beforeHit) throw new Error("Existing thumbnail recaptured instead of using cache")
        proof.video = await decode(await request("GET_THUMBNAIL", { input: fixture.video, size: 8 }))
        proof.folder = await request("READ_FOLDER", { path: fixture.folder, depth: 0, captureFolderContent: true })
        proof.exists = await request("DOES_MEDIA_EXIST", { path: fixture.image })
        proof.noCache = await request("DOES_MEDIA_EXIST", { path: fixture.image, noCache: true })
        proof.missing = await request("GET_THUMBNAIL", { input: fixture.missing, size: 16 })
        proof.cachePath = await request("GET_CACHE_PATH", undefined)
        proof.status = await window.__TAURI__.core.invoke("thumbnails_status")
    } catch (error) { proof.error = String(error) }
    finally {
        HTMLCanvasElement.prototype.toBlob = original
        window.api.removeListener("MAIN", "native-thumbnail-captures")
        proof.done = true
    }
})()

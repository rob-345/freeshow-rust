// Real retained window.api delivery and actual WebKit decoders/canvas.
const fixture = arguments[0]
const proof = window.__assetsProbe = { origin: location.origin, done: false, callbackUrls: {}, media: [] }
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
const until = async (check, name) => {
    const deadline = performance.now() + 12000
    while (performance.now() < deadline) {
        if (check()) return
        await pause(20)
    }
    throw new Error(`Native asset probe timed out: ${name}`)
}
const requestShow = () => new Promise((resolve, reject) => {
    const id = "native-assets-show"
    const timer = setTimeout(() => { window.api.removeListener("MAIN", id); reject(new Error("SHOW media publication timed out")) }, 15000)
    window.api.receive("MAIN", (message, listenerId) => {
        if (message.channel !== "SHOW" || listenerId !== id) return
        clearTimeout(timer)
        window.api.removeListener("MAIN", id)
        for (const path of [fixture.image, fixture.audio, fixture.video]) proof.callbackUrls[path] = window.api.resolveFilePath(path)
        resolve(message.data)
    }, id)
    window.api.send("MAIN", { channel: "SHOW", data: { id: fixture.id, name: fixture.name } }, id)
})
const image = async (url, cors) => {
    const element = new Image()
    if (cors) element.crossOrigin = "anonymous"
    await new Promise((resolve, reject) => { element.onload = resolve; element.onerror = () => reject(new Error("Native image decode failed")); element.src = url })
    const canvas = document.createElement("canvas")
    canvas.width = element.naturalWidth
    canvas.height = element.naturalHeight
    canvas.getContext("2d").drawImage(element, 0, 0)
    return { element, canvas }
}
;(async () => {
    try {
        const importedUrl = fixture.importUrlAtCallback
        if (!/^http:\/\/127\.0\.0\.1:\d+\/[a-f0-9]{64}\/[a-f0-9]{32}$/.test(importedUrl || "")) throw new Error("IMPORT2 callback ran before its committed byte grant")
        const reply = await requestShow()
        if (reply.content?.[0] !== fixture.id || reply.content?.[1]?.name !== fixture.name) throw new Error("Native SHOW payload changed")
        for (const path of [fixture.image, fixture.audio, fixture.video]) if (!proof.callbackUrls[path]?.startsWith("http://127.0.0.1:")) throw new Error(`SHOW callback ran before media publication: ${path}`)
        const url = proof.callbackUrls[fixture.image]
        if (url !== importedUrl) throw new Error("Repeated committed media failed native capability deduplication")
        const full = await fetch(url)
        proof.pngBytes = Array.from(new Uint8Array(await full.arrayBuffer()))
        proof.contentType = full.headers.get("Content-Type")
        const head = await fetch(url, { method: "HEAD" })
        proof.head = { status: head.status, length: Number(head.headers.get("Content-Length")), bodyLength: (await head.arrayBuffer()).byteLength }
        const range = await fetch(url, { headers: { Range: "bytes=0-7" } })
        proof.range = { status: range.status, contentRange: range.headers.get("Content-Range"), bytes: Array.from(new Uint8Array(await range.arrayBuffer())) }
        const granted = await image(url, true)
        proof.image = { width: granted.canvas.width, height: granted.canvas.height, rgba: Array.from(granted.canvas.getContext("2d").getImageData(0, 0, 1, 1).data), exported: granted.canvas.toDataURL("image/png").startsWith("data:image/png") }
        const tainted = await image(url, false)
        try { tainted.canvas.toDataURL("image/png"); throw new Error("A cross-origin canvas unexpectedly exported without CORS") } catch (error) {
            if (error.name !== "SecurityError") throw error
            proof.corsRequired = true
        }
        for (const [kind, path] of [["audio", fixture.audio], ["video", fixture.video]]) {
            const element = document.createElement(kind)
            let error
            element.addEventListener("error", () => error = element.error?.code || "media error")
            element.crossOrigin = "anonymous"
            element.muted = true
            element.src = proof.callbackUrls[path]
            element.load()
            await until(() => error || element.readyState >= 2, `${kind} metadata`)
            if (error) throw new Error(`${kind} WebKit decoder failed: ${error}`)
            element.currentTime = 0.5
            await until(() => !element.seeking && element.currentTime >= 0.49, `${kind} seek`)
            await element.play()
            await until(() => element.currentTime > 0.55, `${kind} playback`)
            const result = { kind, duration: element.duration, playedTime: element.currentTime, playing: !element.paused, source: element.currentSrc }
            if (kind === "video") {
                const canvas = document.createElement("canvas")
                canvas.width = element.videoWidth; canvas.height = element.videoHeight
                canvas.getContext("2d").drawImage(element, 0, 0)
                result.width = canvas.width; result.height = canvas.height
                result.rgba = Array.from(canvas.getContext("2d").getImageData(0, 0, 1, 1).data)
            }
            element.pause(); element.removeAttribute("src"); element.load()
            proof.media.push(result)
        }
        proof.status = await window.__TAURI__.core.invoke("assets_status")
    } catch (error) { proof.error = String(error) }
    finally { proof.done = true }
})()

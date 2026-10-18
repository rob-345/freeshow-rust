// Exercise the installed native bridge and observe types before WebDriver JSON.
const fixture = arguments[0]
return (async () => {
    const started = performance.now(), context = window.api.getRuntimeContext()
    let sequence = 0
    const request = (command, data) => new Promise((resolve, reject) => {
        const id = "native-file-read-" + ++sequence
        const cleanup = () => { clearTimeout(timer); window.api.removeListener("MAIN", id); window.removeEventListener("freeshow:runtime-error", failed) }
        const failed = (event) => {
            if (event.detail?.request?.listenerId !== id) return
            cleanup(); reject(new Error(JSON.stringify(event.detail.error)))
        }
        const timer = setTimeout(() => { cleanup(); reject(new Error(command + " deadline elapsed")) }, 2500)
        window.addEventListener("freeshow:runtime-error", failed)
        window.api.receive("MAIN", (message, listenerId) => {
            if (listenerId !== id || message.channel !== command) return
            cleanup(); resolve(message.data)
        }, id)
        window.api.send("MAIN", { channel: command, data }, id)
    })
    const proof = { context }
    try {
        const [file, image, missing, bibles] = await Promise.all([
            request("READ_FILE", { path: fixture.text }),
            request("READ_EXIF", { id: fixture.image }),
            request("READ_EXIF", { id: fixture.missing }),
            request("READ_BIBLES_FOLDER", { dataPath: "/ignored", path: "/ignored" })
        ])
        proof.file = file
        proof.image = {
            id: image.id, groups: Object.keys(image.exif), imageKeys: Object.keys(image.exif.image),
            make: image.exif.image.Make, date: image.exif.exif.DateTimeOriginal,
            bytes: Array.from(image.exif.exif.ExifVersion), typed: image.exif.exif.ExifVersion instanceof Uint8Array,
            numbers: [image.exif.image.YResolution[1] === Infinity, Number.isNaN(image.exif.image.YResolution[2]), Object.is(image.exif.image.YResolution[3], -0), image.exif.image.YResolution[4] === -Infinity]
        }
        proof.missing = { id: missing.id, ownExif: Object.hasOwn(missing, "exif"), undefinedExif: missing.exif === undefined, keys: Object.keys(missing) }
        proof.bibles = bibles
        proof.current = window.api.getRuntimeContext()
        proof.status = await window.__TAURI__.core.invoke("file_reads_status")
        proof.elapsedMs = performance.now() - started
    } catch (error) { proof.error = String(error) }
    return { result: proof }
})()

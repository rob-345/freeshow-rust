// Adapt native OS file drops to the retained Svelte DropArea/timeline handlers.
// Local descriptors deliberately do not pretend to be empty browser Files.
export type DropContext = { runtimeId: string; libraryGeneration: number; dataPath: string }
export type NativeFileInfo = { path: string; name: string; directory?: boolean; size?: number; lastModified?: number }
export type NativeDropPayload = {
    phase: "enter" | "over" | "drop" | "leave" | "error"
    sequence: number
    context: DropContext
    files?: NativeFileInfo[]
    position?: { x: number; y: number; units: "physical" | "logical"; scaleFactor: number }
    error?: { code: string; message: string }
}
type Options = { getRuntimeContext: () => DropContext | undefined; reportError: (error: unknown) => void }

export function sameDropContext(a: DropContext | undefined, b: DropContext | undefined) {
    return !!a && !!b && a.runtimeId === b.runtimeId && a.libraryGeneration === b.libraryGeneration && a.dataPath === b.dataPath
}

export function dropCoordinates(position: NonNullable<NativeDropPayload["position"]>, devicePixelRatio: number) {
    const { x, y, scaleFactor, units } = position
    if (![x, y, scaleFactor, devicePixelRatio].every(Number.isFinite) || scaleFactor <= 0 || devicePixelRatio <= 0) return null
    // GTK widget coordinates and AppKit points are already logical. WebView2
    // ScreenToClient coordinates are physical. Account for webview zoom too.
    const divisor = units === "physical" ? devicePixelRatio : devicePixelRatio / scaleFactor
    return { x: x / divisor, y: y / divisor }
}

export function createNativeDrop(target: Window, options: Options) {
    const registry = new WeakMap<object, { path: string; context: DropContext }>()
    const generated = new WeakSet<Event>()
    let gesture: { sequence: number; context: DropContext } | undefined
    let latestSequence = 0
    let hovered: Element | null = null
    let internalDrag = false
    let modifiers = { shiftKey: false, ctrlKey: false, altKey: false, metaKey: false }

    function reject(code: string, message: string): never {
        const error = Object.assign(new Error(message), { code })
        options.reportError(error)
        throw error
    }

    function showFilePath(file: unknown): string {
        if (file && typeof file === "object") {
            const local = registry.get(file)
            if (local) {
                if (!sameDropContext(local.context, options.getRuntimeContext())) reject("stale_library", "This dropped file belongs to a library that is no longer active")
                return local.path
            }
            // Real browser Files keep their real data. As in webUtils, a file
            // created in JavaScript has no native path and returns an empty one.
            const path = (file as { path?: unknown }).path
            if (typeof path === "string") return path
        }
        return ""
    }

    function descriptor(info: NativeFileInfo, context: DropContext) {
        const file = {
            name: info.name,
            type: info.directory ? "" : "application/octet-stream",
            size: info.size,
            lastModified: info.lastModified,
            webkitRelativePath: "",
            get path() {
                return showFilePath(file)
            },
            arrayBuffer: () => reject("unsupported_native_file_bytes", "Native file-drop descriptors support paths, not browser File byte reads"),
            text: () => reject("unsupported_native_file_bytes", "Native file-drop descriptors support paths, not browser File byte reads"),
            stream: () => reject("unsupported_native_file_bytes", "Native file-drop descriptors support paths, not browser File byte reads"),
            slice: () => reject("unsupported_native_file_bytes", "Native file-drop descriptors support paths, not browser File byte reads")
        }
        registry.set(file, { path: info.path, context: { ...context } })
        return Object.freeze(file)
    }

    function transfer(files: object[]) {
        const list = [...files] as object[] & { item: (index: number) => object | null }
        list.item = (index) => list[index] || null
        return {
            files: list,
            items: files.map((file) => ({ kind: "file", type: (file as { type: string }).type, getAsFile: () => file })),
            types: ["Files"],
            dropEffect: "copy",
            effectAllowed: "copy",
            getData: () => ""
        }
    }

    function dispatch(type: string, node: Element, point: { x: number; y: number }, files: object[]) {
        const EventType = (target as any).DragEvent || (target as any).MouseEvent
        const event = new EventType(type, { bubbles: true, cancelable: true, clientX: point.x, clientY: point.y, ...modifiers })
        Object.defineProperty(event, "dataTransfer", { value: transfer(files) })
        generated.add(event)
        node.dispatchEvent(event)
    }

    function clear() {
        const previous = hovered
        previous?.removeAttribute("data-native-drop-active")
        hovered = null
        gesture = undefined
        if (previous) dispatch("dragleave", previous, { x: 0, y: 0 }, [])
    }

    function handle(payload: NativeDropPayload) {
        if (!Number.isSafeInteger(payload.sequence) || payload.sequence < latestSequence) return
        latestSequence = payload.sequence
        if (internalDrag) return
        if (payload.phase === "error") {
            clear()
            options.reportError(payload.error || { code: "native_drop_error", message: "Native file drop failed" })
            return
        }
        if (!sameDropContext(payload.context, options.getRuntimeContext())) {
            clear()
            return
        }
        if (payload.phase === "enter") {
            clear()
            if (!payload.files?.length) return
            gesture = { sequence: payload.sequence, context: { ...payload.context } }
        }
        if (!gesture || gesture.sequence !== payload.sequence) return
        if (payload.phase === "leave") {
            clear()
            return
        }
        if (!payload.position) return
        const point = dropCoordinates(payload.position, target.devicePixelRatio)
        if (!point || point.x < 0 || point.y < 0 || point.x >= target.innerWidth || point.y >= target.innerHeight) {
            clear()
            return
        }
        const hit = target.document.elementFromPoint(point.x, point.y)
        const area = hit?.closest(".droparea, .timeline-track-wrapper") || null
        if (area !== hovered) {
            if (hovered) {
                hovered.removeAttribute("data-native-drop-active")
                dispatch("dragleave", hovered, point, [])
            }
            hovered = area
            if (hovered) {
                hovered.setAttribute("data-native-drop-active", "true")
                dispatch("dragenter", hit!, point, [])
            }
        }
        if (!hit || !area) {
            if (payload.phase === "drop") clear()
            return
        }
        if (payload.phase === "drop") {
            // Recheck immediately before entering the original handlers. A
            // library change between Enter and Drop must not reinterpret files.
            if (payload.files?.length && sameDropContext(gesture.context, options.getRuntimeContext()))
                dispatch(
                    "drop",
                    hit,
                    point,
                    payload.files.map((info) => descriptor(info, payload.context))
                )
            clear()
        } else {
            // File-over hints in upstream DropArea never reset on dragleave.
            // Use a scoped native marker instead of latching that Svelte state.
            dispatch("dragover", hit, point, [])
        }
    }

    const start = (event: Event) => {
        if (!generated.has(event)) {
            internalDrag = true
            clear()
        }
    }
    const end = () => {
        internalDrag = false
        clear()
    }
    const keys = (event: Event) => {
        const key = event as KeyboardEvent
        modifiers = { shiftKey: key.shiftKey, ctrlKey: key.ctrlKey, altKey: key.altKey, metaKey: key.metaKey }
        if (key.key === "Escape") clear()
    }
    const preventDuplicate = (event: Event) => {
        if (gesture && !internalDrag && !generated.has(event) && Array.from((event as DragEvent).dataTransfer?.types || []).includes("Files")) {
            event.preventDefault()
            event.stopImmediatePropagation()
        }
    }
    target.addEventListener("dragstart", start, true)
    target.addEventListener("dragend", end, true)
    target.addEventListener("keydown", keys, true)
    target.addEventListener("keyup", keys, true)
    target.addEventListener("drop", preventDuplicate, true)
    target.addEventListener("blur", end)

    return {
        handle,
        showFilePath,
        dispose() {
            clear()
            target.removeEventListener("dragstart", start, true)
            target.removeEventListener("dragend", end, true)
            target.removeEventListener("keydown", keys, true)
            target.removeEventListener("keyup", keys, true)
            target.removeEventListener("drop", preventDuplicate, true)
            target.removeEventListener("blur", end)
        }
    }
}

export async function installNativeDrop(target: Window, options: Options) {
    const tauri = (target as any).__TAURI__
    const label = tauri?.webviewWindow?.getCurrentWebviewWindow?.().label
    if (!tauri?.event?.listen || label !== "main") throw new Error("Native file drop is available only in the main desktop WebView")
    const adapter = createNativeDrop(target, options)
    try {
        const unlisten = await tauri.event.listen("freeshow://drop", (event: { payload: NativeDropPayload }) => adapter.handle(event.payload), { target: { kind: "WebviewWindow", label } })
        return {
            ...adapter,
            dispose: () => {
                unlisten()
                adapter.dispose()
            }
        }
    } catch (error) {
        adapter.dispose()
        throw error
    }
}

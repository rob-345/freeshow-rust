// Restore the one undefined-valued property in original READ_FOLDER records.
// Untouched pinned Node22 Stats have only numeric own properties over IPC.
export function hydrateFileBrowse(command: string, data: unknown): unknown {
    if (command !== "READ_FOLDER" || !data || typeof data !== "object" || Array.isArray(data)) return data
    for (const entry of Object.values(data)) {
        if (entry && typeof entry === "object" && entry.isFolder === true && Object.hasOwn(entry, "noMedia") && entry.noMedia === null) entry.noMedia = undefined
    }
    return data
}

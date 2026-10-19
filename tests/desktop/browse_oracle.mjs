// Original filesystem handlers, with thumbnail modes explicitly excluded here.
// Real packaged IPC/source corpus covers their numeric Stats/undefined shape.
import fs from "node:fs"
import path from "node:path"
import v8 from "node:v8"
import { upstreamFunctions, clean } from "../parity/oracle.mjs"

const [folder, file] = process.argv.slice(2)
if (!folder || !file) throw new Error("Expected a controlled folder and file")
const source = upstreamFunctions("src/electron/utils/files.ts", ["readFolderContent", "getFileStatsAsync", "readFolderAsync", "asyncPool", "getFileInfo", "getFileStats", "getExtension", "getSimularPaths", "getParentFolderName", "similarity", "readFolder", "isPermissionDeniedError"], {
    fs,
    path,
    join: path.join,
    parse: path.parse,
    actionComplete() {},
    createThumbnail() {
        throw new Error("This oracle does not exercise thumbnail modes")
    }
})
const cases = []
for (const [command, data, handler] of [
    ["FILE_INFO", file, source.getFileInfo],
    ["READ_FOLDER", { path: folder, depth: 0, generateThumbnails: false, captureFolderContent: false }, source.readFolderContent],
    ["GET_SIMILAR", { paths: [file] }, source.getSimularPaths]
]) {
    const result = v8.deserialize(v8.serialize(await handler(structuredClone(data))))
    const shape =
        command === "READ_FOLDER"
            ? Object.fromEntries(
                  Object.entries(result)
                      .filter(([, entry]) => entry.isFolder)
                      .map(([key, entry]) => [key, { ownNoMedia: Object.hasOwn(entry, "noMedia"), noMediaType: typeof entry.noMedia }])
              )
            : null
    cases.push({ command, data, result: clean(result), shape })
}
console.log(JSON.stringify({ cases }))

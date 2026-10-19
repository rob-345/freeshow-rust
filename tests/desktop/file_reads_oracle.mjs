// Pinned source/value oracle; no native implementation or fixture parser.
import fs from "node:fs"
import path from "node:path"
import { createRequire } from "node:module"
import { upstreamFunctions } from "../parity/oracle.mjs"
const fixture = JSON.parse(fs.readFileSync(0, "utf8"))
const { ExifImage } = createRequire(import.meta.url)("exif")
const files = upstreamFunctions("src/electron/utils/files.ts", ["readFile", "safeBufferToString", "readFolder", "isPermissionDeniedError", "readExifData"], { fs, ExifImage, MAX_NODE_STRING_LENGTH: 0x1fffffe8, actionComplete() {} })
const bible = upstreamFunctions("src/electron/IPC/responsesMain.ts", ["readBiblesFolder"], { path, readFolder: files.readFolder, getDataFolderPath: () => path.join(fixture.dataRoot, "Bibles") })
const image = await files.readExifData({ id: fixture.image }), missing = await files.readExifData({ id: fixture.missing })
process.stdout.write(JSON.stringify({
    file: { content: files.readFile(fixture.text) },
    image: {
        id: image.id, groups: Object.keys(image.exif), imageKeys: Object.keys(image.exif.image),
        make: image.exif.image.Make, date: image.exif.exif.DateTimeOriginal,
        bytes: Array.from(image.exif.exif.ExifVersion), typed: image.exif.exif.ExifVersion instanceof Uint8Array,
        numbers: [image.exif.image.YResolution[1] === Infinity, Number.isNaN(image.exif.image.YResolution[2]), Object.is(image.exif.image.YResolution[3], -0), image.exif.image.YResolution[4] === -Infinity]
    },
    missing: { id: missing.id, ownExif: Object.hasOwn(missing, "exif"), undefinedExif: missing.exif === undefined, keys: Object.keys(missing) },
    bibles: bible.readBiblesFolder()
}))

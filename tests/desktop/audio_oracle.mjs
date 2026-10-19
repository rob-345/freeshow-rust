import { parseFile } from "music-metadata"
import { upstreamFunctions } from "../parity/oracle.mjs"

const { getAudioMetadata } = upstreamFunctions("src/electron/audio/audio.ts", ["getAudioMetadata"], { parseFile })
process.stdout.write(JSON.stringify(await getAudioMetadata(process.argv[2])) + "\n")

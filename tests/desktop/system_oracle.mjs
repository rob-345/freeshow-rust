import os from "node:os"
import { upstreamFunctions, clean } from "../parity/oracle.mjs"

const captured = process.argv[2] ? JSON.parse(process.argv[2]) : null
const boundary = captured ? { ...os, totalmem: () => captured.total, freemem: () => captured.free } : os
const source = upstreamFunctions("src/electron/IPC/responsesMain.ts", ["getOS", "getDeviceName", "checkRamUsage"], { os: boundary })
process.stdout.write(JSON.stringify(clean({ os: source.getOS(), deviceName: source.getDeviceName(), ram: source.checkRamUsage() })) + "\n")

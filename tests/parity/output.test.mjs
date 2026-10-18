import assert from "node:assert/strict"
import { test } from "node:test"
import { clean, upstreamClasses } from "./oracle.mjs"
import { rust } from "./runner.mjs"

function routerOracle(endpoints) {
    const deliveries = []
    const { OutputHelper } = upstreamClasses(
        [
            ["src/electron/output/helpers/OutputSend.ts", "OutputSend"],
            ["src/electron/output/OutputHelper.ts", "OutputHelper"]
        ],
        {
            OUTPUT: "OUTPUT",
            clone: structuredClone,
            OutputBounds: {},
            OutputIdentify: {},
            OutputLifecycle: {},
            OutputValues: {},
            OutputVisibility: {},
            toApp: (channel, message) => deliveries.push({ target: "main", message: clean(message) })
        }
    )
    for (const endpoint of endpoints) {
        OutputHelper.setOutput(endpoint.id, {
            follower: endpoint.follower,
            window: endpoint.absent
                ? null
                : {
                      isDestroyed: () => endpoint.available === false,
                      webContents: {
                          send: (channel, message) => {
                              assert.equal(channel, "OUTPUT")
                              deliveries.push({
                                  target: endpoint.label,
                                  message: clean(message)
                              })
                          }
                      }
                  }
        })
    }
    return (message) => {
        deliveries.length = 0
        const snapshot = clean(message)
        OutputHelper.receiveOutput(null, message)
        assert.deepEqual(message, snapshot, "routing must not modify its input")
        return clean(deliveries)
    }
}

test("output fanout matches actual upstream classes, including targeting, followers and numeric ID order", () => {
    const endpoints = [
        { id: "日本語", label: "unicode" },
        { id: "10", label: "ten" },
        { id: "2", label: "two" },
        { id: "follower", label: "shared", follower: true },
        { id: "destroyed", label: "dead", available: false },
        { id: "absent", label: "missing", available: false, absent: true },
        { id: "01", label: "non-index" }
    ]
    const oracle = routerOracle(endpoints)
    const messages = []
    for (const channel of ["OUTPUTS", "SHOWS", "OUTPUT_SLIDE_CACHE", "REQUEST_DATA_MAIN", "xMAINx"]) {
        for (const data of [undefined, null, false, 0, "", "text", [], {}, { id: "日本語", value: "🙌" }, { id: "10" }, { id: "unknown" }, { id: 2 }, { id: true }, { id: "" }, { id: false }, { 2: { name: "Two" }, 10: { name: "Ten" }, 日本語: { name: "日本語" } }, { 2: false, 10: 0, 日本語: null }]) {
            messages.push(
                clean({
                    channel,
                    data,
                    id: "top-level metadata",
                    name: "preserve",
                    futureField: { intact: true }
                })
            )
        }
    }
    const [result] = rust([
        {
            op: "batch",
            cases: messages.map((message) => ({
                op: "output.route",
                endpoints,
                message
            }))
        }
    ])
    assert.equal(result.ok, true, JSON.stringify(result.error))
    messages.forEach((message, index) => assert.deepEqual(result.value[index], oracle(message), JSON.stringify(message)))
})

test("display placement matches original OutputVisibility across topology, locks and automatic positioning", () => {
    const cases = []
    const displayBounds = [
        { x: 0, y: 0, width: 1920, height: 1080 },
        { x: -1280, y: -100, width: 1280, height: 1024 },
        { x: 1920, y: 0, width: 2560, height: 1440 }
    ]
    for (const count of [0, 1, 2, 3])
        for (const isMac of [false, true])
            for (const autoPosition of [false, true])
                for (const boundsLocked of [false, true]) {
                    const displays = displayBounds.slice(0, count).map((bounds, index) => ({ id: String(index + 1), bounds }))
                    for (const bounds of [undefined, { x: 100, y: 100, width: 400, height: 300 }, { x: -9999, y: 0, width: 800, height: 600 }, { x: -1280, y: -100, width: 1280, height: 1024 }]) {
                        for (const screen of [null, "2", "missing"])
                            cases.push({
                                op: "output.bounds",
                                output: { bounds, boundsLocked, screen },
                                displays,
                                main: displayBounds[0],
                                isMac,
                                autoPosition
                            })
                    }
                }
    const [result] = rust([{ op: "batch", cases }])
    assert.equal(result.ok, true, JSON.stringify(result.error))
    for (const [index, input] of cases.entries()) {
        const { OutputVisibility } = upstreamClasses([["src/electron/output/helpers/OutputVisibility.ts", "OutputVisibility"]], {
            screen: { getAllDisplays: () => input.displays },
            mainWindow: { getBounds: () => input.main },
            isMac: input.isMac
        })
        assert.deepEqual(result.value[index], clean(OutputVisibility.resolveOutputBounds(input.output, input.autoPosition)), JSON.stringify(input))
    }
})

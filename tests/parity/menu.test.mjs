import assert from "node:assert/strict"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { spawnSync } from "node:child_process"
import { test } from "node:test"
import vm from "node:vm"
import ts from "typescript"
import { clean, root, upstreamFunctions } from "./oracle.mjs"

let probe

// Build the actual native specification without GTK, a display, or changes to
// either application manifest. Pin the dependency to this checkout's lockfile.
function rust(cases) {
    if (!probe) {
        const directory = fs.mkdtempSync(path.join(os.tmpdir(), "freeshow-menu-parity-"))
        const version = fs.readFileSync(path.join(root, "Cargo.lock"), "utf8").match(/name = "serde_json"\nversion = "([^"]+)"/)[1]
        fs.mkdirSync(path.join(directory, "src"))
        fs.writeFileSync(path.join(directory, "Cargo.toml"), `[package]\nname="freeshow-menu-parity"\nversion="0.0.0"\nedition="2021"\n[dependencies]\nserde_json={version="=${version}",features=["preserve_order"]}\n`)
        fs.writeFileSync(
            path.join(directory, "src/main.rs"),
            `#[path=${JSON.stringify(path.join(root, "src-tauri/src/menu_spec.rs"))}] mod spec;
use std::io::{self, BufRead};
use serde_json::{json, Value};
fn main() {
    for line in io::stdin().lock().lines() {
        let input: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let result = if input["op"] == "action" {
            let action = spec::action_from_id(input["id"].as_str().unwrap());
            json!({"known":action.is_some(),"event":action.as_ref().and_then(spec::Action::renderer_event),"effect":action.as_ref().map(spec::Action::original_effect)})
        } else {
            let menu = spec::application_menu(&input["strings"], input["platform"].as_str().unwrap(), input["production"].as_bool().unwrap(), input["appName"].as_str().unwrap());
            spec::normalized(menu.as_deref())
        };
        println!("{}", result);
    }
}`
        )
        const target = path.join(root, "target/menu-parity")
        const build = spawnSync("cargo", ["build", "--offline", "--quiet", "--manifest-path", path.join(directory, "Cargo.toml")], {
            encoding: "utf8",
            env: { ...process.env, CARGO_TARGET_DIR: target, CARGO_BUILD_JOBS: "2" },
            timeout: 120000
        })
        fs.rmSync(directory, { recursive: true, force: true })
        assert.equal(build.error, undefined)
        assert.equal(build.status, 0, build.stderr)
        probe = path.join(target, "debug", process.platform === "win32" ? "freeshow-menu-parity.exe" : "freeshow-menu-parity")
    }
    const child = spawnSync(probe, [], { input: cases.map((input) => JSON.stringify(input)).join("\n") + "\n", encoding: "utf8", maxBuffer: 64 * 1024 * 1024, timeout: 30000 })
    assert.equal(child.error, undefined)
    assert.equal(child.status, 0, child.stderr)
    return child.stdout.trim().split("\n").map(JSON.parse)
}

function oracle(input) {
    const effects = []
    const exports = {}
    const boundaries = {
        electron: { app: { name: input.appName } },
        "..": { isMac: input.platform === "darwin", isProd: input.production },
        "../../types/IPC/ToMain": { ToMain: { MENU: "MENU" } },
        "../IPC/main": {
            sendToMain: (channel, id) => {
                assert.equal(channel, "MENU")
                effects.push({ kind: "menu", id })
            }
        },
        "../IPC/responsesMain": { openURL: (url) => effects.push({ kind: "url", url }) }
    }
    // Execute the entire original module, including its private mc helper.
    // Only Electron/platform/IPC/open-URL module boundaries are mocked.
    const javascript = ts.transpileModule(fs.readFileSync(path.join(root, "src/electron/utils/menuTemplate.ts"), "utf8"), {
        compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 }
    }).outputText
    vm.runInNewContext(javascript, {
        exports,
        require: (name) => {
            assert.ok(name in boundaries, `Unexpected menu dependency: ${name}`)
            return boundaries[name]
        }
    })
    const { template } = exports
    let applicationMenu
    const { setGlobalMenu } = upstreamFunctions("src/electron/index.ts", ["setGlobalMenu"], {
        isWindows: input.platform === "win32",
        isProd: input.production,
        template,
        Menu: { buildFromTemplate: (spec) => spec, setApplicationMenu: (menu) => (applicationMenu = menu) }
    })
    setGlobalMenu(input.strings)
    const normalize = (item) => {
        effects.length = 0
        item.click?.()
        assert.ok(effects.length <= 1)
        const action = effects[0] || null
        return {
            label: item.label ?? null,
            role: item.role ?? null,
            type: item.type ?? "normal",
            enabled: item.enabled ?? true,
            checked: item.checked ?? null,
            accelerator: item.accelerator ?? null,
            action,
            submenu: item.submenu ? Array.from(item.submenu, normalize) : null
        }
    }
    return applicationMenu === null ? null : clean(Array.from(applicationMenu, normalize))
}

test("LANGUAGE menus match the pinned source for every bundled locale and platform/production condition", () => {
    const locales = fs.readdirSync(path.join(root, "public/lang")).filter((name) => name.endsWith(".json"))
    const dictionaries = locales.map((name) => [name, JSON.parse(fs.readFileSync(path.join(root, "public/lang", name), "utf8"))])
    dictionaries.push(
        ["missing", {}],
        ["empty sections", { actions: {}, main: null, titlebar: {}, popup: null }],
        ["empty labels", { titlebar: { file: "", edit: "", view: "", help: "" }, actions: { save: "", delete: "" }, main: { quit: "", about: "" } }],
        ["Unicode and ampersands", { titlebar: { file: "日本語 & Fichier", edit: "تعديل", view: "Ελληνικά", help: "🙌🏾" }, actions: { save: "Speichern & sichern", paste: "貼り付け", selectAll: "\tSelect\nAll" }, main: { quit: "خروج", docs: "説明書" }, guide: { start: " " } }]
    )
    const cases = []
    for (const [locale, strings] of dictionaries) for (const platform of ["linux", "darwin", "win32"]) for (const production of [false, true]) cases.push({ locale, platform, production, strings, appName: "FreeShow" })
    const actual = rust(cases)
    cases.forEach((input, index) => assert.deepEqual(actual[index], oracle(input), `${input.locale}/${input.platform}/production=${input.production}`))
    assert.equal(cases.length, (locales.length + 4) * 6)
})

test("every custom menu action retains its renderer contract; both Quit entries request the save/close handshake", () => {
    const spec = oracle({ strings: {}, platform: "darwin", production: false, appName: "FreeShow" })
    const effects = spec
        .flatMap((menu) => menu.submenu)
        .map((item) => item.action)
        .filter(Boolean)
    const ids = effects.map((effect) => (effect.kind === "menu" ? effect.id : "docs_url"))
    ids.push("app_quit", "reload", "toggleDevTools", "togglefullscreen", "unknown")
    const actual = rust(ids.map((id) => ({ op: "action", id: "freeshow.menu." + id })))
    effects.forEach((effect, index) => {
        assert.deepEqual(actual[index].effect, effect)
        const quit = effect.id === "quit"
        assert.deepEqual(actual[index].event, effect.kind === "url" ? null : { channel: "MAIN", message: { channel: quit ? "CLOSE" : "MENU", data: quit ? false : effect.id } })
    })
    assert.deepEqual(actual[effects.length].event, { channel: "MAIN", message: { channel: "CLOSE", data: false } })
    assert.deepEqual(actual.at(-1), { known: false, event: null, effect: null })
    assert.deepEqual(rust([{ op: "action", id: "quit" }])[0], { known: false, event: null, effect: null })
    // MAIN.CLOSE false and the original MENU.quit enter the same real renderer
    // function without skipping the unsaved-changes popup.
    const calls = []
    const { initializeClosing } = upstreamFunctions("src/frontend/utils/save.ts", ["initializeClosing"], {
        special: { showClosePopup: true },
        errorHasOccurred: false,
        saved: false,
        get: (store) => store,
        activePopup: { set: (value) => calls.push(value) }
    })
    initializeClosing()
    initializeClosing(false)
    assert.deepEqual(calls, ["unsaved", "unsaved"])
})

"""Real GTK menu interactions, with the existing Svelte renderer and IPC bridge.

Build the native application with custom-protocol, then run under
`xvfb-run -a dbus-run-session -- /usr/bin/python3 tests/desktop/menu.py`.
Uses the same local driver/native-header environment as smoke.py.
"""
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi
from gi.repository import GLib

ROOT = Path(__file__).resolve().parents[2]
ARTIFACTS = ROOT / "test-output-rust" / "native-menu"


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def wait(predicate, label):
    deadline = time.monotonic() + 25
    error = None
    while time.monotonic() < deadline:
        try:
            result = predicate()
            if result:
                print("PASS", label, flush=True)
                return result
        except Exception as problem:
            error = problem
        time.sleep(0.1)
    raise AssertionError(f"Timed out: {label}: {error}")


def native_menu():
    pending = [(Atspi.get_desktop(0), 0)]
    while pending:
        node, depth = pending.pop()
        try:
            if node.get_role() == Atspi.Role.MENU_BAR:
                return node
            if depth < 5:
                pending.extend((node.get_child_at_index(i), depth + 1)
                               for i in range(node.get_child_count()))
        except GLib.Error:
            # LANGUAGE replaces the GTK objects. Re-observe after a stale
            # accessibility reference instead of reusing the removed menu.
            continue
    return None


def find(node, label):
    if node is None:
        return None
    try:
        if node.get_name().replace("_", "") == label:
            return node
        for index in range(node.get_child_count()):
            found = find(node.get_child_at_index(index), label)
            if found:
                return found
    except GLib.Error:
        return None
    return None


def run():
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    receipt = ARTIFACTS / "receipt.json"
    receipt.unlink(missing_ok=True)
    binary = Path(os.environ.get("FREESHOW_DESKTOP_BIN", ROOT / "target/debug/freeshow-desktop"))
    driver = os.environ.get("TAURI_DRIVER") or shutil.which("tauri-driver")
    webkit = os.environ.get("WEBKIT_WEBDRIVER") or shutil.which("WebKitWebDriver")
    assert binary.exists() and driver and webkit, "Build custom-protocol desktop and provide WebKitWebDriver/tauri-driver"
    port, native_port = free_port(), free_port()
    session = None
    steps = []
    with tempfile.TemporaryDirectory(prefix="freeshow-native-menu-") as temporary:
        temp = Path(temporary)
        app_data = temp / "app"
        app_data.mkdir()
        # This is an actual supported persisted user preference, ensuring Quit
        # demonstrates the real renderer cancellation path without exiting.
        (app_data / "settings.json").write_text(json.dumps({"initialized": True, "language": "de", "special": {"showClosePopup": True}}))
        env = dict(os.environ, FREESHOW_RUST_APP_DATA=str(app_data),
                   FREESHOW_RUST_DATA_DIR=str(temp / "data"),
                   XDG_DATA_HOME=str(temp / "xdg-data"),
                   XDG_CACHE_HOME=str(temp / "xdg-cache"),
                   XDG_CONFIG_HOME=str(temp / "xdg-config"),
                   GSETTINGS_BACKEND="memory", TAURI_WEBVIEW_AUTOMATION="true",
                   WEBKIT_DISABLE_COMPOSITING_MODE="1", LANG="C.UTF-8", LC_ALL="C.UTF-8")
        log = (ARTIFACTS / "driver.log").open("w")
        process = subprocess.Popen([driver, "--port", str(port), "--native-port", str(native_port), "--native-driver", webkit],
                                   env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)

        def http(method, route, payload=None):
            data = None if payload is None else json.dumps(payload).encode()
            request = urllib.request.Request(f"http://127.0.0.1:{port}" + route,
                                             data=data, method=method, headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(request, timeout=35) as response:
                value = json.load(response)["value"]
            assert not isinstance(value, dict) or "error" not in value, value
            return value

        def script(code, *args):
            return http("POST", f"/session/{session}/execute/sync", {"script": code, "args": list(args)})

        def click(menu, item):
            group = wait(lambda: find(native_menu(), menu), "Native GTK menu present: " + menu)
            assert group.get_action_iface().do_action(0), menu
            entry = wait(lambda: find(native_menu(), item), f"Native {item} exists")
            assert entry.get_state_set().contains(Atspi.StateType.ENABLED), item
            assert entry.get_action_iface().do_action(0), item

        try:
            wait(lambda: http("GET", "/status").get("ready"), "WebDriver ready")
            session = http("POST", "/session", {"capabilities": {"alwaysMatch": {"tauri:options": {"application": str(binary)}}}})["sessionId"]
            wait(lambda: script("return !!window.api && document.body.innerText.length>30"), "Real Svelte renderer ready")
            # Observe native delivery while every original renderer handler and
            # original outbound transport continues to execute.
            script("window.__menuEvents=[];window.__menuSends=[];const send=window.api.send;window.api.send=function(...args){window.__menuSends.push(structuredClone(args));return send.apply(this,args)};return window.__TAURI__.event.listen('freeshow://ipc',e=>window.__menuEvents.push(e.payload),{target:{kind:'WebviewWindow',label:'main'}}).then(()=>true)")
            wait(lambda: script("return !!document.querySelector('main .left')"), "Application layout hydrated")
            settings = script("return window.__TAURI__.core.invoke('ipc_dispatch',{request:{channel:'MAIN',data:{channel:'SETTINGS'}}})")
            assert settings[0]["message"]["data"]["special"]["showClosePopup"] is True
            initial = json.loads((ROOT / "public/lang/de.json").read_text())
            wait(lambda: find(native_menu(), initial["titlebar"]["file"]), "Initial renderer language applied to GTK")
            strings = {"titlebar": {"file": "Fichier 日本語", "edit": "Modifier", "view": "Vue", "help": "Aide"},
                       "actions": {"import": "Importer 日本語"},
                       "popup": {"shortcuts": "Raccourcis"}, "main": {"quit": "Quitter"}}
            result = script("return window.__TAURI__.core.invoke('ipc_dispatch',{request:{channel:'MAIN',data:{channel:'LANGUAGE',data:{lang:'test',strings:arguments[0]}}}}).then(events=>({events}))", strings)
            assert result == {"events": []}, result
            bar = wait(native_menu, "Localized menu ready")
            labels = [bar.get_child_at_index(i).get_name().replace("_", "") for i in range(bar.get_child_count())]
            assert labels == ["Fichier 日本語", "Modifier", "Vue", "Aide"], labels
            steps.append("LANGUAGE replaces actual GTK menu with Unicode labels and English fallbacks")
            click("Aide", "Raccourcis")
            wait(lambda: script("return window.__menuEvents.some(e=>e.message.channel==='MENU'&&e.message.data==='shortcuts')"), "Native shortcut item delivers MAIN.MENU shortcuts")
            steps.append("Native help item uses original renderer event")
            click("Fichier 日本語", "Importer 日本語")
            wait(lambda: script("return window.__menuEvents.some(e=>e.message.channel==='MENU'&&e.message.data==='import_more')"), "Native import item delivers MAIN.MENU import_more")
            steps.append("Native file item uses original renderer event")
            click("Fichier 日本語", "Quitter")
            wait(lambda: script("return window.__menuEvents.some(e=>e.message.channel==='CLOSE'&&e.message.data===false)"), "Quit requests MAIN.CLOSE false")
            (ARTIFACTS / "quit-observation.json").write_text(json.dumps(script("return {body:document.body.innerText,events:window.__menuEvents,sends:window.__menuSends}"), ensure_ascii=False, indent=2))
            wait(lambda: script("return [...document.querySelectorAll('.popup button')].some(e=>e.textContent.includes(arguments[0]))", initial["popup"]["cancel"]), "Real unsaved-changes popup keeps window open")
            assert not script("return window.__menuSends.some(([channel,data])=>channel==='MAIN'&&data?.channel==='CLOSE')"), "Quit bypassed renderer confirmation"
            steps.append("Quit reaches real renderer close confirmation and leaves process/window alive")
            events = script("return window.__menuEvents")
            receipt.write_text(json.dumps({"platform": "linux", "nativeToolkit": "GTK/AT-SPI", "steps": steps, "labels": labels, "events": events}, ensure_ascii=False, indent=2) + "\n")
        finally:
            if session:
                try:
                    http("DELETE", f"/session/{session}")
                except Exception:
                    pass
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=10)
            log.close()


if __name__ == "__main__":
    run()

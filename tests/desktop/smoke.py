"""Exercise the real Tauri binary through WebKitWebDriver on Linux.

Run under `xvfb-run -a dbus-run-session -- python3 tests/desktop/smoke.py`.
Requires tauri-driver and WebKitWebDriver on PATH (or environment overrides).
Every test run has fresh application, WebKit, data, and cache directories.
"""
import base64
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
ARTIFACTS = ROOT / "test-output-rust" / "desktop"
ARTIFACTS.mkdir(parents=True, exist_ok=True)
ELEMENT = "element-6066-11e4-a52e-4f735466cecf"


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def run():
    binary = Path(os.environ.get("FREESHOW_DESKTOP_BIN", ROOT / "target/debug/freeshow-desktop"))
    driver = os.environ.get("TAURI_DRIVER") or shutil.which("tauri-driver")
    native = os.environ.get("WEBKIT_WEBDRIVER") or shutil.which("WebKitWebDriver")
    if not binary.exists() or not driver or not native:
        raise RuntimeError("Build the desktop binary with custom-protocol and install tauri-driver + WebKitWebDriver")
    port, native_port = free_port(), free_port()
    session = None
    steps = []
    with tempfile.TemporaryDirectory(prefix="freeshow-desktop-test-") as temporary:
        temp = Path(temporary)
        env = dict(os.environ, FREESHOW_RUST_APP_DATA=str(temp / "app"), FREESHOW_RUST_DATA_DIR=str(temp / "data"),
                   XDG_DATA_HOME=str(temp / "xdg-data"), XDG_CACHE_HOME=str(temp / "xdg-cache"),
                   TAURI_WEBVIEW_AUTOMATION="true", WEBKIT_DISABLE_COMPOSITING_MODE="1",
                   LANG=os.environ.get("FREESHOW_TEST_LOCALE", "C.UTF-8"), LC_ALL=os.environ.get("FREESHOW_TEST_LOCALE", "C.UTF-8"))
        log = (ARTIFACTS / "driver.log").open("w")
        process = subprocess.Popen([driver, "--port", str(port), "--native-port", str(native_port), "--native-driver", native],
                                   env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        base = f"http://127.0.0.1:{port}"

        def http(method, route, payload=None):
            data = None if payload is None else json.dumps(payload).encode()
            request = urllib.request.Request(base + route, data=data, method=method, headers={"Content-Type": "application/json"})
            try:
                with urllib.request.urlopen(request, timeout=40) as response:
                    result = json.load(response)["value"]
            except urllib.error.HTTPError as error:
                raise RuntimeError(error.read().decode()) from error
            if isinstance(result, dict) and "error" in result:
                raise RuntimeError(result)
            return result

        def command(method, route, payload=None):
            return http(method, f"/session/{session}" + route, payload)

        def script(source, *args):
            return command("POST", "/execute/sync", {"script": source, "args": list(args)})

        def wait(predicate, label, seconds=25):
            deadline = time.monotonic() + seconds
            last_error = None
            while time.monotonic() < deadline:
                try:
                    value = predicate()
                    if value:
                        steps.append(label)
                        print("PASS", label, flush=True)
                        return value
                except Exception as error:
                    last_error = error
                time.sleep(0.15)
            raise AssertionError(f"Timed out: {label}; last error: {last_error}")

        def element(selector):
            return command("POST", "/element", {"using": "css selector", "value": selector})[ELEMENT]

        def click(selector):
            command("POST", f"/element/{element(selector)}/click", {})

        def click_text(text):
            # Locate by text in the real document, then perform a WebDriver click.
            literal = json.dumps(text)
            found = command("POST", "/element", {"using": "xpath", "value": f"//*[normalize-space(.)={literal} and not(.//*[normalize-space(.)={literal}])]"})[ELEMENT]
            command("POST", f"/element/{found}/click", {})

        def fill(selector, text):
            found = element(selector)
            command("POST", f"/element/{found}/clear", {})
            command("POST", f"/element/{found}/value", {"text": text})

        try:
            wait(lambda: http("GET", "/status").get("ready"), "WebDriver ready")
            session = http("POST", "/session", {"capabilities": {"alwaysMatch": {"tauri:options": {"application": str(binary)}}}})["sessionId"]
            wait(lambda: script("return !!window.api && document.body.innerText.includes('Language')"), "Rust IPC bootstrap and onboarding")
            script("window.__testRuntimeErrors=[];window.__testErrors=[];window.__testClicks=[];window.addEventListener('freeshow:runtime-error',e=>window.__testRuntimeErrors.push(e.detail));window.addEventListener('error',e=>window.__testErrors.push(String(e.message)));document.addEventListener('click',e=>window.__testClicks.push({target:e.target.outerHTML?.slice(0,300),edit:!!e.target.closest('.edit')}),true)")
            click(".popup button.start")
            wait(lambda: script("return !!document.querySelector('#guideButtons')"), "Onboarding guide")
            click_text("Skip")
            wait(lambda: script("return !document.querySelector('#guideButtons')"), "Onboarding guide dismissed")
            click_text("New project")
            click_text("New show")
            wait(lambda: script("return !!document.querySelector('#name')"), "Create-show editor")
            fill("#name", "Rust parity test")
            click_text("Quick lyrics")
            wait(lambda: script("return !!document.querySelector('.popup textarea')"), "Quick lyrics input")
            typed_lyrics = "Keyboard input"
            fill('.popup textarea', typed_lyrics)
            actual_input = script("return document.querySelector('.popup textarea').value")
            assert actual_input == typed_lyrics, repr(actual_input)
            # WebKitWebDriver's text injection drops line breaks and non-BMP
            # characters. Simulate the renderer's paste input event for multiline
            # lyrics. Native clipboard integration remains a separate parity gap.
            pasted_lyrics = "[Verse]\nAmazing grace\nHow sweet the sound\n\n[Chorus]\nUnicode 日本語 🙌"
            script("const area=document.querySelector('.popup textarea');area.setRangeText(arguments[0],0,area.value.length,'end');area.dispatchEvent(new InputEvent('input',{bubbles:true,inputType:'insertFromPaste',data:arguments[0]}))", pasted_lyrics)
            assert script("return document.querySelector('.popup textarea').value") == pasted_lyrics
            steps.append("Multiline paste input and Unicode")
            click('[data-testid="create.show.popup.new.show"]')
            wait(lambda: script("return document.body.innerText.includes('Rust parity test') && !document.querySelector('#name')"), "Show created")
            # Ctrl+S travels through the same shortcut handler used by the desktop.
            command("POST", "/actions", {"actions": [{"type": "key", "id": "keyboard", "actions": [
                {"type": "keyDown", "value": "\ue009"}, {"type": "keyDown", "value": "s"},
                {"type": "keyUp", "value": "s"}, {"type": "keyUp", "value": "\ue009"}]}]})
            show_path = temp / "data" / "Shows" / "Rust parity test.show"
            wait(show_path.exists, "Show saved through Rust")
            saved = json.loads(show_path.read_text())
            (ARTIFACTS / "saved.show").write_text(json.dumps(saved, ensure_ascii=False, indent=2))
            assert saved[1]["name"] == "Rust parity test"
            lyrics = "".join(text.get("value", "") for slide in saved[1].get("slides", {}).values()
                             for item in slide.get("items", []) for line in item.get("lines", []) for text in line.get("text", []))
            assert "Amazing grace" in lyrics, repr(lyrics)
            assert "Unicode 日本語 🙌" in lyrics, repr(lyrics)
            steps.append("Saved lyrics and Unicode verified")
            screenshot = command("GET", "/screenshot")
            (ARTIFACTS / "editor.png").write_bytes(base64.b64decode(screenshot))
            (ARTIFACTS / "saved.show").write_text(json.dumps(saved, ensure_ascii=False, indent=2))
            errors = script("return window.__testRuntimeErrors")
            # Unsupported features remain visible in the receipt; they are never
            # interpreted as successful parity checks.
            (ARTIFACTS / "receipt.json").write_text(json.dumps({"steps": steps, "runtimeErrors": errors, "platform": "Linux WebKitGTK"}, indent=2))
            print(f"Desktop editor smoke passed; evidence: {ARTIFACTS}", flush=True)
        except Exception:
            if session:
                try:
                    (ARTIFACTS / "failed.png").write_bytes(base64.b64decode(command("GET", "/screenshot")))
                    (ARTIFACTS / "failed-dom.txt").write_text(script("return document.body.innerText"))
                    (ARTIFACTS / "failed-errors.json").write_text(json.dumps(script("return {runtime:window.__testRuntimeErrors,errors:window.__testErrors,clicks:window.__testClicks}"), indent=2))
                except Exception:
                    pass
            raise
        finally:
            if session:
                try:
                    http("DELETE", f"/session/{session}")
                except Exception:
                    pass
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
            log.close()


if __name__ == "__main__":
    run()

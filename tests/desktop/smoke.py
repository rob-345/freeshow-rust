"""Exercise the real Tauri binary through WebKitWebDriver on Linux.

Run under `xvfb-run -a dbus-run-session -- python3 tests/desktop/smoke.py`.
Requires tauri-driver and WebKitWebDriver on PATH (or environment overrides).
Every test run has fresh application, WebKit, data, and cache directories.
"""
import base64
import http.client
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
import zipfile

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
    xdotool = os.environ.get("XDOTOOL") or shutil.which("xdotool")
    if not binary.exists() or not driver or not native or not xdotool:
        raise RuntimeError("Build the desktop binary with custom-protocol and install tauri-driver, WebKitWebDriver and xdotool")
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
            # The WebKit proxy occasionally resets an idle connection while
            # finding an element. Re-observe that same live session for reads;
            # never retry clicks, key input, or mutating scripts automatically.
            read_only = method == "GET" or route.endswith(("/element", "/elements"))
            for attempt in range(3 if read_only else 1):
                try:
                    with urllib.request.urlopen(request, timeout=40) as response:
                        result = json.load(response)["value"]
                    break
                except (http.client.RemoteDisconnected, ConnectionResetError):
                    if not read_only or attempt == 2 or process.poll() is not None:
                        raise
                    print("Re-observing WebDriver after a reset during", route, flush=True)
                    time.sleep(0.1)
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

        def save_shortcut():
            command("POST", "/actions", {"actions": [{"type": "key", "id": "keyboard", "actions": [
                {"type": "keyDown", "value": "\ue009"}, {"type": "keyDown", "value": "s"},
                {"type": "keyUp", "value": "s"}, {"type": "keyUp", "value": "\ue009"}]}]})

        def native_keys(*arguments, check=True):
            environment = dict(os.environ)
            if os.environ.get("XDOTOOL_LIBRARY_PATH"):
                environment["LD_LIBRARY_PATH"] = os.environ["XDOTOOL_LIBRARY_PATH"]
            return subprocess.run([xdotool, *map(str, arguments)], env=environment, check=check, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=10)

        def native_dialog(title):
            found = native_keys("search", "--onlyvisible", "--name", "^" + title + "$", check=False)
            return found.stdout.strip().splitlines()[-1] if found.returncode == 0 and found.stdout.strip() else None

        def choose_native_file(title, file):
            window_id = wait(lambda: native_dialog(title), title)
            native_keys("windowfocus", "--sync", window_id)
            native_keys("key", "--clearmodifiers", "ctrl+l")
            native_keys("type", "--clearmodifiers", "--delay", "3", str(file))
            native_keys("key", "--clearmodifiers", "Return")
            # GTK can use Enter to resolve the location entry first. Activate
            # its Open button only if that same dialog remains visible.
            time.sleep(0.3)
            if native_dialog(title):
                native_keys("windowfocus", "--sync", window_id)
                native_keys("key", "--clearmodifiers", "alt+o")

        def attach_errors():
            script("window.__testRuntimeErrors=[];window.__testErrors=[];window.__testClicks=[];window.addEventListener('freeshow:runtime-error',e=>window.__testRuntimeErrors.push(e.detail));window.addEventListener('error',e=>window.__testErrors.push(String(e.message)));window.addEventListener('unhandledrejection',e=>window.__testErrors.push(String(e.reason)));document.addEventListener('click',e=>window.__testClicks.push({target:e.target.outerHTML?.slice(0,300),edit:!!e.target.closest('.edit')}),true)")

        try:
            wait(lambda: http("GET", "/status").get("ready"), "WebDriver ready")
            session = http("POST", "/session", {"capabilities": {"alwaysMatch": {"tauri:options": {"application": str(binary)}}}})["sessionId"]
            wait(lambda: script("return !!window.api && document.body.innerText.includes('Language')"), "Rust IPC bootstrap and onboarding")
            attach_errors()
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
            save_shortcut()
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
            assert script("return window.__testErrors") == []
            # Restart the actual application with the same profile and data.
            http("DELETE", f"/session/{session}")
            session = None
            session = http("POST", "/session", {"capabilities": {"alwaysMatch": {"tauri:options": {"application": str(binary)}}}})["sessionId"]
            wait(lambda: script("return !!window.api && document.body.innerText.includes('Rust parity test')"), "Project and song restored after restart")
            attach_errors()
            click_text("Rust parity test")
            wait(lambda: script("return document.body.innerText.includes('Amazing grace') && document.body.innerText.includes('日本語')"), "Saved show loaded and rendered")

            # Exercise real native dialogs and the renderer's IPC bridge.
            script("window.__nativeEvents=[];window.api.receive('MAIN',m=>window.__nativeEvents.push(m),'native-desktop-test');window.api.send('MAIN',{channel:'OPEN_FILE',data:{id:'native-file',channel:'NATIVE_TEST',title:'Read FreeShow Test File',filter:{name:'FreeShow',extensions:['show']},multiple:false,read:true}})")
            choose_native_file("Read FreeShow Test File", show_path)
            selected = wait(lambda: script("return window.__nativeEvents.find(m=>m.channel==='OPEN_FILE2')?.data"), "Native file selection and read callback")
            assert selected["id"] == "native-file"
            assert selected["files"] == [str(show_path)]
            assert json.loads(selected["content"][str(show_path)]) == saved

            script("window.__nativeEvents=[];window.api.send('MAIN',{channel:'SAVE',data:{customTriggers:{backup:true,isAutoBackup:true},closeWhenFinished:false}})")
            backup = wait(lambda: script("return window.__nativeEvents.find(m=>m.channel==='BACKUP' && m.data.finished)?.data"), "Native Rust backup completed")
            backup_path = Path(backup["path"])
            with zipfile.ZipFile(backup_path) as archive:
                assert json.loads(archive.read("SHOWS/Rust parity test.show")) == saved

            # Simulate a changed show on disk, then recover it through the real
            # restore picker. The restored data must match the backup exactly.
            changed = json.loads(json.dumps(saved))
            changed[1]["slides"] = {}
            show_path.write_text(json.dumps(changed))
            script("window.__nativeEvents=[];window.api.send('MAIN',{channel:'RESTORE'})")
            choose_native_file("Restore FreeShow Backup", backup_path)
            wait(lambda: script("return window.__nativeEvents.some(m=>m.channel==='RESTORE2' && m.data.finished===true)"), "Native restore picker and transaction completed")
            assert json.loads(show_path.read_text()) == saved

            script("window.__cancelResult=null;window.__TAURI__.core.invoke('ipc_dispatch',{request:{channel:'MAIN',data:{channel:'OPEN_FOLDER',data:{channel:'NATIVE_TEST',title:'Cancel FreeShow Folder Selection'}}}}).then(events=>window.__cancelResult=events)")
            folder = wait(lambda: native_dialog("Cancel FreeShow Folder Selection"), "Native folder picker")
            native_keys("windowfocus", "--sync", folder)
            native_keys("key", "--clearmodifiers", "Escape")
            wait(lambda: script("return Array.isArray(window.__cancelResult)"), "Native folder cancellation completed")
            assert script("return window.__cancelResult") == []
            settings_path = temp / "app" / "settings.json"
            previous_settings = settings_path.read_bytes()
            mode = settings_path.stat().st_mode & 0o777
            try:
                settings_path.chmod(0o400)
                save_shortcut()
                wait(lambda: script("return document.body.innerText.includes('Could not save all changes') && !!document.querySelector('.status .error')"), "Save failure is visible and leaves edits unsaved")
                assert settings_path.read_bytes() == previous_settings
            finally:
                settings_path.chmod(mode)
            click(".popup button.outlined")
            save_shortcut()
            wait(lambda: script("return !!document.querySelector('.status .saved')"), "Save retry succeeds after permissions are restored")
            assert script("return window.__testErrors") == []
            errors = script("return window.__testRuntimeErrors")
            # Unsupported features remain visible in the receipt; they are never
            # interpreted as successful parity checks.
            (ARTIFACTS / "receipt.json").write_text(json.dumps({"steps": steps, "runtimeErrors": errors, "platform": "Linux WebKitGTK"}, indent=2))
            print(f"Desktop editor smoke passed; evidence: {ARTIFACTS}", flush=True)
        except Exception:
            if shutil.which("ffmpeg"):
                subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-f", "x11grab", "-i", os.environ["DISPLAY"], "-frames:v", "1", str(ARTIFACTS / "failed-desktop.png")], timeout=10)
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

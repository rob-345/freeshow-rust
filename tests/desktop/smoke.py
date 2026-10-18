"""Exercise the real Tauri binary through WebKitWebDriver on Linux.

Run under `xvfb-run -a dbus-run-session -- python3 tests/desktop/smoke.py`.
Requires tauri-driver and WebKitWebDriver on PATH (or environment overrides).
Requires Python GI with the AT-SPI 2 typelib for native dialog controls.
Every test run has fresh application, WebKit, data, and cache directories.
"""
import base64
from http.client import RemoteDisconnected
import json
import os
from pathlib import Path
import random
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import zipfile
import zlib
import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi

ROOT = Path(__file__).resolve().parents[2]
ARTIFACTS = ROOT / "test-output-rust" / "desktop"
ARTIFACTS.mkdir(parents=True, exist_ok=True)
ELEMENT = "element-6066-11e4-a52e-4f735466cecf"


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def run():
    for name in ["receipt.json", "editor.png", "saved.show", "library-switch.png", "stale-save-recovery.json", "responsiveness.json", "failed.png", "failed-desktop.png", "failed-dom.txt", "failed-errors.json"]:
        (ARTIFACTS / name).unlink(missing_ok=True)
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
        data_dir = temp / "selected-data"
        data_dir.mkdir()
        env = dict(os.environ, FREESHOW_RUST_APP_DATA=str(temp / "app"),
                   XDG_DATA_HOME=str(temp / "xdg-data"), XDG_CACHE_HOME=str(temp / "xdg-cache"),
                   XDG_CONFIG_HOME=str(temp / "xdg-config"), GSETTINGS_BACKEND="memory",
                   TAURI_WEBVIEW_AUTOMATION="true", WEBKIT_DISABLE_COMPOSITING_MODE="1",
                   LANG=os.environ.get("FREESHOW_TEST_LOCALE", "C.UTF-8"), LC_ALL=os.environ.get("FREESHOW_TEST_LOCALE", "C.UTF-8"))
        env.pop("FREESHOW_RUST_DATA_DIR", None)
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
                except (RemoteDisconnected, ConnectionResetError):
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

        def wait(predicate, label, seconds=25, record=True):
            deadline = time.monotonic() + seconds
            last_error = None
            while time.monotonic() < deadline:
                try:
                    value = predicate()
                    if value:
                        if record:
                            steps.append(label)
                            print("PASS", label, flush=True)
                        return value
                except Exception as error:
                    last_error = error
                time.sleep(0.15)
            raise AssertionError(f"Timed out: {label}; last error: {last_error}")

        def element(selector):
            return command("POST", "/element", {"using": "css selector", "value": selector})[ELEMENT]

        def click_element(found, label):
            reference = {ELEMENT: found}
            script("arguments[0].scrollIntoView({block:'nearest',inline:'nearest'})", reference)
            # Observe the hit target before the one actual WebDriver click. A
            # fading Svelte backdrop can cover a button after its popup unmounts.
            wait(lambda: script("const e=arguments[0],r=e.getBoundingClientRect();return r.width>0 && r.height>0 && e.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2))", reference), "Clickable " + label, record=False)
            command("POST", f"/element/{found}/click", {})

        def click(selector):
            click_element(element(selector), selector)

        def click_text(text):
            # Locate by text in the real document, then perform a WebDriver click.
            literal = json.dumps(text)
            found = command("POST", "/element", {"using": "xpath", "value": f"//*[normalize-space(.)={literal} and not(.//*[normalize-space(.)={literal}])]"})[ELEMENT]
            click_element(found, text)

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

        def native_open_button(title):
            desktop = Atspi.get_desktop(0)
            for app_index in range(desktop.get_child_count()):
                application = desktop.get_child_at_index(app_index)
                for index in range(application.get_child_count()):
                    frame = application.get_child_at_index(index)
                    if frame.get_name() != title:
                        continue
                    queue = [frame]
                    while queue:
                        node = queue.pop()
                        if node.get_role() == Atspi.Role.PUSH_BUTTON and node.get_name().lstrip("_") == "Open":
                            state = node.get_state_set()
                            if state.contains(Atspi.StateType.ENABLED) and state.contains(Atspi.StateType.SHOWING):
                                return node
                        queue.extend(node.get_child_at_index(child) for child in range(node.get_child_count()))
            return None

        def choose_native_file(title, file):
            window_id = wait(lambda: native_dialog(title), title)
            # A mapped GTK toplevel can precede realization of its entry widgets
            # (observed on CI). Give it a rendering interval before sending input;
            # XSync/windowfocus alone does not wait for the GTK event loop.
            time.sleep(1)
            native_keys("windowfocus", "--sync", window_id)
            native_keys("key", "--clearmodifiers", "ctrl+l")
            time.sleep(0.2)
            native_keys("type", "--clearmodifiers", "--delay", "3", str(file))
            native_keys("key", "--clearmodifiers", "Return")
            # GTK can use Enter to resolve a path completion first. Activate the
            # actual enabled Open button; completion popups can eat mnemonics.
            time.sleep(0.3)
            if native_dialog(title):
                button = wait(lambda: native_open_button(title), "Native Open button ready")
                assert button.get_action_iface().do_action(0), "Could not activate native Open button"

        def attach_errors():
            script("window.__testRuntimeErrors=[];window.__testErrors=[];window.__testClicks=[];window.addEventListener('freeshow:runtime-error',e=>window.__testRuntimeErrors.push(e.detail));window.addEventListener('error',e=>window.__testErrors.push(String(e.message)));window.addEventListener('unhandledrejection',e=>window.__testErrors.push(String(e.reason)));document.addEventListener('click',e=>window.__testClicks.push({target:e.target.outerHTML?.slice(0,300),edit:!!e.target.closest('.edit')}),true)")

        try:
            wait(lambda: http("GET", "/status").get("ready"), "WebDriver ready")
            session = http("POST", "/session", {"capabilities": {"alwaysMatch": {"tauri:options": {"application": str(binary)}}}})["sessionId"]
            wait(lambda: script("return !!window.api && document.body.innerText.includes('Language')"), "Rust IPC bootstrap and onboarding")
            attach_errors()
            click('[data-testid="folder-picker-DATA_SHOWS"] .button-trigger')
            choose_native_file("Data location", data_dir)
            config_path = temp / "app" / "config.json"
            wait(lambda: json.loads(config_path.read_text()).get("dataPath") == str(data_dir), "Selected data library committed")
            wait(lambda: script("return document.querySelector('[data-testid=folder-picker-DATA_SHOWS]').dataset.title === arguments[0]", str(data_dir)), "Selected library shown in onboarding")
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
            show_path = data_dir / "Shows" / "Rust parity test.show"
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
            assert json.loads(config_path.read_text())["dataPath"] == str(data_dir)
            script("window.__selectedRoot=null;window.api.receive('MAIN',m=>{if(m.channel==='DATA_PATH')window.__selectedRoot=m.data},'root-check');window.api.send('MAIN',{channel:'DATA_PATH'})")
            wait(lambda: script("return window.__selectedRoot === arguments[0]", str(data_dir)), "Selected data library reopened after restart")
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

            script("window.__cancelResult=null;window.__TAURI__.core.invoke('ipc_dispatch',{request:{channel:'MAIN',context:window.api.getRuntimeContext(),data:{channel:'OPEN_FOLDER',data:{channel:'NATIVE_TEST',title:'Cancel FreeShow Folder Selection'}}}}).then(events=>window.__cancelResult=events)")
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

            # Switch an already-loaded library with unsaved work. The target has
            # a different song using the SAME ID, exercising cache invalidation.
            old_context = script("return window.api.getRuntimeContext()")
            other = temp / "other-library"
            (other / "Shows").mkdir(parents=True)
            (other / "Config").mkdir()
            other_show = json.loads(json.dumps(saved))
            other_show[1]["name"] = "Second library"
            for slide in other_show[1].get("slides", {}).values():
                for item in slide.get("items", []):
                    for line in item.get("lines", []):
                        for text in line.get("text", []):
                            text["value"] = "Second library lyric"
            (other / "Shows" / "Second library.show").write_text(json.dumps(other_show))
            target_projects = '{"projects":{},"folders":{},"projectTemplates":{}}'
            (other / "Config" / "projects.json").write_text(target_projects)
            click_text("New show")
            wait(lambda: script("return !!document.querySelector('#name')"), "Unsaved show before library switch")
            fill("#name", "Pending before switch")
            click_text("Quick lyrics")
            wait(lambda: script("return !!document.querySelector('.popup textarea')"), "Unsaved lyrics input")
            fill('.popup textarea', "Keep this unsaved song")
            click('[data-testid="create.show.popup.new.show"]')
            wait(lambda: script("return !document.querySelector('#name')"), "Unsaved show created")
            assert not (data_dir / "Shows" / "Pending before switch.show").exists()
            settings_button = '.top button[data-title^="Settings"]'
            wait(lambda: script("const e=document.querySelector(arguments[0]);if(!e)return false;const r=e.getBoundingClientRect();return r.width>0 && e.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2))", settings_button), "Settings navigation ready after closing the editor")
            click(settings_button)
            click_text("Files")
            click('[data-testid="folder-picker-DATA_LOCATION_SETTINGS"] .button-trigger')
            choose_native_file("Data location", other)
            wait(lambda: json.loads(config_path.read_text()).get("dataPath") == str(other), "Existing library selected after confirmed save")
            pending_show = json.loads((data_dir / "Shows" / "Pending before switch.show").read_text())
            pending_lyrics = json.dumps(pending_show)
            assert "Keep this unsaved song" in pending_lyrics
            assert show_path.exists()
            assert not (other / "Shows" / "Pending before switch.show").exists()
            assert (other / "Config" / "projects.json").read_text() == target_projects
            steps.append("Pending edits saved to old library and destination settings preserved")
            click('button[data-title^="Presenting"]')
            wait(lambda: script("return document.body.innerText.includes('Second library')"), "Selected library show list refreshed")
            click_text("Second library")
            wait(lambda: script("return document.body.innerText.includes('Second library lyric') && !document.body.innerText.includes('Amazing grace')"), "Same-ID show cache cleared across libraries")
            (ARTIFACTS / "library-switch.png").write_bytes(base64.b64decode(command("GET", "/screenshot")))

            # A delayed save from the previous library must become a recovery
            # file rather than writing settings or shows into the selected one.
            previous_settings = settings_path.read_bytes()
            previous_show = (other / "Shows" / "Second library.show").read_bytes()
            script("window.api.send('MAIN',{channel:'SAVE',data:{runtimeContext:arguments[0],SETTINGS:{stale:true},PROJECTS:{stale:true},showsCache:{old:{name:'Rejected old-library song',slides:{}}}}})", old_context)
            stale = wait(lambda: script("return window.__testRuntimeErrors.find(e=>e.error?.code==='stale_library')?.error"), "Previous-library save rejected")
            recovery_path = Path(stale["recoveryPath"])
            assert recovery_path.parent == temp / "app" / "Recovery"
            recovery = json.loads(recovery_path.read_text())
            assert recovery["request"]["context"] == old_context
            assert recovery["request"]["data"]["data"]["showsCache"]["old"]["name"] == "Rejected old-library song"
            assert settings_path.read_bytes() == previous_settings
            assert (other / "Shows" / "Second library.show").read_bytes() == previous_show
            assert (other / "Config" / "projects.json").read_text() == target_projects
            assert not (other / "Shows" / "Rejected old-library song.show").exists()
            wait(lambda: script("return document.body.innerText.includes('previous library or session') && document.body.innerText.includes('Recovery file:')"), "Old save retained with visible recovery location")
            (ARTIFACTS / "stale-save-recovery.json").write_text(json.dumps(recovery, indent=2))

            # Real compression, not a test-only sleep: four 16 MiB JSON files
            # make a reproducible workload without sending large data over IPC.
            # They are backup fixtures, not a large-library performance claim.
            workload = {}
            chunk = random.Random(20261018).randbytes(256 * 1024).hex().encode()
            try:
                for index in range(4):
                    path = other / "Shows" / f"__backup_workload_{index}.show"
                    crc = 0
                    with path.open("wb") as output:
                        for piece in [b'{"benchmarkPadding":"', *([chunk] * 32), b'"}']:
                            output.write(piece)
                            crc = zlib.crc32(piece, crc)
                    workload[path] = {"size": path.stat().st_size, "crc": crc}
                script("""
                    const invoke=window.__TAURI__.core.invoke;
                    const context=window.api.getRuntimeContext();
                    const observed=window.__responsiveness={samples:[],done:false,probesComplete:false,context};
                    const started=performance.now();
                    invoke('ipc_dispatch',{request:{channel:'MAIN',listenerId:'backup-responsiveness',context,data:{channel:'SAVE',data:{customTriggers:{backup:true,isAutoBackup:true},closeWhenFinished:false}}}})
                        .then(events=>{observed.backup=events.find(e=>e.message.channel==='BACKUP' && e.message.data.finished)?.message.data;observed.backupMs=performance.now()-started;observed.done=true})
                        .catch(error=>{observed.error=error;observed.done=true});
                    (async()=>{
                        try {
                            while(!observed.done && performance.now()-started<90000 && observed.samples.length<1024) {
                                const before=await invoke('library_status');
                                if(before.activeJob?.listenerId==='backup-responsiveness') {
                                    const probeStart=performance.now();
                                    const [current,maximized,os]=await Promise.all([
                                        invoke('runtime_context'),
                                        invoke('ipc_dispatch',{request:{channel:'MAIN',data:{channel:'MAXIMIZED'}}}),
                                        invoke('ipc_dispatch',{request:{channel:'MAIN',data:{channel:'GET_OS'}}})
                                    ]);
                                    const elapsedMs=performance.now()-probeStart;
                                    const after=await invoke('library_status');
                                    if(after.activeJob?.listenerId==='backup-responsiveness' && !observed.done) {
                                        observed.samples.push({elapsedMs,sinceBackupStartMs:performance.now()-started,current,maximized,os,queueCapacity:after.queueCapacity});
                                    }
                                }
                                await new Promise(resolve=>setTimeout(resolve,100));
                            }
                        } catch(error) { observed.probeError=String(error) }
                        observed.probesComplete=true;
                    })();
                """)
                observed = wait(lambda: script("return window.__responsiveness.done && window.__responsiveness.probesComplete ? window.__responsiveness : null"), "Backup and independent control probes completed", seconds=90)
                (ARTIFACTS / "responsiveness.json").write_text(json.dumps(observed, indent=2))
                assert "error" not in observed and "probeError" not in observed, observed
                assert len(observed["samples"]) >= 8, observed
                for sample in observed["samples"]:
                    assert sample["elapsedMs"] < 1500, sample
                    assert sample["current"] == observed["context"]
                    assert sample["maximized"][0]["message"]["channel"] == "MAXIMIZED"
                    assert isinstance(sample["maximized"][0]["message"]["data"], bool)
                    assert sample["os"][0]["message"]["data"]["platform"] == "linux"
                    assert sample["queueCapacity"] == 8
                with zipfile.ZipFile(observed["backup"]["path"]) as archive:
                    for path, expected in workload.items():
                        info = archive.getinfo("SHOWS/" + path.name)
                        assert info.file_size == expected["size"]
                        assert info.CRC == expected["crc"]
                    assert archive.testzip() is None
                steps.append("Context and window controls respond during real 64 MiB backup; ZIP verified")
            finally:
                for path in workload:
                    path.unlink(missing_ok=True)
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
                    (ARTIFACTS / "failed-errors.json").write_text(json.dumps(script("return {runtime:window.__testRuntimeErrors,errors:window.__testErrors,clicks:window.__testClicks,navigation:[...document.querySelectorAll('.top button')].map(e=>{const r=e.getBoundingClientRect();return {title:e.dataset.title,rect:r.toJSON(),cover:document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)?.outerHTML.slice(0,400)}})}"), indent=2))
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

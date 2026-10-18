"""Exercise the real Tauri binary through WebKitWebDriver on Linux.

Run under `xvfb-run -a dbus-run-session -- python3 tests/desktop/smoke.py`.
Requires tauri-driver and WebKitWebDriver on PATH (or environment overrides).
Requires Python GI with the AT-SPI 2 typelib for native dialog controls.
Every test run has fresh application, WebKit, data, and cache directories.
"""
import base64
from http.client import RemoteDisconnected
import itertools
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
from menu import native_menu, find as find_native_menu
from script_receipts import wrap_script

ROOT = Path(__file__).resolve().parents[2]
ARTIFACTS = ROOT / "test-output-rust" / "desktop"
ARTIFACTS.mkdir(parents=True, exist_ok=True)
ELEMENT = "element-6066-11e4-a52e-4f735466cecf"


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def run():
    for name in ["receipt.json", "editor.png", "saved.show", "output-verse.png", "output-chorus.png", "output-targeted-primary.png", "output-targeted-secondary.png", "library-switch.png", "stale-save-recovery.json", "responsiveness.json", "uncached-show-read.json", "save-revision-race.json", "close-drain.json", "media-probe.json", "osc-loopback.json", "failed.png", "failed-desktop.png", "failed-dom.txt", "failed-output-dom.txt", "failed-output-state.json", "failed-errors.json"]:
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
        assert shutil.which("ffmpeg") and shutil.which("ffprobe"), "Native media integration requires ffmpeg and ffprobe"
        media_fixture = temp / "原音 native probe.m4a"
        subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=8000:duration=1", "-c:a", "aac", "-b:a", "32k", "-threads", "1", "-y", str(media_fixture)], check=True, timeout=15)
        env = dict(os.environ, FREESHOW_RUST_APP_DATA=str(temp / "app"),
                   XDG_DATA_HOME=str(temp / "xdg-data"), XDG_CACHE_HOME=str(temp / "xdg-cache"),
                   XDG_CONFIG_HOME=str(temp / "xdg-config"), GSETTINGS_BACKEND="memory",
                   TAURI_WEBVIEW_AUTOMATION="true", WEBKIT_DISABLE_COMPOSITING_MODE="1",
                   LANG=os.environ.get("FREESHOW_TEST_LOCALE", "C.UTF-8"), LC_ALL=os.environ.get("FREESHOW_TEST_LOCALE", "C.UTF-8"))
        env.pop("FREESHOW_RUST_DATA_DIR", None)
        if os.environ.get("FREESHOW_TEST_FONT_DIR"):
            # Fontconfig includes XDG_DATA_HOME/fonts. Keep locally extracted
            # test fonts inside this isolated profile, outside the user profile.
            shutil.copytree(os.environ["FREESHOW_TEST_FONT_DIR"], temp / "xdg-data/fonts", dirs_exist_ok=True)
        log = (ARTIFACTS / "driver.log").open("w")
        process = subprocess.Popen([driver, "--port", str(port), "--native-port", str(native_port), "--native-driver", native],
                                   env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        base = f"http://127.0.0.1:{port}"
        native_base = f"http://127.0.0.1:{native_port}"

        def http(method, route, payload=None):
            data = None if payload is None else json.dumps(payload).encode()
            # tauri-driver 2.1.0 only transforms POST /session capabilities.
            # Its remaining routes are an unchanged HTTP proxy. Use the same
            # native session directly to avoid its pooled upstream disconnects.
            endpoint = base if method == "POST" and route == "/session" else native_base
            request = urllib.request.Request(endpoint + route, data=data, method=method, headers={"Content-Type": "application/json"})
            # The WebKit proxy occasionally resets an idle connection while
            # finding an element or selecting an existing WebView. These reads
            # and exact window selections can be repeated in the same session.
            repeatable = method == "GET" or route.endswith(("/element", "/elements")) or (method == "POST" and route.endswith("/window") and payload and "handle" in payload)
            for attempt in range(3 if repeatable else 1):
                try:
                    with urllib.request.urlopen(request, timeout=40) as response:
                        result = json.load(response)["value"]
                    break
                except (RemoteDisconnected, ConnectionResetError):
                    if not repeatable or attempt == 2 or process.poll() is not None:
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

        script_sequence = itertools.count()

        def script(source, *args):
            # The proxy can drop a response after executing JavaScript. Keep a
            # receipt in this same WebView so reconnecting never repeats a send,
            # listener installation or other side effect. Errors are retained
            # too; a new WebView/session is not substituted after a failure.
            key = str(next(script_sequence))
            wrapped = wrap_script(key, source)
            for attempt in range(3):
                try:
                    return command("POST", "/execute/sync", {"script": wrapped, "args": list(args)})
                except (RemoteDisconnected, ConnectionResetError):
                    if attempt == 2 or process.poll() is not None:
                        raise
                    print("Recovering script response through its WebView receipt:", key, flush=True)
                    time.sleep(0.1)

        def invoke(name, arguments=None):
            result = script("return window.__TAURI__.core.invoke(arguments[0],arguments[1]).then(value=>({ok:true,value})).catch(error=>({ok:false,error}))", name, arguments or {})
            assert result["ok"], result
            return result.get("value")

        def output_handle(output_id):
            registry = invoke("output_status")
            label = next(item["label"] for item in registry["outputs"] if item["id"] == output_id)
            try:
                for handle in command("GET", "/window/handles"):
                    if handle == main_window:
                        continue
                    command("POST", "/window", {"handle": handle})
                    if script("return window.__TAURI_INTERNALS__.metadata.currentWindow.label") == label:
                        return handle
            finally:
                command("POST", "/window", {"handle": main_window})
            raise AssertionError(f"Output {output_id} ({label}) has no live WebView")

        def visible_lyric(text):
            return script("""
                return [...document.querySelectorAll('.textContainer')].some(node=>{
                    if(!node.textContent.includes(arguments[0]))return false;
                    const rect=node.getBoundingClientRect();
                    if(rect.width<=0 || rect.height<=0)return false;
                    let opacity=1;
                    for(let element=node;element;element=element.parentElement){
                        const style=getComputedStyle(element);
                        if(style.display==='none' || style.visibility==='hidden')return false;
                        opacity*=Number(style.opacity);
                    }
                    return opacity>=0.98;
                });
            """, text)

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
            wait(lambda: script("arguments[0].scrollIntoView({block:'nearest',inline:'nearest'});return true", reference), "Scrolled " + label, record=False)
            # Observe the hit target before the one actual WebDriver click. A
            # fading Svelte backdrop can cover a button after its popup unmounts.
            wait(lambda: script("const e=arguments[0],r=e.getBoundingClientRect();return r.width>0 && r.height>0 && e.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2))", reference), "Clickable " + label, record=False)
            try:
                command("POST", f"/element/{found}/click", {})
            except (RemoteDisconnected, ConnectionResetError):
                if process.poll() is not None:
                    raise
                # A native picker can interrupt the proxy's click response.
                # Never click twice: the caller must observe the resulting UI
                # (or fail its next assertion) in this same live session.
                print("Click response disconnected; observing resulting state:", label, flush=True)

        def click(selector):
            click_element(element(selector), selector)

        def click_text(text):
            # Locate by text in the real document, then perform a WebDriver click.
            literal = json.dumps(text)
            found = command("POST", "/element", {"using": "xpath", "value": f"//*[normalize-space(.)={literal} and not(.//*[normalize-space(.)={literal}])]"})[ELEMENT]
            click_element(found, text)

        def fill(selector, text):
            found = element(selector)
            reference = {ELEMENT: found}
            try:
                command("POST", f"/element/{found}/clear", {})
            except (RemoteDisconnected, ConnectionResetError):
                assert script("return arguments[0].value", reference) == "", "Disconnected clear did not leave the field empty"
            try:
                command("POST", f"/element/{found}/value", {"text": text})
            except (RemoteDisconnected, ConnectionResetError):
                assert script("return arguments[0].value", reference) == text, "Disconnected typing did not complete the expected value"

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
            maximize_result = script("""
                return (async()=>{
                    const context=window.api.getRuntimeContext();
                    const query=async()=>{
                        const events=await window.__TAURI__.core.invoke('ipc_dispatch',{request:{channel:'MAIN',context,data:{channel:'MAXIMIZED'}}});
                        return events[0].message.data;
                    };
                    const before=await query();
                    const reply=await new Promise((resolve,reject)=>{
                        const timer=setTimeout(()=>reject(new Error('MAXIMIZED reply timed out')),3000);
                        window.api.receive('MAIN',(message,listenerId)=>{
                            if(listenerId!=='native-maximize')return;
                            clearTimeout(timer);
                            window.api.removeListener('MAIN','native-maximize-observer');
                            resolve({message,listenerId});
                        },'native-maximize-observer');
                        window.api.send('MAIN',{channel:'MAXIMIZE'},'native-maximize');
                    });
                    const after=await query();
                    if(after!==before)await window.__TAURI__.core.invoke('ipc_dispatch',{request:{channel:'MAIN',context,data:{channel:'MAXIMIZE'}}});
                    // Xvfb without a window manager may not change native state.
                    // Restore the renderer's state from an actual native query.
                    window.api.send('MAIN',{channel:'MAXIMIZED'});
                    return {before,after,reply};
                })();
            """)
            assert maximize_result["reply"]["message"]["channel"] == "MAXIMIZED", maximize_result
            assert maximize_result["reply"]["message"]["data"] is not maximize_result["before"], maximize_result
            assert maximize_result["reply"]["listenerId"] == "native-maximize", maximize_result
            (ARTIFACTS / "maximize-reply.json").write_text(json.dumps(maximize_result, indent=2))
            steps.append("Retained window.api MAXIMIZE receives the original MAXIMIZED transition channel and correlation")
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
            wait(lambda: len(command("GET", "/window/handles")) >= 2, "Initial native output lifecycle completed")
            before_restore_main = command("GET", "/window")
            before_restore_outputs = set(command("GET", "/window/handles")) - {before_restore_main}
            changed = json.loads(json.dumps(saved))
            changed[1]["slides"] = {}
            show_path.write_text(json.dumps(changed))
            script("window.__nativeEvents=[];window.api.send('MAIN',{channel:'RESTORE'})")
            choose_native_file("Restore FreeShow Backup", backup_path)
            wait(lambda: script("return window.__nativeEvents.some(m=>m.channel==='RESTORE2' && m.data.finished===true)"), "Native restore picker and transaction completed")
            assert json.loads(show_path.read_text()) == saved
            # Restoring settings schedules output recreation. Observe that
            # replacement before configuring a window for the cue tests.
            wait(lambda: before_restore_outputs.isdisjoint(command("GET", "/window/handles")) and len(command("GET", "/window/handles")) >= 2, "Restored settings recreated the native output")

            script("window.__cancelResult=null;window.__TAURI__.core.invoke('ipc_dispatch',{request:{channel:'MAIN',context:window.api.getRuntimeContext(),data:{channel:'OPEN_FOLDER',data:{channel:'NATIVE_TEST',title:'Cancel FreeShow Folder Selection'}}}}).then(events=>window.__cancelResult=events)")
            folder = wait(lambda: native_dialog("Cancel FreeShow Folder Selection"), "Native folder picker")
            native_keys("windowfocus", "--sync", folder)
            native_keys("key", "--clearmodifiers", "Escape")
            wait(lambda: script("return Array.isArray(window.__cancelResult)"), "Native folder cancellation completed")
            assert script("return window.__cancelResult") == []

            click_text("Rust parity test")
            wait(lambda: script("return document.body.innerText.includes('Amazing grace')"), "Restored show reopened for output cues")

            # Use the renderer's real initial-state sender and actual slide
            # clicks. Only window placement is configured through native IPC.
            main_window = command("GET", "/window")
            script("""
                const send=window.api.send.bind(window.api);
                window.__latestOutputData=null;window.__outputSnapshotVersion=0;window.__outputStates=[];
                window.__showReadRequests=[];window.__showReadReplies=[];window.__saveRequests=[];window.__saveAcks=[];
                window.api.receive('MAIN',(message,listenerId)=>{
                    if(message.channel==='SHOW') window.__showReadReplies.push({data:message.data,listenerId,at:performance.now()});
                    if(message.channel==='SAVE2') window.__saveAcks.push({data:message.data,listenerId,at:performance.now()});
                },'native-show-read-test');
                window.api.send=(channel,message,listener)=>{
                    if(channel==='MAIN' && message.channel==='SHOW') window.__showReadRequests.push({data:structuredClone(message.data),listenerId:listener,at:performance.now()});
                    if(channel==='MAIN' && message.channel==='SAVE') window.__saveRequests.push({token:message.data?.runtimeSaveToken,closeWhenFinished:message.data?.closeWhenFinished,names:Object.values(message.data?.showsCache || {}).map(show=>show.name),at:performance.now()});
                    if(channel==='OUTPUT' && message.channel==='OUTPUTS' && !message.data?.id) {
                        window.__latestOutputData=structuredClone(message.data);
                        window.__outputSnapshotVersion++;
                    }
                    return send(channel,message,listener);
                };
                window.api.receive('OUTPUT',m=>{if(m.channel==='OUTPUT_STATE')window.__outputStates.push(m.data)},'native-output-test');
                window.api.send('OUTPUT',{channel:'REQUEST_DATA_MAIN'});
            """)
            initial_outputs = wait(lambda: script("return window.__latestOutputData"), "Initial output state routed through main renderer")
            output_id = next(key for key, value in initial_outputs.items() if value.get("enabled"))
            output_config = {**initial_outputs[output_id], "id": output_id, "alwaysOnTop": False, "bounds": {"x": 40, "y": 40, "width": 640, "height": 360}}
            script("window.api.send('OUTPUT',{channel:'CREATE',data:arguments[0]});window.api.send('OUTPUT',{channel:'TOGGLE_OUTPUTS',data:{outputs:[arguments[0]],state:true,autoPosition:false}})", output_config)
            wait(lambda: script("return window.__outputStates.some(states=>states.some(s=>s.id===arguments[0] && s.active===true))", output_id), "Native output window shown")
            wait(lambda: len(command("GET", "/window/handles")) >= 2, "Output WebView registered with desktop driver")
            script("window.__nativeOutputRegistry=null;window.__TAURI__.core.invoke('output_status').then(s=>window.__nativeOutputRegistry=s)")
            native_outputs = wait(lambda: script("return window.__nativeOutputRegistry"), "Native output registry observed")
            output_label = next(entry["label"] for entry in native_outputs["outputs"] if entry["id"] == output_id)
            output_window = None
            for handle in command("GET", "/window/handles"):
                if handle == main_window:
                    continue
                command("POST", "/window", {"handle":handle})
                if script("return window.__TAURI_INTERNALS__.metadata.currentWindow.label") == output_label:
                    output_window = handle
                    break
            assert output_window is not None, native_outputs
            script("window.__outputMessages=[];window.api.receive('OUTPUT',m=>{window.__outputMessages.push(m);if(window.__outputMessages.length>100)window.__outputMessages.shift()},'output-evidence')")
            command("POST", "/window", {"handle":main_window})
            wait(lambda: script("return document.querySelectorAll('#showArea .grid .slide.context').length>=2"), "Two live slide controls available")
            click_element(command("POST", "/elements", {"using":"css selector","value":"#showArea .grid .slide.context"})[0][ELEMENT], "Verse slide")
            command("POST", "/window", {"handle": output_window})
            wait(lambda: visible_lyric("Amazing grace") and script("return !document.body.innerText.includes('Unicode 日本語')"), "Verse visible after native output transition")
            (ARTIFACTS / "output-verse.png").write_bytes(base64.b64decode(command("GET", "/screenshot")))
            command("POST", "/window", {"handle": main_window})
            click_element(command("POST", "/elements", {"using":"css selector","value":"#showArea .grid .slide.context"})[1][ELEMENT], "Chorus slide")
            command("POST", "/window", {"handle": output_window})
            wait(lambda: visible_lyric("Unicode 日本語 🙌") and script("return !document.body.innerText.includes('Amazing grace')"), "Chorus and Unicode visible after live cue")
            (ARTIFACTS / "output-chorus.png").write_bytes(base64.b64decode(command("GET", "/screenshot")))
            command("POST", "/window", {"handle": main_window})

            # Create a second projection through the real settings wizard.
            click('.top button[data-title^="Settings"]')
            click_text("Outputs")
            click('button[data-title="New output"]')
            wizard = wait(lambda: script("return document.querySelector('.popup button[data-title=Normal]') ? 'input' : document.querySelector('.popup .popup-options') ? 'style' : document.querySelector('.popup .types') ? 'type' : null"), "New-output wizard opened")
            if wizard == "input":
                click('.popup button[data-title="Normal"]')
            next_step = wait(lambda: script("return document.querySelector('.popup .types') ? 'type' : document.querySelector('.popup .popup-options') ? 'style' : null"), "Output setup choice ready")
            if next_step == "style":
                click_text("Skip")
            wait(lambda: script("return !!document.querySelector('.popup .types')"), "Local/network output picker")
            click('.popup button[data-title="Window"]')
            click_text("Confirm")
            second_outputs = wait(lambda: script("const data=window.__latestOutputData;return data && Object.keys(data).length===2 ? data : null"), "Second output created in settings")
            secondary_id = next(key for key in second_outputs if key != output_id)
            secondary_config = {**second_outputs[secondary_id], "id": secondary_id, "alwaysOnTop": False, "bounds": {"x": 40, "y": 440, "width": 640, "height": 360}}
            # Placement uses the same native protocol as output settings. Await
            # CREATE/TOGGLE ordering through the actual frontend lifecycle queue.
            script("window.__outputStates=[];window.api.send('OUTPUT',{channel:'CREATE',data:arguments[0]});window.api.send('OUTPUT',{channel:'TOGGLE_OUTPUTS',data:{outputs:[arguments[0]],state:true,autoPosition:false}})", secondary_config)
            wait(lambda: script("return window.__outputStates.some(states=>states.some(s=>s.id===arguments[0] && s.active===true))", secondary_id), "Second native projection shown")
            wait(lambda: len(command("GET", "/window/handles")) >= 3, "Two projection WebViews registered")
            secondary_window = output_handle(secondary_id)
            # Renaming starts automatically after creation. Returning to the
            # presenting page exercises normal blur/navigation handling too.
            click('button[data-title^="Presenting"]')
            wait(lambda: script("return document.querySelectorAll('#showArea .grid .slide.context').length>=2"), "Presenting controls restored after output setup")
            click_element(command("POST", "/elements", {"using":"css selector","value":"#showArea .grid .slide.context"})[0][ELEMENT], "Broadcast verse slide")
            for handle, label in [(output_window, "Primary receives broadcast cue"), (secondary_window, "Secondary receives broadcast cue")]:
                command("POST", "/window", {"handle": handle})
                wait(lambda: visible_lyric("Amazing grace"), label)
            command("POST", "/window", {"handle": main_window})
            snapshot = script("return window.__latestOutputData")
            targeted = {**snapshot[secondary_id], "out": {**snapshot[secondary_id].get("out", {}), "slide": {**snapshot[secondary_id]["out"]["slide"], "index": 1}}}
            script("window.api.send('OUTPUT',{channel:'OUTPUTS',data:{id:arguments[0],[arguments[0]]:arguments[1]}})", secondary_id, targeted)
            command("POST", "/window", {"handle": secondary_window})
            wait(lambda: visible_lyric("Unicode 日本語 🙌") and script("return !document.body.innerText.includes('Amazing grace')"), "Targeted cue changes only secondary projection")
            (ARTIFACTS / "output-targeted-secondary.png").write_bytes(base64.b64decode(command("GET", "/screenshot")))
            command("POST", "/window", {"handle": output_window})
            assert visible_lyric("Amazing grace")
            assert script("return !document.body.innerText.includes('Unicode 日本語')")
            (ARTIFACTS / "output-targeted-primary.png").write_bytes(base64.b64decode(command("GET", "/screenshot")))
            command("POST", "/window", {"handle": main_window})
            snapshot_version = script("return window.__outputSnapshotVersion")
            script("window.api.send('OUTPUT',{channel:'REQUEST_DATA_MAIN'})")
            refreshed = wait(lambda: script("return window.__outputSnapshotVersion>arguments[0] ? window.__latestOutputData : null", snapshot_version), "Fresh main-renderer snapshot after targeted cue")
            assert refreshed == snapshot
            steps.append("Primary projection and main state unchanged by targeted secondary cue")
            script("window.__outputStates=[];window.api.send('OUTPUT',{channel:'TOGGLE_OUTPUTS',data:{outputs:arguments[0],state:false,autoPosition:false}})", [output_config, secondary_config])
            wait(lambda: script("return arguments[0].every(id=>window.__outputStates.at(-1)?.some(s=>s.id===id && s.active===false))", [output_id, secondary_id]), "Both native projections hidden")
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
            layout_id = other_show[1].get("settings", {}).get("activeLayout") or next(iter(other_show[1]["layouts"]))
            for index, reference in enumerate(other_show[1]["layouts"][layout_id]["slides"]):
                slide = other_show[1]["slides"][reference["id"]]
                for item in slide.get("items", []):
                    for line in item.get("lines", []):
                        for text in line.get("text", []):
                            text["value"] = f"Second library lyric {index}"
            (other / "Shows" / "Second library.show").write_text(json.dumps(other_show))
            cold_show = json.loads(json.dumps(other_show))
            cold_show[0] = "uncached-show"
            cold_show[1]["name"] = "Uncached during backup"
            for slide in cold_show[1]["slides"].values():
                for item in slide.get("items", []):
                    for line in item.get("lines", []):
                        for text in line.get("text", []):
                            text["value"] = "Initial backup snapshot lyric"
            cold_path = other / "Shows" / "Uncached during backup.show"
            cold_path.write_text(json.dumps(cold_show))
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
            wait(lambda: {output_window, secondary_window}.isdisjoint(command("GET", "/window/handles")), "Both previous-library projection windows closed")
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
            stale = wait(lambda: script("return window.__testRuntimeErrors.find(e=>e.error?.code==='stale_library' && e.request?.channel==='MAIN' && e.request?.data?.channel==='SAVE')?.error"), "Previous-library save rejected")
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

            # Settings recreate projections after the library reply. Observe
            # that lifecycle before configuring the two current-library windows.
            wait(lambda: {output_id, secondary_id}.issubset({item["id"] for item in invoke("output_status")["outputs"]}), "Selected library recreated both projections")
            script("window.__outputStates=[];for(const config of arguments[0])window.api.send('OUTPUT',{channel:'SET_VALUE',data:{id:config.id,key:'alwaysOnTop',value:false}});window.api.send('OUTPUT',{channel:'TOGGLE_OUTPUTS',data:{outputs:arguments[0],state:true,autoPosition:false}})", [output_config, secondary_config])
            wait(lambda: script("return arguments[0].every(id=>window.__outputStates.at(-1)?.some(s=>s.id===id && s.active===true))", [output_id, secondary_id]), "Both selected-library projections shown")
            wait(lambda: len(command("GET", "/window/handles")) >= 3, "Selected-library projection WebViews ready")
            current_windows = {output_id: output_handle(output_id), secondary_id: output_handle(secondary_id)}
            # A real main-renderer cue primes both windows. Its snapshot also
            # survives either window's normal delayed initial-state request.
            click(".popup button.outlined")
            wait(lambda: script("return !document.querySelector('.popup')"), "Recovery notice dismissed for live cues")
            wait(lambda: script("return document.querySelectorAll('#showArea .grid .slide.context').length>=2"), "Current-library live cue controls ready")
            click_element(command("POST", "/elements", {"using":"css selector","value":"#showArea .grid .slide.context"})[1][ELEMENT], "Prime selected-library chorus")
            for index, (projected_id, handle) in enumerate(current_windows.items()):
                command("POST", "/window", {"handle": handle})
                wait(lambda: visible_lyric("Second library lyric 1"), "Current-library projection rendered " + str(index + 1))
                script((ROOT / "tests/desktop/output_probe.js").read_text(), projected_id)
            command("POST", "/window", {"handle": main_window})
            current_data = script("return window.__latestOutputData")
            templates = {projected_id: json.loads(json.dumps(current_data[projected_id])) for projected_id in current_windows}
            assert all(template["out"]["slide"]["id"] == saved[0] for template in templates.values())

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
                assert not script("return window.__showReadRequests.some(request=>request.data?.id==='uncached-show')")
                script((ROOT / "tests/desktop/backup_probe.js").read_text(), templates, ["Second library lyric 0", "Second library lyric 1"], {"id": saved[0], "name": "Second library"})
                wait(lambda: invoke("library_status")["activeJob"]["phase"] == "backupCompression", "Backup private snapshot prepared")
                # Change the original in place after copying. A new UI load must
                # see this edit while the ZIP retains the prepared original.
                cold_path.write_text(json.dumps(cold_show).replace("Initial backup snapshot lyric", "Loaded from disk during compression"))
                read_start = time.monotonic()
                click_text("Uncached during backup")
                wait(lambda: script("return document.querySelector('#showArea')?.innerText.includes('Loaded from disk during compression')"), "Uncached show loaded through real presenting UI during compression")
                cold_status = invoke("library_status")
                assert cold_status["activeJob"]["listenerId"] == "backup-responsiveness"
                assert cold_status["activeJob"]["phase"] == "backupCompression"
                media_result = script("""
                    return (async()=>{
                        const started=performance.now(), context=window.api.getRuntimeContext(), replies=[];
                        for(const command of ['FFMPEG_CHECK','MEDIA_CODEC','MEDIA_TRACKS']) {
                            const events=await window.__TAURI__.core.invoke('ipc_dispatch',{request:{channel:'MAIN',context,listenerId:'native-'+command,data:{channel:command,data:{path:arguments[0],future:{kept:true}}}}});
                            replies.push({command,events});
                        }
                        return {context,replies,elapsedMs:performance.now()-started};
                    })();
                """, str(media_fixture))
                for reply in media_result["replies"]:
                    assert len(reply["events"]) == 1, reply
                    event = reply["events"][0]
                    assert event["context"] == media_result["context"]
                    assert event["listenerId"] == "native-" + reply["command"]
                    assert event["message"]["channel"] == reply["command"]
                    result = event["message"]["data"]
                    if reply["command"] == "FFMPEG_CHECK":
                        assert result["installed"] is True and result["path"]
                    else:
                        assert result["path"] == str(media_fixture) and result["future"] == {"kept": True}
                        if reply["command"] == "MEDIA_CODEC":
                            assert result["codecs"] == ["mp4a.40.2"], result
                            assert result["mimeType"] == "application/octet-stream"
                            assert result["mimeCodec"] == 'application/octet-stream; codecs="mp4a.40.2"'
                        else:
                            assert result["tracks"] == []
                assert media_result["elapsedMs"] < 1500, media_result
                media_status = invoke("library_status")
                assert media_status["activeJob"]["listenerId"] == "backup-responsiveness" and media_status["activeJob"]["phase"] == "backupCompression", media_status
                (ARTIFACTS / "media-probe.json").write_text(json.dumps(media_result, indent=2))
                steps.append("Native FFmpeg/AAC metadata replies preserve payload, context and correlation during ZIP compression")
                with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as receiver:
                    receiver.bind(("127.0.0.1", 0))
                    receiver.settimeout(2)
                    osc_started = time.monotonic()
                    script("window.api.send('MAIN',{channel:'EMIT_OSC',data:{signal:{host:'127.0.0.1',port:arguments[0]},data:'/native 42 true'}})", receiver.getsockname()[1])
                    packet, address = receiver.recvfrom(65507)
                    # OSC address/type padding and int32 bytes for the known
                    # reference message; full encoder parity is source-driven.
                    assert packet.hex() == "2f6e6174697665002c6954000000002a", packet.hex()
                    assert address[0] == "127.0.0.1"
                    osc_result = {"packetHex":packet.hex(), "sender":list(address), "elapsedMs":(time.monotonic()-osc_started)*1000}
                osc_status = invoke("library_status")
                assert osc_status["activeJob"]["listenerId"] == "backup-responsiveness" and osc_status["activeJob"]["phase"] == "backupCompression", osc_status
                (ARTIFACTS / "osc-loopback.json").write_text(json.dumps(osc_result, indent=2))
                steps.append("Retained window.api emits a real OSC UDP datagram during ZIP compression")
                cold_read = script("const request=window.__showReadRequests.find(request=>request.data?.id==='uncached-show');const reply=window.__showReadReplies.find(reply=>reply.listenerId===request?.listenerId);return {request,reply,elapsedMs:reply?.at-request?.at}")
                assert cold_read["reply"]["data"]["content"][0] == "uncached-show", cold_read
                assert cold_read["elapsedMs"] < 1500, cold_read
                cold_read["uiElapsedMs"] = (time.monotonic() - read_start) * 1000
                (ARTIFACTS / "uncached-show-read.json").write_text(json.dumps(cold_read, indent=2))
                # Admit an ordinary save behind compression, then create real
                # newer work before that save can read its captured snapshot.
                wait(lambda: script("return window.__responsiveness.samples.length>=8"), "Eight paired samples before queued-save race")
                request_count = script("return window.__saveRequests.length")
                save_shortcut()
                older_save = wait(lambda: script("return window.__saveRequests[arguments[0]]", request_count), "Ordinary save captured while backup owns the library")
                assert isinstance(older_save["token"], str), older_save
                click_text("New show")
                wait(lambda: script("return !!document.querySelector('#name')"), "New edits during queued save")
                fill("#name", "After queued save")
                click_text("Quick lyrics")
                wait(lambda: script("return !!document.querySelector('.popup textarea')"), "Queued-save newer lyrics input")
                fill('.popup textarea', "Keep edits after the save snapshot")
                click('[data-testid="create.show.popup.new.show"]')
                wait(lambda: script("return !document.querySelector('#name') && document.body.innerText.includes('After queued save')"), "Newer show created before save acknowledgment")
                queued_status = invoke("library_status")
                assert queued_status["activeJob"]["listenerId"] == "backup-responsiveness", queued_status
                assert queued_status["queuedJobs"] >= 1, queued_status
                assert not (other / "Shows" / "After queued save.show").exists()
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
                    assert sample["phase"] == "backupCompression"
                    assert len(sample["reads"]) == 2
                    for read in sample["reads"]:
                        assert read["elapsedMs"] < 1500, read
                        assert read["context"] == observed["context"]
                        assert read["id"] == saved[0]
                        assert read["name"] == "Second library"
                        assert read["channel"] == "SHOW"
                    assert set(sample["acks"]) == {output_id, secondary_id}
                    for index, projected_id in enumerate(current_windows):
                        ack = sample["acks"][projected_id]
                        assert "error" not in ack, ack
                        assert isinstance(ack["transportRoundTripMs"], (int, float)), ack
                        assert ack["transportRoundTripMs"] < 500, ack
                        assert ack["roundTripMs"] < 1500, ack
                        assert ack["role"] == "output"
                        assert ack["context"] == observed["context"]
                        assert ack["visibility"] == "visible"
                        assert f"Second library lyric {(sample['sequence'] + index) % 2}" in ack["text"], ack
                with zipfile.ZipFile(observed["backup"]["path"]) as archive:
                    for path, expected in workload.items():
                        info = archive.getinfo("SHOWS/" + path.name)
                        assert info.file_size == expected["size"]
                        assert info.CRC == expected["crc"]
                    assert archive.testzip() is None
                    cold_snapshot = json.loads(archive.read("SHOWS/Uncached during backup.show"))
                    assert cold_snapshot == cold_show
                    assert "Loaded from disk during compression" not in json.dumps(cold_snapshot)
                steps.append("Both projections render cues and native handshakes during real 64 MiB backup; ZIP verified")
                wait(lambda: script("return window.__saveAcks.some(ack=>ack.data.runtimeSaveToken===arguments[0])", older_save["token"]), "Older queued save acknowledged")
                wait(lambda: script("return !!document.querySelector('.top .unsaved') && !document.querySelector('.status .saving')"), "Newer work remains visibly unsaved after old acknowledgment")
                assert not (other / "Shows" / "After queued save.show").exists()
                save_shortcut()
                latest_path = other / "Shows" / "After queued save.show"
                wait(lambda: latest_path.exists() and script("return !document.querySelector('.top .unsaved') && !!document.querySelector('.status .saved')"), "Latest snapshot persisted and marked saved")
                latest = json.loads(latest_path.read_text())
                assert "Keep edits after the save snapshot" in json.dumps(latest)
                (ARTIFACTS / "save-revision-race.json").write_text(json.dumps({"older":older_save,"requests":script("return window.__saveRequests.slice(arguments[0])", request_count),"acks":script("return window.__saveAcks"),"savedShow":latest}, indent=2))
            finally:
                for path in workload:
                    path.unlink(missing_ok=True)
            for handle in current_windows.values():
                command("POST", "/window", {"handle": handle})
                probe_state = script("return window.__projectionProbe")
                assert probe_state["errors"] == [], probe_state
                assert probe_state["completed"] >= 8, probe_state
            command("POST", "/window", {"handle": main_window})
            assert script("return window.__testErrors") == []
            errors = script("return window.__testRuntimeErrors")
            # Use the real Quit control while a second compression owns the IO
            # thread. The legacy saved flag is true, but Quit must still send a
            # final snapshot and preserve earlier native writes before exit.
            def app_pids():
                found = []
                for entry in Path("/proc").iterdir():
                    if not entry.name.isdecimal():
                        continue
                    try:
                        pid = int(entry.name)
                        if os.getpgid(pid) == process.pid and (entry / "exe").resolve() == binary.resolve():
                            found.append(pid)
                    except (OSError, RuntimeError):
                        pass
                return found

            pids = app_pids()
            assert len(pids) == 1, pids
            backup_folder = Path(observed["backup"]["path"]).parent
            previous_backups = set(backup_folder.glob("*.zip"))
            close_requests = script("return window.__saveRequests.length")
            try:
                for path in workload:
                    with path.open("wb") as output:
                        for piece in [b'{"benchmarkPadding":"', *([chunk] * 32), b'"}']:
                            output.write(piece)
                assert not script("return !!document.querySelector('.top .unsaved')")
                script("""
                    const context=window.api.getRuntimeContext();
                    window.__closeJobs=[];
                    const dispatch=(channel,data)=>window.__TAURI__.core.invoke('ipc_dispatch',{request:{channel:'MAIN',context,data:{channel,data}}}).then(value=>window.__closeJobs.push({channel,value})).catch(error=>window.__closeJobs.push({channel,error}));
                    dispatch('SAVE',{customTriggers:{backup:true,isAutoBackup:true},closeWhenFinished:false});
                """)
                wait(lambda: (invoke("library_status").get("activeJob") or {}).get("phase") == "backupCompression", "Quit workload compression started")
                script("const context=window.api.getRuntimeContext();window.__TAURI__.core.invoke('ipc_dispatch',{request:{channel:'MAIN',context,data:{channel:'SET_STORE_VALUE',data:{file:'config',key:'closeDrainSentinel',value:true}}}}).then(value=>window.__closeJobs.push({channel:'SET_STORE_VALUE',value})).catch(error=>window.__closeJobs.push({channel:'SET_STORE_VALUE',error}))")
                wait(lambda: invoke("library_status")["queuedJobs"] >= 1, "Native write admitted before Quit")
                menu_bar = wait(native_menu, "Native GTK menu ready for Quit")
                file_menu = find_native_menu(menu_bar, "File")
                assert file_menu and file_menu.get_action_iface().do_action(0)
                quit_item = wait(lambda: find_native_menu(native_menu(), "Quit"), "Native Quit item", record=False)
                assert quit_item.get_state_set().contains(Atspi.StateType.ENABLED)
                assert quit_item.get_action_iface().do_action(0)
                final_request = wait(lambda: script("return window.__saveRequests.slice(arguments[0]).find(request=>request.closeWhenFinished)", close_requests), "Saved-state Quit captured a final snapshot")
                close_status = invoke("library_status")
                assert (close_status.get("activeJob") or {}).get("phase") == "backupCompression", close_status
                assert app_pids() == pids
                assert not json.loads(config_path.read_text()).get("closeDrainSentinel")
                errors = script("return window.__testRuntimeErrors")
                assert script("return window.__testErrors") == []
                wait(lambda: not app_pids(), "Quit exited after pending backup and writes", seconds=90)
                assert json.loads(config_path.read_text())["closeDrainSentinel"] is True
                completed_backups = set(backup_folder.glob("*.zip")) - previous_backups
                assert len(completed_backups) == 1, completed_backups
                completed_backup = next(iter(completed_backups))
                with zipfile.ZipFile(completed_backup) as archive:
                    assert archive.testzip() is None
                    for path, expected in workload.items():
                        info = archive.getinfo("SHOWS/" + path.name)
                        assert info.file_size == expected["size"] and info.CRC == expected["crc"]
                assert "Keep edits after the save snapshot" in latest_path.read_text()
                (ARTIFACTS / "close-drain.json").write_text(json.dumps({"pid":pids[0],"finalSave":final_request,"pending":close_status,"backup":str(completed_backup),"configWritePersisted":True,"processExited":True}, indent=2))
                steps.append("Real Quit preserved a queued native config write, completed ZIP and final save before process exit")
            finally:
                for path in workload:
                    path.unlink(missing_ok=True)
            # Unsupported features remain visible in the receipt; they are never
            # interpreted as successful parity checks.
            (ARTIFACTS / "receipt.json").write_text(json.dumps({"steps": steps, "runtimeErrors": errors, "platform": "Linux WebKitGTK"}, indent=2))
            print(f"Desktop editor smoke passed; evidence: {ARTIFACTS}", flush=True)
        except Exception:
            if shutil.which("ffmpeg"):
                subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-f", "x11grab", "-i", os.environ["DISPLAY"], "-frames:v", "1", str(ARTIFACTS / "failed-desktop.png")], timeout=10)
            if session:
                try:
                    diagnostic_window = next(iter(current_windows.values())) if "current_windows" in locals() else output_window if "output_window" in locals() else None
                    if diagnostic_window:
                        command("POST", "/window", {"handle":diagnostic_window})
                        (ARTIFACTS / "failed-output-dom.txt").write_text(script("return document.body.innerText"))
                        (ARTIFACTS / "failed-output-state.json").write_text(json.dumps(script("return {context:window.api?.getRuntimeContext(),messages:window.__outputMessages,role:window.__TAURI_INTERNALS__?.metadata.currentWindow.label}"), indent=2))
                except Exception:
                    pass
                try:
                    if "main_window" in locals():
                        command("POST", "/window", {"handle": main_window})
                    (ARTIFACTS / "failed.png").write_bytes(base64.b64decode(command("GET", "/screenshot")))
                    (ARTIFACTS / "failed-dom.txt").write_text(script("return document.body.innerText"))
                    (ARTIFACTS / "failed-errors.json").write_text(json.dumps(script("return {runtime:window.__testRuntimeErrors,errors:window.__testErrors,clicks:window.__testClicks,outputData:window.__latestOutputData,outputStates:window.__outputStates,navigation:[...document.querySelectorAll('.top button')].map(e=>{const r=e.getBoundingClientRect();return {title:e.dataset.title,rect:r.toJSON(),cover:document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)?.outerHTML.slice(0,400)}})}"), indent=2))
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

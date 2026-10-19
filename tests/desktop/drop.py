"""GTK text/uri-list drag source -> real Tauri menu/WebView -> Svelte project.

Run with the native smoke-test environment under an isolated Xvfb/DBus session.
For GDK_SCALE=2 use an Xvfb screen of at least 3200x2200.
"""
import base64
from http.client import RemoteDisconnected
import itertools
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.request
import wave

from menu import free_port, native_menu, wait
from script_receipts import wrap_script
import gi

ROOT = Path(__file__).resolve().parents[2]
ARTIFACTS = ROOT / "test-output-rust" / "native-drop"


def source(paths_file):
    gi.require_version("Gtk", "3.0")
    from gi.repository import Gtk, Gdk
    paths = json.loads(Path(paths_file).read_text())
    window = Gtk.Window(title="Native file drag source")
    window.set_default_size(260, 120)
    window.move(20, int(os.environ.get("FREESHOW_DROP_SOURCE_Y", "900")))
    label = Gtk.Label(label="Drag real local files into FreeShow")
    box = Gtk.EventBox()
    box.add(label)
    box.drag_source_set(Gdk.ModifierType.BUTTON1_MASK, [Gtk.TargetEntry.new("text/uri-list", 0, 2)], Gdk.DragAction.COPY)
    box.connect("drag-begin", lambda widget, context: print("GTK drag began", flush=True))
    box.connect("drag-data-get", lambda widget, context, selection, info, timestamp: selection.set_uris([Path(path).as_uri() for path in paths]))
    window.add(box)
    window.connect("destroy", Gtk.main_quit)
    window.show_all()
    Gtk.main()


def run():
    ARTIFACTS.mkdir(parents=True, exist_ok=True)
    scale = int(os.environ.get("GDK_SCALE", "1"))
    receipt = ARTIFACTS / f"receipt-{scale}x.json"
    receipt.unlink(missing_ok=True)
    binary = str(Path(os.environ.get("FREESHOW_DESKTOP_BIN", ROOT / "target/debug/freeshow-desktop")))
    driver = os.environ.get("TAURI_DRIVER") or shutil.which("tauri-driver")
    webkit = os.environ.get("WEBKIT_WEBDRIVER") or shutil.which("WebKitWebDriver")
    xdotool = os.environ.get("XDOTOOL") or shutil.which("xdotool")
    assert driver and webkit and xdotool
    port, native_port = free_port(), free_port()
    session = None
    source_process = None
    with tempfile.TemporaryDirectory(prefix="freeshow-native-drop-") as temporary:
        temp = Path(temporary)
        app_data, data = temp / "app", temp / "data"
        app_data.mkdir()
        (data / "Config").mkdir(parents=True)
        (app_data / "settings.json").write_text(json.dumps({"initialized": True, "language": "en", "activeProject": "p", "special": {"showClosePopup": True}}))
        (data / "Config/projects.json").write_text(json.dumps({"projects": {"p": {"name": "Native drop project", "shows": []}}, "folders": {}, "projectTemplates": {}}))
        image = temp / "Native 日本語 & space.png"
        image.write_bytes(base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aS1sAAAAASUVORK5CYII="))
        audio = temp / "Second Ελληνικά.wav"
        with wave.open(str(audio), "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(8000)
            wav.writeframes(b"\0\0" * 8000)
        paths = [str(image), str(audio)]
        (temp / "paths.json").write_text(json.dumps(paths))
        env = dict(os.environ, FREESHOW_RUST_APP_DATA=str(app_data), FREESHOW_RUST_DATA_DIR=str(data),
                   XDG_DATA_HOME=str(temp / "xdg-data"), XDG_CACHE_HOME=str(temp / "xdg-cache"), XDG_CONFIG_HOME=str(temp / "xdg-config"),
                   GSETTINGS_BACKEND="memory", TAURI_WEBVIEW_AUTOMATION="true", WEBKIT_DISABLE_COMPOSITING_MODE="1", LANG="C.UTF-8", LC_ALL="C.UTF-8")
        log = (ARTIFACTS / f"driver-{scale}x.log").open("w")
        process = subprocess.Popen([driver, "--port", str(port), "--native-port", str(native_port), "--native-driver", webkit], env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)

        def http(method, route, payload=None):
            endpoint = port if method == "POST" and route == "/session" else native_port
            request = urllib.request.Request(f"http://127.0.0.1:{endpoint}" + route, data=None if payload is None else json.dumps(payload).encode(), method=method, headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(request, timeout=35) as response:
                value = json.load(response)["value"]
            assert not isinstance(value, dict) or "error" not in value, value
            return value

        sequence = itertools.count()

        def script(code, *args):
            wrapped = wrap_script(next(sequence), code)
            for attempt in range(3):
                try:
                    return http("POST", f"/session/{session}/execute/sync", {"script": wrapped, "args": list(args)})
                except (RemoteDisconnected, ConnectionResetError):
                    if attempt == 2 or process.poll() is not None:
                        raise
                    time.sleep(0.1)

        def keys(*args):
            environment = dict(os.environ)
            if os.environ.get("XDOTOOL_LIBRARY_PATH"):
                environment["LD_LIBRARY_PATH"] = os.environ["XDOTOOL_LIBRARY_PATH"]
            return subprocess.check_output([xdotool, *map(str, args)], env=environment, text=True, timeout=10).strip()

        def drag_to(x, y):
            source_id = keys("search", "--onlyvisible", "--name", "^Native file drag source$").splitlines()[-1]
            geometry = dict(line.split("=", 1) for line in keys("getwindowgeometry", "--shell", source_id).splitlines())
            sx = int(geometry["X"]) + int(geometry["WIDTH"]) // 2
            sy = int(geometry["Y"]) + int(geometry["HEIGHT"]) // 2
            keys("mousemove", "--sync", sx, sy)
            keys("mousedown", "1")
            keys("mousemove", "--sync", sx + 30, sy - 20)
            time.sleep(0.1)
            keys("mousemove", "--sync", x, y)
            wait(lambda: script("return window.__nativeDropEvents.some(e=>e.phase==='enter')"), "Real GTK source entered Tauri WebView")

        try:
            wait(lambda: http("GET", "/status").get("ready"), "WebDriver ready")
            session = http("POST", "/session", {"capabilities": {"alwaysMatch": {"tauri:options": {"application": binary}}}})["sessionId"]
            wait(lambda: script("return !!window.api && !!document.querySelector('#projectArea .droparea')"), "Seeded real project layout ready")
            script("window.__nativeDropEvents=[];window.__nativeDropDOM=[];window.__nativeDropErrors=[];window.addEventListener('freeshow:runtime-error',e=>window.__nativeDropErrors.push({error:e.detail.error,channel:e.detail.request?.channel,command:e.detail.request?.data?.channel}));document.addEventListener('drop',e=>window.__nativeDropDOM.push({x:e.clientX,y:e.clientY,target:e.target.closest('#projectArea')?.id,names:[...e.dataTransfer.files].map(f=>f.name),paths:[...e.dataTransfer.files].map(f=>window.api.showFilePath(f))}),true);return window.__TAURI__.event.listen('freeshow://drop',e=>window.__nativeDropEvents.push(e.payload),{target:{kind:'WebviewWindow',label:'main'}}).then(()=>true)")
            source_process = subprocess.Popen([sys.executable, __file__, "--source", str(temp / "paths.json")], env=env)
            wait(lambda: keys("search", "--onlyvisible", "--name", "^Native file drag source$"), "Real GTK drag source visible")
            point = script("const r=document.querySelector('#projectArea .droparea').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2,dpr:devicePixelRatio}")
            bar = wait(native_menu, "Native GTK application menu exists")
            from gi.repository import Atspi
            bounds = bar.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
            # GTK AT-SPI coordinates use logical pixels; xdotool uses physical
            # X11 coordinates. The menu is outside the WebView content origin.
            root_x = round((bounds.x + point["x"]) * scale)
            root_y = round((bounds.y + bounds.height + point["y"]) * scale)
            drag_to(root_x, root_y)
            wait(lambda: script("return !!document.querySelector('#projectArea [data-native-drop-active]')"), "Native hit test selects actual project after native menu offset")
            keys("mouseup", "1")
            wait(lambda: script("return window.__nativeDropDOM.length===1"), "Exactly one real Svelte drop delivery")
            wait(lambda: script("return [...document.querySelectorAll('#projectArea .selectElem[data-item]')].map(e=>JSON.parse(e.dataset.item).id).filter(id=>arguments[0].includes(id)).length===2", paths), "Original project dropActions added both media references")
            observation = script("return {native:window.__nativeDropEvents,dom:window.__nativeDropDOM,errors:window.__nativeDropErrors}")
            assert not any(error.get("channel") == "native-drop" for error in observation["errors"]), observation["errors"]
            assert observation["dom"][0]["paths"] == paths, observation
            accepted_sequence = observation["native"][0]["sequence"]
            accepted_phases = [event["phase"] for event in observation["native"] if event["sequence"] == accepted_sequence and event["phase"] != "over"]
            assert accepted_phases == ["enter", "drop"], accepted_phases
            assert observation["dom"][0]["target"] == "projectArea", observation
            assert abs(observation["dom"][0]["x"] - point["x"]) < 3
            assert abs(observation["dom"][0]["y"] - point["y"]) < 3
            script("window.__nativeDropEvents=[]")
            drag_to(root_x, root_y)
            keys("key", "Escape")
            keys("mouseup", "1")
            wait(lambda: script("return !document.querySelector('[data-native-drop-active]')"), "Cancelled GTK drag clears native target")
            assert script("return window.__nativeDropDOM.length") == 1
            script("window.__nativeDropEvents=[]")
            drag_to(root_x, root_y)
            previous = script("return window.api.getRuntimeContext()")
            other = temp / "other-library"
            (other / "Config").mkdir(parents=True)
            (other / "Config/projects.json").write_text(json.dumps({"projects": {"p": {"name": "Different library same project ID", "shows": []}}, "folders": {}, "projectTemplates": {}}))
            script("window.api.send('MAIN',{channel:'UPDATE_DATA_PATH',data:{oldPath:arguments[0],newPath:arguments[1]}})", str(data), str(other))
            wait(lambda: script("return window.api.getRuntimeContext().dataPath===arguments[0]", str(other)), "Real library switch while GTK drag is held")
            keys("mouseup", "1")
            wait(lambda: script("return !document.querySelector('[data-native-drop-active]')"), "Stale-library GTK drop cleared")
            assert script("return window.__nativeDropDOM.length") == 1
            stale = script("return {events:window.__nativeDropEvents,context:window.api.getRuntimeContext()}")
            assert stale["context"]["libraryGeneration"] > previous["libraryGeneration"]
            receipt.write_text(json.dumps({"platform": "linux", "scale": scale, "target": point, "menuHeight": bounds.height, "observation": observation, "cancelledWithoutSecondDrop": True, "staleLibraryIgnored": stale}, ensure_ascii=False, indent=2) + "\n")
        finally:
            if source_process:
                source_process.terminate()
                source_process.wait(timeout=10)
            if session:
                try:
                    http("DELETE", f"/session/{session}")
                except Exception:
                    pass
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=10)
            log.close()


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--source":
        source(sys.argv[2])
    else:
        run()

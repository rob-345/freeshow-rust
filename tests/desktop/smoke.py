"""Exercise the real Tauri binary through WebKitWebDriver on Linux.

Run under `xvfb-run -a dbus-run-session -- python3 tests/desktop/smoke.py`.
Requires tauri-driver and WebKitWebDriver on PATH (or environment overrides).
Requires Python GI with the AT-SPI 2 typelib for native dialog controls.
Every test run has fresh application, WebKit, data, and cache directories.
"""
import base64
from http.client import RemoteDisconnected
import itertools
import hashlib
import gzip
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import random
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import wave
import zipfile
import zlib
import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi
from menu import native_menu, find as find_native_menu
from script_receipts import wrap_script
from audio_peer import AudioPeer
from remote_cover_http import probe_cover_http

ROOT = Path(__file__).resolve().parents[2]
ARTIFACTS = ROOT / "test-output-rust" / "desktop"
ARTIFACTS.mkdir(parents=True, exist_ok=True)
ELEMENT = "element-6066-11e4-a52e-4f735466cecf"


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def free_api_port():
    for _ in range(30):
        with socket.socket() as first, socket.socket() as second, socket.socket(type=socket.SOCK_DGRAM) as udp:
            first.bind(("127.0.0.1", 0))
            candidate = first.getsockname()[1]
            if candidate == 65535:
                continue
            try:
                second.bind(("127.0.0.1", candidate + 1))
                udp.bind(("127.0.0.1", candidate))
                return candidate
            except OSError:
                pass
    raise RuntimeError("Unable to reserve loopback API ports")


def run():
    for name in ["now-playing.json", "timecode.json", "audio-stream.json", "midi.json", "network-queries.json", "media-copy.json", "scriptures.json", "output-scripture.png", "receipt.json", "editor.png", "saved.show", "saved-before-dialog.show", "output-verse.png", "output-chorus.png", "output-targeted-primary.png", "output-targeted-secondary.png", "library-switch.png", "stale-save-recovery.json", "responsiveness.json", "uncached-show-read.json", "save-revision-race.json", "close-drain.json", "media-probe.json", "audio-metadata.json", "media-locate.json", "media-folder-reset.json", "system-info.json", "device-providers.json", "api.json", "imports.json", "assets.json", "thumbnails.json", "file-reads.json", "remotes.json", "encoders.json", "browse.json", "osc-loopback.json", "failed.png", "failed-desktop.png", "failed-dom.txt", "failed-output-dom.txt", "failed-output-state.json", "failed-errors.json"]:
        (ARTIFACTS / name).unlink(missing_ok=True)
    shutil.rmtree(ARTIFACTS / "imported-shows", ignore_errors=True)
    binary = Path(os.environ.get("FREESHOW_DESKTOP_BIN", ROOT / "target/debug/freeshow-desktop"))
    (ARTIFACTS / "ffmpeg-download.json").unlink(missing_ok=True)
    (ARTIFACTS / "now-playing-folder-alerts.json").unlink(missing_ok=True)
    (ARTIFACTS / "ai-spotify.json").unlink(missing_ok=True)
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
        # Actual process-global userData inventory, never a renderer-selected root.
        ai_engine = temp / "app/bin/nemotron"
        ai_fixture_files = {"README": b"Inert AI inventory fixture\n", "models/模型.bin": b"\x00\x01\x02\xff\x10"}
        for relative, content in ai_fixture_files.items():
            target = ai_engine / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(content)
        ai_fixture_receipt = [{"relative":relative,"bytes":len(content),"sha256":hashlib.sha256(content).hexdigest()} for relative, content in ai_fixture_files.items()]
        ai_ignored_root = temp / "ignored-renderer-ai-root"
        (ai_ignored_root / "bin/nemotron").mkdir(parents=True)
        (ai_ignored_root / "bin/nemotron/not-global").write_bytes(b"renderer decoy " * 31)
        assert shutil.which("ffmpeg") and shutil.which("ffprobe"), "Native media integration requires ffmpeg and ffprobe"
        media_fixture = temp / "原音 native probe.m4a"
        subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=8000:duration=1", "-c:a", "aac", "-b:a", "32k", "-threads", "1", "-y", str(media_fixture)], check=True, timeout=15)
        audio_fixture = temp / "音量 Native tags.flac"
        subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=8000:duration=1", "-c:a", "flac", "-threads", "1", "-metadata", "title=Native 敬拜 café", "-metadata", "artist=Artist Ελληνικά", "-metadata", "replaygain_album_gain=+9.0 dB", "-metadata", "replaygain_track_peak=0.8", "-y", str(audio_fixture)], check=True, timeout=15)
        expected_audio = json.loads(subprocess.check_output(["node", "tests/desktop/audio_oracle.mjs", str(audio_fixture)], cwd=ROOT, text=True, timeout=15))
        now_playing_fixtures = json.loads(subprocess.check_output([os.environ.get("FREESHOW_TEST_NODE", "node"), "tests/desktop/now_playing_desktop_oracle.mjs", "--fixtures", str(temp / "now-playing-fixtures")], cwd=ROOT, text=True, timeout=15))
        jpeg_oracle = "tests/desktop/jpeg_now_playing_oracle.mjs"
        jpeg_fixtures = json.loads(subprocess.check_output([os.environ.get("FREESHOW_TEST_NODE", "node"), jpeg_oracle, "--fixtures", str(temp / "jpeg-now-playing-fixtures")], cwd=ROOT, text=True, timeout=15))
        now_playing_fixtures["cases"].extend(jpeg_fixtures["cases"])
        expected_network = json.loads(subprocess.check_output(["node", "tests/desktop/network_queries_oracle.mjs"], cwd=ROOT, text=True, timeout=15))
        assert expected_audio["common"]["title"] == "Native 敬拜 café" and expected_audio["replayGainMultiplier"] == 1.25
        expected_system = json.loads(subprocess.check_output(["node", "tests/desktop/system_oracle.mjs"], cwd=ROOT, text=True, timeout=15))
        browse_folder = temp / "browse-fixtures 敬拜"
        (browse_folder / "Nested").mkdir(parents=True)
        browse_file = browse_folder / "worship.txt"
        browse_file.write_text("Native browse 敬拜", encoding="utf-8")
        (browse_folder / "worship-copy.txt").write_text("similar", encoding="utf-8")
        (browse_folder / "Nested" / "child.txt").write_text("nested", encoding="utf-8")
        # Relatime must not change directory atime between the source and native
        # listing; this preserves the exact captured stat comparison.
        for directory in [browse_folder / "Nested", browse_folder]:
            os.utime(directory, ns=(2051222400000000000, directory.stat().st_mtime_ns))
        expected_browse = json.loads(subprocess.check_output(["node", "tests/desktop/browse_oracle.mjs", str(browse_folder), str(browse_file)], cwd=ROOT, text=True, timeout=15))

        def assert_audio_metadata(actual, expected):
            if isinstance(expected, float):
                assert isinstance(actual, (float, int)) and abs(actual - expected) <= 2e-14 * max(1, abs(expected)), (actual, expected)
            elif isinstance(expected, dict):
                # WebKitWebDriver may reorder returned JSON object members.
                # Observe application-visible ordering as arrays in the WebView.
                assert isinstance(actual, dict) and set(actual) == set(expected), (actual, expected)
                for key in expected:
                    assert_audio_metadata(actual[key], expected[key])
            elif isinstance(expected, list):
                assert isinstance(actual, list) and len(actual) == len(expected), (actual, expected)
                for value, reference in zip(actual, expected):
                    assert_audio_metadata(value, reference)
            else:
                assert actual == expected, (actual, expected)

        def audio_key_order(value, path=None):
            path = [] if path is None else path
            if isinstance(value, dict):
                return [[path, list(value)]] + [entry for key, child in value.items() for entry in audio_key_order(child, [*path, key])]
            if isinstance(value, list):
                return [entry for index, child in enumerate(value) for entry in audio_key_order(child, [*path, index])]
            return []
        discovery_peer = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        discovery_peer.bind(("127.0.0.1", 0))
        inert_download = b"FreeShow inert download fixture, never executed\n\x00\xff"
        inert_archive = gzip.compress(inert_download, mtime=0)
        download_requests = []
        class InertDownload(BaseHTTPRequestHandler):
            def do_GET(self):
                download_requests.append(self.path)
                if self.path != "/inert-ffmpeg.gz":
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "application/gzip")
                self.send_header("Content-Length", str(len(inert_archive)))
                self.end_headers()
                self.wfile.write(inert_archive)
            def log_message(self, *_args):
                pass
        download_server = HTTPServer(("127.0.0.1", 0), InertDownload)
        download_thread = threading.Thread(target=download_server.serve_forever, name="inert-download-fixture", daemon=True)
        download_thread.start()
        env = dict(os.environ, FREESHOW_RUST_APP_DATA=str(temp / "app"), HOSTNAME="freeshow-spoofed-environment.local", FREESHOW_RUST_API_BIND_IP="127.0.0.1", FREESHOW_RUST_REMOTES_BIND_IP="127.0.0.1", FREESHOW_RUST_DISCOVERY_LOOPBACK_TARGET="127.0.0.1:" + str(discovery_peer.getsockname()[1]),
                   XDG_DATA_HOME=str(temp / "xdg-data"), XDG_CACHE_HOME=str(temp / "xdg-cache"),
                   XDG_CONFIG_HOME=str(temp / "xdg-config"), GSETTINGS_BACKEND="memory",
                   TAURI_WEBVIEW_AUTOMATION="true", WEBKIT_DISABLE_COMPOSITING_MODE="1",
                   LANG=os.environ.get("FREESHOW_TEST_LOCALE", "C.UTF-8"), LC_ALL=os.environ.get("FREESHOW_TEST_LOCALE", "C.UTF-8"))
        # Native launch authority only; renderer fields cannot choose codecs.
        ltc_library = ROOT / "node_modules/libltc-wrapper/libltc/bin/libltc.so.11"
        assert hashlib.sha256(ltc_library.read_bytes()).hexdigest() == "f02c75f982fe01d16f345875d2b09404b8b326bb847f2b5456616f6d0227d09f"
        env["FREESHOW_RUST_LTC_LIBRARY"] = str(ltc_library)
        env.pop("FREESHOW_RUST_DATA_DIR", None)
        env["FREESHOW_RUST_FFMPEG_DOWNLOAD_LOOPBACK_URL"] = "http://127.0.0.1:" + str(download_server.server_port) + "/inert-ffmpeg.gz"
        # Exercise the real native OS Documents authority in this private profile.
        documents_root = temp / "Legacy Documents 敬拜"
        (documents_root / "Bibles").mkdir(parents=True)
        (temp / "xdg-config").mkdir()
        (temp / "xdg-config/user-dirs.dirs").write_text('XDG_DOCUMENTS_DIR="' + str(documents_root) + '"\n')
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

        identity_baseline = None
        device_provider_receipts = {}
        def probe_device_providers(phase):
            nonlocal identity_baseline
            persisted = json.loads((temp / "app/config.json").read_text()).get("machineId")
            assert isinstance(persisted, str) and persisted and persisted != "fallback-machine-id", "Startup identity was not persisted"
            if identity_baseline is None:
                identity_baseline = persisted
            assert persisted == identity_baseline, "Global identity changed across editor lifecycle"
            oracle_args = [os.environ.get("FREESHOW_TEST_NODE", "node"),"tests/desktop/provider_catalogue_oracle.mjs"]
            expected = json.loads(subprocess.check_output([*oracle_args,"--expected",phase],cwd=ROOT,env=env,text=True,timeout=15))
            runtime_before = script("return (window.__testRuntimeErrors || []).length")
            observed = script((ROOT / "tests/desktop/device_provider_probe.js").read_text(), {"phase":phase,"expectedIdentity":persisted})
            assert observed["context"] == observed["current"] == invoke("runtime_context"), observed
            assert observed["identity"]["equalsPersisted"] and observed["identity"]["nonempty"] and not observed["identity"]["legacyFallback"], "Retained identity callback did not match persistence"
            assert observed["identity"]["type"] == "string"
            assert observed["providers"]["data"] == {} and observed["providers"]["keys"] == [], observed
            channels = ["DEVICE_ID","GET_CONTENT_PROVIDERS","PROVIDER_CONNECTIONS"]
            assert sorted(observed["callbacks"]) == channels, observed
            assert observed["sendUndefined"] == {channel:True for channel in channels}, observed
            for key, suffix in [("identity","identity"),("providers","providers"),("catalogue","catalogue")]:
                callback = observed[key]
                assert callback["messageKeys"] == ["channel","data"] and callback["args"] == 2, callback
                assert callback["listener"] == "device-provider-probe-" + phase + "-" + suffix, callback
                assert callback["callbackContext"] == observed["context"], callback
            assert observed["catalogue"]["data"] == expected["catalogue"]["data"], observed["catalogue"]
            assert observed["catalogue"]["rowKeys"] == expected["catalogue"]["rowKeys"], observed["catalogue"]
            comparison_input = {"phase":phase,"context":observed["context"],"current":observed["current"],"catalogue":observed["catalogue"],"sendUndefined":observed["sendUndefined"]["GET_CONTENT_PROVIDERS"]}
            verified = json.loads(subprocess.run([*oracle_args,"--verify",phase],input=json.dumps(comparison_input),cwd=ROOT,env=env,text=True,capture_output=True,check=True,timeout=15).stdout)
            assert verified["comparison"] == {"matches":True,"completeRows":5,"completeFields":15,"keyOrderMatched":True,"exactListenerMatched":True,"fullCallbackContextMatched":True,"voidSendMatched":True}, verified
            assert verified["catalogue"] == expected["catalogue"], verified
            confirmed = json.loads(subprocess.check_output([*oracle_args,"--expected",phase],cwd=ROOT,env=env,text=True,timeout=15))
            assert confirmed == expected, "Immutable source catalogue changed around actual native callbacks"
            command_errors = script("return (window.__testRuntimeErrors || []).slice(arguments[0]).filter(entry=>['DEVICE_ID','PROVIDER_CONNECTIONS','GET_CONTENT_PROVIDERS'].includes(entry.request?.data?.channel || entry.request?.message?.channel))", runtime_before)
            assert command_errors == [], command_errors
            assert json.loads((temp / "app/config.json").read_text()).get("machineId") == identity_baseline, "Query changed identity"
            observed["catalogueSource"] = {"before":expected,"verified":verified["comparison"],"after":confirmed,"declarationOnly":True,"authenticationOrBackendHealthVerified":False}
            observed["commandRuntimeErrors"] = 0
            device_provider_receipts[phase] = observed
            (ARTIFACTS / "device-providers.json").write_text(json.dumps({"phases":device_provider_receipts,"rawIdentityArchived":False,"sameGlobalIdentityAcrossPhases":True,"catalogueDeclarationOnly":True,"providerBackendHealthVerified":False},ensure_ascii=False,indent=2))
            steps.append("Retained device identity, empty stored-provider and immutable declared-catalogue callbacks preserve persistence, current context, key order, listener and void send: " + phase)

        ai_spotify_receipts = {}
        def probe_ai_spotify(phase):
            def archive():
                (ARTIFACTS / "ai-spotify.json").write_text(json.dumps({"fixture":{"userData":str(temp / "app"),"files":ai_fixture_receipt},"phases":ai_spotify_receipts,"rawIdentityArchived":False,"spotifyStateCorpus":"actual private Linux bus ServiceUnknown only"},ensure_ascii=False,indent=2))
            def assert_fixture_unchanged():
                assert {entry.relative_to(ai_engine).as_posix() for entry in ai_engine.rglob("*")} == {"README","models","models/模型.bin"}, "Status/inventory query created unexpected global model/stamp entries"
                for relative, content in ai_fixture_files.items():
                    assert (ai_engine / relative).read_bytes() == content, "Global AI fixture changed across actual editor lifecycle"
            assert_fixture_unchanged()
            context = invoke("runtime_context")
            selected_decoy = Path(context["dataPath"]) / "bin/nemotron"
            assert selected_decoy != ai_engine
            selected_decoy.mkdir(parents=True, exist_ok=True)
            (selected_decoy / "not-global").write_bytes(b"selected library decoy " * 19)
            oracle_args = [os.environ.get("FREESHOW_TEST_NODE", "node"),"tests/desktop/ai_spotify_oracle.mjs",str(temp / "app"),phase,str(ai_ignored_root)]
            expected = json.loads(subprocess.check_output(oracle_args,cwd=ROOT,env=env,text=True,timeout=15))
            assert expected["ai"]["data"] == [{"path":str(ai_engine),"name":"nemotron","size":sum(map(len,ai_fixture_files.values()))}]
            assert expected["spotify"]["data"] is None and expected["spotify"]["sourceNullReason"] == "ServiceUnknown"
            assert expected["aiStatus"]["data"] == {"nemotron":{"ready":False,"error":"The local model is missing"}}
            assert expected["aiStatus"]["payload"] == {"engineId":"nemotron","customPath":str(ai_ignored_root)}
            assert expected["aiStatus"]["sourceReplyArgs"] == 3 and expected["aiStatus"]["evidence"] == {"hashed":[],"stats":[]}
            ai_spotify_receipts[phase] = {"sourceBefore":expected,"pending":True}
            archive()
            runtime_before = script("return (window.__testRuntimeErrors || []).length")
            observed = script((ROOT / "tests/desktop/ai_spotify_probe.js").read_text(), {"phase":phase,"ignoredRoot":str(ai_ignored_root)})
            assert observed["context"] == observed["current"] == context == invoke("runtime_context"), observed
            assert sorted(observed["callbacks"]) == ["AI_GET_BIN","AI_GET_STATUS","SPOTIFY_GET_STATE"], observed
            assert observed["sendUndefined"] == {"AI_GET_BIN":True,"AI_GET_STATUS":True,"SPOTIFY_GET_STATE":True}, observed
            assert observed["rawIdentityArchived"] is False, observed
            for command, key in [("AI_GET_BIN","ai"),("AI_GET_STATUS","aiStatus"),("SPOTIFY_GET_STATE","spotify")]:
                actual, original = observed[key], expected[key]
                assert actual["data"] == original["data"], (command,actual,original)
                assert actual["messageKeys"] == original["messageKeys"] == ["channel","data"], actual
                assert actual["listener"] == original["listener"] and actual["args"] == 2, actual
                assert actual["callbackContext"] == context, actual
            assert observed["ai"]["rowKeys"] == expected["ai"]["rowKeys"] == [["path","name","size"]]
            assert observed["aiStatus"]["dataKeys"] == expected["aiStatus"]["dataKeys"] == ["nemotron"]
            assert observed["aiStatus"]["engineKeys"] == expected["aiStatus"]["engineKeys"] == ["ready","error"]
            confirmed = json.loads(subprocess.check_output(oracle_args,cwd=ROOT,env=env,text=True,timeout=15))
            assert confirmed == expected, "Actual original source or private bus changed around native callbacks"
            command_errors = script("return (window.__testRuntimeErrors || []).slice(arguments[0]).filter(entry=>['AI_GET_BIN','AI_GET_STATUS','SPOTIFY_GET_STATE'].includes(entry.request?.data?.channel || entry.request?.message?.channel))", runtime_before)
            assert command_errors == [], command_errors
            assert_fixture_unchanged()
            ai_spotify_receipts[phase] = {"sourceBefore":expected,"nativeCallbacks":observed,"sourceAfter":confirmed,"fixtureBytesUnchanged":True,"commandRuntimeErrors":0,"pending":False}
            archive()
            steps.append("Retained AI inventory, explicit Nemotron missing status and genuine Linux Spotify service-null match immutable source callbacks, native global root, void send and current context: " + phase)

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

        def click_text(text, virtual_show=False):
            if virtual_show:
                materialized = script(r"""
                    return (async () => {
                        const text=arguments[0], rowSelector='button[class~="#drawer_show_button"]';
                        const normalize=value=>value.replace(/\s+/g,' ').trim();
                        const visible=viewport=>{
                            if(!viewport.isConnected || viewport.clientHeight<=0) return false;
                            const rect=viewport.getBoundingClientRect(), style=getComputedStyle(viewport);
                            return rect.width>0 && rect.height>0 && style.display!=='none' && style.visibility!=='hidden';
                        };
                        // Navigation has its own Categories VirtualList. Only
                        // the retained Content .main owns actual show buttons.
                        let previous=null, step=0, scrolls=0;
                        for(let attempt=0;attempt<128;attempt++) {
                            const viewport=Array.from(document.querySelectorAll('.drawer > .content > .main svelte-virtual-list-viewport'))
                                .find(node=>visible(node) && node.querySelector(rowSelector));
                            if(viewport) {
                                if(viewport!==previous) { previous=viewport;step=0; }
                                const found=Array.from(viewport.querySelectorAll(rowSelector)).some(button=>
                                    Array.from(button.querySelectorAll('*')).some(node=>normalize(node.textContent||'')===text &&
                                        !Array.from(node.children).some(child=>normalize(child.textContent||'')===text)));
                                if(found) return true;
                                if(scrolls<64) {
                                    const end=Math.max(0,viewport.scrollHeight-viewport.clientHeight);
                                    const top=Math.min(step++*Math.max(30,viewport.clientHeight*0.75),end);
                                    if(top!==viewport.scrollTop || step===1) {
                                        viewport.scrollTop=top;
                                        viewport.dispatchEvent(new Event('scroll'));
                                        scrolls++;
                                    }
                                }
                            }
                            // Tab content/offsetHeight and keyed rows settle
                            // asynchronously; requery the live DOM after paint.
                            await new Promise(resolve=>setTimeout(resolve,25));
                        }
                        return false;
                    })()
                """, text)
                assert materialized, "Show not materialized within bounded ordinary viewport scan: " + text
            # Locate by text in the real document, then perform a WebDriver click.
            # XPath has no JSON backslash escapes. Keep Unicode literal and
            # construct concat() only when the text contains both quote kinds.
            if '"' not in text:
                literal = '"' + text + '"'
            elif "'" not in text:
                literal = "'" + text + "'"
            else:
                literal = "concat(" + ", '\"', ".join('"' + part + '"' for part in text.split('"')) + ")"
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
            probe_device_providers("normal")
            probe_ai_spotify("normal")
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
            probe_device_providers("restarted")
            probe_ai_spotify("restarted")
            configured_media_folder = data_dir.parent / "Native media folder 敬拜"
            media_folder_reset = script("""
                return (async()=>{
                    let sequence=0;
                    const read=()=>new Promise((resolve,reject)=>{
                        const id='native-media-folder-'+(++sequence);
                        const timer=setTimeout(()=>{window.api.removeListener('MAIN',id);reject(new Error('GET_MEDIA_FOLDER_PATH timed out'));},3000);
                        window.api.receive('MAIN',(message,listenerId)=>{
                            if(listenerId!==id)return;
                            clearTimeout(timer);window.api.removeListener('MAIN',id);
                            resolve({message,listenerId});
                        },id);
                        window.api.send('MAIN',{channel:'GET_MEDIA_FOLDER_PATH'},id);
                    });
                    window.api.send('MAIN',{channel:'SET_MEDIA_FOLDER_PATH',data:arguments[0]});
                    const configured=await read();
                    window.api.send('MAIN',{channel:'SET_MEDIA_FOLDER_PATH',data:''});
                    const reset=await read();
                    return {context:window.api.getRuntimeContext(),configured,reset};
                })();
            """, str(configured_media_folder))
            assert media_folder_reset["configured"]["message"] == {"channel":"GET_MEDIA_FOLDER_PATH","data":str(configured_media_folder)}, media_folder_reset
            assert media_folder_reset["reset"]["message"] == {"channel":"GET_MEDIA_FOLDER_PATH","data":str(data_dir / "Media")}, media_folder_reset
            assert not configured_media_folder.exists()
            assert (data_dir / "Media").is_dir()
            assert json.loads(config_path.read_text())["mediaFolderPath"] == ""
            (ARTIFACTS / "media-folder-reset.json").write_text(json.dumps(media_folder_reset, indent=2))
            steps.append("Retained window.api media-folder set/query/reset observes ordered committed settings and lazy directories")
            click_text("Rust parity test")
            wait(lambda: script("return document.body.innerText.includes('Amazing grace') && document.body.innerText.includes('日本語')"), "Saved show loaded and rendered")
            api_port, api_password = free_api_port(), os.urandom(24).hex()
            api_receipts = {}

            def probe_api(phase, expected_text):
                context = script("return window.api.getRuntimeContext()")
                payload = {"port":api_port,"password":api_password,"showId":saved[0],"expectedText":expected_text}
                result = subprocess.run(["node", "tests/desktop/api_client.mjs"], input=json.dumps(payload), text=True, capture_output=True, cwd=ROOT, timeout=20)
                assert result.returncode == 0, (result.stdout, result.stderr)
                reply = json.loads(result.stdout)
                assert len(reply["replies"]) == 4 and reply["unauthorizedStatus"] == 401
                assert context == script("return window.api.getRuntimeContext()")
                api_receipts[phase] = {"context":context,"binding":invoke("api_status")["binding"],"result":reply}
                (ARTIFACTS / "api.json").write_text(json.dumps(api_receipts, indent=2))
                return reply

            script("window.api.send('MAIN',{channel:'WEBSOCKET_START',data:{port:arguments[0],password:arguments[1]}})", api_port, api_password)
            initial_api = wait(lambda: invoke("api_status")["binding"], "Retained window.api starts native REST, Socket.IO and OSC listeners")
            probe_api("initial", "Amazing grace")
            steps.append("Real REST on both ports and Socket.IO polling/WebSocket reach retained show and dynamic-value handlers")

            remote_password, stage_password = os.urandom(20).hex(), os.urandom(20).hex()
            remote_receipts = {}
            configured_remotes = script("""
                return (async()=>{
                    const context=window.api.getRuntimeContext();
                    const write=(file,key,value)=>window.__TAURI__.core.invoke('ipc_dispatch',{request:{channel:'MAIN',data:{channel:'SET_STORE_VALUE',data:{file,key,value}},context}});
                    await write('SETTINGS','ports',{remote:'0',stage:'0',controller:'0',companion:arguments[0]});
                    await write('SETTINGS','disabledServers',{remote:false,stage:false,controller:false,output_stream:true});
                    await write('SETTINGS','serverData',{remote:{password:arguments[1]},companion:{enabled:false,password:arguments[2]}});
                    await write('STAGE','native-stage-test',{name:'Rust native Stage 敬拜',password:arguments[3],settings:{color:'#000000'},items:{}});
                    const read=command=>new Promise((resolve,reject)=>{
                        const id='native-remote-settings-'+command,timer=setTimeout(()=>reject(new Error(command+' deadline')),4000);
                        window.api.receive('MAIN',(message,listenerId)=>{if(listenerId!==id)return;clearTimeout(timer);window.api.removeListener('MAIN',id);resolve(message.data);},id);
                        window.api.send('MAIN',{channel:command},id);
                    });
                    await read('STAGE');await read('SETTINGS');return true;
                })();
            """, api_port, remote_password, api_password, stage_password)
            assert configured_remotes is True
            initial_remote = wait(lambda: (lambda value: value if value and len(value['bound']['endpoints']) == 3 else None)(invoke('remotes_status')['binding']), 'Retained settings start all three bounded native remote listeners')

            def probe_remotes(phase, expected_text, pages=False):
                context = script('return window.api.getRuntimeContext()')
                binding = wait(lambda: invoke('remotes_status')['binding'], 'Current remote binding is available')
                assert binding['context'] == context, binding
                endpoints = binding['bound']['endpoints']
                payload = {'endpoints':endpoints,'password':remote_password,'stagePassword':stage_password,'showId':saved[0],'expectedText':expected_text}
                result = subprocess.run(['node','tests/desktop/remotes_client.mjs'],input=json.dumps(payload),text=True,capture_output=True,cwd=ROOT,timeout=30)
                assert result.returncode == 0,(result.stdout,result.stderr)
                receipt = {'context':context,'bound':binding['bound'],'clients':json.loads(result.stdout),'pages':[]}
                assert len(receipt['clients']['receipts']) == 6
                if pages:
                    for kind in ['REMOTE','STAGE','CONTROLLER']:
                        url = 'http://' + endpoints[kind] + '/'
                        if kind == 'STAGE': url += '?id=native-stage-test&password=' + stage_password
                        page_fixture = {'kind':kind,'url':url,'password':remote_password}
                        page = subprocess.run(['/usr/bin/python3','tests/desktop/remotes_pages.py'],input=json.dumps(page_fixture),text=True,capture_output=True,cwd=ROOT,env=env,timeout=25)
                        assert page.returncode == 0,(kind,page.stdout,page.stderr)
                        page_receipt=json.loads(page.stdout)
                        assert page_receipt['resources'] and page_receipt['authenticated'] and page_receipt['errors'] == [], page_receipt
                        for asset in ['client.js','styles.css']:
                            assert any(resource['name'].endswith('/'+asset) and resource['bytes'] > 0 for resource in page_receipt['resources']),page_receipt
                        receipt['pages'].append(page_receipt)
                assert context == script('return window.api.getRuntimeContext()')
                wait(lambda: invoke('remotes_status')['pendingDeliveries'] == 0 and invoke('remotes_status')['binding']['connections'] == 0, 'Real remote clients disconnect through retained cleanup')
                def current_discovery():
                    status = invoke("remotes_status")
                    discovery = status.get("discovery")
                    if not discovery or not status.get("binding") or discovery["acceptedEvents"] < 3:
                        return None
                    bound = status["binding"]
                    advertised = discovery["owner"]["binding"]
                    if not advertised or advertised["actualPorts"] != endpoints:
                        return None
                    assert advertised["context"] == bound["context"] == context
                    assert advertised["listenerRun"] == bound["bound"]["generation"]["run"]
                    assert advertised["generation"] == bound["discoveryRun"]["generation"]
                    assert discovery["warnings"] == 0, discovery
                    return discovery
                advertised = wait(current_discovery, "Native discovery publishes only the three actual current remote listener ports")
                proof = {"snapshot":expected_network["snapshot"],"endpoints":endpoints,"packets":[]}
                original_result = subprocess.run(["node","tests/desktop/discovery_receipts.mjs",str(discovery_peer.fileno())],input=json.dumps(proof),text=True,capture_output=True,cwd=ROOT,timeout=15,pass_fds=(discovery_peer.fileno(),))
                assert original_result.returncode == 0, (original_result.stdout,original_result.stderr,advertised)
                original_records = json.loads(original_result.stdout)
                receipt["discovery"] = {"status":advertised,"source":original_records}
                remote_receipts[phase]=receipt
                (ARTIFACTS/'remotes.json').write_text(json.dumps(remote_receipts,indent=2))
                return receipt

            probe_remotes('initial','Amazing grace',pages=True)
            steps.append('Real remote and stage passwords, show queries and controller replies reach retained handlers over polling and WebSockets')
            steps.append('Unchanged remote, stage and controller client pages load and authenticate in separate ephemeral WebKit views')

            # Import raw native/text formats through the retained Svelte
            # converters, then persist with the real keyboard shortcut.
            import_dir = temp / "import-fixtures"
            import_dir.mkdir()
            text_import = import_dir / "Native text 敬拜.txt"
            text_import.write_text("[Verse]\n[C]Imported lyric 敬拜 🙌\nSecond line\n", encoding="utf-8")
            chord_import = import_dir / "Native chords.cho"
            chord_import.write_text("{title: Native ChordPro}\n{artist: Native Artist}\n[C]Chord lyric 敬拜 [G]world\n", encoding="utf-8")
            csv_import = import_dir / "Native CSV.csv"
            csv_import.write_text('"Native CSV lyric",敬拜\n"Second verse",world\n', encoding="utf-8")
            native_import = json.loads(json.dumps(saved))
            native_import[0] = "native-import-show"
            native_import[1]["name"] = "Native show import"
            native_import[1]["nativeFutureField"] = {"preserved":[42,"敬拜 🙌"]}
            native_import_path = import_dir / "native.show"
            native_import_path.write_text(json.dumps(native_import, ensure_ascii=False))
            bundle_show = json.loads(json.dumps(native_import[1]))
            bundle_show["name"] = "Native bundle show"
            bundle_show["nativeMediaPath"] = "/missing/original/imported-image.png"
            bundle_data = {"project":{"id":"native-import-project","name":"Native Import Project","shows":[{"id":"native-bundle-show","type":"show","name":"Native bundle show"}],"created":1},
                           "shows":{"native-bundle-show":bundle_show},"files":[bundle_show["nativeMediaPath"]],"media":{bundle_show["nativeMediaPath"]:{"volume":0.5}},"nativeFutureBundle":{"preserved":True}}
            media_bytes = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            project_import = import_dir / "native.project"
            with zipfile.ZipFile(project_import, "w", compression=zipfile.ZIP_DEFLATED) as archive:
                archive.writestr("data.json", json.dumps(bundle_data, ensure_ascii=False))
                archive.writestr("imported-image.png", media_bytes)
            import_fixture = {"requests":[{"id":kind,"paths":[str(path)]} for kind,path in [
                ("txt",text_import),("chordpro",chord_import),("csv",csv_import),("freeshow",native_import_path),("freeshow_project",project_import)]],
                "shows":[{"name":"Native text 敬拜","text":"Imported lyric 敬拜 🙌"},{"name":"Native ChordPro","text":"Chord lyric 敬拜"},{"name":"Native CSV","text":"Native CSV lyric"},
                         {"name":"Native show import","text":"Amazing grace","unknownKey":"nativeFutureField","unknownValue":native_import[1]["nativeFutureField"]},
                         {"name":"Native bundle show","text":"Amazing grace","unknownKey":"nativeFutureField","unknownValue":native_import[1]["nativeFutureField"]}]}
            text_imports = json.loads(subprocess.check_output([os.environ.get("FREESHOW_TEST_NODE", "node"),"tests/desktop/imports_text_fixtures.mjs","--fixtures",str(import_dir / "ordinary-text")],cwd=ROOT,text=True,timeout=20))
            assert len(text_imports["requests"]) == 9 and len(text_imports["shows"]) == 10
            assert [item["id"] for item in text_imports["requests"]] == ["openlp","opensong","mediashout","quelea","songbeamer","easyslides","verseview","videopsalm","chordpro"]
            # Preserve the original project alert/converter as the final import.
            import_fixture["requests"][-1:-1] = text_imports["requests"]
            import_fixture["shows"].extend(text_imports["shows"])
            script((ROOT / "tests/desktop/imports_probe.js").read_text(), import_fixture)
            wait(lambda: script("return window.__importsProbe.error || window.__importsProbe.readyToSave"), "Native import transport reaches retained text, ChordPro, CSV, show and project converters")
            assert not script("return window.__importsProbe.error"), script("return window.__importsProbe")
            if script("return !!document.querySelector('.popup button')"):
                click(".popup button")
                wait(lambda: script("return !document.querySelector('.popup')"), "Imported project alert dismissed")
            save_shortcut()
            script("window.__importsProbe.saveRequested=true")
            imported = wait(lambda: script("return window.__importsProbe.done ? window.__importsProbe : null"), "Converted native imports saved through real Ctrl+S")
            assert "error" not in imported, imported
            assert len(imported["saved"]) == len(import_fixture["shows"]) == 15
            assert len({item["id"] for item in imported["saved"]}) == 15, "Distinct source IDs must not overwrite imported shows"
            assert all(item["sendUndefined"] for item in imported["requests"])
            assert sum(item["rawMatchesSource"] is True for item in imported["requests"]) == 9
            assert any(event["message"] == {"channel":"ALERT","data":"popup.importing"} for event in imported["events"]), "Original project alert was lost"
            persisted_imports = ARTIFACTS / "imported-shows"
            persisted_imports.mkdir()
            imported["persistedFiles"] = []
            saved_by_name = {item["name"]: item for item in imported["saved"]}
            expected_filenames = {expected["name"] + ".show" for expected in import_fixture["shows"]}
            assert len(expected_filenames) == len(saved_by_name) == 15, "Duplicate or missing persisted import names"
            for expected in import_fixture["shows"]:
                filename = expected["name"] + ".show"
                assert Path(filename).name == filename, "Fixture archive must contain only direct show files"
                actual_bytes = (data_dir / "Shows" / filename).read_bytes()
                actual = json.loads(actual_bytes)
                assert actual[1]["name"] == expected["name"]
                assert actual[0] == saved_by_name[expected["name"]]["id"], "Persisted import ID differs from native SHOW read"
                text = "\n".join(span.get("value", "") for slide in actual[1].get("slides", {}).values() for item in slide.get("items", []) for line in item.get("lines", []) for span in line.get("text", []))
                assert expected["text"] in text, expected["name"] + " persisted lyric differs"
                archived = persisted_imports / filename
                archived.write_bytes(actual_bytes)
                assert archived.read_bytes() == actual_bytes, "Archived persisted import bytes differ"
                imported["persistedFiles"].append({"path":archived.relative_to(ARTIFACTS).as_posix(),"name":expected["name"],"id":actual[0],"bytes":len(actual_bytes),"sha256":hashlib.sha256(actual_bytes).hexdigest()})
            assert {entry.name for entry in persisted_imports.iterdir()} == expected_filenames, "Duplicate or missing archived import files"
            assert len({entry["path"] for entry in imported["persistedFiles"]}) == len(imported["persistedFiles"]) == 15
            raw_bundle = json.loads(next(item for item in imported["requests"] if item["id"] == "freeshow_project")["raw"][0]["content"])
            installed_media = Path(raw_bundle["files"][0])
            assert installed_media.is_relative_to(data_dir / "Imports" / "Projects"), installed_media
            assert hashlib.sha256(installed_media.read_bytes()).hexdigest() == hashlib.sha256(media_bytes).hexdigest()
            saved_bundle = json.loads((data_dir / "Shows" / "Native bundle show.show").read_text())
            assert saved_bundle[1]["nativeMediaPath"] == str(installed_media)
            assert raw_bundle["nativeFutureBundle"] == {"preserved":True}
            imported["media"] = {"path":str(installed_media),"sha256":hashlib.sha256(media_bytes).hexdigest(),"bytes":len(media_bytes)}
            imported["ordinaryTextSource"] = text_imports
            (ARTIFACTS / "imports.json").write_text(json.dumps(imported, ensure_ascii=False, indent=2))
            steps.append("Retained Svelte ordinary text, eight source text formats, standalone ChordPro PRO, CSV, native-show and project converters persist Unicode lyrics and unknown fields")
            steps.append("Native project ZIP installs identical media bytes before publishing remapped paths and saves those references")
            asset_audio = import_dir / "Native asset audio.wav"
            with wave.open(str(asset_audio), "wb") as audio:
                audio.setnchannels(1)
                audio.setsampwidth(2)
                audio.setframerate(8000)
                audio.writeframes(bytes(16000))
            asset_video = import_dir / "Native asset video.webm"
            shutil.copyfile(ROOT / "crates/freeshow-assets/tools/fixtures/blue.webm", asset_video)
            asset_show = json.loads(json.dumps(saved[1]))
            asset_show["name"] = "Native asset delivery"
            asset_show["media"] = {str(index): {"path":str(path)} for index,path in enumerate([installed_media,asset_audio,asset_video,*[entry["path"] for entry in now_playing_fixtures["cases"]]])}
            (data_dir / "Shows" / "Native asset delivery.show").write_text(json.dumps(["native-assets-show",asset_show]))
            script((ROOT / "tests/desktop/assets_probe.js").read_text(), {"id":"native-assets-show","name":asset_show["name"],"image":str(installed_media),"audio":str(asset_audio),"video":str(asset_video),"importUrlAtCallback":imported.get("importUrlAtCallback")})
            assets = wait(lambda: script("return window.__assetsProbe.done ? window.__assetsProbe : null"), "Native grants reach retained callbacks and actual WebKit image/audio/video decoders", seconds=40)
            assert "error" not in assets, assets
            assert assets["origin"] == "tauri://localhost"
            assert bytes(assets["pngBytes"]) == media_bytes
            assert assets["head"] == {"status":200,"length":len(media_bytes),"bodyLength":0}
            assert assets["range"] == {"status":206,"contentRange":f"bytes 0-7/{len(media_bytes)}","bytes":list(media_bytes[:8])}
            assert assets["image"] == {"width":1,"height":1,"rgba":[0,0,0,255],"exported":True}
            assert assets["corsRequired"] is True
            assert len(assets["media"]) == 2
            for media in assets["media"]:
                assert abs(media["duration"]-1) < 0.02 and media["playedTime"] > 0.55 and media["playing"], media
                if media["kind"] == "video":
                    assert [media["width"],media["height"]] == [16,16]
                    assert media["rgba"][0] < 10 and media["rgba"][1] < 10 and media["rgba"][2] > 240 and media["rgba"][3] == 255
            (ARTIFACTS / "assets.json").write_text(json.dumps(assets, ensure_ascii=False, indent=2))
            steps.append("Native HTTP GET, HEAD and Range preserve imported PNG bytes and CORS canvas pixels")
            steps.append("Native media capabilities support real WebKit WAV/VP8 seeking and playback")
            now_playing_receipts = []
            cover_http_receipts = []
            def probe_selected_cover(phase, expected_path=None, blocker=False):
                context = invoke("runtime_context")
                receipt = probe_cover_http(script, invoke, ROOT, phase, context, expected_path, blocker=blocker)
                cover_http_receipts.append(receipt)
                (ARTIFACTS / "remote-covers.json").write_text(json.dumps(cover_http_receipts,ensure_ascii=False,indent=2))
                steps.append("Native remote cover " + phase + " preserves selected-library HTTP responses on all three ports with original alerts and zero retained owners")
                return receipt
            def now_playing_values(raw):
                # Keep every typed value and nested key order. The separate
                # skipCovers/native read cannot certify root picture position.
                assert raw[0] == "object", raw
                return {"ordinary":[row for row in raw[1] if row[0] != "picture"], "picture":[row for row in raw[1] if row[0] == "picture"]}
            for entry in now_playing_fixtures["cases"]:
                observed = script((ROOT / "tests/desktop/now_playing_probe.js").read_text(), {"phase":"initial-" + entry["id"], "action":"set", "case":entry})
                replay_file = temp / "now-playing-script.json"
                replay_file.write_text(json.dumps(observed["script"],ensure_ascii=False))
                oracle = jpeg_oracle if entry["imageCapability"] == "small-jpeg" else "tests/desktop/now_playing_desktop_oracle.mjs"
                expected = json.loads(subprocess.check_output([os.environ.get("FREESHOW_TEST_NODE", "node"), oracle, "--replay", str(replay_file)], cwd=ROOT, text=True, timeout=15))
                assert observed["context"] == observed["current"] == invoke("runtime_context"), observed
                assert observed["sourceAcks"] == observed["browserErrors"] == [] and observed["ordinaryIdleVerified"], observed
                assert len(observed["events"]) == 1, observed
                assert now_playing_values(observed["events"][0]["raw"]) == now_playing_values(expected["metadataRaw"]), (observed,expected)
                audio_folder = Path(observed["script"]["audioFolder"])
                text_path, cover_path = audio_folder / "NowPlaying.txt", audio_folder / "NowPlayingCover.png"
                files = {"textExists":text_path.exists(), "coverExists":cover_path.exists()}
                if expected["text"] is not None:
                    files["text"] = text_path.read_text(encoding="utf-8")
                    assert files["text"] == expected["text"], (files,expected)
                if expected["coverExpectedAbsent"]:
                    assert not cover_path.exists(), (entry,files)
                elif entry["imageCapability"] == "small-png":
                    actual_png = cover_path.read_bytes()
                    assert actual_png[:8] == b'\x89PNG\r\n\x1a\n'
                    assert list(int.from_bytes(actual_png[index:index+4], "big") for index in [16,20]) == [1,1]
                    files["coverSha256"] = hashlib.sha256(actual_png).hexdigest()
                    files["coverDimensions"] = [1,1]
                if entry["imageCapability"] == "small-jpeg":
                    files["jpeg"] = json.loads(subprocess.check_output([os.environ.get("FREESHOW_TEST_NODE", "node"), jpeg_oracle, "--verify", str(replay_file)], cwd=ROOT, text=True, timeout=15))
                    assert files["jpeg"]["actualFilesystemRead"] and files["jpeg"]["exactSourceRgbaVerified"], files
                probe_selected_cover("set-" + entry["id"], cover_path if cover_path.exists() else None)
                cleared = script((ROOT / "tests/desktop/now_playing_probe.js").read_text(), {"phase":"unset-" + entry["id"], "action":"unset"})
                assert text_path.read_bytes() == b'' and not cover_path.exists(), (entry,cleared)
                assert cleared["sourceAcks"] == cleared["browserErrors"] == cleared["events"] == cleared["errors"] == [], cleared
                if entry["imageCapability"] == "small-jpeg":
                    replay_file.write_text(json.dumps(cleared["script"],ensure_ascii=False))
                    cleared["filesystem"] = json.loads(subprocess.check_output([os.environ.get("FREESHOW_TEST_NODE", "node"), jpeg_oracle, "--verify", str(replay_file)], cwd=ROOT, text=True, timeout=15))
                    assert cleared["filesystem"]["actualFilesystemRead"] and not cleared["filesystem"]["coverExists"] and cleared["filesystem"]["text"] == "", cleared
                probe_selected_cover("unset-" + entry["id"])
                now_playing_receipts.append({"native":observed,"source":expected,"files":files,"unset":cleared,"rootPicturePositionCompared":False,"encodedPngParityClaimed":False})
                (ARTIFACTS / "now-playing.json").write_text(json.dumps(now_playing_receipts,ensure_ascii=False,indent=2))
                steps.append("Retained now-playing " + entry["id"] + " preserves raw source metadata, send-time input, actual file effects and void UNSET with drained native work")
            folder_context = invoke("runtime_context")
            audio_folder = Path(folder_context["dataPath"]) / "Audio"
            assert audio_folder.is_dir() and not audio_folder.is_symlink(), audio_folder
            saved_audio_folder = temp / "saved-now-playing-audio"
            audio_folder.rename(saved_audio_folder)
            blocker = b"Native now-playing Audio folder blocker\x00\xff\n"
            folder_receipts = []
            try:
                audio_folder.write_bytes(blocker)
                remote_blocker = probe_selected_cover("blocked-audio-http", blocker=True)
                assert audio_folder.is_file() and not audio_folder.is_symlink() and audio_folder.read_bytes() == blocker
                remote_blocker["filesystem"] = {"regularBlockerBytesUnchanged":True,"blockerBytes":len(blocker),"blockerSha256":hashlib.sha256(blocker).hexdigest(),"verifiedBy":"independent Python filesystem read"}
                expected_remote_alert = remote_blocker["observer"]["expectedFolderMessage"]
                remote_visible = wait(lambda: script("const p=document.querySelector('.popup p');return p?.textContent?.trim()===arguments[0] && document.querySelector('.popup button') ? p.textContent.trim() : null", expected_remote_alert), "Actual original HTTP folder alert text visible")
                assert remote_visible == expected_remote_alert
                remote_blocker["visibleAlertText"] = remote_visible
                (ARTIFACTS / "remote-covers.json").write_text(json.dumps(cover_http_receipts,ensure_ascii=False,indent=2))
                click('.popup button.outlined')
                wait(lambda: script("return !document.querySelector('.popup')"), "Remote cover folder alert dismissed")
                wav_entry = next(entry for entry in now_playing_fixtures["cases"] if entry["path"].lower().endswith(".wav"))
                for action, expected_counts in [("set", (1, 4, 0)), ("unset", (0, 2, 0)), ("open", (0, 1, 1))]:
                    observed = script((ROOT / "tests/desktop/now_playing_folder_probe.js").read_text(), {"phase":"blocked-audio-" + action, "action":action, "audioFolder":str(audio_folder), "case":wav_entry})
                    assert observed["context"] == observed["current"] == folder_context == invoke("runtime_context"), observed
                    counts = observed["counts"]
                    assert (counts["metadata"], counts["folderAlerts"], counts["missingTextAlerts"]) == expected_counts, observed
                    assert counts["admitted"] == counts["completed"] == "1" and counts["revealCalls"] == "0", observed
                    assert counts["sourceAcks"] == counts["runtimeErrors"] == counts["browserErrors"] == 0, observed
                    assert observed["ordinaryIdleVerified"] and observed["actualCounterAdvanceVerified"] and observed["noNowPlayingRevealCallbackInvoked"], observed
                    assert audio_folder.is_file() and not audio_folder.is_symlink() and audio_folder.read_bytes() == blocker, observed
                    observed["filesystem"] = {"regularBlockerBytesUnchanged":True, "blockerBytes":len(blocker), "blockerSha256":hashlib.sha256(blocker).hexdigest(), "verifiedBy":"independent Python filesystem read"}
                    # The retained ALERT handler replaces one popup message;
                    # callbacks above independently observed every notification.
                    expected_visible = "This does not exist!" if action == "open" else observed["expectedFolderMessage"]
                    visible = wait(lambda: script("const p=document.querySelector('.popup p');return p?.textContent?.trim()===arguments[0] && document.querySelector('.popup button') ? p.textContent.trim() : null", expected_visible), "Actual original now-playing alert text visible")
                    assert visible == expected_visible, (visible, observed)
                    observed["visibleAlertText"] = visible
                    folder_receipts.append(observed)
                    (ARTIFACTS / "now-playing-folder-alerts.json").write_text(json.dumps(folder_receipts,ensure_ascii=False,indent=2))
                    steps.append("Retained now-playing " + action.upper() + " preserves exact ordered folder/missing-file alerts, visible original text, void returns and drained work without invoking native reveal")
                    click(".popup button")
                    wait(lambda: script("return !document.querySelector('.popup')"), "Now-playing folder alert dismissed")
            finally:
                assert audio_folder.is_file() and not audio_folder.is_symlink() and audio_folder.read_bytes() == blocker, "Native code changed the private Audio blocker"
                audio_folder.unlink()
                saved_audio_folder.rename(audio_folder)
            read_text = import_dir / "日本語 literal%20.txt"
            read_text.write_bytes(b"\xef\xbb\xbfNative\x00\n\xf0\x9f\x80 invalid\xff")
            read_image = import_dir / "日本語 literal%20.jpg"
            shutil.copyfile(ROOT / "crates/freeshow-exif/fixtures/full-II.jpg", read_image)
            def probe_file_reads():
                context = invoke("runtime_context")
                bible_root = Path(context["dataPath"]) / "Bibles"
                bible_root.mkdir(exist_ok=True)
                for name in ["Native duplicate.FSB", "Native duplicate.fsb"]:
                    (bible_root / name).write_text("Not parsed during folder discovery")
                (bible_root / "Native directory.fsb").mkdir(exist_ok=True)
                fixture = {"text":str(read_text),"image":str(read_image),"missing":str(import_dir / "missing-exif.jpg"),"dataRoot":context["dataPath"]}
                expected = json.loads(subprocess.run([os.environ.get("FREESHOW_TEST_NODE", "node"),str(ROOT / "tests/desktop/file_reads_oracle.mjs")],input=json.dumps(fixture),text=True,capture_output=True,check=True,timeout=10).stdout)
                observed = script((ROOT / "tests/desktop/file_reads_probe.js").read_text(),fixture)["result"]
                (ARTIFACTS / "file-reads.json").write_text(json.dumps({"source":expected,"native":observed},ensure_ascii=False,indent=2))
                assert "error" not in observed, observed
                assert observed["context"] == context and observed["current"] == context, observed
                for key in ["file","image","missing","bibles"]:
                    assert observed[key] == expected[key], (key,observed,expected)
                assert observed["status"]["requests"] == 0 and observed["elapsedMs"] < 2500, observed
                return observed
            probe_file_reads()
            steps.append("Native document, Bible-folder and EXIF callbacks match pinned source values, byte types, IEEE specials and own undefined properties")
            scripture_id = "native-scripture-fixture"
            scripture_name = "Native Bible 敬拜"
            scripture_value = ["stored-id", {"name":scripture_name,"metadata":{"title":"Native Bible","unknown":{"retained":[True,None]}},"future":{"preserve":"e\u0301 🙌"},"books":[
                {"number":1,"name":"Genesis","chapters":[
                    {"number":1,"verses":[{"number":1,"value":"Native scripture first 敬拜","unknown":True},{"number":2,"text":"Native scripture second verse","value":"retained legacy field"}]},
                    {"number":2,"verses":[{"number":1,"value":"Native scripture next chapter"}]}]},
                {"number":2,"name":"Exodus","chapters":[{"number":1,"verses":[{"number":1,"value":"Native scripture next book"}]}]}
            ]}]
            scripture_root = Path(invoke("runtime_context")["dataPath"]) / "Bibles"
            (scripture_root / (scripture_name + ".fsb")).write_text(json.dumps(scripture_value,ensure_ascii=False))
            (documents_root / "Bibles/Legacy native.fsb").write_text(json.dumps(["old",{"unknown":[False,None],"books":[]}]))
            scripture_payloads = [{"name":scripture_name,"id":scripture_id},{"name":"Legacy native","id":"legacy-request"},{"name":"Missing native","id":{"keep":[False,None]}}]
            scripture_receipts = {}
            def probe_scriptures(phase):
                context = invoke("runtime_context")
                scripture_fixture = {"dataRoot":context["dataPath"],"documentsRoot":str(documents_root),"payloads":scripture_payloads}
                scripture_source = json.loads(subprocess.run([os.environ.get("FREESHOW_TEST_NODE", "node"),str(ROOT / "tests/desktop/scriptures_oracle.mjs")],input=json.dumps(scripture_fixture),text=True,capture_output=True,check=True,timeout=10).stdout)
                scripture_observed = script((ROOT / "tests/desktop/scriptures_probe.js").read_text(),scripture_payloads)
                assert scripture_observed["context"] == scripture_observed["current"] == context, scripture_observed
                assert [reply["data"] for reply in scripture_observed["replies"]] == scripture_source["replies"], (scripture_observed,scripture_source)
                assert all(reply["listenerId"] == reply["expectedListenerId"] for reply in scripture_observed["replies"]), scripture_observed
                assert scripture_observed["status"]["requests"] == 0 and scripture_source["alerts"] == [], (scripture_observed,scripture_source)
                scripture_receipts[phase] = {"source":scripture_source,"native":scripture_observed}
                (ARTIFACTS / "scriptures.json").write_text(json.dumps(scripture_receipts,ensure_ascii=False,indent=2))
                return scripture_observed
            probe_scriptures("initial")
            steps.append("Native BIBLE callbacks preserve complete pinned-source primary, legacy, migration, unknown fields and missing values through retained window.api")
            copy_context = invoke("runtime_context")
            copy_originals = temp / "Media copy originals 敬拜"
            copy_originals.mkdir()
            copy_paths = [copy_originals / "opaque 日本語.bin", copy_originals / "other Ελληνικά.bin"]
            copy_paths[0].write_bytes(bytes([0,255,128,3,0]))
            copy_paths[1].write_bytes(b"source copy content\r\n")
            os.chmod(copy_paths[0], 0o640)
            os.chmod(copy_paths[1], 0o751)
            copy_native_root = temp / "Native media copied 敬拜"
            copy_source_root = temp / "Original media copied 敬拜"
            copy_oracle_data = temp / "media-copy-oracle-data"
            copy_oracle_data.mkdir()
            copy_receipts = {}
            def media_tree(root):
                return {str(file.relative_to(root)): {"sha256":hashlib.sha256(file.read_bytes()).hexdigest(), "mode":file.stat().st_mode & 0o7777} for file in root.rglob("*") if file.is_file()}
            def copy_source(command, payload, target, stores=None):
                fixture = {"command":command, "dataRoot":str(copy_oracle_data), "syncRoot":str(target), "stores":stores or {"projects":{"projects":{}},"overlays":{},"templates":{}}, "data":payload}
                return json.loads(subprocess.run([os.environ.get("FREESHOW_TEST_NODE", "node"), str(ROOT / "tests/desktop/media_copy_oracle.mjs")], input=json.dumps(fixture), text=True,capture_output=True,check=True,timeout=20).stdout)
            copy_payload = {"paths":[str(copy_paths[0]),str(copy_paths[0]),str(copy_paths[1]),str(copy_originals / "missing.bin"),"https://invalid.local/literal"]}
            for phase in ["first","existing"]:
                expected = copy_source("MEDIA_FOLDER_COPY", copy_payload, copy_source_root)
                observed = script((ROOT / "tests/desktop/media_copy_probe.js").read_text(), {"command":"MEDIA_FOLDER_COPY","data":copy_payload,"setting":str(copy_native_root)})
                assert observed["context"] == observed["current"] == copy_context, observed
                assert observed["reply"] == expected["reply"] == (phase == "first"), (observed,expected)
                assert observed["listenerId"] == observed["expectedListenerId"] and observed["status"]["requests"] == 0, observed
                assert observed["alerts"] == expected["alerts"] == [] and expected["errors"] == 0, (observed,expected)
                assert media_tree(copy_native_root) == media_tree(copy_source_root) and len(media_tree(copy_native_root)) == 2
                copy_receipts[phase] = {"source":expected,"native":observed,"files":media_tree(copy_native_root)}
                # Existing destinations preserve their original bytes even when
                # the original file changes before the repeated source call.
                copy_paths[0].write_bytes(b"changed original, retained destination")
            steps.append("Retained media-copy callbacks match actual source true/false, correlation, opaque bytes, Unicode, permissions and existing-file preservation")
            bundle_show = Path(copy_context["dataPath"]) / "Shows/Native media copy.show"
            bundle_show.write_text(json.dumps(["native-copy",{"media":{"opaque":{"path":str(copy_paths[0])},"second":{"path":str(copy_paths[1])}},"slides":{},"unknown":{"retained":True}}],ensure_ascii=False))
            shutil.copytree(Path(copy_context["dataPath"]) / "Shows", copy_oracle_data / "Shows")
            bundle_stores = {}
            for file, key in [("PROJECTS","projects"),("OVERLAYS","overlays"),("TEMPLATES","templates")]:
                values = invoke("ipc_dispatch", {"request":{"channel":"MAIN","context":copy_context,"data":{"channel":file}}})
                bundle_stores[key] = next(event["message"]["data"] for event in values if event["message"]["channel"] == file)
            bundle_native = temp / "Native bundle 敬拜"
            bundle_source = temp / "Original bundle 敬拜"
            expected_bundle = copy_source("BUNDLE_MEDIA_FILES", {"outputPath":str(bundle_source)}, bundle_source, bundle_stores)
            assert expected_bundle["reply"] is None and expected_bundle["alerts"] == [] and expected_bundle["errors"] == 0, expected_bundle
            expected_bundle_files = media_tree(bundle_source)
            assert len(expected_bundle_files) >= 2, expected_bundle
            bundle_pending = script((ROOT / "tests/desktop/media_copy_probe.js").read_text(), {"command":"BUNDLE_MEDIA_FILES","data":{"outputPath":str(bundle_native)},"setting":str(bundle_native)})
            wait(lambda: media_tree(bundle_native) == expected_bundle_files and invoke("media_copy_status")["requests"] == 0, "Native bundle installs the complete pinned-source file tree", seconds=20)
            observed_bundle = script((ROOT / "tests/desktop/media_copy_probe.js").read_text(), {"action":"collect","id":bundle_pending["id"]})
            assert observed_bundle["context"] == observed_bundle["current"] == copy_context, observed_bundle
            assert observed_bundle["replies"] == [] and observed_bundle["alerts"] == [] and observed_bundle["status"]["requests"] == 0, observed_bundle
            copy_receipts["bundle"] = {"source":expected_bundle,"native":observed_bundle,"files":media_tree(bundle_native)}
            (ARTIFACTS / "media-copy.json").write_text(json.dumps(copy_receipts,ensure_ascii=False,indent=2))
            bundle_show.unlink()
            invoke("ipc_dispatch", {"request":{"channel":"MAIN","context":copy_context,"data":{"channel":"SET_MEDIA_FOLDER_PATH","data":""}}})
            steps.append("Retained BUNDLE installs exact original show/project/overlay/template media bytes without inventing a success callback")
            encoder_cache = temp / "app" / "encoder-detection.json"
            download_receipts = {}
            def probe_download(phase):
                observed = script((ROOT / "tests/desktop/ffmpeg_download_probe.js").read_text())
                assert observed["context"] == observed["current"] == invoke("runtime_context"), observed
                assert observed["reply"]["listenerId"] == observed["reply"]["expectedListenerId"], observed
                assert observed["reply"]["message"]["data"] == {"success":True}, observed
                assert observed["progress"][-1] == {"url":"ffmpeg","name":"FFmpeg","progress":100,"total":100,"status":"complete"}, observed
                assert len(observed["progress"]) <= 257 and all(item["status"] in ["downloading","complete"] for item in observed["progress"]), observed
                assert observed["status"]["admittedRequests"] == observed["status"]["readyUncollected"] == 0, observed
                installed = temp / "app/bin/ffmpeg"
                assert installed.read_bytes() == inert_download and installed.stat().st_mode & 0o777 == 0o755
                assert {path.name for path in installed.parent.iterdir()} == {"ffmpeg","nemotron"}, list(installed.parent.iterdir())
                for relative, content in ai_fixture_files.items():
                    assert (ai_engine / relative).read_bytes() == content, "Download changed the preexisting AI fixture"
                assert not (Path(observed["context"]["dataPath"]) / "bin/ffmpeg").exists()
                assert download_requests == ["/inert-ffmpeg.gz"] * (len(download_receipts) + 1), download_requests
                download_receipts[phase] = {"native":observed,"sha256":hashlib.sha256(installed.read_bytes()).hexdigest(),"mode":installed.stat().st_mode & 0o777,"requests":list(download_requests),"inert":True}
                (ARTIFACTS / "ffmpeg-download.json").write_text(json.dumps(download_receipts,ensure_ascii=False,indent=2))
            probe_download("initial")
            steps.append("Retained FFMPEG_DOWNLOAD installs exact inert loopback bytes/mode in private userData, reports original completion and drains staging")
            network_receipts = {}
            def probe_network_queries(phase):
                observed = script((ROOT / "tests/desktop/network_queries_probe.js").read_text())
                assert observed["context"] == observed["current"] == invoke("runtime_context"), observed
                assert len(observed["replies"]) == 7 and observed["status"]["requests"] == 0, observed
                assert observed["status"]["snapshots"]["nativeCaptures"] >= observed["before"]["snapshots"]["nativeCaptures"] + 7, observed
                for reply in observed["replies"]:
                    assert reply["listenerId"] == reply["expectedListenerId"], reply
                    assert reply["message"]["data"] == expected_network["ips"], (reply,expected_network)
                network_receipts[phase] = {"source":expected_network,"native":observed}
                (ARTIFACTS / "network-queries.json").write_text(json.dumps(network_receipts,ensure_ascii=False,indent=2))
                return observed
            probe_network_queries("normal")
            steps.append("Retained MAIN.IP callbacks match actual pinned-source OS addresses and perform fresh native captures while ignoring spoofed payloads")
            midi_receipts = {}
            def probe_midi(phase):
                observed = script((ROOT / "tests/desktop/midi_probe.js").read_text())
                assert observed["context"] == observed["current"] == invoke("runtime_context"), observed
                assert observed["closed"] == [] and observed["hardwareDeliveryVerified"] is False, observed
                assert observed["status"]["admittedRequests"] == observed["status"]["pendingDeliveries"] == 0, observed
                assert len(observed["results"]) == 2, observed
                for result in observed["results"]:
                    if "unavailable" in result:
                        assert result["unavailable"]["code"] == "midi_backend_unavailable", result
                    else:
                        assert result["listenerId"] == result["expectedListenerId"], result
                        assert result["message"]["channel"] == result["command"], result
                        assert all(isinstance(device.get("name"),str) and list(device) == ["name"] for device in result["message"]["data"]), result
                midi_receipts[phase] = observed
                (ARTIFACTS / "midi.json").write_text(json.dumps(midi_receipts,ensure_ascii=False,indent=2))
                return observed
            probe_midi("normal")
            steps.append("Retained MIDI getters preserve actual native names or explicit backend unavailability, while CLOSE remains void and host leases drain")
            audio_phases, audio_receipts = [], {}
            def probe_audio_stream(phase):
                with AudioPeer(timeout=20) as peer:
                    observed = script((ROOT / "tests/desktop/audio_probe.js").read_text(), {"phase":phase,"seed":12345 + len(audio_phases) * 100,"config":peer.config})
                    wire = peer.receipt(timeout=10)
                assert observed["context"] == observed["current"] == invoke("runtime_context"), observed
                assert observed["sourceAcks"] == observed["errors"] == [], observed
                assert wire["eos"] is True, wire
                audio_phases.append({"script":observed["script"],"wire":wire,"capability":observed["capability"],"statuses":observed["statuses"]})
                source = subprocess.run([os.environ.get("FREESHOW_TEST_NODE", "node"),"tests/desktop/audio_stream_oracle.mjs"],input=json.dumps({"phases":audio_phases}),text=True,capture_output=True,cwd=ROOT,timeout=15)
                assert source.returncode == 0, source.stderr
                oracle = json.loads(source.stdout)
                assert len(oracle["phases"]) == len(audio_phases) and all(item["nativeCompared"] for item in oracle["phases"]), oracle
                assert oracle["globalEncodedPackets"] == 4 * len(audio_phases) and len(oracle["constructors"]) == 1, oracle
                audio_receipts[phase] = {"observed":observed,"wire":wire}
                (ARTIFACTS / "audio-stream.json").write_text(json.dumps({"phases":audio_receipts,"oracle":oracle},ensure_ascii=False,indent=2))
                return observed
            probe_audio_stream("normal")
            steps.append("Real transferred AUDIO port and raw PCM/CAPTURE preserve exact source Opus/Ogg bytes, own undefined statuses and void commands over a joined loopback stream")
            timecode_phases = []
            def probe_timecode(phase, fps):
                observed = script((ROOT / "tests/desktop/timecode_probe.js").read_text(), {"phase":phase,"fps":fps})
                assert observed["context"] == observed["current"] == invoke("runtime_context"), observed
                assert observed["sourceAcks"] == observed["errors"] == [], observed
                timecode_phases.append(observed)
                source = subprocess.run([os.environ.get("FREESHOW_TEST_NODE", "node"),"tests/desktop/timecode_desktop_oracle.mjs"],input=json.dumps({"phases":timecode_phases}),text=True,capture_output=True,cwd=ROOT,timeout=20)
                assert source.returncode == 0, source.stderr
                oracle = json.loads(source.stdout)
                assert len(oracle["phases"]) == len(timecode_phases) and all(item["nativeCompared"] and item["rawAudioBytesMatched"] for item in oracle["phases"]), oracle
                (ARTIFACTS / "timecode.json").write_text(json.dumps({"phases":timecode_phases,"oracle":oracle},ensure_ascii=False,indent=2))
                return observed
            probe_timecode("normal", 25)
            steps.append("Retained LTC START/STOP/reopen and actual typed audio/decode callbacks match original addon bytes, own envelopes and void commands")
            encoder_check = invoke("ipc_dispatch", {"request":{"channel":"MAIN","context":invoke("runtime_context"),"data":{"channel":"FFMPEG_CHECK"}}})[0]["message"]["data"]
            assert encoder_check["installed"] is True
            encoder_receipts = {}
            def probe_encoders(phase, force=False):
                cache_before = encoder_cache.read_text() if encoder_cache.exists() else None
                observed = script((ROOT / "tests/desktop/encoders_probe.js").read_text(), {"force":force})
                cache_after = encoder_cache.read_text()
                fixture = {"executable":shutil.which("ffmpeg"),"sourcePath":encoder_check["path"],"force":force,"actual":observed["reply"]["message"]["data"],"cacheAfter":cache_after}
                if cache_before is not None: fixture["cache"] = cache_before
                comparison = subprocess.run([os.environ.get("FREESHOW_TEST_NODE", "node"),str(ROOT / "tests/desktop/encoder_oracle.mjs")],input=json.dumps(fixture),text=True,capture_output=True,cwd=ROOT,timeout=40)
                assert comparison.returncode == 0, (phase,comparison.stdout,comparison.stderr,observed)
                source = json.loads(comparison.stdout)
                assert observed["status"]["admittedRequests"] == 0
                assert observed["status"]["binding"]["service"]["active_children"] == 0
                encoder_receipts[phase] = {"native":observed,"source":source,"cacheBytes":cache_after}
                (ARTIFACTS / "encoders.json").write_text(json.dumps(encoder_receipts,ensure_ascii=False,indent=2))
                return observed,source
            encoder_forced,source_forced = probe_encoders("forced", True)
            assert source_forced["cacheValuesMatchSource"] is True and source_forced["calls"]
            assert any(entry["id"] == "x264" and entry["available"] is True for entry in encoder_forced["reply"]["message"]["data"]["encoders"])
            encoder_hit,source_hit = probe_encoders("cache")
            assert not source_hit["calls"]
            assert encoder_hit["status"]["binding"]["service"]["children_started"] == encoder_forced["status"]["binding"]["service"]["children_started"]
            raw_cache = json.loads(encoder_cache.read_text())
            raw_cache["detection"] = {"encoders":[],"recommended":"native-future-cache","nativeFuture":{"unicode":"敬拜 🙌","raw":[42,None]}}
            encoder_cache.write_text(json.dumps(raw_cache,ensure_ascii=False,separators=(",",":")))
            encoder_raw,source_raw = probe_encoders("raw-cache")
            assert encoder_raw["reply"]["message"]["data"] == raw_cache["detection"] and not source_raw["calls"]
            encoder_reforced,source_reforced = probe_encoders("reforced", True)
            assert source_reforced["cacheValuesMatchSource"] is True and source_reforced["calls"]
            assert encoder_reforced["reply"]["message"]["data"] != raw_cache["detection"]
            steps.append("Retained native encoder detection matches actual original FFmpeg probes and source-compatible cache values")
            steps.append("Encoder cache hits preserve raw unknown fields without children, and force performs new supervised probes")
            thumbnail_folder = import_dir / "Native thumbnail folder"
            (thumbnail_folder / "pictures").mkdir(parents=True)
            thumbnail_image = thumbnail_folder / "pictures" / "日本語.png"
            thumbnail_image.write_bytes(media_bytes)
            script((ROOT / "tests/desktop/thumbnails_probe.js").read_text(), {"image":str(installed_media),"video":str(asset_video),"folder":str(thumbnail_folder),"missing":str(import_dir / "missing-thumbnail.png")})
            thumbnails = wait(lambda: script("return window.__thumbnailsProbe.done ? {result:window.__thumbnailsProbe} : null"), "Retained WebKit canvas completes native thumbnails and captured folder listings", seconds=70)["result"]
            (ARTIFACTS / "thumbnails.json").write_text(json.dumps(thumbnails, ensure_ascii=False, indent=2))
            assert "error" not in thumbnails, thumbnails
            assert thumbnails["image"]["data"]["nativeFutureField"] == "preserved"
            assert [thumbnails["image"]["width"],thumbnails["image"]["height"]] == [16,16]
            assert thumbnails["image"]["rgba"] == [0,0,0,255]
            assert [thumbnails["video"]["width"],thumbnails["video"]["height"]] == [8,8]
            assert thumbnails["video"]["rgba"][2] > 240 and thumbnails["video"]["rgba"][3] == 255
            assert thumbnails["hit"] == {"data":thumbnails["image"]["data"],"callbackUrls":thumbnails["image"]["callbackUrls"]}
            folder_entry = thumbnails["folder"]["data"][str(thumbnail_image)]
            outputs = [(thumbnails["image"]["data"]["output"],16,16),(thumbnails["video"]["data"]["output"],8,8),(folder_entry["thumbnailPath"],250,250)]
            for output,width,height in outputs:
                assert Path(output).parent == temp / "app" / "media-cache"
                observed = next(blob for blob in thumbnails["blobs"] if [blob["width"],blob["height"]] == [width,height])
                assert Path(output).read_bytes() == bytes(observed["bytes"])
            assert thumbnails["folder"]["callbackUrls"][folder_entry["thumbnailPath"]].startswith("http://127.0.0.1:")
            assert thumbnails["exists"]["data"]["exists"] is True
            assert thumbnails["exists"]["data"]["creationTime"] == int(thumbnails["exists"]["data"]["creationTime"])
            assert thumbnails["noCache"]["data"] == {"path":str(installed_media),"exists":True}
            assert thumbnails["missing"]["data"]["output"] == ""
            assert Path(thumbnails["cachePath"]["data"]) == temp / "app" / "media-cache"
            assert thumbnails["status"]["captures"] == 0 and thumbnails["status"]["requests"] == 0
            steps.append("Actual retained image/video canvas PNG bytes install unchanged before native media URL callbacks")
            steps.append("Folder capture preserves depth semantics, cache reuse, source existence replies and terminal request cleanup")
            click_text("Rust parity test", virtual_show=True)
            wait(lambda: script("return document.body.innerText.includes('Amazing grace')"), "Original show reselected after import checks")

            # Applying the retained default output template can add align/auto
            # defaults to loaded slide items before the imports Ctrl+S. Validate
            # only those source-defined additions, then use the current saved
            # bytes as the exact dialog/backup/restore baseline.
            current_saved = json.loads(show_path.read_text())
            expected_saved = json.loads(json.dumps(saved))
            for slide_id, slide in expected_saved[1]["slides"].items():
                current_items = current_saved[1]["slides"][slide_id]["items"]
                for index, item in enumerate(slide["items"]):
                    for field, default in [("align", ""), ("auto", False)]:
                        if field not in item and field in current_items[index]:
                            assert current_items[index][field] == default
                            item[field] = default
            assert current_saved == expected_saved, {"expected":expected_saved,"actual":current_saved}
            saved = current_saved
            (ARTIFACTS / "saved-before-dialog.show").write_text(json.dumps(saved, ensure_ascii=False, indent=2))

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

            click_text("Rust parity test", virtual_show=True)
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
            wait(lambda: script("return !!document.querySelector('main.background')"), "Output role bootstrap reached retained Svelte view")
            output_asset = script("return {url:window.api.resolveFilePath(arguments[0]),context:window.api.getRuntimeContext()}", str(installed_media))
            assert output_asset["url"] == assets["callbackUrls"][str(installed_media)], output_asset
            assert output_asset["context"] == imported["context"], output_asset
            assets["outputReplay"] = output_asset
            (ARTIFACTS / "assets.json").write_text(json.dumps(assets, ensure_ascii=False, indent=2))
            steps.append("New output WebView receives existing trusted media grants before its retained state handlers")
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
            invoke("ipc_dispatch", {"request":{"channel":"MAIN","context":invoke("runtime_context"),"data":{"channel":"SET_STORE_VALUE","data":{"file":"SYNCED_SETTINGS","key":"scriptures","value":{scripture_id:{"name":scripture_name}}}}}})
            script("window.api.send('MAIN',{channel:'SYNCED_SETTINGS'})")
            click('section.drawer button#scripture')
            wait(lambda: script("return document.body.innerText.includes(arguments[0])",scripture_name), "Native local Bible appears in the retained scripture drawer")
            click_text(scripture_name)
            wait(lambda: script("return document.querySelector('.scripture .verses')?.textContent.includes('Native scripture first 敬拜')"), "Retained Bible reader renders migrated verses from native BIBLE loading")
            click('.scripture .chapters span[id="2"]')
            wait(lambda: script("return document.querySelector('.scripture .verses')?.textContent.includes('Native scripture next chapter')"), "Retained Bible chapter navigation uses loaded native content")
            click('.scripture .books span[id="2"]')
            wait(lambda: script("return document.querySelector('.scripture .verses')?.textContent.includes('Native scripture next book')"), "Retained Bible book navigation uses loaded native content")
            # Use the original display button; this invokes original scripture
            # show construction and output routing, without injecting output data.
            click('section.drawer .main > .row.right button[data-title="Present"]')
            for handle, label in [(output_window, "Primary"), (secondary_window, "Secondary")]:
                command("POST", "/window", {"handle":handle})
                wait(lambda: visible_lyric("Native scripture next book"), label + " projection displays the retained Bible cue")
            (ARTIFACTS / "output-scripture.png").write_bytes(base64.b64decode(command("GET", "/screenshot")))
            command("POST", "/window", {"handle":main_window})
            click('section.drawer button#shows')
            click_text("Rust parity test", virtual_show=True)
            wait(lambda: script("return document.querySelectorAll('#showArea .grid .slide.context').length>=2"), "Original show controls restored after the Bible cue")
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
            old_cover_sentinel = Path(old_context["dataPath"]) / "Audio/NowPlayingCover.png"
            old_cover_bytes = b"Old library cover must never be served by the new run"
            old_cover_sentinel.write_bytes(old_cover_bytes)
            other = temp / "other-library"
            (other / "Shows").mkdir(parents=True)
            (other / "Config").mkdir()
            (other / "Bibles").mkdir()
            shutil.copyfile(scripture_root / (scripture_name + ".fsb"), other / "Bibles" / (scripture_name + ".fsb"))
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
            switched_api = invoke("api_status")["binding"]
            assert switched_api["context"] == script("return window.api.getRuntimeContext()")
            assert switched_api["endpoints"]["generation"]["run"] != initial_api["endpoints"]["generation"]["run"]
            probe_api("switched", "Second library lyric")
            steps.append("Native API automatically restarts for the selected library and returns its same-ID show")
            probe_remotes('switched','Second library lyric')
            switched_cover_context = invoke("runtime_context")
            probe_device_providers("switched")
            probe_ai_spotify("switched")
            assert switched_cover_context != old_context and old_cover_sentinel.read_bytes() == old_cover_bytes
            assert not (Path(switched_cover_context["dataPath"]) / "Audio/NowPlayingCover.png").exists()
            switched_cover = probe_selected_cover("switched-library")
            switched_cover["previousLibraryCoverUnchanged"] = {"path":str(old_cover_sentinel),"bytes":len(old_cover_bytes),"sha256":hashlib.sha256(old_cover_bytes).hexdigest(),"verifiedBy":"independent Python filesystem read"}
            (ARTIFACTS / "remote-covers.json").write_text(json.dumps(cover_http_receipts,ensure_ascii=False,indent=2))
            timecode_retired = invoke("timecode_status")
            assert timecode_retired["lastRetired"]["allPortOwnersJoined"] and timecode_retired["lastRetired"]["ltcJoined"], timecode_retired
            probe_timecode("switched", 29.97)
            steps.append("Library selection joins obsolete timecode owners and retains source global mode/framerate semantics for exact LTC callbacks")
            probe_audio_stream("switched")
            steps.append("Library selection retains the process Opus encoder while new-context AUDIO bytes match one continuous original source encoder")
            probe_download("switched")
            steps.append("FFmpeg download after library selection keeps native userData authority and ignores renderer URL/path overrides")
            assert remote_receipts['switched']['bound']['generation']['run'] != initial_remote['bound']['generation']['run']
            steps.append('Retained settings restart remotes only after library selection and return the new same-ID show')
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
                        function keyOrder(value,path=[]) {
                            if(Array.isArray(value))return value.flatMap((child,index)=>keyOrder(child,[...path,index]));
                            if(!value||typeof value!=='object')return [];
                            const keys=Object.keys(value);
                            return [[path,keys],...keys.flatMap(key=>keyOrder(value[key],[...path,key]))];
                        }
                        for(const command of ['FFMPEG_CHECK','ENCODER_DETECT','MEDIA_CODEC','MEDIA_TRACKS','READ_AUDIO_METADATA','LOCATE_MEDIA_FILE','GET_MEDIA_FOLDER_PATH','GET_OS','GET_DEVICE_NAME','CHECK_RAM_USAGE']) {
                            const data=command==='READ_AUDIO_METADATA'?{filePath:arguments[1]}:
                                command==='LOCATE_MEDIA_FILE'?{filePath:arguments[0],folders:[]}:
                                ['GET_MEDIA_FOLDER_PATH','GET_OS','GET_DEVICE_NAME','CHECK_RAM_USAGE'].includes(command)?null:{path:arguments[0],future:{kept:true}};
                            const events=await window.__TAURI__.core.invoke('ipc_dispatch',{request:{channel:'MAIN',context,listenerId:'native-'+command,data:{channel:command,data}}});
                            replies.push({command,events,...(command==='READ_AUDIO_METADATA'?{keyOrder:keyOrder(events[0].message.data)}:{})});
                        }
                        return {context,replies,elapsedMs:performance.now()-started};
                    })();
                """, str(media_fixture), str(audio_fixture))
                for reply in media_result["replies"]:
                    assert len(reply["events"]) == 1, reply
                    event = reply["events"][0]
                    assert event["context"] == media_result["context"]
                    assert event["listenerId"] == "native-" + reply["command"]
                    assert event["message"]["channel"] == reply["command"]
                    result = event["message"]["data"]
                    if reply["command"] == "FFMPEG_CHECK":
                        assert result["installed"] is True and result["path"]
                    elif reply["command"] == "ENCODER_DETECT":
                        assert result == encoder_reforced["reply"]["message"]["data"]
                    elif reply["command"] == "READ_AUDIO_METADATA":
                        assert_audio_metadata(result, expected_audio)
                        assert reply["keyOrder"] == audio_key_order(expected_audio), reply
                        (ARTIFACTS / "audio-metadata.json").write_text(json.dumps({"reply":event,"keyOrder":reply["keyOrder"],"original":expected_audio}, indent=2))
                    elif reply["command"] == "LOCATE_MEDIA_FILE":
                        assert result == {"path": str(media_fixture), "hasChanged": False}, result
                    elif reply["command"] == "GET_MEDIA_FOLDER_PATH":
                        assert result == str(Path(media_result["context"]["dataPath"]) / "Media"), result
                        assert Path(result).is_dir()
                    elif reply["command"] == "GET_OS":
                        assert result == expected_system["os"], result
                        assert result["name"] != env["HOSTNAME"]
                    elif reply["command"] == "GET_DEVICE_NAME":
                        assert result == expected_system["deviceName"], result
                    elif reply["command"] == "CHECK_RAM_USAGE":
                        assert result["total"] == expected_system["ram"]["total"]
                        assert result["total"] > 0 and result["free"] >= 0
                        captured_system = json.loads(subprocess.check_output(["node", "tests/desktop/system_oracle.mjs", json.dumps(result)], cwd=ROOT, text=True, timeout=15))
                        assert result == captured_system["ram"], (result, captured_system)
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
                steps.append("Cached native encoder detection preserves context and correlation independently of ZIP compression")
                steps.append("Native FFmpeg/AAC metadata replies preserve payload, context and correlation during ZIP compression")
                steps.append("Native FLAC tags and clamped ReplayGain match actual original music-metadata during ZIP compression")
                steps.append("Native existing-file lookup preserves path, context and correlation during ZIP compression")
                steps.append("Native media-folder query reads its committed snapshot and resolves the default during ZIP compression")
                steps.append("Native OS and device names match original kernel queries despite spoofed environment during ZIP compression")
                steps.append("Native RAM and optimized-mode decision match original source on captured inputs during ZIP compression")
                network_during_backup = probe_network_queries("backup")
                assert network_during_backup["elapsedMs"] < 1500, network_during_backup
                network_backup_status = invoke("library_status")
                assert network_backup_status["activeJob"]["listenerId"] == "backup-responsiveness" and network_backup_status["activeJob"]["phase"] == "backupCompression", network_backup_status
                steps.append("Seven retained MAIN.IP callbacks preserve source addresses, fresh capture counts and current context during the same real ZIP compression")
                probe_midi("backup")
                midi_backup_status = invoke("library_status")
                assert midi_backup_status["activeJob"]["listenerId"] == "backup-responsiveness" and midi_backup_status["activeJob"]["phase"] == "backupCompression", midi_backup_status
                steps.append("Retained native MIDI getter/error callbacks and void close finish independently during the same real ZIP compression")
                probe_audio_stream("backup")
                audio_backup_status = invoke("library_status")
                assert audio_backup_status["activeJob"]["listenerId"] == "backup-responsiveness" and audio_backup_status["activeJob"]["phase"] == "backupCompression", audio_backup_status
                steps.append("Real port, PCM/CAPTURE, native Opus and joined TCP/Ogg stream remain independent during the same actual ZIP compression with continuous source codec state")
                probe_timecode("backup", 25)
                timecode_backup_status = invoke("library_status")
                assert timecode_backup_status["activeJob"]["listenerId"] == "backup-responsiveness" and timecode_backup_status["activeJob"]["phase"] == "backupCompression", timecode_backup_status
                steps.append("LTC typed callbacks and exact original codec bytes remain independent during the same actual ZIP compression")
                api_during_backup = probe_api("backup", "Second library lyric")
                assert api_during_backup["elapsedMs"] < 2500, api_during_backup
                api_backup_status = invoke("library_status")
                assert api_backup_status["activeJob"]["listenerId"] == "backup-responsiveness" and api_backup_status["activeJob"]["phase"] == "backupCompression", api_backup_status
                steps.append("Real REST and Socket.IO actions/dynamic values remain responsive during the same ZIP compression")
                file_reads_during_backup = probe_file_reads()
                file_reads_status = invoke("library_status")
                assert file_reads_status["activeJob"]["listenerId"] == "backup-responsiveness" and file_reads_status["activeJob"]["phase"] == "backupCompression", file_reads_status
                steps.append("Four concurrent native document/EXIF/Bible reads preserve callback types and current context during the same ZIP compression")
                scriptures_during_backup = probe_scriptures("backup")
                assert scriptures_during_backup["elapsedMs"] < 1500, scriptures_during_backup
                scriptures_status = invoke("library_status")
                assert scriptures_status["activeJob"]["listenerId"] == "backup-responsiveness" and scriptures_status["activeJob"]["phase"] == "backupCompression", scriptures_status
                steps.append("Independent native scripture workers preserve full source callbacks and current library while the same real ZIP compression remains active")
                browse_result = script("""
                    return (async()=>{
                        const started=performance.now(),context=window.api.getRuntimeContext(),replies=[];
                        for(const item of arguments[0]) {
                            const listener='native-browse-'+item.command;
                            const data=await new Promise((resolve,reject)=>{
                                const timer=setTimeout(()=>{window.api.removeListener('MAIN',listener);reject(new Error('Native browse timeout: '+item.command));},2500);
                                window.api.receive('MAIN',(message,listenerId)=>{
                                    if(listenerId!==listener)return;
                                    clearTimeout(timer);window.api.removeListener('MAIN',listener);resolve(message.data);
                                },listener);
                                window.api.send('MAIN',{channel:item.command,data:item.data},listener);
                            });
                            const shape=item.command==='READ_FOLDER'?Object.fromEntries(Object.entries(data).filter(([,entry])=>entry.isFolder).map(([key,entry])=>[key,{ownNoMedia:Object.hasOwn(entry,'noMedia'),noMediaType:typeof entry.noMedia}])):null;
                            // WebDriver clones own undefined as null. Keep the
                            // direct property proof above and normalize only
                            // this JSON comparison to the source oracle.
                            replies.push({command:item.command,data:JSON.parse(JSON.stringify(data)),shape});
                        }
                        return {context,current:window.api.getRuntimeContext(),replies,elapsedMs:performance.now()-started};
                    })();
                """, expected_browse["cases"])
                assert browse_result["context"] == browse_result["current"] and browse_result["elapsedMs"] < 2500, browse_result
                for actual, expected in zip(browse_result["replies"], expected_browse["cases"]):
                    assert actual["command"] == expected["command"] and actual["data"] == expected["result"], (actual,expected)
                    assert actual["shape"] == expected["shape"], (actual,expected)
                browse_status = invoke("library_status")
                assert browse_status["activeJob"]["listenerId"] == "backup-responsiveness" and browse_status["activeJob"]["phase"] == "backupCompression", browse_status
                (ARTIFACTS / "browse.json").write_text(json.dumps({"source":expected_browse,"native":browse_result}, ensure_ascii=False, indent=2))
                steps.append("Native file info, folder listing and UTF-16 similar paths match original handlers during ZIP compression, including own undefined folder fields")
                (ARTIFACTS / "media-locate.json").write_text(json.dumps({"context":media_result["context"],"replies":[reply for reply in media_result["replies"] if reply["command"] in ["LOCATE_MEDIA_FILE","GET_MEDIA_FOLDER_PATH"]]}, indent=2))
                (ARTIFACTS / "system-info.json").write_text(json.dumps({"context":media_result["context"],"source":expected_system,"capturedSource":captured_system,"replies":[reply for reply in media_result["replies"] if reply["command"] in ["GET_OS","GET_DEVICE_NAME","CHECK_RAM_USAGE"]]}, indent=2))
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
                for api_tcp_port in [api_port, api_port + 1]:
                    with socket.socket() as closed_api:
                        closed_api.settimeout(1)
                        assert closed_api.connect_ex(("127.0.0.1", api_tcp_port)) != 0
                with socket.socket(type=socket.SOCK_DGRAM) as released_api_udp:
                    released_api_udp.bind(("127.0.0.1", api_port))
                steps.append("Native Quit closes both API TCP listeners and releases its UDP socket")
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
                    (ARTIFACTS / "failed-errors.json").write_text(json.dumps(script("return {runtime:window.__testRuntimeErrors,errors:window.__testErrors,clicks:window.__testClicks,nativeEvents:window.__nativeEvents,outputData:window.__latestOutputData,outputStates:window.__outputStates,drawerButtons:[...document.querySelectorAll('section.drawer button')].map(e=>({title:e.dataset.title,html:e.outerHTML.slice(0,250)})),navigation:[...document.querySelectorAll('.top button')].map(e=>{const r=e.getBoundingClientRect();return {title:e.dataset.title,rect:r.toJSON(),cover:document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)?.outerHTML.slice(0,400)}})}"), indent=2))
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
            discovery_peer.close()
            download_server.shutdown()
            download_server.server_close()
            download_thread.join(timeout=5)
            assert not download_thread.is_alive()


if __name__ == "__main__":
    run()

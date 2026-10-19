"""Full-editor functional CORS/soft-loop evidence, no performance/hardware claim.

Uses a private persisted profile, actual bounded native READ_FOLDER grants and
production VideoPlayer/AudioAnalyser/routing. Every process belongs to this run.
"""
import ctypes
import hashlib
import itertools
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(HERE.parent))
from script_receipts import wrap_script


def terminate(process):
    if process is None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=3)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def free_port():
    with socket.socket() as handle:
        handle.bind(("127.0.0.1", 0))
        return handle.getsockname()[1]


def wait(predicate, label, seconds=12):
    until = time.monotonic() + seconds
    error = None
    while time.monotonic() < until:
        try:
            result = predicate()
            if result:
                print("PASS", label, flush=True)
                return result
        except Exception as failure:
            error = str(failure)
        time.sleep(0.075)
    raise AssertionError(f"Bounded video observation failed: {label}: {error}")


def run():
    artifacts = ROOT / "test-output-rust/video-cors"
    artifacts.mkdir(parents=True, exist_ok=True)
    binary = Path(os.environ.get("FREESHOW_DESKTOP_BIN", ROOT / "target/debug/freeshow-desktop")).resolve()
    driver = os.environ.get("TAURI_DRIVER") or shutil.which("tauri-driver")
    webkit = os.environ.get("WEBKIT_WEBDRIVER") or shutil.which("WebKitWebDriver")
    assert binary.is_file() and driver and webkit
    assert ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) == 0
    build = json.loads((artifacts / "build.json").read_text())
    receipt = {"ok": False, "build": build, "binary": str(binary), "binarySha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "performanceSample": False, "physicalAudioVerified": False}
    observations = []
    process = pulse = None
    session = None
    with tempfile.TemporaryDirectory(prefix="freeshow-video-cors-") as temporary:
        temp = Path(temporary)
        data, app, runtime = temp / "library", temp / "app", temp / "runtime"
        for directory in [data, app, runtime]:
            directory.mkdir(mode=0o700)
        folder = data / "Media"
        folder.mkdir()
        fixture = folder / "Actual video 音楽.webm"
        # Controlled small genuine VP8/Vorbis bytes. No downloaded executable,
        # no path supplied by a renderer and no codec equivalence assertion.
        frozen_fixture = artifacts / "fixture.webm"
        if not frozen_fixture.exists():
            subprocess.run(["/usr/bin/ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-f", "lavfi", "-i", "color=c=blue:s=64x64:r=10", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", "12", "-c:v", "libvpx", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "20k", "-c:a", "libvorbis", "-q:a", "3", "-shortest", str(frozen_fixture)], check=True, timeout=20, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert frozen_fixture.stat().st_size <= 256 * 1024
        assert frozen_fixture.read_bytes()[:4] == bytes.fromhex("1a45dfa3")
        shutil.copyfile(frozen_fixture, fixture)
        alternative = folder / "Different direct resource.webm"
        shutil.copyfile(fixture, alternative)
        assert fixture.stat().st_size <= 256 * 1024
        receipt["fixture"] = {"sha256": hashlib.sha256(fixture.read_bytes()).hexdigest(), "bytes": fixture.stat().st_size, "video": "VP8", "audio": "Vorbis", "seconds": 12, "generator": subprocess.check_output(["/usr/bin/ffmpeg", "-version"], timeout=3).decode().splitlines()[0]}
        (app / "settings.json").write_text(json.dumps({"initialized": True, "language": "en", "special": {"showClosePopup": False, "icecast": {"enabled": False}}}))
        socket_path = temp / "pulse.sock"
        env = dict(os.environ, FREESHOW_RUST_APP_DATA=str(app), FREESHOW_RUST_DATA_DIR=str(data), XDG_DATA_HOME=str(temp / "xdg-data"), XDG_CACHE_HOME=str(temp / "xdg-cache"), XDG_CONFIG_HOME=str(temp / "xdg-config"), XDG_RUNTIME_DIR=str(runtime), PULSE_SERVER="unix:" + str(socket_path), PULSE_SINK="freeshow_video_cors", GSETTINGS_BACKEND="memory", TAURI_WEBVIEW_AUTOMATION="true", WEBKIT_DISABLE_COMPOSITING_MODE="1", LANG="C.UTF-8", LC_ALL="C.UTF-8", FREESHOW_RUST_API_BIND_IP="127.0.0.1", FREESHOW_RUST_REMOTES_BIND_IP="127.0.0.1")
        log = (artifacts / "driver.log").open("w")
        pulse_log = (artifacts / "pulse.log").open("w")
        try:
            pulse = subprocess.Popen(["pulseaudio", "-n", "--daemonize=no", "--use-pid-file=no", "--exit-idle-time=-1", "--log-target=stderr", "--load=module-native-protocol-unix socket=" + str(socket_path) + " auth-anonymous=1", "--load=module-null-sink sink_name=freeshow_video_cors rate=48000 channels=2"], env=env, stdout=pulse_log, stderr=subprocess.STDOUT, start_new_session=True)
            wait(lambda: socket_path.exists(), "private virtual OS sink", 5)
            port, native_port = free_port(), free_port()
            process = subprocess.Popen([driver, "--port", str(port), "--native-port", str(native_port), "--native-driver", webkit], env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            count = 0

            def http(method, route, payload=None):
                nonlocal count
                count += 1
                assert count <= 512
                endpoint = port if method == "POST" and route == "/session" else native_port
                request = urllib.request.Request(f"http://127.0.0.1:{endpoint}" + route, data=None if payload is None else json.dumps(payload).encode(), method=method, headers={"Content-Type": "application/json"})
                try:
                    with urllib.request.urlopen(request, timeout=20) as response:
                        raw = response.read(2 * 1024 * 1024 + 1)
                        assert len(raw) <= 2 * 1024 * 1024
                        value = json.loads(raw)["value"]
                except urllib.error.HTTPError as failure:
                    raise RuntimeError(failure.read(16384).decode()) from failure
                assert not (isinstance(value, dict) and isinstance(value.get("error"), str) and "message" in value), value
                return value

            keys = itertools.count()

            def script(source, *args):
                return http("POST", f"/session/{session}/execute/sync", {"script": wrap_script(next(keys), source), "args": list(args)})

            def snapshot():
                result = script("return window.__videoCors.snapshot()")
                assert result["origin"] == "tauri://localhost"
                assert result["context"] == receipt["grant"]["context"]
                assert len(observations) < 128
                observations.append(result)
                return result

            def matching(predicate):
                value = snapshot()
                return value if predicate(value) else None

            def active(value):
                return value["audio"] and not value["audio"]["paused"] and value["audio"]["readyState"] >= 2 and value["audio"]["time"] > 0.1 and value["contextState"] == "running"

            def loud(value):
                return active(value) and all(value[key] > -45 for key in ["sourceLevel", "mainLevel", "speakerLevel"])

            def quiet(value):
                return active(value) and all(value[key] <= -55 for key in ["sourceLevel", "mainLevel", "speakerLevel"])

            def button_call(name):
                script("const button=document.createElement('button');button.id='video-cors-test';button.textContent='Video functional fixture';button.style='position:fixed;z-index:999999;left:20px;top:20px';button.onclick=()=>window.__videoCors[arguments[0]]().then(v=>window.__videoResult={value:v},e=>window.__videoResult={error:String(e)});window.__videoResult=undefined;document.body.appendChild(button)", name)
                element = http("POST", f"/session/{session}/element", {"using": "css selector", "value": "#video-cors-test"})["element-6066-11e4-a52e-4f735466cecf"]
                http("POST", f"/session/{session}/element/{element}/click", {})
                result = wait(lambda: script("return window.__videoResult || null"), "real VideoPlayer fixture action")
                script("document.querySelector('#video-cors-test').remove()")
                assert "error" not in result, result
                return result["value"]

            wait(lambda: http("GET", "/status").get("ready"), "private WebDriver")
            session = http("POST", "/session", {"capabilities": {"alwaysMatch": {"tauri:options": {"application": str(binary)}}}})["sessionId"]
            wait(lambda: script("return !!window.api && !!window.__videoCors && !!document.querySelector('main .left')"), "full retained editor")
            pids = []
            for candidate in Path('/proc').glob('[0-9]*'):
                try:
                    if (candidate / 'exe').resolve() == binary:
                        pids.append(int(candidate.name))
                except (OSError, RuntimeError):
                    pass
            assert len(pids) == 1
            receipt["appPid"] = pids[0]
            if script("return !!document.querySelector('#guideButtons')"):
                element = http("POST", f"/session/{session}/element", {"using": "css selector", "value": "#guideButtons button"})["element-6066-11e4-a52e-4f735466cecf"]
                http("POST", f"/session/{session}/element/{element}/click", {})
            receipt["grant"] = script("return window.__videoCors.configure(arguments[0],arguments[1])", str(fixture), str(folder))
            assert receipt["grant"]["directResolveUnchanged"]
            receipt["started"] = button_call("start")
            assert 11.5 <= receipt["started"]["duration"] <= 12.5
            if build["original"]:
                values = [wait(lambda: matching(quiet), "original initial graph silence")]
                for _ in range(2):
                    previous = values[-1]
                    values.append(wait(lambda: matching(lambda value: quiet(value) and value["audio"]["time"] > previous["audio"]["time"] + 0.15 and value["contextClock"] > previous["contextClock"] + 0.15), "original advancing silent media and context clocks"))
                assert values[-1]["audio"]["time"] > values[0]["audio"]["time"] and values[-1]["contextClock"] > values[0]["contextClock"]
                receipt["initial"] = values
                original_url = values[0]["audio"]["source"]
                receipt["diagnosticReload"] = script("return window.__videoCors.diagnosticReload()")
                assert receipt["diagnosticReload"]["source"] == original_url
            receipt["initialNonzero"] = wait(lambda: matching(loud), "same source/routing nonzero")
            receipt["loopStart"] = script("return window.__videoCors.loop()")
            swap = wait(lambda: matching(lambda value: value["swaps"] > receipt["loopStart"]["baselineSwaps"]), "actual source soft-loop swap")
            check = quiet if build["original"] else loud
            receipt["loopAfter"] = wait(lambda: matching(check), "soft-loop routed signal boundary")
            assert receipt["loopAfter"]["audio"]["id"] != receipt["initialNonzero"]["audio"]["id"]
            assert receipt["loopAfter"]["audio"]["source"] == receipt["initialNonzero"]["audio"]["source"]
            assert receipt["loopAfter"]["audio"]["cors"] == (None if build["original"] else "anonymous")
            if not build["original"]:
                script("return window.__videoCors.disableLoop()")
                wait(lambda: matching(lambda value: value["crossfade"] is None and not value["swapping"]), "source disposes inactive crossfade and releases swap guard")
                direct_url = script("return window.api.resolveFilePath(arguments[0])", str(alternative))
                receipt["resourceReplacement"] = script("return window.__videoCors.replaceResource(arguments[0])", direct_url)
                assert receipt["resourceReplacement"]["before"] == receipt["resourceReplacement"]["id"]
                receipt["replacementNonzero"] = wait(lambda: matching(loud), "same formerly-native element plays direct replacement")
                before = script("return window.__videoCors.loop()")
                wait(lambda: matching(lambda value: value["swaps"] > before["baselineSwaps"]), "direct replacement legacy clone swap")
                receipt["directClone"] = wait(lambda: matching(quiet), "direct replacement retains legacy no-CORS graph")
                assert receipt["directClone"]["audio"]["cors"] is None
            receipt["stopped"] = script("return window.__videoCors.stop()")
            receipt["clean"] = wait(lambda: matching(lambda value: not value["playing"] and not value["sources"] and value["mainLevel"] <= -55 and value["speakerLevel"] <= -55), "real video/source cleanup and signal cessation")
            def known_gap(item):
                if item.get("kind") != "freeshow:runtime-error":
                    return False
                detail = item.get("detail", {})
                request = detail.get("request", {})
                message = request.get("data", {})
                error = detail.get("error", {})
                if request.get("channel") != "MAIN":
                    return False
                if message.get("channel") in ["DEVICE_ID", "PROVIDER_CONNECTIONS", "SPOTIFY_GET_STATE"]:
                    return error.get("code") == "unsupported_command" and error.get("message") == "unsupported command: MAIN." + message["channel"]
                return message.get("channel") == "READ_AUDIO_METADATA" and message.get("data", {}).get("filePath") == str(fixture) and error.get("code") == "audio_capability_error" and error.get("message") == "audio_unsupported_variant: unsupported audio variant: ASF/Matroska metadata"
            receipt["knownIndependentGaps"] = [item for item in receipt["clean"]["errors"] if known_gap(item)]
            unexpected = [item for item in receipt["clean"]["errors"] if not known_gap(item)]
            script("window.api.send('MAIN',{channel:'CLOSE'})")
            wait(lambda: not Path('/proc').joinpath(str(receipt["appPid"])).exists(), "normal native CLOSE", 15)
            receipt["normalCloseExitedBeforeFixtureTermination"] = True
            assert unexpected == [], unexpected
            receipt["observations"] = observations
            receipt["ok"] = True
        except Exception as failure:
            receipt["error"] = str(failure)
            receipt["observations"] = observations
            if session:
                try:
                    receipt["failureSnapshot"] = script("return window.__videoCors.snapshot()")
                    receipt["body"] = script("return document.body.innerText.slice(0,16384)")
                    script("return window.__videoCors.stop()")
                except Exception as cleanup:
                    receipt["cleanupError"] = str(cleanup)
            raise
        finally:
            terminate(process)
            terminate(pulse)
            log.close()
            pulse_log.close()
            until = time.monotonic() + 5
            while True:
                try:
                    pid, _ = os.waitpid(-1, os.WNOHANG)
                except ChildProcessError:
                    break
                if not pid:
                    assert time.monotonic() < until
                    time.sleep(0.05)
            receipt["actualFixtureCleanup"] = {"driverJoined": process is None or process.poll() is not None, "pulseJoined": pulse is None or pulse.poll() is not None, "ownedChildren": Path(f"/proc/{os.getpid()}/task/{os.getpid()}/children").read_text().strip()}
            (artifacts / "receipt.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    run()

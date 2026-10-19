"""Narrow full-editor functional integration; no CPU/hardware measurement.

Run under owned xvfb-run/dbus-run-session. The controller uses the independent
custom-protocol binary, private persisted profile, virtual Pulse sink and actual
loopback Icecast peer. Every process/socket/thread belongs to this invocation.
"""
import ctypes
import hashlib
import itertools
import json
import math
import os
from pathlib import Path
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import wave

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(HERE.parent))
from audio_peer import AudioPeer
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


def wait(predicate, label, seconds=20):
    end = time.monotonic() + seconds
    problem = None
    while time.monotonic() < end:
        try:
            result = predicate()
            if result:
                print("PASS", label, flush=True)
                return result
        except Exception as error:
            problem = str(error)
        time.sleep(0.1)
    raise AssertionError(f"Bounded observation failed: {label}: {problem}")


def expected_cold_contexts(build):
    identities = {
        "ff000b9ddcd06eb88069a210abe09fb293e75a34e116bc702de0045768146eff": ("immutable-eager-control", 1),
        "20630ed8e7f025ee664cb0bd7851e2daf5e80ba63c42fe10784d344168e361d1": ("candidate-lazy", 0),
    }
    identity = identities.get(build.get("effectiveAnalyserSha256"))
    assert identity and build.get("analyserVariant") == identity[0], "Unknown or inconsistent effective analyser identity"
    return identity[1]


def run():
    artifacts = ROOT / "test-output-rust/audio-context-fullfork"
    artifacts.mkdir(parents=True, exist_ok=True)
    binary = Path(os.environ.get("FREESHOW_DESKTOP_BIN", ROOT / "target/debug/freeshow-desktop")).resolve()
    driver = os.environ.get("TAURI_DRIVER") or shutil.which("tauri-driver")
    webkit = os.environ.get("WEBKIT_WEBDRIVER") or shutil.which("WebKitWebDriver")
    assert binary.is_file() and driver and webkit
    assert ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) == 0, "Own and reap actual descendants"
    receipt = {"ok": False, "binary": str(binary), "binarySha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "performanceSample": False, "physicalAudioVerified": False}
    receipt["build"] = json.loads((artifacts / "build.json").read_text())
    receipt["diagnosticAnonymousReload"] = os.environ.get("FREESHOW_AUDIO_FULLFORK_DIAGNOSE") == "1"
    cold_contexts = expected_cold_contexts(receipt["build"])
    receipt["immutableEagerControl"] = cold_contexts == 1
    session = None
    process = pulse = None
    observations = []
    with tempfile.TemporaryDirectory(prefix="freeshow-audio-fullfork-") as temporary:
        temp = Path(temporary)
        data, app, runtime = temp / "library", temp / "app", temp / "runtime"
        for directory in [data, app, runtime]:
            directory.mkdir(mode=0o700)
        folder = data / "Media"
        folder.mkdir()
        fixture = folder / "Real retained WAV 敬拜.wav"
        # Less than30s means the actual source treats this as an effect and does
        # not change source-global now-playing title during the PCM comparison.
        pcm = b"".join(struct.pack("<hh", int(0.6 * 32767 * math.sin(2 * math.pi * 440 * i / 48000)), int(0.4 * 32767 * math.sin(2 * math.pi * 660 * i / 48000))) for i in range(48000 * 20))
        with wave.open(str(fixture), "wb") as output:
            output.setnchannels(2)
            output.setsampwidth(2)
            output.setframerate(48000)
            output.writeframes(pcm)
        receipt["fixture"] = {"sha256": hashlib.sha256(fixture.read_bytes()).hexdigest(), "rate": 48000, "channels": 2, "seconds": 20}
        (app / "settings.json").write_text(json.dumps({"initialized": True, "language": "en", "special": {"showClosePopup": False, "icecast": {"enabled": False}}}))
        pulse_socket = temp / "pulse.sock"
        env = dict(os.environ, FREESHOW_RUST_APP_DATA=str(app), FREESHOW_RUST_DATA_DIR=str(data), XDG_DATA_HOME=str(temp / "xdg-data"), XDG_CACHE_HOME=str(temp / "xdg-cache"), XDG_CONFIG_HOME=str(temp / "xdg-config"), XDG_RUNTIME_DIR=str(runtime), PULSE_SERVER="unix:" + str(pulse_socket), PULSE_SINK="freeshow_fullfork", GSETTINGS_BACKEND="memory", TAURI_WEBVIEW_AUTOMATION="true", WEBKIT_DISABLE_COMPOSITING_MODE="1", LANG="C.UTF-8", LC_ALL="C.UTF-8", FREESHOW_RUST_API_BIND_IP="127.0.0.1", FREESHOW_RUST_REMOTES_BIND_IP="127.0.0.1")
        log = (artifacts / "driver.log").open("w")
        pulse_log = (artifacts / "pulse.log").open("w")
        try:
            pulse = subprocess.Popen(["pulseaudio", "-n", "--daemonize=no", "--use-pid-file=no", "--exit-idle-time=-1", "--log-target=stderr", "--load=module-native-protocol-unix socket=" + str(pulse_socket) + " auth-anonymous=1", "--load=module-null-sink sink_name=freeshow_fullfork rate=48000 channels=2"], env=env, stdout=pulse_log, stderr=subprocess.STDOUT, start_new_session=True)
            wait(lambda: pulse_socket.exists(), "private actual virtual Pulse sink", 5)
            port, native_port = free_port(), free_port()
            process = subprocess.Popen([driver, "--port", str(port), "--native-port", str(native_port), "--native-driver", webkit], env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            request_count = 0
            controller_deadline = time.monotonic() + 120

            def http(method, route, payload=None):
                nonlocal request_count
                request_count += 1
                assert request_count <= 512, "WebDriver observation budget"
                assert time.monotonic() < controller_deadline, "Full-fork functional controller deadline"
                endpoint = port if method == "POST" and route == "/session" else native_port
                request = urllib.request.Request(f"http://127.0.0.1:{endpoint}" + route, data=None if payload is None else json.dumps(payload).encode(), method=method, headers={"Content-Type": "application/json"})
                try:
                    with urllib.request.urlopen(request, timeout=25) as response:
                        raw = response.read(4 * 1024 * 1024 + 1)
                        assert len(raw) <= 4 * 1024 * 1024
                        value = json.loads(raw)["value"]
                except urllib.error.HTTPError as error:
                    raise RuntimeError(error.read(16384).decode()) from error
                assert not (isinstance(value, dict) and isinstance(value.get("error"), str) and "message" in value), value
                return value

            keys = itertools.count()

            def script(source, *args):
                return http("POST", f"/session/{session}/execute/sync", {"script": wrap_script(next(keys), source), "args": list(args)})

            def snapshot():
                return script("return window.__audioFullFork.snapshot()")

            def observe(label):
                value = snapshot()
                observations.append({"label": label, "value": value})
                return value

            def click(selector):
                element = http("POST", f"/session/{session}/element", {"using": "css selector", "value": selector})["element-6066-11e4-a52e-4f735466cecf"]
                http("POST", f"/session/{session}/element/{element}/click", {})

            def text_element(text):
                return script("return [...document.querySelectorAll('button')].find(button=>button.textContent.includes(arguments[0])) || null", text)

            def click_text(text):
                element = wait(lambda: text_element(text), "actual UI button: " + text)["element-6066-11e4-a52e-4f735466cecf"]
                http("POST", f"/session/{session}/element/{element}/click", {})

            wait(lambda: http("GET", "/status").get("ready"), "private WebDriver")
            session = http("POST", "/session", {"capabilities": {"alwaysMatch": {"tauri:options": {"application": str(binary)}}}})["sessionId"]
            wait(lambda: script("return !!window.api && !!window.__audioFullFork && !!document.querySelector('main .left')"), "actual retained Svelte editor")
            app_pids = []
            for candidate in Path('/proc').glob('[0-9]*'):
                try:
                    if (candidate / 'exe').resolve() == binary:
                        app_pids.append(int(candidate.name))
                except (OSError, RuntimeError):
                    pass
            assert len(app_pids) == 1, app_pids
            receipt["appPid"] = app_pids[0]
            if script("return !!document.querySelector('#guideButtons')"):
                click("#guideButtons button")
            cold = observe("full-editor-cold")
            assert cold["origin"] == "tauri://localhost" and cold["secure"], cold
            assert len(cold["contexts"]) == cold_contexts, "Actual constructor boundary before genuine use"
            script("return window.__audioFullFork.configure(arguments[0])", str(folder))
            # Navigation is an ordinary retained UI effect. A startup auto-read
            # can hit the actual fixed browse quota; do not invent its result.
            wait(lambda: text_element(fixture.stem) or script("return [...document.querySelectorAll('button')].some(button=>button.textContent.includes('Actual native WAV') && button.textContent.includes('1'))"), "actual native folder-count or file-list result")
            receipt["beforeNavigation"] = observe("startup-folder-result")
            click_text("All")
            click_text("Actual native WAV")
            wait(lambda: text_element(fixture.stem), "real native READ_FOLDER audio UI")
            receipt["capability"] = script("return window.__TAURI__.core.invoke('audio_capability')")
            before = script("return window.__TAURI__.core.invoke('audio_status')")
            assert receipt["capability"].get("Available", {}).get("version") == "libopus 1.4", receipt["capability"]
            # Genuine WebDriver click enters unchanged Svelte AudioFile handler.
            click_text(fixture.stem)
            playback_observations = []
            receipt["playbackObservations"] = playback_observations
            def playing_nonzero():
                value = snapshot()
                if len(playback_observations) < 64:
                    playback_observations.append({key: value[key] for key in ["contexts", "playing", "level", "sourceLevels", "speakerLevel", "routing", "channelData", "gains", "consoleAudio"]})
                return bool(value["playing"] and value["playing"][0]["time"] > 0.1 and value["level"] > -45)
            if os.environ.get("FREESHOW_AUDIO_FULLFORK_DIAGNOSE") == "1":
                for _ in range(8):
                    playing_nonzero()
                    time.sleep(0.15)
                receipt["corsBefore"] = observe("actual-original-no-cors")
                script("return window.__audioFullFork.anonymousReload(arguments[0])", str(fixture))
                wait(playing_nonzero, "controlled anonymous reload actual nonzero source/routing", 5)
                receipt["corsAfter"] = observe("controlled-anonymous-reload")
            else:
                wait(playing_nonzero, "UI playback and actual nonzero routed samples", 8)
            playing = observe("actual-ui-local-playback")
            assert len(playing["contexts"]) == 1 and playing["contexts"][0]["state"] == "running"
            assert playing["playing"][0]["src"].startswith("http://127.0.0.1:"), playing
            script("return window.__audioFullFork.processing(arguments[0],3,0.9)", str(fixture))
            wait(lambda: (value := snapshot())["level"] > -45 and any(item["actualSoundTouch"] and item["pitch"] == 3 and item["tempo"] == 0.9 for item in value["effects"]), "actual retained SoundTouch worklet and nonzero routed output")
            observe("actual-soundtouch")
            script("return window.__audioFullFork.processing(arguments[0],0,1)", str(fixture))
            script("return window.__audioFullFork.reattach(arguments[0])", str(fixture))
            wait(lambda: snapshot()["level"] > -45, "same actual media source reattachment")
            observe("actual-reattachment")
            receipt["sourceRecordingMime"] = script("return window.__audioFullFork.recorderFailure()")
            if receipt["sourceRecordingMime"]["attempted"]:
                assert receipt["sourceRecordingMime"]["error"]["name"] == "NotSupportedError", receipt["sourceRecordingMime"]
                assert not receipt["sourceRecordingMime"]["active"]
                assert all(track == "ended" for track in receipt["sourceRecordingMime"]["fixtureDisposedTracks"])
            def packets_done():
                value = snapshot()
                if len(value["packets"]) >= 12 and all(item["state"] == "completed" for item in value["packets"]) and not value["processors"] and not value["pendingPorts"]:
                    return value
                return None
            def native_drained():
                value = script("return window.__TAURI__.core.invoke('audio_status')")
                return value if value["ingress"]["retained"] == 0 and value["host"]["retained"] == 0 else None
            with AudioPeer(timeout=20) as peer:
                script("return window.__audioFullFork.sender(arguments[0],12)", peer.config)
                final = wait(packets_done, "real worklet through native PCM/Opus completion and source stop")
                script("return window.__audioFullFork.cleanup()")
                wire = peer.receipt(timeout=10)
            status = wait(native_drained, "actual native audio lease drain")
            receipt.update(before=before, after=status, wire=wire, audio=final)
            assert all(item["origin"] == "port" and item["metadata"]["id"] == "icecast" for item in final["packets"])
            assert final["context"] == cold["context"]
            assert all(item["context"] == final["context"] == item["receipt"]["lastReceipt"]["context"] for item in final["packets"])
            assert max(item["peak"] for item in final["packets"]) > 0.01
            assert final["sourceAcks"] == []
            assert len(final["errors"]) < 64, "Bounded diagnostic overflow cannot certify no audio errors"
            audio_errors = [item for item in final["errors"] if item.get("kind") == "runtime" and (str(item.get("detail", {}).get("error", {}).get("code", "")).startswith("audio_") or item.get("detail", {}).get("request", {}).get("channel") == "AUDIO")]
            assert not audio_errors, audio_errors
            assert status["ingress"]["completedPackets"] - before["ingress"]["completedPackets"] == len(final["packets"])
            assert status["ingress"]["acceptedPackets"] - before["ingress"]["acceptedPackets"] == len(final["packets"])
            assert all(item["receipt"]["lastReceipt"]["failure"] is None and not item["receipt"]["lastReceipt"]["diagnostics"] for item in final["packets"]), final["packets"]
            assert wire["threadJoined"] and wire["eos"]
            receipt["cleanupSnapshot"] = observe("actual-source/native-drain")
            assert receipt["cleanupSnapshot"]["sourceIds"] == receipt["cleanupSnapshot"]["processors"] == receipt["cleanupSnapshot"]["playing"] == []
            assert receipt["cleanupSnapshot"]["pendingPorts"] == 0
            assert any(item["connected"] for item in receipt["cleanupSnapshot"]["statuses"])
            assert any(not item["connected"] and not item["connecting"] for item in receipt["cleanupSnapshot"]["statuses"])
            assert all(item["keys"] == ["connected", "connecting", "error"] and item["errorOwn"] and item["errorUndefined"] and item["listenerIdUndefined"] for item in receipt["cleanupSnapshot"]["statuses"])
            receipt["observations"] = observations
            (artifacts / "before-close.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2))
            script("window.api.send('MAIN',{channel:'CLOSE'})")
            wait(lambda: not Path('/proc').joinpath(str(receipt["appPid"])).exists(), "normal native CLOSE", 15)
            receipt["normalCloseExitedBeforeFixtureTermination"] = True
            receipt["ok"] = True
        except Exception as error:
            receipt["error"] = str(error)
            if session:
                try:
                    receipt["failureSnapshot"] = snapshot()
                    receipt["failureBody"] = script("return document.body.innerText.slice(0,32768)")
                    receipt["failureButtons"] = script("return [...document.querySelectorAll('button')].filter(button=>button.textContent.includes('Real retained') || button.textContent.includes('Actual native')).map(button=>button.outerHTML.slice(0,4096)).slice(0,8)")
                    script("return window.__audioFullFork?.cleanup()")
                except Exception as cleanup_error:
                    receipt["cleanupObservationError"] = str(cleanup_error)
            raise
        finally:
            terminate(process)
            terminate(pulse)
            log.close()
            pulse_log.close()
            end = time.monotonic() + 5
            while True:
                try:
                    pid, _ = os.waitpid(-1, os.WNOHANG)
                except ChildProcessError:
                    break
                if not pid:
                    assert time.monotonic() < end, "Owned descendant did not exit/reap"
                    time.sleep(0.05)
            receipt["actualFixtureCleanup"] = {"driverJoined": process is None or process.poll() is not None, "pulseJoined": pulse is None or pulse.poll() is not None, "ownedChildren": Path(f"/proc/{os.getpid()}/task/{os.getpid()}/children").read_text().strip()}
            (artifacts / "receipt.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    run()

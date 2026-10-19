"""Functional-only real Linux WebKit checks; distinct Xvfb/profile/PulseAudio.

Build with build.mjs, then run under xvfb-run -a dbus-run-session. No CPU sample
or real-hardware claim. Test-only loopback HTTP/virtual OS audio sinks are owned
and joined; no FreeShow application binary or shared cache is modified.
"""
import argparse
import ctypes
import hashlib
from http.server import SimpleHTTPRequestHandler, HTTPServer
import json
import math
import os
from pathlib import Path
import selectors
import shutil
import signal
import struct
import subprocess
import tempfile
import threading
import time
import uuid
import wave

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def terminate(process):
    if process is None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=3)
    # Actual subprocesses share this explicitly owned session group.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def run(site, artifacts, scenarios):
    artifacts.mkdir(parents=True, exist_ok=True)
    for tool in ["pulseaudio", "pactl", "pacat"]:
        if not shutil.which(tool):
            raise RuntimeError("Actual functional fixture requires installed " + tool)
    xdotool = os.environ.get("XDOTOOL") or shutil.which("xdotool")
    if not xdotool:
        raise RuntimeError("Actual trusted gesture requires installed xdotool")
    # Reap actual WebKit grandchildren after process-group termination.
    if ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) != 0:
        raise RuntimeError("Unable to own test subprocess descendants")
    requests = []

    class Handler(SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=str(site), **kwargs)

        def do_GET(self):
            if len(requests) >= 512:
                self.send_error(503)
                return
            requests.append(self.path)
            super().do_GET()

        def log_message(self, *_args):
            pass

    server = HTTPServer(("127.0.0.1", 0), Handler)
    server.timeout = 1
    thread = threading.Thread(target=server.serve_forever, name="bounded-audio-context-fixtures")
    thread.start()
    results = []
    reaped_groups = 0
    with tempfile.TemporaryDirectory(prefix="freeshow-audio-context-webkit-") as temporary:
        temp = Path(temporary)
        runtime = temp / "runtime"
        runtime.mkdir(mode=0o700)
        socket = temp / "pulse.sock"
        env = dict(os.environ, XDG_RUNTIME_DIR=str(runtime), PULSE_SERVER="unix:" + str(socket), PULSE_SINK="freeshow_audio_context", PULSE_SOURCE="freeshow_audio_input", WEBKIT_DISABLE_COMPOSITING_MODE="1", GSETTINGS_BACKEND="memory", LANG="C.UTF-8", LC_ALL="C.UTF-8")
        pulse_log = (artifacts / "pulse.log").open("w")
        pulse = subprocess.Popen(["pulseaudio", "-n", "--daemonize=no", "--use-pid-file=no", "--exit-idle-time=-1", "--log-target=stderr", "--load=module-native-protocol-unix socket=" + str(socket) + " auth-anonymous=1", "--load=module-null-sink sink_name=freeshow_audio_context rate=48000 channels=2"], env=env, stdout=pulse_log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            end = time.monotonic() + 5
            while not socket.exists() and time.monotonic() < end and pulse.poll() is None:
                time.sleep(0.05)
            assert socket.exists(), "Private real PulseAudio server failed"
            subprocess.run(["pactl", "load-module", "module-remap-source", "source_name=freeshow_audio_input", "master=freeshow_audio_context.monitor", "source_properties=device.description=FreeShowVirtualAudioFixture"], env=env, check=True, timeout=5, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            subprocess.run(["pactl", "set-default-source", "freeshow_audio_input"], env=env, check=True, timeout=5, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            pcm = b"".join(struct.pack("<hh", int(0.6 * 32767 * math.sin(2 * math.pi * 440 * i / 48000)), int(0.4 * 32767 * math.sin(2 * math.pi * 660 * i / 48000))) for i in range(48000 * 8))
            with wave.open(str(site / "fixture.wav"), "wb") as output:
                output.setnchannels(2)
                output.setsampwidth(2)
                output.setframerate(48000)
                output.writeframes(pcm)
            fixture_hash = hashlib.sha256((site / "fixture.wav").read_bytes()).hexdigest()
            for original in [True, False]:
                for scenario, gesture in scenarios:
                    identity = ("original" if original else "candidate") + "-" + scenario + ("-gesture" if gesture else "-programmatic")
                    title = "FreeShow Audio Context " + uuid.uuid4().hex
                    profile = temp / identity
                    profile.mkdir()
                    child_env = dict(env, XDG_DATA_HOME=str(profile / "data"), XDG_CACHE_HOME=str(profile / "cache"), XDG_CONFIG_HOME=str(profile / "config"))
                    url = f"http://127.0.0.1:{server.server_port}/index.html?original={int(original)}&scenario={scenario}&gesture={int(gesture)}"
                    start_request = len(requests)
                    feed = None
                    browser = None
                    log_path = artifacts / (identity + ".stderr.log")
                    log = log_path.open("w")
                    try:
                        if scenario == "capture":
                            feed = subprocess.Popen(["pacat", "--playback", "--file-format=wav", str(site / "fixture.wav")], env=child_env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                        browser = subprocess.Popen(["/usr/bin/python3", str(HERE / "webview.py"), url, title, "allow"], env=child_env, stdout=subprocess.PIPE, stderr=log, text=True, start_new_session=True)
                        selected = selectors.DefaultSelector()
                        selected.register(browser.stdout, selectors.EVENT_READ)
                        lines = []
                        result = None
                        deadline = time.monotonic() + 22
                        while time.monotonic() < deadline:
                            if not selected.select(timeout=0.2):
                                if browser.poll() is not None:
                                    break
                                continue
                            line = browser.stdout.readline()
                            if not line:
                                break
                            if len(line.encode()) > 256 * 1024 or len(lines) >= 8:
                                raise RuntimeError("Native result bound")
                            item = json.loads(line)
                            lines.append(item)
                            if item.get("stage") == "ready" and gesture:
                                gesture_env = dict(child_env)
                                if os.environ.get("XDOTOOL_LIBRARY_PATH"):
                                    gesture_env["LD_LIBRARY_PATH"] = os.environ["XDOTOOL_LIBRARY_PATH"]
                                found = subprocess.check_output([xdotool, "search", "--onlyvisible", "--name", "^" + title + "$"], env=gesture_env, text=True, timeout=3).strip().splitlines()[-1]
                                subprocess.run([xdotool, "windowfocus", "--sync", found, "mousemove", "--window", found, "50", "50", "click", "1"], env=gesture_env, check=True, timeout=3)
                            if item.get("finished"):
                                result = item
                                break
                        selected.close()
                        if result is None:
                            raise RuntimeError("No real WebKit completion: " + json.dumps(lines))
                        result.update(identity=identity, requests=requests[start_request:])
                        if scenario == "capture":
                            result["captureProvider"] = "actual private PulseAudio remap-source from null-sink monitor fed by pacat; no physical hardware"
                        (artifacts / (identity + ".json")).write_text(json.dumps(result, ensure_ascii=False, indent=2))
                        results.append(result)
                        print(identity, "PASS" if result.get("ok") else "FAIL", result.get("error", ""), flush=True)
                    finally:
                        terminate(browser)
                        terminate(feed)
                        log.close()
                        # Linux subreaper observes and actually reaps descendants.
                        end = time.monotonic() + 3
                        while browser is not None:
                            try:
                                pid, _ = os.waitpid(-browser.pid, os.WNOHANG)
                            except ChildProcessError:
                                reaped_groups += 1
                                break
                            if time.monotonic() >= end:
                                raise RuntimeError("Owned WebKit descendants did not actually reap")
                            if pid == 0:
                                time.sleep(0.02)
        finally:
            terminate(pulse)
            pulse_log.close()
            server.shutdown()
            server.server_close()
            thread.join(timeout=3)
            assert not thread.is_alive(), "Fixture HTTP owner did not join"
    children = Path(f"/proc/{os.getpid()}/task/{os.getpid()}/children").read_text().strip()
    assert not children, "Owned native fixture children remain: " + children
    for scenario, gesture in scenarios:
        pair = [result for result in results if result["scenario"] == scenario and result["gesture"] == gesture]
        assert len(pair) == 2, "Original/candidate receipt missing"
        if all(result.get("ok") for result in pair):
            assert [stage["stage"] for stage in pair[0]["stages"]] == [stage["stage"] for stage in pair[1]["stages"]], "Real source/candidate phase mismatch"
            assert pair[0]["capabilities"] == pair[1]["capabilities"], "Real source/candidate capability mismatch"
            for result in pair:
                worklet = "/assets/soundtouch-processor.js" if scenario == "local" else "/assets/pcm-worklet.js" if scenario in ["network", "deferred"] else None
                if worklet:
                    assert result["requests"].count(worklet) == 1, "Actual worklet must load once per singleton"
                assert result["final"]["messageListeners"] == 0 and result["final"]["pendingPortCleanups"] == 0 and result["final"]["senderProcessors"] == 0, "Real listener/processor cleanup mismatch"
                assert result["harnessContextsActuallyClosed"], "Harness AudioContext did not actually close"
    receipt = {"cleanup": {"webviewGroupsActuallyReaped": reaped_groups, "ownedChildrenRemaining": [], "pulseJoined": pulse.poll() is not None, "httpJoined": not thread.is_alive()}, "functionalOnly": True, "nativeAppIntegrationVerified": False, "cpuSamplesCollected": False, "build": json.loads((site / "build.json").read_text()), "fixtureSha256": fixture_hash, "results": results, "passed": sum(bool(result.get("ok")) for result in results), "failed": sum(not result.get("ok") for result in results)}
    (artifacts / "receipt.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2))
    assert results and receipt["failed"] == 0, "Real WebKit functional failures; inspect receipts"


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--site", type=Path, default=ROOT / "test-output-rust/audio-context-webkit/site")
    parser.add_argument("--artifacts", type=Path, default=ROOT / "test-output-rust/audio-context-webkit/run")
    parser.add_argument("--only", choices=["idle", "explicit", "local", "network", "deferred", "capture"])
    options = parser.parse_args()
    cases = [(name, False) for name in ["idle", "explicit", "local", "network", "deferred", "capture"]] + [("local", True)]
    run(options.site.resolve(), options.artifacts.resolve(), [(name, gesture) for name, gesture in cases if not options.only or name == options.only])

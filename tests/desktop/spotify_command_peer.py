"""Bounded owned GIO peer on Smoke's real private bus; no account or activation."""
import json
import os
from pathlib import Path
import selectors
import subprocess
import time
from gi.repository import Gio, GLib


class SpotifyCommandPeer:
    def __init__(self, root, env):
        address = env.get("DBUS_SESSION_BUS_ADDRESS", "")
        assert address.startswith("unix:path=") and ";" not in address
        self.connection = Gio.DBusConnection.new_for_address_sync(address, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
        self.process = None
        self.selector = selectors.DefaultSelector()
        self.buffers = {"stdout": bytearray(), "stderr": bytearray()}
        self.events = []
        self.bytes = 0
        self.ready = False
        self.closed = False
        self.exit = None
        try:
            assert not self.owned(), "Private fixture must not replace an existing Spotify service"
            self.process = subprocess.Popen(["/usr/bin/python3", str(Path(root) / "crates/freeshow-spotify/tests/fixtures/commands.py"), "hold"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.selector.register(self.process.stdout, selectors.EVENT_READ, "stdout")
            self.selector.register(self.process.stderr, selectors.EVENT_READ, "stderr")
            self.wait(lambda: self.ready)
            assert self.owned()
        except BaseException:
            self.close()
            raise

    def owned(self):
        reply = self.connection.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "NameHasOwner", GLib.Variant("(s)", ("org.mpris.MediaPlayer2.spotify",)), GLib.VariantType("(b)"), Gio.DBusCallFlags.NONE, 1000, None)
        return reply.unpack()[0]

    def pump(self, timeout=0.02):
        for key, _mask in self.selector.select(timeout):
            part = os.read(key.fileobj.fileno(), 4096)
            if not part:
                self.selector.unregister(key.fileobj)
                continue
            self.bytes += len(part)
            assert self.bytes <= 65536, "Peer output byte quota"
            data = self.buffers[key.data]
            data.extend(part)
            assert len(data) <= 16384, "Peer line quota"
            if key.data == "stdout":
                while b"\n" in data:
                    line, _, rest = data.partition(b"\n")
                    data[:] = rest
                    if line == b"READY":
                        assert not self.ready
                        self.ready = True
                    else:
                        value = json.loads(line)
                        assert list(value) == ["name", "args"] and isinstance(value["name"], str) and isinstance(value["args"], list)
                        assert len(self.events) < 64
                        self.events.append(value)

    def wait(self, condition, seconds=3):
        assert 0 < seconds <= 5
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.pump()
            if condition():
                return
            assert self.process.poll() is None, "Peer exited before its observation"
        raise TimeoutError("Actual Spotify peer observation timed out")

    def release(self):
        self.process.stdin.write(b"release\n")
        self.process.stdin.flush()

    def close(self):
        if self.closed:
            return self.exit
        errors = []
        try:
            if self.process:
                if self.process.poll() is None:
                    try:
                        self.process.stdin.write(b"stop\n")
                        self.process.stdin.flush()
                    except (BrokenPipeError, OSError):
                        pass
                    try:
                        self.process.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        self.process.kill()
                        self.process.wait(timeout=2)
                        errors.append("Peer required emergency kill")
                self.exit = {"pid": self.process.pid, "returncode": self.process.returncode, "waited": True}
                # Actual wait precedes bounded pipe EOF drain; no observer timeout
                # or bus-name disappearance is substituted for child ownership.
                deadline = time.monotonic() + 1
                while self.selector.get_map() and time.monotonic() < deadline:
                    self.pump(0.01)
                assert not self.selector.get_map(), "Waited fixture pipes did not close"
                assert not self.buffers["stdout"], "Incomplete fixture JSON line"
                if self.process.returncode != 0:
                    errors.append("Peer failed: " + self.buffers["stderr"].decode("utf8", errors="replace")[:2048])
                if self.owned():
                    errors.append("Spotify bus name remains owned after fixture wait")
        finally:
            if self.process:
                for pipe in [self.process.stdin, self.process.stdout, self.process.stderr]:
                    pipe.close()
            self.selector.close()
            self.connection.close_sync(None)
            self.closed = True
        if errors:
            raise RuntimeError("; ".join(errors))
        return self.exit


def validate_callback(observed, original, context):
    assert observed["context"] == observed["current"] == context
    assert observed["action"] == "command" and observed["sendUndefined"] is True
    assert len(observed["callbacks"]) == 1
    callback = observed["callbacks"][0]
    assert list(callback) == ["message", "messageKeys", "args", "listener", "callbackContext"]
    assert list(callback["message"]) == ["channel", "data"] and callback["message"]["data"] is True
    for key in ["message", "messageKeys", "args", "listener"]:
        assert callback[key] == original[key], (key, callback, original)
    assert callback["callbackContext"] == context


def commands_settled(status, before, count):
    assert isinstance(status, dict) and status["commands"] >= before["commands"]
    assert status["commandFailures"] == before["commandFailures"], status
    assert status["terminalError"] is None and status["lastCommandError"] is None, status
    assert status["commands"] <= before["commands"] + count, status
    return status["commands"] == before["commands"] + count and status["commandsCompleted"] == before["commandsCompleted"] + count and status["retained"] == 0 and status["commandPids"] == [] and status["activePid"] == 0 and status["spawned"] == status["reaped"]

"""Run under dbus-run-session, without an application or Spotify account."""
import ast
import copy
import json
import os
from pathlib import Path
import shlex
import subprocess
import unittest
from spotify_command_peer import SpotifyCommandPeer, commands_settled, validate_callback

ROOT = Path(__file__).resolve().parents[2]


class FixtureTests(unittest.TestCase):
    def test_actual_peer_six_effects_hold_release_wait_and_name_cleanup(self):
        original = json.loads(subprocess.check_output([os.environ.get("FREESHOW_TEST_NODE", "node"), "tests/desktop/spotify_command_oracle.mjs", "ordinary"], cwd=ROOT, timeout=15))
        peer = SpotifyCommandPeer(ROOT, dict(os.environ))
        child = None
        try:
            for index, scheduled in enumerate(original["scheduled"]):
                argv = shlex.split(scheduled)
                assert argv.pop(0) == "dbus-send"
                child = subprocess.Popen(["/usr/bin/dbus-send", *argv], env=os.environ, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                if "--print-reply" in argv:
                    peer.wait(lambda: len(peer.events) == index+1)
                    self.assertEqual(child.poll(), None)
                    self.assertEqual(peer.events[-1]["name"], "Get")
                    peer.release()
                stdout, stderr = child.communicate(timeout=2)
                self.assertEqual(child.returncode, 0)
                self.assertEqual(stderr, b"")
                self.assertLess(len(stdout), 4096)
                child = None
                peer.wait(lambda: len(peer.events) == index+1)
            self.assertEqual(peer.events, original["effects"])
            # A second helper must not replace or remove this actual owner.
            with self.assertRaisesRegex(AssertionError, "must not replace"):
                SpotifyCommandPeer(ROOT, dict(os.environ))
            self.assertTrue(peer.owned())
        finally:
            if child is not None:
                if child.poll() is None:
                    child.kill()
                child.communicate(timeout=2)
            exit = peer.close()
        self.assertEqual(exit["returncode"], 0)
        self.assertTrue(exit["waited"])
        self.assertEqual(peer.close(), exit)
        self.assertFalse(Path("/proc", str(exit["pid"])).exists())
        # Actual waited name cleanup supports a fresh peer without retry.
        replacement = SpotifyCommandPeer(ROOT, dict(os.environ))
        self.assertEqual(replacement.close()["returncode"], 0)

    def test_callback_comparator_rejects_malformed_receipts(self):
        context = {"runtimeId":"runtime", "libraryGeneration":4, "dataPath":"/tmp/library"}
        original = {"message":{"channel":"SPOTIFY_COMMAND", "data":True}, "messageKeys":["channel","data"], "args":2, "listener":"exact"}
        actual = {"context":context,"current":context,"action":"command","sendUndefined":True,"callbacks":[{**original,"callbackContext":context}]}
        validate_callback(actual, original, context)
        shuffled = copy.deepcopy(actual)
        shuffled["callbacks"][0] = dict(reversed(list(shuffled["callbacks"][0].items())))
        shuffled["callbacks"][0]["message"] = dict(reversed(list(shuffled["callbacks"][0]["message"].items())))
        validate_callback(shuffled, original, context)
        for change in [lambda v:v.update(sendUndefined=False), lambda v:v["callbacks"].append(v["callbacks"][0]), lambda v:v["callbacks"][0].update(args=3), lambda v:v["callbacks"][0]["message"].update(data=1), lambda v:v["callbacks"][0].update(listener="wrong"), lambda v:v["callbacks"][0].update(messageKeys=["channel","data","nativeSpotifyCommand"]), lambda v:v["callbacks"][0].update(messageKeys=["data","channel"]), lambda v:v["callbacks"][0].update(private=True), lambda v:v["callbacks"][0]["message"].update(private=True), lambda v:v.update(current={**context,"libraryGeneration":5})]:
            bad = copy.deepcopy(actual)
            change(bad)
            with self.assertRaises(AssertionError):
                validate_callback(bad,original,context)

    def test_actual_gtk_helper_accept_hook_runs_after_path_typing_before_return(self):
        parsed = ast.parse((ROOT / "tests/desktop/smoke.py").read_text())
        node = next(node for node in ast.walk(parsed) if isinstance(node,ast.FunctionDef) and node.name == "choose_native_file")
        for with_hook in [False,True]:
            calls = []
            namespace = {"wait":lambda condition,label:condition(), "native_dialog":lambda title:"window", "native_keys":lambda *args:calls.append(args), "native_open_button":lambda title:type("Button",(),{"get_action_iface":lambda self:self,"do_action":lambda self,action:True})(), "time":type("Clock",(),{"sleep":lambda seconds:None})}
            exec(compile(ast.fix_missing_locations(ast.Module(body=[node],type_ignores=[])),"smoke-real-helper","exec"),namespace)
            namespace["choose_native_file"]("Data location","/tmp/path",**({"before_accept":lambda:calls.append(("accepted-seek",))} if with_hook else {}))
            expected = [("windowfocus","--sync","window"),("key","--clearmodifiers","ctrl+l"),("type","--clearmodifiers","--delay","3","/tmp/path")]
            if with_hook:
                expected.extend([("accepted-seek",),("windowfocus","--sync","window")])
            expected.append(("key","--clearmodifiers","Return"))
            self.assertEqual(calls,expected)

    def test_settlement_requires_real_counts_no_pids_or_ready_leases(self):
        before = {"commands":10,"commandsCompleted":10,"commandFailures":0,"terminalError":None,"lastCommandError":None,"retained":0,"commandPids":[],"activePid":0,"spawned":12,"reaped":12}
        settled = {**before,"commands":11,"commandsCompleted":11,"spawned":14,"reaped":14}
        self.assertTrue(commands_settled(settled,before,1))
        for change in [{"commandsCompleted":10},{"retained":1},{"commandPids":[123]},{"activePid":123},{"reaped":13}]:
            self.assertFalse(commands_settled({**settled,**change},before,1))
        for change in [{"commandFailures":1},{"commands":12},{"terminalError":{"code":"failure"}},{"lastCommandError":{"code":"deadline"}}]:
            with self.assertRaises(AssertionError):
                commands_settled({**settled,**change},before,1)


if __name__ == "__main__":
    unittest.main()

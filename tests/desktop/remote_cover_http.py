"""Real bounded HTTP checks; the main-WebView observer sees original callbacks.

Root owns the actual GUI run, fixture writes/blockers and popup assertions. This
helper reads fixtures, makes loopback requests and returns a JSON-only receipt.
"""

import hashlib
import http.client
import ipaddress
import json
import os
import re
import stat
import time
from pathlib import Path


MAX_COVER = 4 * 1024 * 1024
KINDS = ("REMOTE", "STAGE", "CONTROLLER")
ROUTE = "/nowplayingcover"


def _endpoint(value):
    assert isinstance(value, str) and len(value) <= 256, value
    host, separator, port = value.rpartition(":")
    assert separator and re.fullmatch(r"[0-9]{1,5}", port), value
    host = host.removeprefix("[").removesuffix("]")
    address = ipaddress.ip_address(host)
    assert address.is_loopback or address.is_unspecified, value
    assert 1 <= int(port) <= 65535, value
    target = ("::1" if address.version == 6 else "127.0.0.1") if address.is_unspecified else str(address)
    return target, int(port)


def _folder_observation(folder):
    try:
        value = folder.lstat()
    except FileNotFoundError:
        return {"exists": False}
    assert stat.S_ISDIR(value.st_mode) or stat.S_ISREG(value.st_mode), str(folder)
    return {
        "exists": True,
        "kind": "directory" if stat.S_ISDIR(value.st_mode) else "file",
        "device": str(value.st_dev),
        "inode": str(value.st_ino),
        "mode": value.st_mode,
        "size": value.st_size,
        "mtimeNs": str(value.st_mtime_ns),
        "ctimeNs": str(value.st_ctime_ns),
    }


def probe_cover_http(script, invoke, root, phase, context, expected_path, blocker=False):
    """Observe one <=15s Linux fixture phase on all three actual native ports.

    ``script(source, fixture)`` executes the retained WebView helper; ``invoke``
    is Root's status-only Tauri wrapper. No source command or path grant is sent.
    ``expected_path`` is the trusted fixture's real cover Path, or None for404.
    The receipt proves observed completion/zero retention, not native IO joins.
    """
    assert isinstance(phase, str) and re.fullmatch(r"[a-zA-Z0-9_-]{1,64}", phase)
    assert isinstance(context, dict) and set(context) == {"runtimeId", "libraryGeneration", "dataPath"}
    assert isinstance(blocker, bool) and (not blocker or expected_path is None)
    assert isinstance(context["dataPath"], str) and context["dataPath"].startswith("/")
    source = (Path(root) / "tests/desktop/remote_cover_probe.js").read_text()
    fixture = {"phase": phase, "context": context, "blocker": blocker}
    expected = None
    if expected_path is not None:
        expected_path = Path(expected_path)
        assert expected_path == Path(context["dataPath"]) / "Audio/NowPlayingCover.png", expected_path
        descriptor = os.open(expected_path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC)
        with os.fdopen(descriptor, "rb") as stream:
            # fstat the opened descriptor, then bound the independent byte read.
            info = os.fstat(stream.fileno())
            assert stat.S_ISREG(info.st_mode) and 0 < info.st_size <= MAX_COVER, info.st_size
            expected = stream.read(MAX_COVER + 1)
            assert len(expected) == info.st_size, "Cover fixture changed during independent read"
            after = os.fstat(stream.fileno())
            assert (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns) == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns), "Cover fixture version changed during read"
        expected_hash = hashlib.sha256(expected).hexdigest()
    else:
        expected_hash = None
    started = False
    completed = False
    responses = []
    deadline = time.monotonic() + 15

    def request(kind, method, headers=None):
        remaining = deadline - time.monotonic()
        assert remaining > 0, "Remote cover HTTP phase exceeded15s"
        host, port = _endpoint(endpoints[kind])
        connection = http.client.HTTPConnection(host, port, timeout=min(1, remaining))
        try:
            connection.request(method, ROUTE, headers=headers or {})
            response = connection.getresponse()
            raw_headers = response.getheaders()
            assert len(raw_headers) <= 32 and sum(len(key) + len(value) for key, value in raw_headers) <= 16384, "HTTP header budget"
            normalized = {}
            for key, value in raw_headers:
                key = key.lower()
                assert key not in normalized and len(value) <= 8192, "Duplicate/oversized response header"
                normalized[key] = value
            # read1 avoids an unbounded read-to-EOF on a slowly trickling body.
            # HEAD is checked as HTTP-framed body, not raw bytes after that frame.
            collected = bytearray()
            for _ in range(4096):
                assert time.monotonic() < deadline, "HTTP body deadline"
                block = response.read1(min(32768, MAX_COVER + 1 - len(collected)))
                if not block:
                    break
                collected.extend(block)
                assert len(collected) <= MAX_COVER, "HTTP body budget"
            else:
                raise AssertionError("HTTP read-operation budget")
            body = bytes(collected)
            assert len(body) <= MAX_COVER and time.monotonic() < deadline, "HTTP body/deadline budget"
            item = {
                "kind": kind,
                "method": method,
                "requestHeaders": headers or {},
                "status": response.status,
                "headers": normalized,
                "bytes": len(body),
                "sha256": hashlib.sha256(body).hexdigest(),
            }
            if len(body) <= 32:
                item["bodyHex"] = body.hex()
            responses.append(item)
            assert len(responses) <= 15, "HTTP request count budget"
            return response.status, normalized, body
        finally:
            connection.close()

    try:
        observation = script(source, {**fixture, "stage": "start"})
        started = True
        assert observation["listenerRegistered"] and observation["context"] == context, observation
        initial = invoke("remotes_status")
        assert not initial["closing"] and not initial.get("failure") and initial["binding"]["context"] == context and not initial["binding"]["retiring"], initial
        assert initial["cover"] == {"filesRetained": 0, "publicationsRetained": 0}, initial
        endpoints = initial["binding"]["bound"]["endpoints"]
        assert set(endpoints) == set(KINDS) and endpoints == observation["before"]["endpoints"], endpoints
        assert initial["binding"]["bound"]["generation"] == observation["before"]["generation"], initial["binding"]["bound"]
        assert len({_endpoint(value) for value in endpoints.values()}) == 3, endpoints
        audio_folder = Path(context["dataPath"]) / "Audio"
        folder_before = _folder_observation(audio_folder)
        if blocker:
            assert folder_before.get("kind") == "file", folder_before
        for kind in KINDS:
            status, headers, body = request(kind, "OPTIONS")
            assert status == 200 and body == b"GET,HEAD" and headers.get("allow") == "GET,HEAD" and headers.get("content-length") == "8", responses[-1]
        options = script(source, {**fixture, "stage": "options"})
        folder_after = _folder_observation(audio_folder)
        assert folder_after == folder_before and options["alerts"] == 0 and options["noAlertObserved"], (folder_before, folder_after, options)
        for kind in KINDS:
            status, headers, body = request(kind, "GET")
            if expected is None:
                assert status == 404 and body == b"" and headers.get("content-length") == "0", responses[-1]
            else:
                assert status == 200 and body == expected and headers.get("content-length") == str(len(expected)) and headers.get("content-type") == "image/png", responses[-1]
            if blocker:
                continue  # A HEAD would cause three additional genuine folder ALERTs.
            head_status, head_headers, head_body = request(kind, "HEAD")
            assert head_status == status and head_body == b"", responses[-1]
            if expected is None:
                assert "content-length" not in head_headers, responses[-1]
                continue
            assert head_headers.get("content-length") == str(len(expected)) and head_headers.get("content-type") == "image/png" and head_headers.get("etag") == headers.get("etag"), responses[-1]
            assert isinstance(headers.get("etag"), str) and 0 < len(headers["etag"]) <= 128, headers
            range_status, range_headers, range_body = request(kind, "GET", {"Range": "bytes=0-0"})
            assert range_status == 206 and range_body == expected[:1] and range_headers.get("content-length") == "1" and range_headers.get("content-range") == f"bytes 0-0/{len(expected)}" and range_headers.get("content-type") == "image/png", responses[-1]
            cached_status, cached_headers, cached_body = request(kind, "GET", {"If-None-Match": headers["etag"]})
            assert cached_status == 304 and cached_body == b"" and cached_headers.get("etag") == headers["etag"] and "content-length" not in cached_headers and "content-type" not in cached_headers, responses[-1]
        result = script(source, {**fixture, "stage": "finish"})
        completed = True
        assert result["context"] == context and result["counts"]["folderAlerts"] == (3 if blocker else 0) and result["retainedZeroVerified"], result
        assert result["counts"]["sourceAcks"] == result["counts"]["runtimeErrors"] == result["counts"]["browserErrors"] == 0 and result["noNowPlayingRevealCallbackInvoked"], result
        assert len(responses) == (6 if blocker else 9 if expected is None else 15), responses
        receipt = {
            "phase": phase,
            "context": context,
            "blocker": blocker,
            "expectedPath": str(expected_path) if expected_path is not None else None,
            "expectedBytes": len(expected) if expected is not None else None,
            "expectedSha256": expected_hash,
            "bound": initial["binding"]["bound"],
            "responses": responses,
            "options": {"observer": options, "folderBefore": folder_before, "folderAfter": folder_after, "noObservedLookupEffects": True},
            "observer": result,
            "independentByteComparison": expected is not None,
            "nativeIoJoinVerified": False,
        }
        assert len(json.dumps(receipt, ensure_ascii=False).encode("utf-8")) <= 512 * 1024, "JSON receipt budget"
        return receipt
    except BaseException as failure:
        if started and not completed:
            try:
                script(source, {**fixture, "stage": "abort"})
            except Exception as cleanup_failure:
                failure.add_note("Remote-cover observer cleanup also failed: " + str(cleanup_failure)[:1024])
        raise

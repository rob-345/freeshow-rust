"""One bounded actual loopback Icecast capture, owned by the desktop controller."""
import base64
import hashlib
import secrets
import socket
import struct
import threading
import time

MAX_WIRE = 1024 * 1024
MAX_HEADER = 16384
MAX_PAGES = 4096


def ogg_crc(data):
    crc = 0
    for byte in data:
        crc ^= byte << 24
        for _ in range(8):
            crc = ((crc << 1) ^ (0x04C11DB7 if crc & 0x80000000 else 0)) & 0xFFFFFFFF
    return crc


def parse_wire(wire):
    """Validate every original CRC before any consumer can filter/normalize."""
    if not isinstance(wire, bytes) or not wire or len(wire) > MAX_WIRE:
        raise ValueError("Audio wire size bound")
    end = wire.find(b"\r\n\r\n")
    if end < 0 or end + 4 > MAX_HEADER:
        raise ValueError("Truncated/oversized audio HTTP header")
    end += 4
    header = wire[:end].decode("utf-8", errors="strict")
    if not header.startswith("PUT "):
        raise ValueError("Audio peer expected real HTTP PUT")
    pages, cursor = [], end
    while cursor < len(wire):
        if len(pages) >= MAX_PAGES or cursor + 27 > len(wire):
            raise ValueError("Audio Ogg page count/header bound")
        if wire[cursor:cursor + 4] != b"OggS" or wire[cursor + 4] != 0:
            raise ValueError("Invalid original audio Ogg capture/version")
        segments = wire[cursor + 26]
        head_end = cursor + 27 + segments
        if head_end > len(wire):
            raise ValueError("Truncated original audio lacing")
        lacing = list(wire[cursor + 27:head_end])
        page_end = head_end + sum(lacing)
        if page_end > len(wire):
            raise ValueError("Truncated original audio packet")
        page = wire[cursor:page_end]
        stored = struct.unpack_from("<I", page, 22)[0]
        cleared = bytearray(page)
        cleared[22:26] = bytes(4)
        computed = ogg_crc(cleared)
        if computed != stored:
            raise ValueError("Original audio Ogg CRC mismatch before normalization")
        pages.append({"rawBase64": base64.b64encode(page).decode("ascii"), "payloadBase64": base64.b64encode(wire[head_end:page_end]).decode("ascii"), "bytes": len(page), "flags": page[5], "granule": str(struct.unpack_from("<Q", page, 6)[0]), "serial": struct.unpack_from("<I", page, 14)[0], "sequence": struct.unpack_from("<I", page, 18)[0], "lacing": lacing, "crc": stored, "computedCrc": computed, "crcValid": True})
        cursor = page_end
    if len(pages) < 3 or not pages[0]["flags"] & 2 or not pages[-1]["flags"] & 4 or base64.b64decode(pages[-1]["payloadBase64"]):
        raise ValueError("Audio stream lacks real BOS/EOS")
    if [page["sequence"] for page in pages] != list(range(len(pages))):
        raise ValueError("Original audio Ogg sequence mismatch")
    if len({page["serial"] for page in pages}) != 1:
        raise ValueError("Unexpected metadata chain in controlled audio phase")
    return {"header": header, "pages": pages, "wireBase64": base64.b64encode(wire).decode("ascii"), "wireBytes": len(wire), "wireSha256": hashlib.sha256(wire).hexdigest(), "rawCrcsVerified": len(pages), "eos": True}


class AudioPeer:
    """Exactly one listener, one thread and at most one accepted TCP stream."""
    def __init__(self, timeout=20):
        if not 0 < timeout <= 30:
            raise ValueError("Audio peer timeout bound")
        self.timeout = timeout
        self._listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._listener.bind(("127.0.0.1", 0))
        self._listener.listen(1)
        self._listener.settimeout(0.1)
        self.port = self._listener.getsockname()[1]
        self.config = {"enabled": True, "host": "127.0.0.1", "port": self.port, "mount": "/native-audio-" + secrets.token_hex(8), "password": "native-音楽-" + secrets.token_hex(8)}
        self._stop = threading.Event()
        self._done = threading.Event()
        self._connection = None
        self._wire = bytearray()
        self._error = None
        self._accepted = 0
        self._thread = threading.Thread(target=self._run, name="freeshow-audio-peer", daemon=False)
        self._thread.start()

    def _run(self):
        deadline = time.monotonic() + self.timeout
        try:
            while not self._stop.is_set() and time.monotonic() < deadline:
                try:
                    connection, address = self._listener.accept()
                except socket.timeout:
                    continue
                if address[0] != "127.0.0.1":
                    connection.close()
                    raise ValueError("Non-loopback audio peer")
                self._accepted += 1
                self._connection = connection
                self._listener.close()  # No second connection/job can be retained.
                connection.settimeout(0.1)
                responded = False
                while not self._stop.is_set() and time.monotonic() < deadline:
                    try:
                        data = connection.recv(4096)
                    except socket.timeout:
                        continue
                    if not data:
                        return
                    if len(self._wire) + len(data) > MAX_WIRE:
                        raise ValueError("Audio peer exceeded one MiB raw wire")
                    self._wire.extend(data)
                    header_end = self._wire.find(b"\r\n\r\n")
                    if header_end < 0 and len(self._wire) > MAX_HEADER:
                        raise ValueError("Audio peer header bound")
                    if not responded and header_end >= 0:
                        if header_end + 4 > MAX_HEADER:
                            raise ValueError("Audio peer header bound")
                        connection.sendall(b"HTTP/1.1 100 Continue\r\n\r\n")
                        responded = True
                raise TimeoutError("Audio peer actual connection deadline")
            if not self._stop.is_set():
                raise TimeoutError("Audio peer accept deadline")
        except Exception as error:
            if not self._stop.is_set():
                self._error = str(error)[:2048]
        finally:
            if self._connection is not None:
                self._connection.close()
            self._listener.close()
            self._done.set()

    def receipt(self, timeout=10):
        if not 0 < timeout <= 30:
            raise ValueError("Audio peer receipt timeout bound")
        if not self._done.wait(timeout):
            self.close()
            raise TimeoutError("Audio peer has not received real EOF")
        self._thread.join(timeout=1)
        if self._thread.is_alive():
            raise RuntimeError("Audio peer worker did not join")
        if self._error:
            raise RuntimeError(self._error)
        result = parse_wire(bytes(self._wire))
        expected_auth = base64.b64encode(("source:" + self.config["password"]).encode("utf-8")).decode("ascii")
        if not result["header"].startswith("PUT " + self.config["mount"] + " HTTP/1.1\r\n") or "\r\nAuthorization: Basic " + expected_auth + "\r\n" not in result["header"]:
            raise ValueError("Actual audio PUT target/auth mismatch")
        return {**result, "config": self.config.copy(), "acceptedConnections": self._accepted, "threadJoined": True, "loopbackOnly": True}

    def close(self):
        self._stop.set()
        if self._connection is not None:
            try:
                self._connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            self._connection.close()
        self._listener.close()
        self._thread.join(timeout=2)
        if self._thread.is_alive():
            raise RuntimeError("Audio peer cleanup did not join its actual thread")

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()

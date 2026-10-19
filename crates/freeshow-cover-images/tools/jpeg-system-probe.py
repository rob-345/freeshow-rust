#!/usr/bin/python3
"""Explicit Linux capability experiment; never an application decoder fallback.

Uses existing GdkPixbuf's compiled libjpeg consumer and installed djpeg. No
guessed C struct ABI, dependency installation, or production capability change.
"""
import base64
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import zlib

import gi

gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "fixtures/jpeg/electron-linux.json"


def sha(data):
    return hashlib.sha256(data).hexdigest()


def identity(path):
    actual = Path(path).resolve(strict=True)
    return {"path": str(actual), "sha256": sha(actual.read_bytes())}


def compare(fixture, rgba, width, height):
    original = zlib.decompress(base64.b64decode(fixture["source"]["rgba"]["deflateBase64"]))
    assert len(original) == len(rgba) <= 16 * 1024 * 1024
    assert sha(original) == fixture["source"]["rgba"]["sha256"]
    assert {"width": width, "height": height} == fixture["source"]["size"]
    differences = [abs(a - b) for a, b in zip(original, rgba)]
    return {
        "name": fixture["name"],
        "size": {"width": width, "height": height},
        "rgbaSha256": sha(rgba),
        "differentPixels": sum(rgba[p:p + 4] != original[p:p + 4] for p in range(0, len(rgba), 4)),
        "differentChannels": sum(d > 0 for d in differences),
        "maximumDifference": max(differences),
    }


def main():
    assert sys.platform == "linux", "actual loaded-library evidence is Linux-only"
    receipt = json.loads(SOURCE.read_text())
    assert len(receipt["fixtures"]) == 15
    inputs = []
    for fixture in receipt["fixtures"]:
        data = base64.b64decode(fixture["input"]["base64"])
        assert sha(data) == fixture["input"]["sha256"]
        assert len(data) == fixture["input"]["length"] <= 4 * 1024 * 1024
        inputs.append((fixture, data))

    default = []
    for fixture, data in inputs:
        loader = GdkPixbuf.PixbufLoader.new_with_type("jpeg")
        assert loader.write(data) and loader.close()
        image = loader.get_pixbuf()
        width, height = image.get_width(), image.get_height()
        assert image.get_n_channels() == 3 and image.get_bits_per_sample() == 8
        assert 0 < width <= 4096 and 0 < height <= 4096 and width * height <= 4 * 1024 * 1024
        pixels, stride = image.get_pixels(), image.get_rowstride()
        rgba = bytearray()
        for y in range(height):
            for x in range(width):
                start = y * stride + x * 3
                rgba.extend(pixels[start:start + 3])
                rgba.append(255)
        default.append(compare(fixture, rgba, width, height))
        del image, loader
    libraries = sorted({line.split()[-1] for line in Path("/proc/self/maps").read_text().splitlines() if "/libjpeg." in line or "/libgdk_pixbuf-" in line})
    assert any("libjpeg.so.8" in name for name in libraries)
    assert not any("libjpeg.so.9" in name for name in libraries)

    executable = shutil.which("djpeg")
    modes = []
    linked_jpeg = []
    if executable:
        linked = subprocess.run(["ldd", executable], capture_output=True, text=True, timeout=5, check=True).stdout
        linked_jpeg = [identity(m) for m in re.findall(r"libjpeg[^\s]*\s+=>\s+(\S+)", linked)]
        for name, arguments in [
            ("accurate-fancy", ["-dct", "int"]),
            ("accurate-no-smooth", ["-dct", "int", "-nosmooth"]),
            ("fast-fancy", ["-dct", "fast"]),
            ("fast-no-smooth", ["-dct", "fast", "-nosmooth"]),
        ]:
            results = []
            for fixture, data in inputs:
                command = [executable, "-rgb", "-pnm", "-maxmemory", "32768", *arguments]
                output = subprocess.run(command, input=data, capture_output=True, timeout=10, check=True)
                assert len(output.stdout) <= 16 * 1024 * 1024 and len(output.stderr) <= 65536
                header = re.match(rb"P6\s+(\d+)\s+(\d+)\s+255\s", output.stdout)
                assert header
                width, height = map(int, header.groups())
                rgb = output.stdout[header.end():]
                assert len(rgb) == width * height * 3
                rgba = bytearray()
                for p in range(0, len(rgb), 3):
                    rgba.extend(rgb[p:p + 3])
                    rgba.append(255)
                results.append(compare(fixture, rgba, width, height))
            modes.append({"name": name, "arguments": arguments, "fixtures": results})

    header_paths = ["/usr/include/jpeglib.h", "/usr/include/x86_64-linux-gnu/jpeglib.h", "/usr/local/include/jpeglib.h"]
    packages = None
    if shutil.which("dpkg-query"):
        packages = subprocess.run(
            ["dpkg-query", "-W", "-f=${binary:Package}\t${Version}\n", "libgdk-pixbuf-2.0-0", "libjpeg-turbo8", "libjpeg9", "libjpeg-progs"],
            capture_output=True, text=True, timeout=5, check=True,
        ).stdout.splitlines()
    result = {
        "schema": 1,
        "sourceReceiptSha256": sha(SOURCE.read_text().replace("\r\n", "\n").encode()),
        "probeSourceSha256": sha(Path(__file__).read_text().replace("\r\n", "\n").encode()),
        "platform": sys.platform,
        "gdkPixbufVersion": GdkPixbuf.PIXBUF_VERSION,
        "packages": packages,
        "loadedNativeLibraries": [identity(name) for name in libraries],
        "consumerDefault": {"settings": "Compiled consumer defaults; direct DCT/fancy controls not exposed", "fixtures": default},
        "djpeg": {"available": bool(executable), "executable": identity(executable) if executable else None, "linkedJpegLibraries": linked_jpeg, "modes": modes},
        "directAbi": {"probed": False, "auditedHeaderPaths": {path: Path(path).is_file() for path in header_paths}, "reason": "No direct C ABI declarations or initialization/allocation proof; consumer evidence only"},
        "productionDecoderEnabled": False,
    }
    serialized = json.dumps(result, indent=2) + "\n"
    if len(sys.argv) == 2:
        Path(sys.argv[1]).write_text(serialized)
    else:
        assert len(sys.argv) == 1
        print(serialized, end="")
    print(json.dumps({"consumerExact": sum(f["differentPixels"] == 0 for f in default), "fixtures": len(default), "directAbiProbed": False, "productionDecoderEnabled": False}), file=sys.stderr)


if __name__ == "__main__":
    main()

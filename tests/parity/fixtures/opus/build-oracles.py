"""Private Linux/x64 source oracle; never modifies node_modules or installs anything.

python3 tests/parity/fixtures/opus/build-oracles.py --output <private build dir>
Uses original deps/binding.gyp source list/config and original node-opus.cc.
"""
import argparse
import ast
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("--output", required=True)
node = Path(shutil.which("node") or "node").resolve()
bundled_headers = node.parent.parent / "include/node"
parser.add_argument("--node-headers", default=str(bundled_headers if (bundled_headers / "node_api.h").is_file() else Path("/usr/include/node")))
parser.add_argument("--system-opus", default="/lib/x86_64-linux-gnu/libopus.so.0")
args = parser.parse_args()
assert (Path(args.node_headers) / "node_api.h").is_file(), "N-API headers required for original addon build"
root = Path(__file__).resolve().parents[4]
assert platform.system() == "Linux" and platform.machine() == "x86_64", "oracle build verified Linux/x64 only"
package = root / "node_modules/@discordjs/opus"
deps = package / "deps"
output = Path(args.output).resolve()
assert output != package.resolve() and package.resolve() not in output.parents
output.mkdir(parents=True, exist_ok=True)
gyp = ast.literal_eval((deps / "binding.gyp").read_text())
target = gyp["targets"][0]
assert target["target_name"] == "libopus"
sources = [deps / p for p in target["sources"]]
includes = [deps / "config/opus/linux/x64", deps / "opus/include", deps / "opus/celt", deps / "opus/silk", deps / "opus/silk/float"]
library = output / "libopus.so.0"
commands = [[os.environ.get("CC", "cc"), "-O2", "-fPIC", "-shared", "-std=gnu99", "-DNDEBUG", "-DHAVE_CONFIG_H", "-DPIC", *[f"-I{p}" for p in includes], *map(str, sources), "-lm", "-Wl,-soname,libopus.so.0", "-o", str(library)]]
for name, lib in [("bundled", library), ("system", Path(args.system_opus).resolve())]:
    destination = output / name
    destination.mkdir(exist_ok=True)
    commands.append([os.environ.get("CXX", "c++"), "-O2", "-std=c++17", "-fPIC", "-shared", "-DNAPI_VERSION=3", "-DNAPI_DISABLE_CPP_EXCEPTIONS", f"-I{Path(args.node_headers).resolve()}", f"-I{root / 'node_modules/node-addon-api'}", str(package / "src/node-opus.cc"), str(lib), f"-Wl,-rpath,{output if name == 'bundled' else lib.parent}", "-o", str(destination / "opus.node")])
for command in commands:
    print("Building", command[-1], flush=True)
    subprocess.run(command, check=True, timeout=180)
hashes = {}
for p in [package / "package.json", package / "src/node-opus.cc", package / "src/node-opus.h", deps / "binding.gyp", deps / "config/opus/linux/x64/config.h", *sources, deps / "opus/include/opus.h", deps / "opus/include/opus_defines.h"]:
    hashes[str(p.relative_to(package))] = hashlib.sha256(p.read_bytes().replace(b"\r\n", b"\n")).hexdigest()
receipt = {"package": json.loads((package / "package.json").read_text())["version"], "hashes": hashes, "commands": commands, "outputs": {str(p.relative_to(output)): hashlib.sha256(p.read_bytes()).hexdigest() for p in [library, output / "bundled/opus.node", output / "system/opus.node"]}}
(output / "build-receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
print(json.dumps({"output": str(output), "receipt": str(output / "build-receipt.json")}))

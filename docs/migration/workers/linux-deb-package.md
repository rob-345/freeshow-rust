# Unsigned Linux Debian package review

This is a private local artifact built from `834e9da3`, with the unchanged normal
Svelte frontend, the release/custom-protocol native binary, and the normal Tauri
Debian bundler. It is neither an Electron package nor a diagnostic frontend.
Package version `0.1.0` and runtime `1.6.7-beta.2-rust.1` retain the existing two
version seams. No signing, system installation, upload or external release is
performed.

## Build boundary

`config/packaging/linux-deb.json` is a separate bundler override; the shared
Tauri config, Cargo manifests, main entrypoint and root target stay untouched.
`prepare-linux-deb.mjs` creates a private resource/notice stage and a second
override. It pins the existing Linux x86_64 LTC resource by its previously
verified SHA-256, checks its ELF machine, and copies held bytes. Nothing selects
a codec from a renderer path. The executable continues using its normal
`resource_dir()/codecs/libltc.so.11` lookup.

The stage includes GPLv3, available Rust/Node root license and notice texts,
existing application dependency notices, the libltc LGPL/GPL texts and header,
and a conservative dependency inventory. This includes build dependencies as a
superset, not a claim that every listed dependency runs in the package.
Inventory limits: 8,192 notice files, 2 MiB per file, 64 MiB aggregate, 2,048
Cargo packages, and 4,096 locked Node entries. Missing installed root notices
remain explicit. No Node/Electron executable or `node_modules` tree is bundled.

GTK/WebKit/ALSA/Opus are system dependencies. Opus is explicitly declared because
the native encoder loads it dynamically and ELF dependency scanning cannot
discover it. Media codec recommendations retain the system GStreamer boundary.
The initial supported packaging environment is Ubuntu 24.04 x86_64; the override
conservatively requires glibc 2.39. A successful local launch does not establish
compatibility with older Debian/Ubuntu. Tauri likewise recommends building on
the oldest intended distribution to control the glibc floor.
([Tauri Debian documentation](https://v2.tauri.app/distribute/debian/))

## Reproduction

Keep the target/tool caches on `/mnt/data`, separate from the coordinator and
frozen performance binaries. The first build used a by-value registry copy;
there are no hard-linked mutable cache files. With the installed Node 22.21.1
first on `PATH` and the available native header pkg-config environment:

```sh
export FREESHOW_PACKAGE_ROOT=/mnt/data/cache/freeshow-rust-build/linux-deb-package
export CARGO_HOME="$FREESHOW_PACKAGE_ROOT/cargo-home"
mkdir -p "$CARGO_HOME"
cp -a --reflink=auto /home/rob-admin/.cargo/registry "$CARGO_HOME/registry"
config/packaging/build-linux-deb.sh
```

The wrapper uses one Cargo job, disables incremental compilation, retains the
normal release profile, and passes `--locked --offline` to Cargo. No npm install
or shared dependency mutation occurs. Bundling uses `--ci --no-sign --bundles
deb` with the two overrides; the ordinary `beforeBuildCommand` builds the
production UI and the three unchanged browser clients.

## Artifact verification and actual runtime

`tests/desktop/package_linux/verify.py` uses `dpkg-deb --field/--extract`, never a
package installer. It requires a fresh extraction directory under `/mnt/data`.
It verifies architecture/version/dependencies, a runnable ELF, icon/desktop
entry, exact GPL/LTC bytes, notice inventory, and every packaged remote resource
against the normal build output. It checks all ELF dependencies resolve from
the installed system, no private RPATH/RUNPATH, and the required glibc symbol
floor. For this trusted local-bundler artifact, checks cap verified extracted
content at 10,000 files/512 MiB, with a 256 MiB package input and declared
installed-size precheck; this is not an untrusted archive extraction API. A
sorted path/mode/hash digest identifies the extracted content.

`run.py` launches that exact extracted release executable via a private
WebDriver, isolated Xvfb/profile/ports and a private virtual Pulse sink. Run it
under `dbus-run-session` to isolate desktop/audio bus ownership. Profiles use a
short unique path below `/mnt/data/cache/fspkg` so native Unix socket paths stay
short, independently of the artifact directory. It
observes the original editor, original `VERSION` callback, actual native
remote GET/HEAD index resources, and existing controlled source LTC commands.
The LTC probe requires real construction, typed encoded audio, decode frames,
and matching create/free counters. `FREESHOW_RUST_LTC_LIBRARY`, `APPDIR` and
private loader overrides are absent. No module/store/API or codec stub is
installed. Original `MAIN.CLOSE` must exit the actual application PID before
fixture termination, followed by real driver/Pulse joins and an empty owned
child list. A virtual sink is not hardware or physical delivery evidence.

The runner limits WebDriver requests to 256, responses to 1 MiB, and the overall
interactive phase to 120 seconds, with short per-observation deadlines. The
existing bounded timecode probe is reused without modification. Receipts keep
the actual values and errors; no performance comparison is made.

## Actual artifact and evidence

The actual unsigned `FreeShow Rust_0.1.0_amd64.deb` is 15,844,430 bytes at:

```text
/mnt/data/cache/freeshow-rust-build/linux-deb-package/target/release/bundle/deb/FreeShow Rust_0.1.0_amd64.deb
```

SHA-256: `aa80e5c28f2d57a0a5764743884504fb727097eda01a2a31a068183ce1ce93d7`.
The stripped release executable is
`6a1a37b487f146a2420a8dc20b6707d51f938a2e51f14b17fc65806f603c575d`.
The package is `free-show-rust`, architecture `amd64`; actual control dependencies
include GTK3/WebKit4.1, glibc >=2.39, ALSA and Opus. There are 1,659 regular
files/37,169,493 bytes, including 20 exact remote resources and only two ELFs
(application and LTC). The 1,635 documentation files include an inventory with
1,695 bounded notice-copy entries, including duplicate dependency copies. The
maximum referenced glibc symbol is 2.39. Resources live below
`usr/lib/FreeShow Rust`, matching Tauri's product-name-based resource lookup;
custom notices are below `usr/share/doc/freeshow-desktop`.

Two actual extracted-package GUI runs passed, each with 15 WebDriver requests.
The durable latest run rendered the ordinary editor and version, made six
real GET/HEAD requests against all three native loopback bindings, and used
the source LTC commands to construct/free one decoder and three encoders with
24 real typed callbacks. Original CLOSE exited the actual application PID
before fixture termination. Driver/Pulse/Xvfb joined and owned children were
empty. Observed independent startup gaps at this base were `DEVICE_ID` and
`PROVIDER_CONNECTIONS`; there were no observed browser errors. Desktop portal,
headless display, and unavailable PipeWire warnings remain in the raw launch
log; file dialogs and physical capture/playback were not certified.

The second passing run additionally observed the real native loader. The
actual process had no override variables, and its executable matched the
extracted binary. The canonical packaged resource's device/inode/hash were
recorded. Its loader copies verified bytes into a sealed memfd, so `/proc/maps`
correctly names that memfd rather than the original resource path. Held FD 76
had all four seals (15), 75,888 bytes and the exact packaged LTC hash. Five
actual mappings, including executable pages, matched the held snapshot's
device `00:01` and inode `782436`. Source hashes document the only two path
choices: an explicit environment override, otherwise the packaged resource;
there is no installed/system or Cargo-manifest fallback. This combines actual
mapped bytes with the verified source selection, rather than assuming that a
successful encoder alone establishes origin.

[Durable raw receipt](../../../tests/desktop/package_linux/receipt.json)
contains build identities/logs, package metadata/resource hashes, actual GUI
observations, native mapping evidence and prior failed fixture receipts.
Receipt SHA-256:
`9e903b3a141e7d876c8b5102f0115e02c30ac68cca7891d4272c81ddfeab22d5`.
Earlier failures are preserved: the initial long-profile/shared-D-Bus fixture
timed out before a WebDriver session; another run reached the editor but a
Python helper's `http` name shadowed its module. Both were fixture corrections,
with no package/runtime change. A pre-launch profile-root permission failure
and the first successful run also remain in the external artifact directory.

Commands for the actual extraction/GUI boundary:

```sh
python3 tests/desktop/package_linux/verify.py "$DEB" "$FRESH_EXTRACTED" "$ARTIFACTS/verification.json"
dbus-run-session -- env WEBKIT_WEBDRIVER=/home/rob-admin/.local/share/freeshow-native/usr/bin/WebKitWebDriver \
  python3 tests/desktop/package_linux/run.py "$ARTIFACTS/verification.json" "$ARTIFACTS/gui"
node tests/desktop/package_linux/record.mjs "$ARTIFACTS"
```

Static Python/JavaScript/shell syntax and configured Prettier checks passed.
There was no system package installation, binary download, upload, signing,
performance sample or root/shared-source modification.

## External release gaps

The pinned `libltc-wrapper` tree identifies the exact installed binary but does
not record its source/build revision. The installed 75,888-byte ELF matches
the exact pinned upstream Git blob `b3ed83fb52084710a49869a6448168da04e9d9a9`
(the GitHub tree entry and locally recomputed Git blob hash agree). Its header version is insufficient to
prove the binary's complete corresponding source. The pinned wrapper declares
MIT in package.json but has no standalone license text. These are unresolved
release audit gaps, despite included LGPL texts and existing provenance notices.
([Pinned wrapper repository](https://github.com/bitfocus/libltc-wrapper/tree/9637e39773317e82e4112ff42b2c63afd144549f))

The unchanged frontend contains CMG Sans font assets without accompanying font
license text. Their external redistribution authorization and a complete
release dependency/corresponding-source audit remain open. This local package
is not represented as an externally distributable compliant release. Native
device availability, broad codec support, playback/capture and whole migration
parity remain separate from the normal editor/package checks.

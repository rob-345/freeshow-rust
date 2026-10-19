# Bounded JPEG cover decoding and source evidence

`freeshow-cover-images` now decodes a narrow baseline JPEG subset on verified Linux x64 GdkPixbuf 2.42.10. All fifteen actual source fixtures match decoded RGBA through real service sessions and PNG output. The existing Rust JPEG candidate remains a measured mismatch and is not used for decoding. PNG compressed bytes and the optional alternate resize/JPEG encoder retain their existing explicit differences. Fixed workers, whole-session quotas, context authority, cancellation, and joins remain unchanged.

## Actual source and result

The oracle reads immutable `2d94defcf98175e4f5c9504e7f4fd1d33c35c975:src/electron/audio/nowPlaying.ts`, normalizes CRLF, and extracts the actual `setPlayingState`, `convertDynamicValues`, `getArtist`, and `dynamicValues` declarations. It executes those unchanged declarations in the packaged Electron main realm, with real `nativeImage` and controlled metadata/path/file/title callbacks. Audio parsing, Icecast delivery, and filesystem writes are stubs for this image experiment; the separate extraction tests open real MP3/FLAC files with the actual parser. No source success ACK is invented.

The verified executable is the untouched FreeShow 1.6.7-beta.2, SHA-256 `eb3263aad96ce7c35df3db4516bb04c67bdc6c2f30ae8ebc0cb5649783c82a15`, Electron 37.10.3 / Node 22.21.1 on Linux x64. The receipt records runtime versions, immutable source/declaration hashes, measurement tool/example hashes, every actual JPEG input, losslessly compressed source RGBA, exact source/candidate pixel hashes, PNG hashes, and source cover-byte observations. Each source PNG written by the actual function is decoded again by real nativeImage and checked against the original source bitmap; decoded pixels and compressed PNG equality stay separate.

Fifteen fixtures cover generated solid and textured baseline JPEGs, actual generated standard sRGB ICC versus ICC removed, minimal EXIF orientation 2/6/8, and pinned Rust-encoded single-component grayscale and three-component RGB. The candidate is existing `image@0.25.10` with `zune-jpeg@0.5.15`, not a JavaScript or FFmpeg substitute.

| Input                            | Different pixels | Maximum channel difference |
| -------------------------------- | ---------------: | -------------------------: |
| Textured 9×5                     |            22/45 |                          2 |
| Textured 17×11                   |           87/187 |                          2 |
| Textured 64×48                   |        1462/3072 |                          3 |
| Single-component grayscale 17×11 |            1/187 |                          1 |
| Rust-encoded RGB 17×11           |            5/187 |                          2 |

Removing the actual generated ICC profile leaves exactly the same decoded source pixels and candidate mismatch in the checked cases. Thus rejecting ICC alone does not establish a correct generic subset. Uniform red 17×11 and 641×481 have equal decoded pixels, but that narrow observation does not certify other JPEGs. All fifteen candidate PNG compressed hashes differ from the actual source PNG hashes, including the matching uniform rasters. The investigation does not attribute the difference to a specific IDCT, upsampler, color-conversion step, or library defect without further evidence.

The candidate PNG hashes in the receipt describe its original standalone build. They are historical evidence, not stable fingerprints across Cargo feature graphs: the integrated workspace enables flate2's zlib-rs backend through zip, while the standalone build used miniz_oxide. This changed a candidate compressed PNG hash without changing decoded RGBA or its exact source mismatch. Tests retain the original receipt, enforce exact raw pixels/deltas, independently verify candidate PNG chunk CRCs, framing, inflate size and filters, and require that PNG to reconstruct the actual candidate raster. Compressed source PNG inequality remains explicit; no tolerance or source pixel expectation is relaxed.

On these actual nativeImage buffer calls, EXIF orientation 2/6/8 leaves 17×11 dimensions and the same bitmap as the corresponding untagged image. A future decoder must preserve that observed API behavior rather than automatically rotate it. This does not establish arbitrary EXIF, ICC/P3, CMYK/YCCK, progressive/arithmetic, malformed-recovery, grayscale-precision, or cross-platform contracts.

The unchanged source writes the decoded image as PNG before formatting artwork. At or below 640×480, the artwork data URL retains the exact original JPEG bytes. For the 641×481 uniform input, it uses the actual source resize→JPEG90 output at 640×480 for that data URL, while the written PNG still has the original raster. The probe observes source title→cover path→PNG write→two formatting cover-path calls→text write order. It does not claim native resize or JPEG encoding parity.

## Evidence-only candidate bounds

The first two evidence commits did not enable a production decoder. Their receipts intentionally retain `productionDecoderEnabled: false`. The subsequent native path described below uses their measured matching consumer, rather than changing source pixel expectations to fit the Rust candidate.

`examples/cover-jpeg-candidate.rs` is a standalone controlled-corpus measurement program, never an app command or service codec. It caps input, baseline header count, 8-bit component forms and dimensions, uses the pinned real decoder, and projects opaque RGBA plus a real PNG. The example's upstream best-effort decoder allocation limit and whole-call work are **not** a production allocation/cancellation audit. Its temporary process/CPU ownership is test infrastructure; it does not add application threads or per-request subprocesses. Enabling a new codec would require its own allocation/CPU bounds and cancellation review on the existing fixed owners.

## Reproduction and checks

Build the existing `freeshow-cover-images-parity` binary and new `cover-jpeg-candidate` example, then run Node 22.21.1:

```sh
FREESHOW_COVER_IMAGES_PARITY=/absolute/target/debug/freeshow-cover-images-parity \
FREESHOW_JPEG_CANDIDATE=/absolute/target/debug/examples/cover-jpeg-candidate \
node --test tests/parity/cover-jpeg.test.mjs
```

The ordinary test suite needs no installed Electron. It verifies the original receipt/provenance, computes real current candidate pixels against that source, checks exact Linux x64 candidate mismatch measurements without tolerance, and independently checks encoded PNG pixels. On Linux x64 it exercises real service JPEG→PNG pixels under both policies and real parser MP3/FLAC→first-picture→create→PNG on the same session, including actual worker joins. Unsupported native platforms explicitly assert JPEG rejection. Other platforms still exercise candidate dimensions/opacity and observable mismatch, without claiming an original runtime there. Existing PNG source/lifecycle suites remain required.

To regenerate the actual original receipt, use the explicit packaged oracle with isolated temporary profile/data/XDG paths:

```sh
FREESHOW_ELECTRON_BIN=/absolute/verified/opt/FreeShow/freeshow \
FREESHOW_JPEG_CANDIDATE=/absolute/target/debug/examples/cover-jpeg-candidate \
FREESHOW_JPEG_RECEIPT=crates/freeshow-cover-images/fixtures/jpeg/electron-linux.json \
xvfb-run -a node crates/freeshow-cover-images/tools/jpeg-reference.mjs
```

The tool verifies the packaged versions, removes its private profile on exit, bounds candidate subprocess output/deadline, and never mutates `node_modules`. The committed receipt is roughly 90 KiB, containing fixture bytes and evidence rather than an executable.

## Installed native consumer follow-up

The separate explicit `tools/jpeg-system-probe.py` consumes that same immutable-source fixture receipt using installed native software. It never invokes an application decoder fallback or installs dependencies. `/usr/bin/python3` with existing GObject introspection/GdkPixbuf 2.0 support is required for this optional experiment; ordinary Node tests validate its committed receipt without requiring Python, GdkPixbuf, or `djpeg` on that test machine.

The local GdkPixbuf 2.42.10 default JPEG buffer loader matches all fifteen source decoded RGBA hashes and dimensions exactly. The loader is used without its optional embedded-orientation transform, matching the measured source behavior. The probe records actual `/proc/self/maps` identities and SHA-256: its compiled consumer loads only `libjpeg.so.8.2.2` (Ubuntu `libjpeg-turbo8` 2.1.5-2ubuntu2), not libjpeg9. This is genuine libjpeg-turbo-through-consumer evidence. It does not establish direct C ABI declarations, decoder initialization/default controls, allocations, cancellation, or arbitrary JPEG parity.

Local `jpeglib.h` development headers and the libjpeg pkg-config entry are absent. The experiment does not guess `jpeg_decompress_struct` layout. GdkPixbuf's public loader interface exposes no DCT/upsampling switches. Installed `djpeg`, independently linked to **libjpeg9e**, provides the controlled switch comparison below; its results must not be attributed to turbo:

| Actual libjpeg9e settings | Exact fixtures |
| ------------------------- | -------------: |
| Integer DCT, fancy        |           6/15 |
| Integer DCT, no smoothing |           6/15 |
| Fast DCT, fancy           |           4/15 |
| Fast DCT, no smoothing    |           4/15 |

Reproduce this bounded local experiment with:

```sh
/usr/bin/python3 crates/freeshow-cover-images/tools/jpeg-system-probe.py \
  crates/freeshow-cover-images/fixtures/jpeg/system-linux.json
```

Every controlled decoder invocation has a finite input corpus, dimensions/byte checks, and a subprocess deadline. The receipt pins the source receipt, probe code, loaded consumer/jpeg libraries, actual installed package versions, and CLI executable/library identities. These consumer measurements preceded the bounded native path below. No system library or binary packaging is solved by a probe receipt.

## Native supported subset

`create` dispatches JPEG bytes to private `jpeg.rs`; all existing public session APIs stay the same. The legacy `ImagePolicy::VerifiedPngPixels` name now admits the checked JPEG subset as well as its existing PNG subset. JPEG admission does not opt into native alternate resize/JPEG encoding. Large covers still require that separate explicit policy for `resize_jpeg90`, and PNG replies still carry `native_png_encoding_differs`.

Preflight occurs before constructing a native image loader. It enforces ≤4 MiB encoded data, ≤4096 per side, ≤4M pixels, ≤256 segments, one SOF0 frame, 8-bit grayscale or JFIF YCbCr 4:4:4/4:2:0, component/table identities from the actual corpus, 8-bit quantizers, bounded canonical Huffman tables, and one interleaved baseline scan. A bounded Huffman pass validates the exact declared MCU/block count, coefficient runs, byte stuffing, complete entropy, canonical pad bits and final EOI. This prevents Gdk's tolerated truncated-entropy recovery from being published as certified source pixels. Cooperative checks occur during marker scanning, every 64 KiB of entropy framing, and every 256 MCUs.

Metadata is deliberately narrow: exactly the observed JFIF 1.01/1.02 no-thumbnail header, absent ICC or the exact single source-generated standard sRGB profile, absent EXIF or the exact measured minimal little-endian orientation 2/6/8 payload. No orientation transform is applied. Other ICC/P3, arbitrary EXIF, CMYK/YCCK/RGB component-ID variants, progressive/arithmetic/multiple scans, restart intervals, other sampling, 12/16-bit forms, comment/unknown metadata and malformed streams are explicit unsupported/invalid/resource errors. Some valid source JPEGs remain outside this subset. No tolerant empty-image success is substituted for an unsupported/corrupt input.

Only Linux x86_64 is enabled. The native runtime version must be GdkPixbuf 2.42.10. Three immutable `const guint` version globals use the exact public scalar API from `gdk-pixbuf-features.h`; no native struct layout is guessed. The maintained pinned `gdk-pixbuf=0.18.5` binding owns all opaque loaders, images and pixel buffers. Each existing worker verifies two tiny source-reference JPEGs (textured 4:2:0 and grayscale) before its first decode; a missing/mismatching consumer returns `cover_unsupported`. Interrupted or resource-failed verification does not poison the worker capability. A genuine mismatch remains unavailable until that worker exits. These sample canaries guard an installed provider; they are not exhaustive pixel proof for every possible JPEG or another operating system.

Bytes are fed in ≤32 KiB synchronous writes. An RAII guard closes the actual native loader on success, decode error and cooperative cancellation. The code copies bounded RGB rows from `read_pixel_bytes` into owned opaque RGBA, checking dimensions, rowstride, color/alpha and actual buffer length against preflight. GObject/image/pixel-buffer ownership stays on the same worker and ends before the operation returns. No task/thread/runtime or application image path is passed to Gdk. Its trusted module/loader cache and native resources may perform filesystem reads; this is not a zero-filesystem-read claim.

## Native resources and cancellation

The same two fixed owners and four whole 128 MiB reservations remain. JPEG preflight uses small fixed Huffman arrays; each entropy bit comes from the ≤4 MiB input and block/MCU work is bounded by dimensions and admitted sampling. The code checks actual rows and produces at most 16 MiB RGBA. At the maximum raster, old/new RGBA consume at most 32 MiB, native RGB plus a possible pixel-byte copy about 24 MiB, and encoded input ≤4 MiB. Admitted marker data is tiny; existing metadata/picture/output buffers remain within the original 128 MiB session reservation. Native allocator/module/global overhead is outside that reservation, as with the existing codec accounting.

The upstream [Gdk 2.42.10 JPEG loader](https://raw.githubusercontent.com/GNOME/gdk-pixbuf/2.42.10/gdk-pixbuf/io-jpeg.c) has a 64 KiB input buffer and allocates output at decoded dimensions; stop destroys its decoder/context. Restricting one interleaved baseline scan avoids the full-image coefficient mode in [libjpeg-turbo 2.1.5](https://raw.githubusercontent.com/libjpeg-turbo/libjpeg-turbo/2.1.5/jdcoefct.c). Source code also shows why truncated entropy must be rejected before native stop. The [loader lifecycle](https://raw.githubusercontent.com/GNOME/gdk-pixbuf/2.42.10/gdk-pixbuf/gdk-pixbuf-loader.c) remains synchronous.

Cancellation/deadlines are cooperative between Rust preflight blocks, native writes and copied rows. A native write/close, module load, filesystem operation, allocator or final thread destructor cannot be forcibly preempted. Dropping an accepted caller does not release its native loader or whole reservation early. The focused lifecycle test holds a real Gdk loader after an actual accepted write and before close, cancels/drops the caller, observes shutdown timeout with the owner still active/reserved, releases the gate and requires real close, both OS-thread joins and reservation zero. The existing PNG/queued/caller-drop/panic/retirement tests remain intact. This is ownership and bounded-work evidence, not a whole-app performance or universal native-memory peak claim.

## Dependency and host integration

The only new dependency is target-specific `gdk-pixbuf = "=0.18.5"` for Linux x64 (cached sys 0.18.0, glib 0.18.5, gio 0.18.4). Linux build/runtime provision must supply its native GdkPixbuf/GLib libraries and JPEG consumer; Ubuntu's `libgdk-pixbuf-2.0-dev` provides the pkg-config build entries. The runtime gate remains stricter than the binding's minimum 2.36.8. Coordinator-owned workspace/native lockfiles and CI packages must be updated separately; no shared file is changed here.

The verified distro/runtime is Ubuntu 24.04 with GdkPixbuf 2.42.10 (`2.42.10+dfsg-3ubuntu3.3` locally), GLib 2.80.0, and libjpeg-turbo 2.1.5. Linux CI exercising successful JPEG decoding must provide that runtime version and the development pkg-config package. A different GdkPixbuf version, missing JPEG module or differing source canaries returns explicit `cover_unsupported`; there is no approximate fallback to image/zune. Another security patch with the same API version still has to pass the real pixel canaries. Other Linux distributions/architectures are not automatically certified by these Ubuntu receipts.

Local checks reuse the existing extracted native development files, without installs:

```sh
PKG_CONFIG_SYSROOT_DIR="$HOME/.local/share/freeshow-native" \
PKG_CONFIG_PATH="$HOME/.local/share/freeshow-native/usr/lib/x86_64-linux-gnu/pkgconfig" \
cargo test -p freeshow-cover-images --all-targets
```

The production driver keeps its original name. Worker verification uses a temporary standalone manifest naming the driver `freeshow-cover-jpeg-service-parity`, so it cannot overwrite the coordinator's pre-integration binary during concurrent source checks. The old `tools/electron-reference.mjs` now verifies its actual generated JPEG on the enabled platform instead of retaining the former expected JPEG rejection; historical receipts are not rewritten. Windows/macOS/other architectures remain buildable with explicit unavailable JPEG behavior and no new Gdk dependency there.

Final focused validation: 18 Rust tests, ten repeated full native runs (180 passes), eight JPEG and ten existing cover/image Node suites, strict Clippy and all-target compilation for Linux/Windows GNU/macOS ARM, rustfmt/Prettier/diff checks. The actual packaged reference rerun preserves all nine previous PNG bitmap comparisons and verifies one additional genuinely generated 2×1 JPEG through native create→PNG with exact source bitmap equality. Its local receipt is `/tmp/freeshow-cover-jpeg-native-electron.json`; the committed fifteen-fixture immutable-source receipt remains unchanged.

Coordinator integration now includes both lockfiles and the Linux core CI
GdkPixbuf build package. The rebuilt actual Linux editor verifies two unchanged
MP3 APIC/FLAC PICTURE files against an actual packaged SET/UNSET recording, with
exact installed PNG RGBA hashes and explicit unequal compressed bytes. The
[desktop evidence](jpeg-desktop-evidence.md#coordinator-integration-evidence)
and [local receipt](../baselines/jpeg-desktop-linux.json) distinguish real GUI
observations from the independent decoder/ownership tests. The newer complete
CI run remains required.

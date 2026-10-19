# JPEG cover decoding investigation

Production `freeshow-cover-images` remains PNG-only. The pinned Rust JPEG decoder does not reproduce the actual source pixels on ordinary baseline JPEGs. This evidence slice adds only a test candidate, an explicit packaged-source oracle, a small fixture receipt, and focused tests. It does not add a decoder dependency or change service policy, workers, reservations, cancellation, disposal, or joins.

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

On these actual nativeImage buffer calls, EXIF orientation 2/6/8 leaves 17×11 dimensions and the same bitmap as the corresponding untagged image. A future decoder must preserve that observed API behavior rather than automatically rotate it. This does not establish arbitrary EXIF, ICC/P3, CMYK/YCCK, progressive/arithmetic, malformed-recovery, grayscale-precision, or cross-platform contracts.

The unchanged source writes the decoded image as PNG before formatting artwork. At or below 640×480, the artwork data URL retains the exact original JPEG bytes. For the 641×481 uniform input, it uses the actual source resize→JPEG90 output at 640×480 for that data URL, while the written PNG still has the original raster. The probe observes source title→cover path→PNG write→two formatting cover-path calls→text write order. It does not claim native resize or JPEG encoding parity.

## Bounds and ownership

No production image code changes. The service retains two fixed workers, four whole sessions with 128 MiB reservations, a 4 MiB encoded limit, a 4M-pixel limit, and its existing accepted-work cancellation/retirement/actual-join contract. JPEG `create` still returns `cover_unsupported` under both existing policies; extracting and publishing raw embedded JPEG picture bytes remains supported independently.

`examples/cover-jpeg-candidate.rs` is a standalone controlled-corpus measurement program, never an app command or service codec. It caps input, baseline header count, 8-bit component forms and dimensions, uses the pinned real decoder, and projects opaque RGBA plus a real PNG. The example's upstream best-effort decoder allocation limit and whole-call work are **not** a production allocation/cancellation audit. Its temporary process/CPU ownership is test infrastructure; it does not add application threads or per-request subprocesses. Enabling a new codec would require its own allocation/CPU bounds and cancellation review on the existing fixed owners.

## Reproduction and checks

Build the existing `freeshow-cover-images-parity` binary and new `cover-jpeg-candidate` example, then run Node 22.21.1:

```sh
FREESHOW_COVER_IMAGES_PARITY=/absolute/target/debug/freeshow-cover-images-parity \
FREESHOW_JPEG_CANDIDATE=/absolute/target/debug/examples/cover-jpeg-candidate \
node --test tests/parity/cover-jpeg.test.mjs
```

The ordinary test suite needs no installed Electron. It verifies the committed original receipt/provenance, computes real current candidate pixels against that source, checks exact Linux x64 mismatch measurements without tolerance, exercises actual service JPEG rejection and real worker joins, and compares original/parser/native raw JPEG extraction from controlled tagged MP3/FLAC files. Other native platforms still exercise candidate dimensions/opacity and observable mismatch, without claiming an actual original runtime there. Existing PNG source/lifecycle suites remain required.

To regenerate the actual original receipt, use the explicit packaged oracle with isolated temporary profile/data/XDG paths:

```sh
FREESHOW_ELECTRON_BIN=/absolute/verified/opt/FreeShow/freeshow \
FREESHOW_JPEG_CANDIDATE=/absolute/target/debug/examples/cover-jpeg-candidate \
FREESHOW_JPEG_RECEIPT=crates/freeshow-cover-images/fixtures/jpeg/electron-linux.json \
xvfb-run -a node crates/freeshow-cover-images/tools/jpeg-reference.mjs
```

The tool verifies the packaged versions, removes its private profile on exit, bounds candidate subprocess output/deadline, and never mutates `node_modules`. The committed receipt is roughly 90 KiB, containing fixture bytes and evidence rather than an executable. A native libjpeg-turbo/system-libjpeg comparison is a separate next investigation, not an enabled capability or bundled distribution claim.

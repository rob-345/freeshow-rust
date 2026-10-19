# Desktop JPEG now-playing evidence and coordinator integration

`tests/desktop/jpeg_now_playing_oracle.mjs` adds two deterministic real tagged files for the unchanged `now_playing_probe.js`. It does not alter that probe, the existing oracle, production code, or the GUI controller. Ordinary fixture/replay/verification modes do not import Playwright or launch Electron. The separate, create-only `--record-original` mode launches the official packaged application through the existing disposable-profile helper.

The new recording is `tests/desktop/jpeg_now_playing_original.json`, SHA256 `8155b7815489c80428fbb5c7af103f0469f44d37c254db939c62f660d1796d5f` (46,274 bytes). It contains actual original `setPlayingState` and `unsetPlayingAudio` executions, including actual music-metadata 7.14.0, Electron nativeImage JPEG decode/toPNG, typed metadata snapshots, title/file effect order, exact PNG bytes, pixel hashes, and void returns. The extracted immutable source and its declarations are hashed after CRLF normalization. Package contents, input files, earlier image receipt, recorder, executable, and runtime are identified separately. No migrated GUI execution is claimed by this receipt.

| Case        | Real file                                                                                           | First JPEG                                | Source PNG pixels                                                              | Other evidence                                                                                                                                     |
| ----------- | --------------------------------------------------------------------------------------------------- | ----------------------------------------- | ------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `jpeg-apic` | ID3v2.4 MP3, 4,387 bytes, SHA256 `138bc57aeeb29c543d44abaa0038d62fedcc373edeb7ed6f9fa9df727ceb32f8` | `texture-17x11-no-icc`, 17×11             | RGBA SHA256 `16aaaea16ec707cdfd1a7a963e6bafe1737a81dfdbfd0992002f23ac9a3615ce` | Two genuine APIC pictures; first type 255 becomes own `undefined`; second grayscale JPEG remains published; Unicode and two artists; duration `-0` |
| `jpeg-flac` | FLAC, 2,992 bytes, SHA256 `838f737604556ce2bec38d91f9511b0f53d218e0d887e86f4157e84ed97a4dc3`        | `texture-64x48`, 64×48, measured sRGB ICC | RGBA SHA256 `4d638d0d9992d7aa73451ba3c95f76249e049f4308002a8e251d87a21730a177` | Genuine PICTURE fields/data and repeated Vorbis artist tags; Unicode; duration `Infinity`                                                          |

Both first images come unchanged from the existing immutable packaged JPEG receipt. They are below the source resize threshold. Their actual newly recorded PNG pixels exactly match that earlier receipt. The unknown APIC _picture type_ is deliberate; it does not imply an unknown JPEG variant is supported. Unsupported JPEG layouts, other decoder versions/platforms, resize, physical playback, title transmission, and compressed PNG equivalence remain outside this evidence.

## Recording and replay distinction

The original recording ran on Linux x64, official executable SHA256 `eb3263aad96ce7c35df3db4516bb04c67bdc6c2f30ae8ebc0cb5649783c82a15`, Electron 37.10.3, Node 22.21.1. One source factory was used for both cases. Only infrastructure dependencies were controlled: the genuine parser and nativeImage were supplied; original title calls were observed rather than sent to a server; fixed output paths used a real private directory; file helpers performed observed real reads/writes/deletes. This does not certify the original helper error paths or full application IPC. Each SET overwrote existing text/cover; its own real UNSET then emptied text and deleted cover without publishing metadata or returning a source ACK.

Only the known fixed cover path inside formatted text is projected to `${AUDIO_FOLDER}/NowPlayingCover.png`, after asserting exactly one occurrence. Input event file paths are represented as an explicit `input` projection. Output effect targets are sealed by the recorder to `text` or `cover`. Metadata strings and raw picture data receive no search/replace or synthesized values. Temporary profile paths are omitted from the receipt.

Ordinary `--replay` validates the receipt/input/provenance and executes the actual original metadata reader and formatter with the real current 7.14.0 parser. It compares the complete raw common value to the packaged recording, and compares formatted text to the canonical recorded full SET result. The small original first JPEG bytes supplied to the formatter are justified by the actual recorded source SET; nativeImage is never mocked. UNSET additionally executes the actual original function against fresh real controlled files and compares its void return, effects, and file disposition with the packaged recording. Results explicitly label recorded image evidence separately from current parser/formatter execution.

`--verify` reads the actual GUI's fixed `Audio/NowPlaying.txt` and `Audio/NowPlayingCover.png`. It checks exact source text and independently validates PNG CRCs, framing, dimensions, complete bounded zlib consumption, row lengths, and filters before comparing decoded RGBA with the actual original pixels. RGB/RGBA, 8-bit, noninterlaced PNGs within these two fixture dimensions are supported; other inputs fail explicitly. It records compressed-byte equality as an observation without asserting it. This avoids depending on the Rust feature graph's compression backend.

Metadata comparisons must keep the existing `now_playing_values` split: every ordinary root field, typed value, nested/picture field order, own `undefined`, and all picture bytes are compared; root `picture` insertion position remains excluded. The new recording does not prove that the native separate cover read preserves that original root insertion position.

## Minimal coordinator smoke.py changes

These are integration instructions; this worker has not edited or executed `smoke.py`.

1. Beside the existing fixture generation, invoke the new helper and append its cases **before** adding all now-playing paths to the controlled show's media grants:

    ```python
    jpeg_oracle = "tests/desktop/jpeg_now_playing_oracle.mjs"
    jpeg_fixtures = json.loads(subprocess.check_output(
        [node, jpeg_oracle, "--fixtures", str(temp / "jpeg-now-playing-fixtures")],
        cwd=ROOT, text=True, timeout=15))
    now_playing_fixtures["cases"].extend(jpeg_fixtures["cases"])
    ```

2. Keep the existing unchanged retained `now_playing_probe.js`, counter/context/idle/void/ACK assertions, send-time mutation, and exact typed metadata comparison. Select the replay helper per case:

    ```python
    oracle = jpeg_oracle if entry["imageCapability"] == "small-jpeg" else "tests/desktop/now_playing_desktop_oracle.mjs"
    expected = json.loads(subprocess.check_output(
        [node, oracle, "--replay", str(replay_file)], cwd=ROOT, text=True, timeout=15))
    ```

3. For `small-jpeg`, verify the actual native files independently after the existing metadata/text comparisons and before UNSET:

    ```python
    files = json.loads(subprocess.check_output(
        [node, jpeg_oracle, "--verify", str(replay_file)], cwd=ROOT, text=True, timeout=15))
    assert files["actualFilesystemRead"] and files["exactSourceRgbaVerified"]
    ```

    Keep `rootPicturePositionCompared: False` and `encodedPngParityClaimed: False` in the resulting desktop artifact. The current remote-cover checks can consume the actual installed fixed PNG normally; the JPEG helper itself makes no HTTP claim.

4. After the existing real retained UNSET and its no-event/no-ACK/drain assertions, write `cleared["script"]` to the replay file and run `--verify` with the new helper. This also runs the original UNSET oracle and checks native text empty/cover absent. Preserve the original script in the resulting artifact rather than substituting a predicted idle result.

The same fixture inputs may be reused in switched/ZIP library phases; each replay uses that phase's frozen actual `audioFolder`. No global path replacement or new media grant inference is necessary. The helper accepts only its exact two input identities and original recorded command data; unsupported command variants fail rather than claim image parity.

## Focused checks and reproduction

Completed checks:

- Node 22.21.1 `--self-test`: five explicit groups, including actual deterministic MP3/FLAC parser checks, unknown APIC own-undefined, exact input/command guards, IEEE duration restoration, malformed PNG CRC/truncation/trailing/dimension controls, recorded-source provenance, actual parser/formatter replay, real controlled UNSET, and file verification/tamper rejection. Its PNG files come from the archived source and are explicitly **not GUI evidence**.
- Actual authorized `--record-original`: two real SET/UNSET executions, both measured RGBA hashes equal the earlier source measurements; disposable profile/input/output directories and Electron/Xvfb processes removed.
- Prettier check of the new helper/note and `git diff --check`. No Rust build or migrated GUI launch.

Ordinary reproducible check, including absence of a local official executable:

```sh
FREESHOW_ELECTRON_BIN=/not-installed node tests/desktop/jpeg_now_playing_oracle.mjs --self-test
```

Explicit recording reproduction requires the official unchanged Linux executable, installed project dependencies/Playwright, an isolated display, and Node 22.21.1. It creates a **new** file and refuses to overwrite a recording:

```sh
xvfb-run -a /path/to/node-v22.21.1/bin/node tests/desktop/jpeg_now_playing_oracle.mjs --record-original /absolute/private/new-recording.json
```

Do not run this during reserved GUI/performance measurements. Ordinary CI requires only the committed recording and real parser dependencies; a missing official executable is neither a hidden dependency nor a skipped parity claim.

## Coordinator integration evidence

The rebuilt Linux custom-protocol editor now executes both unchanged fixtures
through real retained `window.api`. Native typed metadata and source text match;
`--verify` reads the actual installed files, independently decodes the PNGs and
matches both recorded RGBA hashes. Actual UNSET empties text/removes the cover,
with original void commands, no source ACK, admitted/completed advances and
ordinary idle. The existing HTTP probe serves each current cover through all
three ports and verifies its actual bytes. Compressed PNG bytes differ, and root
picture placement remains excluded explicitly.

The first run caught invalid ID3 timestamp text and multiple FLAC comment-block
parser differences before decoding. Separate bounded compatibility corrections
fixed those without modifying the fixtures or recording. The complete local run
passes 493 Rust, 563 source, 20 frontend and 352 native executions, plus 187 editor
steps, four menus and both drop scales. Fourteen cover phases make 141 actual HTTP
requests. All original folder-alert/blocker, library-switch, backup and real Quit
checks remain intact; the whole editor retains 55 expected diagnostics.
[Durable local receipt](../baselines/jpeg-desktop-linux.json) records executable,
original recording, pixel and complete artifact hashes. Native Windows/macOS and
the complete CI run remain separate acceptance work; no performance gain is
claimed.

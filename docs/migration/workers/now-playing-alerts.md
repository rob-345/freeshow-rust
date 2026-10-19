# Now-playing native alert delivery

`createNowPlayingBridge` preserves four command promises and now reserves up to twenty private completion promises across its entire UI lifetime. The native host's same four whole-operation budget can retain up to five publications per operation: one metadata publication and four actual folder-failure alerts. The UI reserves a completion before hydration or the retained callback, including reentrant publications, and holds it until the entered native `complete` promise actually settles. Context changes, source callback return, private native retirement completion and observer timeouts do not release those promises. A twenty-first publication reports `now_playing_completion_busy`, does not invoke the original callback, and does not enter another completion promise. The rejected sequence remains available for a later valid retry after capacity genuinely frees.

The private publication schema is:

```ts
type NowPlayingPublication = {
    event: { channel: string; message: any; listenerId?: string; context?: RuntimeContext }
    id: string
    sequence: string
    hydration?: AudioMetadataHydration
    alertKind?: "missingText" | "folderCreationFailed"
    alertFolder?: string
}
```

`AUDIO_METADATA` requires its restoration envelope and forbids own `alertKind`/`alertFolder` fields, including own undefined placeholders. `ALERT` forbids an own hydration field. `missingText` requires exactly `This does not exist!` and no own `alertFolder`. `folderCreationFailed` requires a native folder string and exactly `Error: Could not create folder at: ${alertFolder}!`. Unknown kinds, missing or mismatching fields, arbitrary alert text, changed prefix/suffix and malformed strings reject with a private negative completion and no source callback. Rejection does not commit the sequence. Invalid authority/schema before reservation does not create a completion call. Ordinary source delivery still receives only the original `{channel,data}` object; private fields never enter source data.

The folder has at most32,774 UTF8 bytes: the existing32,768-byte context path budget plus the six-byte separator/`Audio` suffix. The complete message has at most32,814 UTF8 bytes, conservatively covering the fixed source prefix/suffix. Both reject NUL and lone UTF16 surrogates. Validation first checks code-unit length, then counts UTF8 without allocating an encoded copy. The folder must end in exact `/Audio` or `\Audio`; case and all other spelling remain unchanged. The source can report literal newlines, punctuation, Unicode or Linux backslashes in a valid native path. The UI does not normalize slashes, resolve components, reconstruct UNC/device paths, infer a platform or join a filesystem path.

The coordinator must attach `alertKind` and `alertFolder` only from sealed native `AlertKind::{MissingText,FolderCreationFailed{folder:PathBuf}}` getters. The folder variant must come from the actual ordered failed EnsureFolder observation's fixed Audio folder, with its captured full RuntimeContext. It must never be populated from renderer request fields or inferred from arbitrary message text. That native constructor and accepted full context establish provenance and the context/Audio relationship. These strings are notifications, not grants, paths to open or filesystem authority. Matching the string/leaf in the UI alone is not proof a directory operation occurred or a path belongs to the current library. Native path/syscall limits may be stricter and must remain explicit host errors.

Existing full-context acceptance, process-wide monotonic sequence replay checks, frozen completion contexts, source void returns and retirement retry/error behavior are unchanged. Metadata completion remains independent of following source effects and alerts. Native operation/publication bounds and actual resource lifetime remain native responsibilities; the UI's twenty promises are not proof of host IO drain, physical delivery or five-publications-per-operation membership.

Seventeen Node suites execute the actual exported helper, wire encoder and metadata hydrator. Coverage includes all prior duration/metadata/context/retirement tests, twenty simultaneous and reentrant completion reservations versus four command reservations, mixed four metadata plus sixteen folder alerts in source order while all ACKs remain pending, private-field stripping, twenty-first rejection/retry, and old-context/terminal retirement timeouts with twenty lost completion responses retained until genuine settlement. The actual immutable `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` `files.ts::makeDir` executes with a controlled failing filesystem boundary to produce the reference alerts. Nine path cases cover Unicode, exact UTF8 boundary, literal Linux backslashes/newlines, POSIX root, Windows drive/mixed separators, UNC and extended Windows spelling. This verifies source message construction and UI handling, not Windows/macOS filesystem execution or a real GUI notification.

```sh
"$ORACLE_NODE" --test tests/parity/now-playing-bridge.test.mjs
npx tsc --noEmit --strict --skipLibCheck --target ES2022 --module ESNext --moduleResolution Node --lib ES2022,DOM src/frontend/rust/nowPlayingBridge.ts
npx prettier --config config/formatting/.prettierrc.yaml --check src/frontend/rust/nowPlayingBridge.ts tests/parity/now-playing-bridge.test.mjs docs/migration/workers/now-playing-alerts.md
git diff --check
```

No Cargo, application, GUI or profiling run is needed for this frontend slice. Shared bridge/main/native host/Cargo/CI wiring remains coordinator-owned.

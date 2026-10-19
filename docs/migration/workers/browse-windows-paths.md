# Bounded lexical Windows browse paths

This slice is based on sealed stat commit
`ddf6ff487024ace6e4dbc7f3f178f5fd60651535`. It adds only a pure helper module,
private driver, differential/unit tests, source-oracle helper, and this note.
Shared paths/engine/manifests and filesystem dispatch remain unchanged. It does
not enable Windows READ_FOLDER/GET_SIMILAR or claim native filesystem behavior.

## API and capability boundary

`crates/freeshow-browse/src/paths/windows.rs` is platform independent. Its
`parse(&str)` returns borrowed `Parts { root, dir, base, ext, name }`.
`basename`, `extension`, `stem`, and `parent` are borrowed-slice wrappers;
`join(parent, relative_name)` returns an owned normalized String. Every operation
returns a typed `PathError { code, message }` on a capability/resource violation.
There is no OS call, current-drive/cwd lookup, task, thread, cache, or authority.

The supported subset includes ordinary absolute drive-letter paths with either
separator and unrooted lexical relatives. Raw parsing retains drive case,
separator spelling, repeated separators in the parent, trailing separator
semantics, dots, and Rust-valid Unicode. Join handles dot/parent components,
clamps above an absolute root, preserves relative leading parents, normalizes
separators to backslash, and preserves the source terminal separator. Case and
extensions are not lowercased by these helpers.

Drive-relative, current-drive-rooted, UNC, device/verbatim prefixes and additional
colon/ADS spellings return `unsupported_windows_path`. Join's child must be
unrooted; a second absolute/rooted argument is explicitly unsupported even where
Node can return a lexical string. NUL returns `invalid_request`, despite pure
Node path functions allowing it, because the existing native browse contract
rejects it. Unpaired JavaScript surrogates are not representable as Rust UTF-8
strings. Other component spellings are lexical data, not Windows existence,
validity, device, symlink, or traversal authorization checks.

Input and output have fixed 32,768-byte bounds, with 4,096 lexical components.
Join caps aggregate input bytes and components before normalization; dot removal
does not evade the work bound. Join's temporary is at most one byte beyond the
input bound and its returned output is checked independently. Parsing allocates
no path copies. Join uses a bounded temporary, component vector and output;
all work is linear in bounded input. Nothing is silently truncated.

## Actual retained parent contract

Pinned `getParentFolderName` in `src/electron/utils/files.ts` uses
`parse(filePath).dir`. It does not call dirname. Thus `parent("foo") == ""`
matches the source; Node dirname's `"."` is a different contract and does not
establish a gap in the existing POSIX helper. The differential corpus records
both functions and tests this distinction explicitly.

The actual pinned `getExtension` uses `path.extname(...).substring(1).toLowerCase()`.
This module preserves exact raw extname; the coordinator retains responsibility
for the source's later case conversion. The test invokes the original source
function and checks that raw Rust extension plus the same JavaScript conversion
matches it. It does not certify Rust/Node Unicode lowercasing equivalence.

[Node22's path contract](https://nodejs.org/download/release/v22.21.1/docs/api/path.html)
provides `path.win32` independently of the host OS. That is the real executable
oracle here; no POSIX filesystem or Windows simulation supplies expected strings.

## Verification

Passed locally with pinned Node22.21.1:

```sh
PATH=/home/rob-admin/.local/share/freeshow-oracle-node/node-v22.21.1-linux-x64/bin:/home/rob-admin/.cargo/bin:$PATH \
  node --test tests/parity/windows-paths.test.mjs
```

Eight Node tests pass, including 8,192 deterministic generated parse/join cases,
crafted dot/root/trailing/Unicode cases, explicit capability errors, byte/output/
component limits, and three actual Rust unit tests. Each expected supported
string comes from actual Node path.win32 or the immutable retained source
function, not a second hand-written model. The source helper verifies SHA256
`98e563c55026d2dbbdd217ee9b94962ed7fc8b82d893795355bd7dc0f54fc5de`
against upstream commit `2d94defcf98175e4f5c9504e7f4fd1d33c35c975` before
extracting and executing the real TypeScript functions in one realm.

The first genuine Windows core run exposed checkout CRLF in this lexical
oracle. Source certification now normalizes CRLF on both checkout and Git text
before its immutable and fixed-SHA checks. One portable control exercises all
four LF/CRLF combinations and rejects mutations on either side, both sides, and
a lone carriage return. It uses the exact gate used before AST extraction.
The real source functions, fixtures and expected lexical outputs are unchanged.

The Node test builds the std-only driver from the actual new module with rustc
in its private temporary directory. It also runs the native unit binary and
removes both afterward. It never writes a root binary/cache or invokes Cargo.
`FREESHOW_WINDOWS_PATHS_BIN` may supply a separately built private driver; the
unit test still builds its private unit binary. `RUSTC` may select the compiler.
The bounded hex UTF-8 protocol preserves empty fields, controls, Unicode and NUL
inputs; driver framing and child output/time limits are explicit.

Strict `clippy-driver -D warnings` passes for the production-shaped private
driver and tests on Linux; Windows GNU and macOS ARM test metadata compilation
also pass. Rustfmt, Prettier, and whitespace checks pass. These are lexical
execution and portable compilation results, not Windows filesystem, native GUI,
ordering, IO cancellation, or browse-operation parity evidence.

## Coordinator integration

Declare the new helper under shared `paths.rs` only when ready. All existing
borrowed helper signatures become fallible when using this implementation;
propagate `PathError.code/message` into `BrowseError` rather than substituting
an empty string, null or a successful empty listing. Callers needing several
fields can use `parse` once instead of repeatedly scanning the same input.
Existing tighter per-request `BrowseConfig` and aggregate `Budget::string`
limits still apply before/after helper use; the fixed helper bounds do not
replace them.

The sealed stat backend already accepts normalized verbatim drive paths, and
Windows native fixtures obtain these through `canonicalize()`. Keep FILE_INFO's
existing narrow separator-based extension handling for those accepted paths;
do not route them through this parser, which deliberately rejects verbatim
prefixes. The two capability subsets differ. This helper slice does not revoke
the stat foundation's accepted input or silently expand its own contract.

Do not enable folder or similarity commands on the strength of this slice.
Windows directory enumeration ordering/name decoding, wider filesystem path
spellings (including canonical verbatim paths), source error handling, native
stat capability gates, and host context/ownership remain separate integration
obligations. The preserved stat foundation and cover branches are unchanged.

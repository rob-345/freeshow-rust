# Ordered now-playing file effects

This is a native host dependency, not production NOW_PLAYING wiring. The retained UI still needs the continuation scheduler, typed metadata publication, actual image effects and lifecycle integration described in [the host plan](now-playing-host-plan.md). The pinned source is `2d94defcf98175e4f5c9504e7f4fd1d33c35c975`.

`src-tauri/src/now_playing_files.rs` submits native-only intents through the sealed `LibraryPhase` API. Only the existing ordered library IO thread accesses the filesystem. It derives `runtime.stores.data/Audio/NowPlaying.txt` and `NowPlayingCover.png` after actual context validation. There is no renderer path constructor, deserialized intent, selected executable, extra thread, task or timer. A path observation does not create the folder; the source's explicit folder intent does. Failed folder creation retains the intended paths and a caught false receipt.

`FileTicket` retains both the actual phase ticket and a private typed observation channel. Collection first waits for the real ordered/context/result fence. The small observation is never an IPC event. Dropping the ticket cannot cancel an admitted write. Completed/uncollected tickets keep the existing phase job quota; after consuming the observation, the host must retain its own whole-operation lease. It must also keep the phase open while any source continuation remains unsubmitted. Phase drain is not an IO-thread join.

PNG writes always open/truncate/write, including identical bytes. Text writes skip only when an existing path's source-style UTF8 read equals the requested string. The reader keeps a BOM, uses replacement decoding for malformed UTF8 and treats directories/caught read failures as empty. Thus an existing directory with desired empty text skips; nonempty writes fail as caught false. Writes follow ordinary existing symlinks and preserve the target inode rather than atomically replacing it. Delete is a real unlink attempt and returns caught false for a directory or other filesystem error. These receipts do not roll back earlier effects.

Native bounds are1MiB UTF8 text,4MiB PNG and4MiB existing-text comparison. Comparisons inspect size, limit the actual read to cap+1, and reject growth. These bounds are narrower than Node's much larger string domain: an oversized comparison is an explicit `now_playing_file_resource` error, never a fabricated source empty read or false success. Nonregular comparison/output leaves are likewise explicit capability errors. Unix opens use `O_NONBLOCK` while retaining ordinary symlink following; held descriptors are checked again. General filesystem syscalls can still block, and path/metadata checks do not make Windows filesystem races atomic. No deadline or immutable-file claim is made.

Caught failures keep at most two diagnostic strings, each truncated on a UTF8 boundary to1024 bytes. Folder/write/delete helper failures remain boolean observations; context, closed admission, resource and stopped-owner errors retain their distinct native errors. OPEN reveal/ALERT is a separate host effect and is not implemented by launching an editor here.

Seven Linux native tests use actual temporary files, held descriptors, symlinks and FIFOs. They verify selected-root paths, absent versus existing empty text, seven malformed/UTF8 byte cases, unconditional cover writes, actual inode truncation, false filesystem outcomes, explicit resource caps, nonregular rejection, and an admitted dropped-caller write completing before later ordinary library work. The source suite executes actual pinned `files.ts` functions against real Node filesystem operations for the same seven byte cases, held-inode writes, identical Buffer rewrites and directory failures. The source probe does not call this Rust module; the native tests independently assert those observed fixture outcomes. Linux execution is verified; Windows/macOS execution remains pending CI.

Focused commands:

```sh
python3 /tmp/freeshow-native-cargo.py test --manifest-path src-tauri/Cargo.toml --features custom-protocol --test now_playing_files --test assets --test assets_open
node --test tests/parity/now-playing.test.mjs
```

The local helper only supplies the existing Linux native-library prefix and shared target directory. Source tests require the pinned Node22 toolchain. This slice does not certify complete helper behavior, image codec bytes, all filesystem races, source scheduling, full feature parity or application performance.

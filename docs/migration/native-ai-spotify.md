# Native local-model inventory and Spotify state

These bindings retain the Svelte interface and the original `MAIN` commands.
They are separate, lazy, process-lifetime native owners. Local-model directory
walking and Spotify subprocess waits do not run on the ordered library IO owner.
Neither starts a thread until a valid query needs its capability.

## Local-model inventory

`MAIN.AI_GET_BIN` reads only native app-data `bin/nemotron`, using the actual
source inventory and folder-size contract. Native startup captures this root;
renderer payloads and selected libraries cannot replace it. The foundation
supports bounded ordinary filesystem trees and preserves the source path, name,
size and property order. It does not validate or execute a model.

One filesystem thread owns at most four complete queued, active or retained
queries. Dropped callers cancel their observers; accepted filesystem frames keep
their leases through actual finish. A Rust-only `Event.native_lifetime` retains
the real `InventoryRead` through event clones and native serialization. Serde
skips this field in both directions. It adds no public acknowledgment and does
not prove WebView delivery or disposal of encoded transport buffers.

The host validates the main window, actual full runtime/library context and
listener bounds before admission. A library change cancels stale observers and
the final context check rejects an old reply. The global root and existing
filesystem owner survive selection. CLOSE seals host admission and awaits the
foundation's actual retained native-thread join; abandoned or timed-out shutdown
observers do not substitute logical completion for that join.

Six focused native host tests cover actual files, invalid/stale admission,
retained/cloned replies, caller abort, real library selection and an AI query
finishing while a real library commit remains held. This demonstrates independent
execution, not a latency or whole-application performance improvement. Library
cleanup in these tests is an ordered terminal fence, not a library-thread join.

[Source contract and resource limits](workers/ai-local.md) and
[host lifecycle evidence](workers/ai-host-verification.md).

## Linux Spotify state

`MAIN.SPOTIFY_GET_STATE` captures the native session environment and uses fixed
`/usr/bin/dbus-send` queries to Spotify's MPRIS object. Neither executable nor bus
address comes from renderer input. The private native owner is process-lived;
actual request provenance is checked separately before admission and reply.
Library selection cancels old observers without rebuilding global OS state.

One fixed thread/runtime supervises queries and real subprocess kill/wait. Four
complete global jobs include ready receipts, with four bounded shutdown
observers. Dropping a request future cancels its ticket without releasing the
accepted subprocess frame. A borrowed receipt projection keeps its real lease
until `Event.native_lifetime` is dropped after native serialization. CLOSE seals
admission and awaits actual native return/join. A still-running post-done frame
keeps its named handle and remains observable by a later shutdown attempt.

The paired private `nativeSpotify` projection records only known own-undefined
and IEEE restorations. After an exact runtime/generation/path check, the bridge
validates the original command and listener, hydrates a fresh source-shaped
value, and delivers only `{channel, data}` to the original callback. Ordinary
source `null` carries empty restoration lists. Missing or malformed restoration
is an error; it cannot become a fabricated source result.

Fourteen foundation Rust tests and six actual immutable-source/private-bus Node
suites cover parser behavior, real MPRIS profiles, cancellation/child reaping,
retained quotas, borrowed replies and native shutdown. The post-done panic
fixture is an actual held owner frame, not a TLS-destructor claim. A controlled
negative version that joined an unfinished handle defeated the external timeout;
the corrected implementation preserves cooperative observation through return.
The private parity driver uses one reusable time-enabled Tokio runtime.

Seven additional hydration suites check exact ordered properties, own undefined,
all explicit special-number tags and rejection without mutating input. Native
host, ordinary bridge and rebuilt GUI verification are separate integration
requirements; the foundation results alone do not certify them.

The integrated host now passes three entry tests driving seven actual private-bus
scenarios and seven pinned-source comparisons. Six ordinary bridge tests include
a reproduced caller-mutation regression and preserve the command's send-time
snapshot through the native wait. The isolated normal Linux binary passes193
editor steps, four menu cases and both drop scales. Actual source queries before
and after three retained callback phases compare nonempty32-byte AI inventory and
genuine absent-service Spotify null across restart and library selection. Normal
Quit waits its pending save, backup and writes;49 other editor diagnostics remain.
[Local receipt](baselines/ai-spotify-linux-local.json),
[native host tests](workers/spotify-native-host-tests.md) and
[desktop fixture](workers/ai-spotify-desktop.md).

Windows and macOS state backends return explicit unsupported capability errors.
Playback commands, OAuth, account providers, real Spotify sessions, broad
album-art/media behavior and whole-feature parity remain open.
[Source, supervision and limits](workers/spotify-linux.md) and
[hydration contract](workers/spotify-hydration.md).

## Provider preparation

`freeshow-provider-shows` is now a root workspace member alongside the two native
foundations. Its ChurchApps show builders preserve typed ordered values and
bounded preparation, including the original `24:00` fractional-tail date rule.
Its lossy JSON projection is not an own-undefined/IEEE hydration protocol.
Provider authentication, network JSON parsing, accepted imports and persistence
must be integrated through the original importer boundary before this foundation
can be used by the application.
[Certified source subset](workers/provider-shows.md).

Both workspace locks include the new local crates without changing previously
locked registry versions. Temporary nested workspaces and standalone locks were
removed at integration. Linux CI installs the real private-bus/GIO prerequisites;
foreign runtime and GUI evidence must be reported independently.

# Local Socket.IO connect admission patch

Source: socketioxide 0.18.7, crates.io registry package.
Registry checksum (SHA-256): `6d23ebfa62e73bcc05c40301f8b4cb8400489400c5e794e789300a04fd28a3b8`.
Upstream: https://github.com/Totodore/socketioxide
Registry VCS commit: `708ea9fc5ee549229d7236924e4975ec5977a59c`, path `crates/socketioxide`.
Package: https://crates.io/crates/socketioxide/0.18.7
License source: https://raw.githubusercontent.com/Totodore/socketioxide/708ea9fc5ee549229d7236924e4975ec5977a59c/LICENSE

Original MIT license is retained in LICENSE (the registry archive omits this
workspace-root file). Only src/client.rs has runtime source changes; freeshow-connect-guard.patch
is the complete source diff against that package. The guard admits at most one
namespace-connect task for each known namespace and engine session and rejects
duplicate CONNECT when already joined. The in-progress guard releases on
completion or cancellation. No parser or namespace routing is changed. FreeShow
registers only the root namespace. Its bounded custom MessageHandler bypasses the
upstream async data handler adapter.

Upstream spawns a namespace-connect task for every CONNECT before public connect
middleware executes, so public middleware cannot bound this path. The real raw
CONNECT flood/normal reconnect test in tests/parity/api.test.mjs exercises this
patch along with per-run task/session accounting. Remaining upstream files are
unchanged required source/docs/package files; upstream tests, examples, assets,
build outputs and Cargo.lock are excluded.

Packaging-only Cargo.toml changes remove explicit unavailable upstream test/bench
target declarations and add a local [workspace]. The complete separate diff is
freeshow-package-manifest.patch. Required source and documentation files otherwise
match the registry archive. Parent integration moves the directory outside the
API workspace member subtree (root vendor/socketioxide), explicitly excludes it
from the root workspace and retains vendor-local [workspace]. An equivalent
virtual-root check verified API-only workspace_members and cargo fmt --all.
Upstream test/benchmark/assets stay out of the vendored subset.

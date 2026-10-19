#!/usr/bin/env bash
set -eu
cd "$(dirname "$0")/../.."
export FREESHOW_PACKAGE_ROOT="${FREESHOW_PACKAGE_ROOT:-/mnt/data/cache/freeshow-rust-build/linux-deb-package}"
case "$FREESHOW_PACKAGE_ROOT" in /mnt/data/*) ;; *) echo 'Private package cache must be under /mnt/data' >&2; exit 1 ;; esac
export CARGO_HOME="${CARGO_HOME:-$FREESHOW_PACKAGE_ROOT/cargo-home}"
export CARGO_TARGET_DIR="$FREESHOW_PACKAGE_ROOT/target"
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export XDG_CACHE_HOME="$FREESHOW_PACKAGE_ROOT/tool-cache" TMPDIR="$FREESHOW_PACKAGE_ROOT/tmp"
mkdir -p "$CARGO_HOME" "$CARGO_TARGET_DIR" "$XDG_CACHE_HOME" "$TMPDIR" "$FREESHOW_PACKAGE_ROOT/artifacts"
node -e 'if (process.versions.node.split(".")[0] !== "22") throw Error("Use the pinned Node 22 toolchain")'
# Populate the private registry beforehand (a by-value copy is sufficient).
# Offline/locked resolution never updates the root workspace or its target.
node config/packaging/prepare-linux-deb.mjs
node node_modules/@tauri-apps/cli/tauri.js build --ci --no-sign --bundles deb \
    --config config/packaging/linux-deb.json \
    --config "$FREESHOW_PACKAGE_ROOT/stage/tauri-stage.json" \
    -- --locked --offline

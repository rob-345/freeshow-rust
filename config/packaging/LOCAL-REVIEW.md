# FreeShow Rust Linux package: local review

This unsigned package is a local review artifact of the Rust migration, not an
upstream FreeShow/Electron release. Package version 0.1.0 and source-compatible
runtime version 1.6.7-beta.2-rust.1 describe different existing version seams.

Source: https://github.com/rob-345/freeshow-rust
Original source: https://github.com/ChurchApps/FreeShow
License: GPL-3.0-only; the included GPL text applies to application code.
Third-party software retains its own included license and copyright notices.

The GTK/WebKit/ALSA/Opus runtime comes from declared system dependencies, not
bundled Electron or a private extracted system-library directory. This build is
verified only on its Ubuntu 24.04 x86_64 build/runtime system. It is not a claim
of older Debian/Ubuntu or other architecture support.

The package includes the exact hash-verified Linux x64 libltc.so.11 previously
certified by the native codec tests, from locked libltc-wrapper revision
9637e39773317e82e4112ff42b2c63afd144549f. Its public header declares 1.3.1, but
that does not prove the binary's complete corresponding-source provenance.
The wrapper records no exact library build revision. Its pinned tree declares MIT
in package.json but contains no standalone wrapper license text; this remains
another release-audit gap. LGPL texts, its public
header, and provenance notices are included. Exact corresponding-source and
rebuild/relink distribution obligations remain unresolved for external release;
this local review build is not published or offered as a compliant release.

Rust/Node notice inventories include a conservative dependency superset and
available installed package-root license/NOTICE texts. They do not assert that
every build-time component is executed or shipped, nor replace a complete
release license/source audit. Missing notice entries remain listed explicitly.
The unchanged frontend also contains CMG Sans font assets without an included
font license; their external redistribution authorization remains unaudited.

No automatic updater, signing, system installation, or upload is performed.
Media codec availability depends on the system WebKit/GStreamer installation.
Native device availability, playback/capture, and broad migration parity remain
separate capabilities; normal editor startup is not proof of all of them.

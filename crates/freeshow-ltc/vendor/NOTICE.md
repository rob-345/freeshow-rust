# libltc public header

`ltc.h` is copied without changes from FreeShow's pinned `libltc-wrapper` dependency, Git revision `9637e39773317e82e4112ff42b2c63afd144549f`, at `libltc/include/ltc.h`. It declares libltc 1.3.1 and retains its upstream copyright and LGPL-3.0-or-later notice. Its SHA-256 is `a2d5666fb300ffd05cbfada622029f2647dc26ba3ff03a834e3d5bd7ff77dff7`. `COPYING.LESSER` and `COPYING` provide the LGPLv3 and GPLv3 texts.

The original project is [libltc](https://github.com/x42/libltc). The source dependency's [build instructions](https://github.com/bitfocus/libltc-wrapper/blob/9637e39773317e82e4112ff42b2c63afd144549f/libltc/build.md) are not treated as a runtime version proof: this crate identifies the installed Linux x64 binary by its exact SHA-256, not by the header version or SONAME alone.

No libltc shared library, original N-API addon, or other codec binary is committed here. The application host must separately provide an authorized, correctly licensed resource and retain the applicable LGPL notices and corresponding-source/relinking obligations when distributing it. Linux loading uses an immutable snapshot of that exact resource. This crate does not accept a library path from renderer IPC.

`native/shim.c` and the Rust implementation are new GPL-3.0-only application code. The shim accesses the public header and dynamically resolves public libltc functions; it does not include guessed private layouts or a codec implementation.

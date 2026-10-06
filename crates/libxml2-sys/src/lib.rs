//! FFI bindings to libxml2: parser, XML Schema compile/validate, structured errors,
//! resource/entity loader.
//!
//! Owned by WP-LIBXML2 (`docs/TASKS.md`). The default build compiles a pinned libxml2
//! release from source and links it statically, with HTTP/FTP support disabled.
//! Setting `WASHBOARD_LIBXML2=pkg-config` links a system copy found via pkg-config instead
//! (Homebrew on macOS, distro package on Linux) — for faster local builds only, never for
//! release builds. See `docs/PLAN.md` §5 "libxml2: vendored, not system".

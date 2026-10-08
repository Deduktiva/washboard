# libxml2-sys

Hand-written FFI to the parts of libxml2 that `washboard_core::validate` uses: the parser,
XML Schema compilation and validation, structured errors and resource loaders. The safe
wrapper is `crates/washboard-core/src/validate/xsd.rs`.

## Pinned version: libxml2 v2.15.4

`vendor/libxml2` is a git submodule of <https://github.com/GNOME/libxml2> (a mirror of
<https://gitlab.gnome.org/GNOME/libxml2>) checked out at tag `v2.15.4`
(commit `96498992efa48d52b0e8b83058bd88dbdaf153c1`, released 2026-09-01).

Why this one:

- It is the newest stable release; there is no 2.16 yet. The 2.15 branch gets security
  releases (2.15.3 and 2.15.4 both contain security fixes, e.g. out-of-bounds reads in the
  regexp engine that XSD patterns use).
- 2.15 **removed the built-in HTTP client** (and LZMA). Combined with our build options, the
  library we link has no network code at all, so "the schema loader never touches the network"
  is a property of the binary (`docs/PLAN.md` §5, §6).
- 2.14+ has per-context resource loaders (`xmlSchemaSetResourceLoader`,
  `xmlCtxtSetResourceLoader`), which lets compiles run on several threads. See the caveat below.
- macOS ships 2.9.13 (2022); see PLAN §5 "libxml2: vendored, not system".

Maintenance status (checked 2026-10): upstream describes libxml2 as maintained by volunteers
on a best-effort basis, and the original long-time maintainer stepped back in 2025; releases
still appear (2.15.3 in April, 2.15.4 in September 2026). Upstream plans to drop the Python
bindings and Schematron in 2.16; neither affects us.

How we watch for advisories: subscribe to releases of the GitLab project (or the GitHub
mirror's tags) and read the "Security" section of `NEWS` for each 2.15.x; also watch
oss-security for "libxml2". Updating is `git -C vendor/libxml2 checkout v2.15.N`, then run the
full test suite (Linux and macOS CI) and update this file. Re-check the upstream bug and the
behaviour we depend on (both below) on every update.

## Upstream issue we work around

In 2.15.4 `xmlSchemaParseNewDoc` creates a temporary parser context for every imported or
included schema document and copies the error handlers to it, but **not** the resource loader
set with `xmlSchemaSetResourceLoader`. Documents from the second import level on are then
loaded through the process-global external entity loader, which by default reads files.
`validate/xsd.rs` therefore installs, once, a global loader that serves only the bundle of the
compile running on the current thread and refuses everything else, and has a regression test
(`nested_imports_cannot_reach_the_file_system`). If a later release fixes this, the global
loader can stay as defence in depth.

## Upstream behaviour we depend on

Things a libxml2 update could change without breaking the build. Each has a test that fails if
it changes:

- **Attribute in schema errors.** 2.15.4 reports attribute errors with the element as the
  node (`xmlVUpdateError` in `error.c` replaces the attribute), so `validate/xsd.rs` reads the
  attribute's name from the message prefix `Element '…', attribute '{ns}a': ` (PLAN §5.2).
  Tests: `attribute_errors_span_the_attribute` (`validate/xsd.rs`),
  `soap_attribute_errors_span_the_attribute` (`validate/request.rs`).
- **Error codes.** The span choice and the abstract type/element explanation key on the
  `XML_SCHEMAV_*` codes in `src/lib.rs`; tests in `validate/xsd.rs` and `validate/request.rs`
  assert the resulting spans and messages.

## Build

`build.rs` builds the submodule with CMake (the `cmake` crate) as a static library. Only libc
and libm are linked.

| Off | On |
|---|---|
| HTTP (already removed upstream), catalogs, iconv, ICU, zlib, LZMA (removed upstream), legacy API, Python, readline/history, modules (dlopen), HTML, C14N, XInclude, XPointer, RELAX NG, Schematron, debug module, programs, tests, docs | schemas, pattern, regexps, push parser, reader, threads, DTD validation, XPath, output, SAX1, ISO-8859-x tables (used instead of iconv) |

Without iconv, libxml2 itself reads UTF-8, UTF-16, ISO-8859-1…16 and ASCII. That is a superset
of what `xml::decode` accepts.

Cross-checking from Linux (`cargo clippy -p washboard-app --target aarch64-apple-darwin`) runs
this build script for a macOS target. There is no Apple SDK there, so the C build is skipped
(only the link directive is emitted, plus a `cargo:warning` that the result is type-check-only);
type-checking does not need it and linking needs a Mac anyway. On a Mac, the `cmake` crate
passes the target architecture and sysroot; `CMAKE_OSX_DEPLOYMENT_TARGET` follows
`MACOSX_DEPLOYMENT_TARGET` (default 11.0, rustc's minimum for `aarch64-apple-darwin`).

`git submodule update --init` is needed after cloning; CI checks out submodules.

### `WASHBOARD_LIBXML2=pkg-config`

Links a system or Homebrew libxml2 (≥ 2.14, for the resource loader API) found via
pkg-config instead of building the submodule. For faster local builds only: release builds
and CI always use the vendored copy. A system copy may have catalogs or HTTP enabled; the
wrapper's loaders still refuse everything outside the bundle, but the "no network code"
property only holds for the vendored build.

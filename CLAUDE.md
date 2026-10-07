# Washboard

Native macOS SOAP client in Rust. AppKit UI via `objc2`; everything else in a UI-free core
library that builds and tests on Linux.

- Plan and decisions: `docs/PLAN.md` (read §1–§6 before larger changes)
- Work packages and path ownership: `docs/TASKS.md`
- GUI draft: `docs/gui-draft.html`
- Test inputs: `fixtures/` (see `fixtures/README.md`)

## Layout

```
crates/washboard-core   UI-free library: model, diag, xml, wsdl, schema, validate, project, http, secrets, soap
crates/libxml2-sys      FFI to a vendored, statically linked libxml2
crates/washboard-cli    `washboard` command-line harness
crates/washboard-app    macOS app (objc2 + AppKit); stub `main` on other platforms
fixtures/               WSDL/XSD/request fixtures + lxml oracle
```

## Commands

```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
LIBSQLITE3_SYS_USE_PKG_CONFIG=1 cargo clippy -p washboard-app --target aarch64-apple-darwin -- -D warnings   # macOS type-check from Linux
python3 -I fixtures/check_fixtures.py                                        # after changing fixtures
```

Run all of the first four before every commit; CI runs them with `-D warnings`.
Linking the app needs a Mac; from Linux only type-check it. The env var is needed because no
macOS SDK is available to compile bundled C code (SQLite) for the Apple target; C code you add
must likewise skip its build when cross-checking for macOS from another host (see `libxml2-sys`). The toolchain and the
`aarch64-apple-darwin` target come from `rust-toolchain.toml`.

## Rules

- **Network:** only `washboard_core::http` opens connections, only to a configured `Server`.
  No other HTTP crates, no telemetry, no update checks, no fetching schemas. libxml2 must never
  load anything outside the `SchemaBundle`.
- **Scope:** SOAP 1.1 only, `document/literal` and `rpc/literal`. SOAP 1.2 and rpc/encoded
  operations are shown as unsupported, never silently dropped and never an error.
- **Encodings:** every XML input goes through `xml::decode` before a Rust parser sees it
  (BOMs and UTF-16 occur in real WSDLs). libxml2 gets the original bytes.
- **Positions:** `diag::TextPos` is 1-based line and char column, BOM not counted.
- **Errors:** per-module error enums with `thiserror`. No `unwrap`/`expect` on user input or
  file contents outside tests; malformed WSDLs are normal input.
- **Unsafe:** only in `libxml2-sys`, the libxml2 wrapper in `validate`, and `washboard-app`.
  Every `unsafe` block gets a `// SAFETY:` comment.
- **Contracts:** types listed under "Shared contracts" in `docs/TASKS.md` change additively only.
- **Ownership:** when working on a work package, edit only the paths it owns, plus additive
  contract changes and new dependencies in that crate's `Cargo.toml`. If you need something from
  another package, stub it locally in tests and say so in your report.
- **Tests:** use `fixtures/` for WSDL/XSD inputs; build temp dirs for project tests. Fixture
  files are byte-exact (`.gitattributes`); never reformat them.
- **Style:** match the surrounding code; doc comments on public items explain *why* and
  constraints, not restate the signature. Keep `max_width = 100`.

## Working as a parallel agent

- You get one work package from `docs/TASKS.md`. Read its section, `docs/PLAN.md` sections it
  cites, and the shared contract files before writing code.
- Commit on your own branch with clear messages; do not push unless told to. The orchestrator
  merges.
- Finish with a short report: what is done, what is stubbed, contract additions, new
  dependencies, open questions, and anything that needs a human on a Mac.

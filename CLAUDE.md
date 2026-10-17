# Washboard

Native macOS SOAP client in Rust. AppKit UI via `objc2`; the app's behaviour in a toolkit-free
UI model, and the domain logic in a UI-free core library, both of which build and test on Linux.

- Plan and decisions: `docs/PLAN.md`; behaviour per topic: `docs/specs/` (read the specs you
  touch, and `architecture.md` and `network.md`, before larger changes)
- Work packages and path ownership: `docs/TASKS.md`
- GUI draft: `docs/gui-draft.html`, ASCII versions in `docs/gui-drafts.md`
- Test inputs: `fixtures/` (see `fixtures/README.md`)

## Layout

```
crates/washboard-core   UI-free library: model, diag, xml, wsdl, schema, validate, project, http, secrets, soap
crates/washboard-ui-model  toolkit-free app behaviour: window state, commands, buffers, jobs
crates/libxml2-sys      FFI to a vendored, statically linked libxml2
crates/washboard-cli    `washboard` command-line harness
crates/washboard-app    macOS app (objc2 + AppKit); stub `main` on other platforms
crates/washboard-test-support  dev-dependency only: fixtures path, local HTTP server for tests
fixtures/               WSDL/XSD/request fixtures + lxml oracle
```

## Commands

```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
LIBSQLITE3_SYS_USE_PKG_CONFIG=1 cargo clippy -p washboard-app --target aarch64-apple-darwin -- -D warnings   # macOS type-check from Linux
cargo deny check bans                                                        # after adding a dependency
uv run --script fixtures/check_fixtures.py                                   # after changing fixtures
make app                                                                     # Washboard.app, on a Mac
```

Run all of the first four before every commit. CI runs fmt, clippy and tests on Linux and
clippy and tests on macOS, all with `-D warnings`, plus `cargo deny check bans` (the ban list
in `deny.toml` enforces the network and no-async rules); the cross-target clippy is the local
stand-in for the macOS job when you have no Mac.
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
  (BOMs and UTF-16 occur in real WSDLs). libxml2 gets the decoded text as UTF-8, too.
- **Positions:** `diag::TextPos` is 1-based line and char column, BOM not counted. Between
  `washboard-ui-model` and a front end, text positions are UTF-16 offsets, converted with
  `xml::utf16`.
- **Layering:** what the app *does* lives in `washboard-ui-model`; `washboard-app` only draws its
  state and forwards input. No toolkit types cross that boundary (`architecture.md`).
- **Undo:** the text widget owns undo. Programmatic edits (format, templates, completion,
  restore) are applied through the widget so they land on its undo stack; the model never keeps
  its own.
- **Threads:** no `tokio` or other async runtime; threads and channels. `washboard-ui-model`
  state is main-thread only and deliberately not `Send`; workers get immutable `Arc` snapshots,
  never project state.
- **libxml2 validation:** when libxml2 objects to something SOAP allows, remove it from the
  parsed tree before validating (`CompiledSchema::validate` with `strip`); never filter its
  errors afterwards (`validation.md`).
- **Errors:** per-module error enums with `thiserror`. No `unwrap`/`expect` on user input or
  file contents outside tests; malformed WSDLs are normal input.
- **Unsafe:** only in `libxml2-sys`, the libxml2 wrapper in `validate`, and `washboard-app`.
  Every `unsafe` block gets a `// SAFETY:` comment.
- **Contracts:** types listed under "Shared contracts" in `docs/TASKS.md` change additively only.
- **Ownership:** when working on a work package, edit only the paths it owns, plus additive
  contract changes and new dependencies in that crate's `Cargo.toml`. If you need something from
  another package, stub it locally in tests and say so in your report.
- **objc2 (`washboard-app`):** AppKit is main-thread-only; pass `MainThreadMarker` down rather
  than creating one deep inside. Every delegate, data source and subclass via `define_class!`,
  Rust state in `#[ivars]` behind `RefCell`/`Cell`. AppKit delegate properties are weak: the
  controller owns its delegates and data sources. Check method names on docs.rs for the exact
  versions in `Cargo.lock`; the selector-to-Rust naming is easy to guess wrong.
- **Forms (`washboard-app`):** build Settings-style forms (sections, grouped rows, footnotes)
  with `form.rs`, not hand-made stacks; it owns the margins and alignment, listed in its module
  doc. Add a row kind there rather than positioning a one-off. `tests/appkit.rs`
  (`assert_form_layout`) checks the geometry; call it for a new pane.
- **Colours (`washboard-app`):** status colours come from `colors.rs` (`error`, `warning`,
  `success`); `NSColor::system*Color` appears only there and in the editor's highlighting
  palette. AppKit's semantic colours are used directly.
- **Tests:** use `fixtures/` for WSDL/XSD inputs; build temp dirs for project tests. Fixture
  files are byte-exact (`.gitattributes`); never reformat them.
- **Style:** match the surrounding code; doc comments on public items explain *why* and
  constraints, not restate the signature. Keep `max_width = 100`.

## Commits

- One logical change per commit. Never mix code and plan/docs changes, and keep refactors,
  behaviour changes and dependency swaps apart, even inside one PR.
- The message says what changed and why; mention behaviour changes explicitly.

## Working as a parallel agent

- You get one work package from `docs/TASKS.md`. Read its section, the specs it
  cites, and the shared contract files before writing code.
- Commit on your own branch with clear messages; do not push unless told to. The orchestrator
  merges.
- Finish with a short report: what is done, what is stubbed, contract additions, new
  dependencies, open questions, and anything that needs a human on a Mac.

# Work packages

How the work in `docs/PLAN.md` is split so several agents can work in parallel without
stepping on each other. Each package owns a set of paths. Shared contracts are listed
separately and change only additively unless coordinated.

Finished packages are listed under "Done" with what they own; their specs live in git history
and their behaviour is the code and its tests. Follow-up work on a finished package's paths is
a new package that names those paths.

## Shared contracts (exist now, owned by the orchestrator)

| Contract | Path | Used by |
|---|---|---|
| IDs, `QName`, `OperationRef`, `Server`, `Auth`, `RequestMeta`, `HistoryEntry` | `crates/washboard-core/src/model.rs` | all |
| `SchemaBundle`, `SchemaDoc`, `SchemaOrigin` | `crates/washboard-core/src/model.rs` | WSDL → LIBXML2, SCHEMA |
| `Diagnostic`, `TextPos`, `LineIndex`, `pos_at_byte`, `has_errors`, `error_count` | `crates/washboard-core/src/diag.rs` | XML, LIBXML2, VALIDATE, WSDL, CLI, APP |
| `SendRequest`, `Exchange`, `RawMessage`, `TlsInfo` | `crates/washboard-core/src/http/exchange.rs` | HTTP, PROJECT (history), APP |
| `SecretStore`, `MemorySecretStore` | `crates/washboard-core/src/secrets.rs` | PROJECT, HTTP callers |
| `xml::decode`, `xml::encode_utf8` | `crates/washboard-core/src/xml/encoding.rs` | everything that reads XML |
| SOAP/WSDL/XSD namespace constants | `crates/washboard-core/src/soap.rs` | all |
| Fixtures + oracle | `fixtures/` | all tests |

Adding a field, variant or function to a contract is fine; say so in your final report.
Renaming or removing anything in a contract: don't — report the need instead.

The orchestrator also owns everything not listed below: workspace `Cargo.toml`,
`crates/washboard-core/src/lib.rs`, `.github/`, `deny.toml`, `docs/`, `README.md`, `CLAUDE.md`,
and the workspace-level tests `crates/washboard-core/tests/{pipeline,network_boundary}.rs`.

## Done

| Package | Owns | Notes |
|---|---|---|
| WP-LIBXML2 | `crates/libxml2-sys/**`, `vendor/libxml2`, `crates/washboard-core/src/validate/xsd.rs` | libxml2 2.15.4, static, no network code. Whole-document validation; positions point at the start tag's `<`. Loader design and thread rules: `xsd.rs` module docs. |
| WP-WSDL | `crates/washboard-core/src/wsdl/**` | Model, import graph and check, bundle incl. rpc/literal and split-namespace wrappers, operation lookup, structural report. |
| WP-SCHEMA | `crates/washboard-core/src/schema/**`, `crates/washboard-core/tests/schema_perf.rs` | Rust XSD model for completion and templates; perf targets asserted in release builds. |
| WP-XML | `crates/washboard-core/src/xml/**` | Tokenizer, cursor context, pretty-printer, start-tag scanner, encodings. Well-formedness goes through libxml2 (`validate::xsd`), so this package depends on WP-LIBXML2. |
| WP-PROJECT | `crates/washboard-core/src/project/**`, `KeychainSecretStore` in `secrets.rs` | Folders, SQLite, requests, servers, history, app state, read-only open. |
| WP-HTTP | `crates/washboard-core/src/http/**` except `exchange.rs` | `ureq` + `native-tls` send, TLS policy, fault detection. |
| WP-VALIDATE | `crates/washboard-core/src/validate/**` except `xsd.rs` (`mod.rs`, `request.rs`, `soap-envelope-1.1.xsd`) | PLAN §4 "Validation semantics", one libxml2 pass; checked by `tests/pipeline.rs`. |
| WP-CLI | `crates/washboard-cli/**` | `washboard` noun-verb CLI on the app's project folders. Exit codes: `src/main.rs` docs; read-only commands open without the lock (`src/support.rs`). `send --skip-validation` still validates and prints errors, then sends. |

## Open

### WP-APP-SHELL — AppKit skeleton (macOS; needs a Mac — see `docs/handoff/APP-SHELL.md`)
Owns: `crates/washboard-app/**`.
- App delegate via `define_class!`, main menu (File/Edit/View/Project/Window/Help with the
  shortcuts from the plan), welcome window, project window with toolbar, split view, sidebar
  `NSOutlineView` (requests + operations sections, static data for now), `NSTextView` editor on
  TextKit 1 with a line-number `NSRulerView` and a highlighting hook that takes token spans,
  response pane, HTTP log panel. No core integration beyond types yet.
- `cargo-packager` configuration that produces an unsigned `.app` bundle with `Info.plist`
  (`LSMinimumSystemVersion` 27.0); no hand-written bundling code.
- Acceptance: `cargo clippy -p washboard-app --target aarch64-apple-darwin` clean; CI macOS build
  green; list in the final report exactly what must be eyeballed on a Mac.

### Later packages

| Package | Depends on | Owns | Scope |
|---|---|---|---|
| WP-UI-MODEL | core (done) | `crates/washboard-ui-model/**` (+ its workspace member entry) | New crate (PLAN §2.1): app/window state, commands, editor buffers, autosave, background jobs, events to the front end; front-end traits (`MainThread`, `Timers`, `Dialogs`). Tested on Linux with a fake front end. No toolkit dependency. |
| WP-DIAG-DETAIL | VALIDATE (done) | `crates/washboard-core/src/validate/**`, `crates/washboard-cli/src/validation.rs`; additive `detail` and span on `Diagnostic` | PLAN §5.2 "Validation errors": libxml2 error code, element index and attribute QName on diagnostics; byte spans for attribute, value and start-tag errors; abstract type/element errors list the allowed concrete types/members from `schema::SchemaModel`; CLI excerpt via `annotate-snippets` once spans exist. |
| WP-REPLACE-REPORT | VALIDATE (done) | new `crates/washboard-core/src/project/replace_report.rs`, `crates/washboard-cli/src/commands/project.rs` | PLAN §4 "Replace WSDL": after a replace, report operations added/removed and requests that no longer validate; `project replace-wsdl` prints it. Requests are never rewritten. |
| WP-VALIDATE-PERF | VALIDATE (done) | new `crates/washboard-core/tests/validate_perf.rs` (may move the generator from `schema_perf.rs` to `tests/common/`) | PLAN §5.1: libxml2 compile time for the synthetic 2 MB set and per-request validation time. Record the numbers in PLAN §5.1; they decide whether live validation is on (target < 100 ms). |
| WP-APP-INTEGRATION | APP-SHELL + UI-MODEL | `crates/washboard-app/**` (after APP-SHELL) | Implement the front-end traits for AppKit and bind views to the model's events and commands. No app behaviour in the AppKit layer. |
| WP-DIST | APP-SHELL | `cargo-packager` metadata in `crates/washboard-app/Cargo.toml`, a release workflow in `.github/workflows/` | DMG via `cargo-packager`; codesign and notarization via `rcodesign` (apple-codesign) or `cargo-packager`'s signing support, configured, not scripted. Check first that both handle Developer ID + hardened runtime + notarytool on macOS 27. |

Not packaged yet: external-change detection with FSEvents (PLAN M5), and a manual check of
`KeychainSecretStore` on a Mac (type-checked only so far).

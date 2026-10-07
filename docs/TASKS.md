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
| WP-VALIDATE-PERF | `crates/washboard-core/tests/validate_perf.rs`, `crates/washboard-core/tests/common/` (synthetic schema generator, shared with `schema_perf.rs`) | libxml2 compile and `validate_request` timings on the synthetic 2 MB set; numbers in PLAN §5.1, targets asserted in release builds. |

## Open

### WP-APP-SHELL — AppKit skeleton
Owns: `crates/washboard-app/**`.

A runnable skeleton matching `docs/gui-draft.html` (the visual target) and PLAN §8, proving the
objc2 patterns the app will use everywhere. No project logic, persistence, validation, HTTP,
autosave or dirty tracking: those live in `washboard-ui-model` (PLAN §2.1), so controllers stay
thin (views, layout, forwarding input). Static sample data shaped like the core types. Can be
done in the cloud: the macOS CI job compiles, tests and packages it; looking at it needs a Mac
(checklist below).
- **Lifecycle:** `AppDelegate` via `define_class!`, kept alive for the app's lifetime;
  `applicationShouldTerminateAfterLastWindowClosed` returns `false` (the welcome window takes
  over).
- **Main menu in code**, no nib. App: About, Settings… (disabled), Hide, Quit. File: New
  Project… ⇧⌘N, Open Project… ⌘O, Open Recent (`NSDocumentController`), Close ⌘W, Save All ⌘S.
  Edit: standard first-responder items (Undo/Redo, Cut/Copy/Paste, Select All, Find). Project:
  New Request ⌘N, Duplicate ⌘D, Rename, Delete ⌘⌫, Validate ⌘B, Send ⌘↩, Replace WSDL…,
  Project Settings…. Window: HTTP Log ⌥⌘L plus the window list. Stub handlers log.
- **Welcome window** when no project is open: icon, name, New/Open; `NSTableView` of recent
  projects.
- **Project window**, one controller per project: unified `NSToolbar` via a delegate (sidebar
  toggle, server popup, Validate, Send, Save All, HTTP Log); `NSSplitViewController` with a
  source-list `NSOutlineView` (REQUESTS, and OPERATIONS as service › port › operation; unsaved
  dot and ⚠ markers; inline rename on Return; +/−/⋯ footer) and a content split of editor,
  collapsible issues bar and response pane.
- **Editor:** `NSTextView` on TextKit 1 (`initUsingTextLayoutManager(false)` or an explicit
  `NSLayoutManager` stack); monospaced system font; smart quotes, dashes and text replacement
  off. Line numbers and error markers from an `NSRulerView` subclass over the visible glyph
  range. Highlighting from `washboard_core::xml`'s tokenizer, applied as temporary attributes on
  the layout manager for the edited range extended to line boundaries, so undo and the saved
  text are unaffected. Undo stays with `NSTextView`. Issues bar: click selects the line.
- **Response pane:** status label, Response/Headers/History tabs, read-only highlighted text
  view, history table.
- **HTTP log panel:** one `NSPanel` for the app; exchange table over request/response side by
  side; `Authorization` masked with click to reveal.
- **Sheets:** New Project (fields, references with ✓/✗, Create disabled on ✗) and Project
  Settings › Servers.
- **Threading pattern:** a fake Send runs on a `std::thread` and posts back to the main queue
  with `dispatch2`; this is the pattern every background job uses.
- **Bundle:** `cargo-packager` builds `Washboard.app` (PLAN §2 "Packaging"), unsigned, with a
  placeholder icon; no hand-written bundling code.
- **Acceptance (CI):** clippy clean natively on macOS and with `--target
  aarch64-apple-darwin` from Linux; the macOS job builds the `.app` with `cargo-packager` and
  uploads it as an artifact; a test measures `NSLayoutManager` layout of a 1 MB XML document on
  the runner and prints the time.
- **Needs a Mac (the user, from the CI artifact):** launches from Finder; matches the GUI
  draft in light and dark mode; menus, shortcuts, sidebar rename, ruler, highlighting,
  issue click-to-line, fake send, log panel and both sheets behave; typing stays responsive in a
  1 MB file.

### Later packages

| Package | Depends on | Owns | Scope |
|---|---|---|---|
| WP-UI-MODEL | core (done) | `crates/washboard-ui-model/**` (+ its workspace member entry) | New crate (PLAN §2.1): app/window state, commands, editor buffers, autosave, background jobs, events to the front end; front-end traits (`MainThread`, `Timers`, `Dialogs`). Diagnostics as in PLAN §2.1: spans to UTF-16, one shared `SchemaModel` also passed to `RequestSchema::with_model`. Completion needs the editor path (`xml::PathElement`s) as `schema::PathStep`s; `validate::request::block_path` already converts it, so move that into `schema` (may edit `schema/query.rs` and `validate/request.rs` for this) and use it from both. Tested on Linux with a fake front end. No toolkit dependency. |
| WP-DIAG-DETAIL | VALIDATE (done) | `crates/washboard-core/src/validate/**`, `crates/washboard-cli/src/validation.rs`; additive `detail` and span on `Diagnostic` | PLAN §5.2 "Validation errors": libxml2 error code, element index and attribute QName on diagnostics; byte spans for attribute, value and start-tag errors; abstract type/element errors list the allowed concrete types/members from `schema::SchemaModel`; CLI excerpt via `annotate-snippets` once spans exist. |
| WP-REPLACE-REPORT | VALIDATE (done) | new `crates/washboard-core/src/project/replace_report.rs`, `crates/washboard-cli/src/commands/project.rs` | PLAN §4 "Replace WSDL": after a replace, report operations added/removed and requests that no longer validate; `project replace-wsdl` prints it. Requests are never rewritten. |
| WP-APP-INTEGRATION | APP-SHELL + UI-MODEL | `crates/washboard-app/**` (after APP-SHELL) | Implement the front-end traits for AppKit and bind views to the model's events and commands. No app behaviour in the AppKit layer. |
| WP-DIST | APP-SHELL | `cargo-packager` metadata in `crates/washboard-app/Cargo.toml`, a release workflow in `.github/workflows/` | DMG via `cargo-packager`; codesign and notarization via `rcodesign` (apple-codesign) or `cargo-packager`'s signing support, configured, not scripted. Check first that both handle Developer ID + hardened runtime + notarytool on macOS 26. |

Not packaged yet: external-change detection with FSEvents (PLAN M5), a manual check of
`KeychainSecretStore` on a Mac (type-checked only so far), and macOS numbers for PLAN §5.1:
`cargo test --release --test validate_perf --test schema_perf -- --nocapture` on a Mac or as a
step in the macOS CI job (CI runs debug tests with captured output, so it shows none today).

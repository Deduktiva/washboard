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
| `Diagnostic`, `TextPos`, `TextSpan`, `DiagDetail`, `LineIndex`, `pos_at_byte`, `has_errors`, `error_count` | `crates/washboard-core/src/diag.rs` | XML, LIBXML2, VALIDATE, WSDL, CLI, APP |
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
| WP-DIAG-DETAIL | follow-up on the VALIDATE, LIBXML2 and CLI paths above; added `TextSpan`, `DiagDetail` and `Diagnostic::{span, detail}` to `diag` | PLAN §5.2 "Validation errors": libxml2 error code, element index and attribute on diagnostics, spans, abstract type/element alternatives from `schema::SchemaModel`, CLI excerpts via `annotate-snippets`. |

## Open

### WP-APP-SHELL — AppKit skeleton
Owns: `crates/washboard-app/**`; for step 1 also the macOS job in `.github/workflows/ci.yml`.

A runnable skeleton matching `docs/gui-draft.html` (the visual target) and PLAN §8, proving the
objc2 patterns the app will use everywhere. No project logic, persistence, validation, HTTP,
autosave or dirty tracking: those live in `washboard-ui-model` (PLAN §2.1), so controllers stay
thin (views, layout, forwarding input). Static sample data shaped like the core types.

Built as a stack of PRs, one per step, each merged before the next is reviewed. Every step is
checked by CI on a real Mac: the crate is a library plus a thin `main`, and
`crates/washboard-app/tests/appkit.rs` (`harness = false`, so it runs on the main thread)
builds the real objects headless and asserts on them. Each step adds its checks there. Only
the "Needs a Mac" list at the end needs a person.

**Step 1 — library, lifecycle, bundle, CI.**
- `src/lib.rs` with the app code, `src/main.rs` only calls it.
- `AppDelegate` via `define_class!`, kept alive for the app's lifetime;
  `applicationShouldTerminateAfterLastWindowClosed` returns `false`. A placeholder window.
- `cargo-packager` builds `Washboard.app` (PLAN §2 "Packaging"), unsigned, with a placeholder
  icon; no hand-written bundling code. `MACOSX_DEPLOYMENT_TARGET` 26.0 for the bundle build.
- CI: the macOS job runs on `macos-26` (pinned, not `-latest`), builds the bundle, checks its
  `Info.plist` with `plutil`, and uploads it as an artifact.
- Check: `tests/appkit.rs` creates the application, installs the delegate, finishes launching
  and asserts the window exists and closing it does not terminate.

**Step 2 — main menu and welcome window.**
- Main menu in code, no nib. App: About, Settings… (disabled), Hide, Quit. File: New
  Project… ⇧⌘N, Open Project… ⌘O, Open Recent (`NSDocumentController`), Close ⌘W, Save All ⌘S.
  Edit: standard first-responder items (Undo/Redo, Cut/Copy/Paste, Select All, Find). Project:
  New Request ⌘N, Duplicate ⌘D, Rename, Delete ⌘⌫, Validate ⌘B, Send ⌘↩, Replace WSDL…,
  Project Settings…. Window: HTTP Log ⌥⌘L plus the window list. Stub handlers log.
- Welcome window when no project is open: icon, name, New/Open; `NSTableView` of recent
  projects (sample data).
- Check: every menu item's title, key equivalent, modifiers and action selector against one
  table in the test; the welcome window's table shows the sample rows.

**Step 3 — project window.**
- One controller per project. Unified `NSToolbar` via a delegate (sidebar toggle, server popup,
  Validate, Send, Save All, HTTP Log).
- `NSSplitViewController` with a source-list `NSOutlineView` (REQUESTS, and OPERATIONS as
  service › port › operation; unsaved dot and ⚠ markers; inline rename on Return; +/−/⋯
  footer) and a content split of editor placeholder, collapsible issues bar and response pane.
- Check: toolbar item identifiers in order; outline row counts with groups expanded; the
  content split's panes; delegates and data sources are still alive after an autorelease pool
  drain (they are weak in AppKit).

**Step 4 — editor.**
- `NSTextView` on TextKit 1 (`initUsingTextLayoutManager(false)` or an explicit
  `NSLayoutManager` stack); monospaced system font; smart quotes, dashes and text replacement
  off.
- Line numbers and error markers from an `NSRulerView` subclass over the visible glyph range.
- Highlighting from `washboard_core::xml`'s tokenizer, applied as temporary attributes on the
  layout manager for the edited range extended to line boundaries, so undo and the saved text
  are unaffected. Undo stays with `NSTextView`.
- Check: the view has a layout manager and no text layout manager; substitutions are off;
  after an edit the temporary attributes match the tokens while the text storage has no
  colour attributes and undo restores the text; layout of a 1 MB XML document, timed and
  printed.

**Step 5 — issues bar, response pane, HTTP log, threading.**
- Issues bar: click selects the line. Response pane: status label, Response/Headers/History
  tabs, read-only highlighted text view, history table. HTTP log: one `NSPanel` for the app;
  exchange table over request/response side by side; `Authorization` masked with click to
  reveal.
- Threading pattern: a fake Send runs on a `std::thread` and posts back to the main queue with
  `dispatch2`; this is the pattern every background job uses.
- Check: a fake send's result arrives on the main thread and updates the status label; the log
  shows `Authorization` masked until revealed; clicking an issue selects its line.

**Step 6 — sheets.**
- New Project (fields, references with ✓/✗, Create disabled on ✗) and Project Settings ›
  Servers.
- Check: Create is disabled while a reference is ✗ and enabled once all are ✓; the servers
  table edits its sample rows.

**Needs a Mac (the user, from the CI artifact, after step 6):** launches from Finder; matches
the GUI draft in light and dark mode; menus, shortcuts, sidebar rename, ruler, highlighting,
issue click-to-line, fake send, log panel and both sheets behave; typing stays responsive in a
1 MB file.

### Later packages

| Package | Depends on | Owns | Scope |
|---|---|---|---|
| WP-UI-MODEL | core (done) | `crates/washboard-ui-model/**` (+ its workspace member entry) | New crate (PLAN §2.1): app/window state, commands, editor buffers, autosave, background jobs, events to the front end; front-end traits (`MainThread`, `Timers`, `Dialogs`). Diagnostics as in PLAN §2.1: spans to UTF-16, one shared `SchemaModel` also passed to `RequestSchema::with_model`. Completion needs the editor path (`xml::PathElement`s) as `schema::PathStep`s; `validate::request::block_path` already converts it, so move that into `schema` (may edit `schema/query.rs` and `validate/request.rs` for this) and use it from both. Tested on Linux with a fake front end. No toolkit dependency. |
| WP-REPLACE-REPORT | VALIDATE (done) | new `crates/washboard-core/src/project/replace_report.rs`, `crates/washboard-cli/src/commands/project.rs` | PLAN §4 "Replace WSDL": after a replace, report operations added/removed and requests that no longer validate; `project replace-wsdl` prints it. Requests are never rewritten. |
| WP-APP-INTEGRATION | APP-SHELL + UI-MODEL | `crates/washboard-app/**` (after APP-SHELL) | Implement the front-end traits for AppKit and bind views to the model's events and commands. No app behaviour in the AppKit layer. |
| WP-DIST | APP-SHELL | `cargo-packager` metadata in `crates/washboard-app/Cargo.toml`, a release workflow in `.github/workflows/` | DMG via `cargo-packager`; codesign and notarization via `rcodesign` (apple-codesign) or `cargo-packager`'s signing support, configured, not scripted. Check first that both handle Developer ID + hardened runtime + notarytool on macOS 26. |

Not packaged yet: external-change detection with FSEvents (PLAN M5), a manual check of
`KeychainSecretStore` on a Mac (type-checked only so far), and macOS numbers for PLAN §5.1:
`cargo test --release --test validate_perf --test schema_perf -- --nocapture` on a Mac or as a
step in the macOS CI job (CI runs debug tests with captured output, so it shows none today).

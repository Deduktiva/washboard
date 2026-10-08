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

### WP-UI-MODEL — the app without the toolkit
Owns: `crates/washboard-ui-model/**` and its workspace member entry; for step 7 also
`schema/query.rs` and `validate/request.rs` (moving `block_path`).

New crate (PLAN §2.1): app and window state, commands, editor buffers, autosave, background
jobs, and events to the front end. Depends on `washboard-core` only, no toolkit. State lives
on the main thread (`Rc<RefCell<…>>`, not `Send`); workers get owned snapshots, queue their results in
the model and wake the front end through `MainThread`; the front end then calls `App::pump`.

Built as a stack of PRs like WP-APP-SHELL, one per step. Every step is tested on Linux with a
fake front end in the crate: `MainThread` counts wakes and the test pumps explicitly, `Timers`
uses a manual clock the test advances, `Dialogs` answers from a script, and the secret store is
`MemorySecretStore`. Tests assert on the emitted events, so timing (autosave, debounce,
stale results) is deterministic. Project tests use temp dirs and `fixtures/`.

**Step 1 — crate, front-end traits, events, app state.**
- The traits `MainThread`, `Timers`, `Dialogs` (PLAN §2.1) and the fake front end.
- Events as a typed enum the front end drains; positions cross as UTF-16 offsets.
- App state on `project::AppState`: open/close projects, recent projects, restore on launch
  (missing folders reported once, then dropped), the welcome-window condition, save on quit.
- Check: open, close and restore round-trip through `state.json`; welcome shows exactly when
  no project is open.

**Step 2 — project window state and commands.**
- Sidebar tree: requests with dirty/invalid markers, operations by service › port incl.
  unsupported ones; selection; server popup items and selection (last server per request).
- Commands: new (`<Operation> <n>`), rename, duplicate, delete (confirm via `Dialogs`, history
  deleted with it); server CRUD and the settings form state, passwords through `SecretStore`.
- Check: each command's events and its effect on the project folder.

**Step 3 — editor buffers, autosave, Save All.**
- A buffer per opened request: text, encoding/BOM kept on save, `xml::TokenBuffer`, dirty flag.
- `edit(range, text)` in UTF-16 → `TokensChanged(range)`; no undo (the widget owns it).
- Autosave 1 s after the last edit via `Timers`; flush on switch, send, focus loss, quit.
  Save All across projects; the window's edited state.
- Check: debounce timing on the manual clock, flush triggers, BOM/UTF-16 round trip.

**Step 4 — background jobs and diagnostics.**
- Schema compile per project on a worker, shared as `Arc`; one `SchemaModel` also passed to
  `RequestSchema::with_model`.
- Well-formedness (debounced 150 ms) and live validation (1 s), plus the Validate command.
  Results carry the buffer version; stale ones are dropped.
- Diagnostics to UTF-16 spans, the issues list, sidebar invalid markers.
- Check: stale results dropped, debounce, markers follow diagnostics.

**Step 5 — send, response pane, history, HTTP log.**
- Send validates first and refuses on errors (showing the issues); worker thread per send,
  cancel detaches; the request's last server updated.
- Response pane state (status line, body, headers, fault), history list and "Restore
  request", the HTTP log ring buffer (50, Authorization masked until revealed).
- Check against a local test server (as `http/tests.rs` does): refused send, success, fault,
  transport error, cancel.

**Step 6 — new project and replace WSDL.**
- Import-check sheet state from `wsdl` (references resolved/unresolved, warnings); Create
  enabled only when nothing is unresolved and the schema set compiles.
- Create: copy files, create the project, default server from `soap:address` disabled until
  confirmed. Replace WSDL: recompile and re-validate every request; the report itself is
  WP-REPLACE-REPORT's.
- Check with `fixtures/` sets including a missing include.

**Step 7 — completion and hover.**
- Move `validate::request::block_path` into `schema` and use it from both.
- Completion at a UTF-16 offset (element names, attributes, enumeration values) and hover
  (type, cardinality, documentation) from the shared `SchemaModel`.
- Check against fixture schemas.

Front-end binding is WP-APP-INTEGRATION.

### WP-APP-INTEGRATION — the shell on the model
Owns: `crates/washboard-app/**`; additive public API in `crates/washboard-ui-model` where the
binding needs it, tested there.

Replaces the shell's sample data and stubs with `washboard-ui-model`. Controllers stay thin:
when an event names something, they read it from the model and redraw it; user input becomes a
model command. Nothing the model already decides is decided again in AppKit.

Built as a stack of PRs on WP-UI-MODEL step 7, one per step; the app stays runnable after each.
From step 1 on, `tests/appkit.rs` runs against project folders built from `fixtures/` in a temp
dir, a temp state dir, `MemorySecretStore` and a recording `Dialogs`, so CI never touches the
Keychain or blocks on a modal; each step adds its checks there.

Rules for every step:
- The app delegate owns the `App` in a `RefCell`. A command borrows it, runs, and releases it;
  then the delegate drains `take_events` and applies each event, reading state in short
  borrows. The model is never borrowed across an AppKit call, and view callbacks caused by
  applying an event (selection, text) are ignored while it is being applied.
- `MainThread::wake` posts one block to the main dispatch queue (coalesced by an atomic flag)
  that pumps and applies events. `Timers` are main-queue `after` blocks with a generation per
  id, so restart and cancel need no timer objects. `Dialogs` never run a modal loop inside a
  model call: they present on a later main-queue turn, as sheets on the key window when there
  is one, and answer through `App::dialog_answered`. Errors returned by commands become alerts.
- Text positions are UTF-16 offsets on both sides, so the app never converts them.

**Step 1 — model, front end, lifecycle.**
- `install` takes the state dir, secret store and dialogs, so tests inject theirs; `run` uses
  `~/Library/Application Support/Washboard`, `KeychainSecretStore` and AppKit panels.
- Launch restores projects; the welcome window lists `recent_projects` and follows
  `WelcomeVisibility`. Open Project… (`NSOpenPanel`, folders), a double-click on a recent row
  and File ▸ Open Recent open through the model; `FocusProject` brings the window forward.
  Open Recent is built from `recent_projects` on `RecentProjectsChanged` rather than by
  `NSDocumentController`, which would need an `NSDocument` type to reopen a folder; Clear Menu
  clears the model's list.
- Project windows come and go with `ProjectOpened`/`ProjectClosed`. The close button asks the
  model (`windowShouldClose:` → `close_project`, which keeps the window if saving fails); Quit
  asks `quit` from `applicationShouldTerminate:`.
- Check: a state file listing a fixture project opens its window on launch; closing it shows
  the welcome window with the project listed; opening it from the list again; a missing
  project is one recorded alert.

**Step 2 — sidebar, request commands, server popup.**
- Sidebar from `ProjectWindow::sidebar` on `SidebarChanged`, markers from the row flags;
  selection ↔ `select_request`/`SelectionChanged`; inline rename commits `rename_request` and
  `BeginRename` starts it. New Request (the selected operation, else the first supported one),
  double-click on an operation, Duplicate, Delete (confirmed through `Dialogs`).
- Server popup from `servers`/`selected_server`; picking an item is `choose_server`.
- Check: rows for a fixture project; new, rename, duplicate and delete through the menu
  actions change the rows and the folder; the popup follows the selected request.

**Step 3 — editor buffer, autosave, Save All.**
- The editor shows `Editor::text` on `EditorReplaced`; user edits go to `App::edit` with the
  range and string from `shouldChangeTextInRange:replacementString:`; highlighting comes from
  `Editor::tokens_utf16` on `TokensChanged` (the controller's own `TokenBuffer` goes). The
  window's edited dot follows `edited()`. Save All; `windowDidResignKey:` and
  `applicationDidResignActive:` flush.
- Check: typing marks the row and the window; after the autosave delay the file holds the
  text; undo restores it and reaches the model as an edit.

**Step 4 — diagnostics and Validate.**
- Issues bar and ruler markers from `Editor::issues` on `DiagnosticsChanged`; issue ranges
  underlined with a temporary attribute; a click selects the issue's range (else its line).
  Validate; `ShowIssues` reveals the bar.
- Check: an invalid fixture request lists its errors, marks the gutter and the sidebar row;
  fixing it clears all three.

**Step 5 — send, response pane, history, HTTP log.**
- Send ↔ Cancel on `SendStateChanged`; the response pane from `ResponseView` (status line,
  highlighted body, headers, fault); the History tab from `history()`, selecting shows the
  entry, Restore request; the HTTP log from `http_log()` with `request_headers(reveal)`.
- Check against a one-shot local server in the test: refused send shows the issues, a send
  fills the pane, history and log, cancel returns to Send.

**Step 6 — sheets.**
- New Project and Replace WSDL bound to `ImportSheet`: fields, `NSOpenPanel` for location,
  WSDL and XSDs, reference rows and warnings, Create/Replace enabled by `can_finish`; the
  replace outcome shown after Replace. Project Settings › Servers edits the model's servers
  (`add_server`, `update_server`, `delete_server`, password into the secret store) and offers
  the suggested servers for confirmation.
- Check: a fixture set with a missing include keeps Create disabled until the file is added;
  Create opens the project; server edits survive closing and reopening the project.

**Step 7 — completion and hover.**
- `textView:completions:forPartialWordRange:indexOfSelectedItem:` and
  `rangeForUserCompletion` from `App::completions`, triggered on `<`, a space in a tag and `="`
  as well as ⌥⎋; hover as a tool tip from `App::hover`.
- Check: completions inside a fixture request's body element; the hover text of an element.

**Needs a Mac (the user, after step 7):** the whole loop with real windows on a real project:
restore, edit, autosave, validate, send, history, log, both sheets, the Keychain prompt.

### Later packages

| Package | Depends on | Owns | Scope |
|---|---|---|---|
| WP-REPLACE-REPORT | VALIDATE (done) | new `crates/washboard-core/src/project/replace_report.rs`, `crates/washboard-cli/src/commands/project.rs` | PLAN §4 "Replace WSDL": after a replace, report operations added/removed and requests that no longer validate; `project replace-wsdl` prints it. Requests are never rewritten. |
| WP-DIST | APP-SHELL | `cargo-packager` metadata in `crates/washboard-app/Cargo.toml`, a release workflow in `.github/workflows/` | DMG via `cargo-packager`; codesign and notarization via `rcodesign` (apple-codesign) or `cargo-packager`'s signing support, configured, not scripted. Check first that both handle Developer ID + hardened runtime + notarytool on macOS 26. |

Not packaged yet: external-change detection with FSEvents (PLAN M5), a manual check of
`KeychainSecretStore` on a Mac (type-checked only so far), and macOS numbers for PLAN §5.1:
`cargo test --release --test validate_perf --test schema_perf -- --nocapture` on a Mac or as a
step in the macOS CI job (CI runs debug tests with captured output, so it shows none today).

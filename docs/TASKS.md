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
| WP-APP-SHELL | `crates/washboard-app/**`, the macOS job in `.github/workflows/ci.yml` | AppKit skeleton in `objc2`: lifecycle, menus, welcome and project windows, TextKit 1 editor with ruler and highlighting, response pane, HTTP log, sheets; `tests/appkit.rs` runs headless on the macOS runner. Unsigned `Washboard.app` from `cargo-packager` (`make app`), uploaded by CI. |
| WP-UI-MODEL | `crates/washboard-ui-model/**` | PLAN §2.1: app and window state, commands, editor buffers, autosave, background jobs, send, history, HTTP log, import sheet, completion and hover; tested on Linux with a fake front end. `block_path` moved into `schema`. |
| WP-APP-INTEGRATION | `crates/washboard-app/**`, additive API in `washboard-ui-model` | The shell bound to the model, steps 1–7. Departures from the shell spec, agreed after screenshots: icon-only toolbar without Save All or a sidebar toggle (View menu instead), server picker beside Send, New Project as its own window, sidebar ⋯ menu (Rename, Duplicate, Validate). |
| WP-FORMAT-XML | `indent` in `crates/washboard-core/src/xml/pretty.rs` and its callers; `crates/washboard-ui-model/src/format.rs`; `request format` in `crates/washboard-cli/src/commands/request.rs`; Format XML and `app_settings.rs` in `crates/washboard-app/**` | PLAN §4 "Format XML (⌃I)": one undo step through the widget, malformed requests untouched. Settings `FormatIndent` (1–8, default 2) and `FormatOnSave` (default off) in user defaults; format on save applies to Save All only, never to autosave or send. The CLI takes `--indent` and `--check` and reads no app settings. |
| WP-DRAFT-GAPS | `crates/washboard-app/**`; additive API in `crates/washboard-ui-model` | The request bar (name, "SOAP 1.1 · Operation" chip, well-formedness state), the sidebar's port chips, and the HTTP log's TLS line. native-tls does not report the TLS version, so the line says "TLS", not "TLS 1.3". |
| WP-SIDEBAR-MENU | `crates/washboard-app/src/sidebar.rs`, the sidebar footer in `project_window.rs` (removed); request order in `crates/washboard-ui-model` | Context menus on sidebar rows instead of the +/−/⋯ footer: a request gets Rename, Duplicate, Validate, Delete; an operation gets New Request (disabled when unsupported); the REQUESTS header gets New Request. The menu acts on the clicked row; Rename and Validate select it first. Requests sort by name in Finder order; `sort_order` stays in the database, unused. |
| WP-SETTINGS-WINDOW | `crates/washboard-app/src/settings_window.rs`; the project settings sheet in `sheets.rs` (removed); the Settings… item in `menu.rs` | One Settings window (⌘,) laid out like System Settings: a "Washboard" section for the app settings (user defaults) and one section per open project (General, Servers; stored in the project). Changes apply as made, no Done button. New Project's server confirmation opens it on the project's Servers pane. Selected pane in `SettingsPane`; the frame autosaves. Replaces the Project Settings sheet and WP-FORMAT-XML's small window; `app_settings.rs` keeps only the defaults keys. |
| WP-DARK-MODE | `crates/washboard-app/**` | PLAN M5 "Dark Mode check": the editor's highlighting palette (`highlight_palette`) with explicit text and background colours on the text views; `tests/appkit.rs` measures each palette colour's contrast in darkAqua (at least 3:1) and prints the aqua values. The look in both appearances is left to the manual checks on a Mac. |
| WP-RESPONSE-LAYOUT | `crates/washboard-app/**`; older exchanges, Show Latest and the History drawer's state in `crates/washboard-ui-model` | PLAN §4 "Response pane and history": response beside the request, a History drawer under the response, an older exchange shown whole (request as sent, read-only) with a titlebar accessory instead of the toolbar items; Send refused while it shows; Restore Request is one undo step. |
| WP-STATUS-COLORS | `crates/washboard-app/src/colors.rs` and the status-colour call sites; the colour rule in `CLAUDE.md` | `error()`, `warning()`, `success()`: functions, not cached values, since the system colours resolve per appearance. `NSColor::system*Color` appears only there and in the editor's highlighting palette. |
| WP-WSDL-SERVERS | the WSDL servers in `crates/washboard-ui-model` (`WsdlServer`, `ProjectWindow::wsdl_servers`); the Servers pane in `settings_window.rs`; `tagged_subtitle_row` in `form.rs` | The WSDL's SOAP 1.1 addresses are read-only "From WSDL" rows after the project's servers, read from the loaded WSDL each time, never stored or sendable. Add as Server… opens the server sheet filled in; nothing is stored before Save. Replaces the "Suggested by the WSDL" section. legacy-rpc lists its rpc/encoded port too, since its binding is SOAP 1.1. |
| WP-SENT-HEADERS | the Headers tab in `crates/washboard-app/**`; `ResponseView::headers_text` in `crates/washboard-ui-model`; `history.request_headers` (schema v2) in `crates/washboard-core/src/project/`; `RawMessage::masked_headers` | The Headers tab shows the request's start line and headers as sent, then the response's. `Authorization` is masked before it is stored, so credentials never reach the database. Projects migrate to v2 on open; older builds refuse them. Rows from before say "not recorded". `history --show` in the CLI prints them too. |
| WP-OPERATION-PICKER | `crates/washboard-app/src/operation_picker.rs`; `pick_operations` and `OperationNode::input` in `crates/washboard-ui-model` | New Request… (⌘N, the REQUESTS header) opens a sheet with a focused search field; ↑/↓ move while typing, Return or a double-click creates, Esc cancels. Prefix matches first; "Service › Port" headers only with more than one supported port; unsupported operations greyed at the end. `App::default_operation` is gone. Until WP-REQUEST-FOLDERS-APP, an operation row still creates directly. |
| WP-RULER-HOVER | the ruler, its tool tips and the underline tool tips in `crates/washboard-app/**`; `text::{line_markers, text_tool_tip}` | Error lines read `!12` in `colors::error()`, warning-only lines `⚠12` in `colors::warning()`. Hovering a marker lists the line's messages, errors first; hovering an underlined error shows its message, then the element's hover. |
| WP-REPLACE-REPORT | `crates/washboard-core/src/project/replace_report.rs`; `project replace-wsdl` in the CLI; the outcome in `crates/washboard-ui-model/src/import.rs` | One `ReplaceReport` (operations added and removed, requests that no longer validate) used by the app and the CLI; `ReplaceOutcome` is an alias. The CLI now lists every request that fails against the new WSDL. Requests are never rewritten. |

## Open

Every package below that owns `crates/washboard-app/**` touches the same controllers; run
those one at a time, or split the paths before handing them out in parallel.

### Manual checks on a Mac
Not a coding package: what CI cannot see, done by a person from `make app` or the CI artifact.
- The whole loop on a real project: restore on launch, edit, autosave, validate, send, history,
  HTTP log, New Project, Replace WSDL, the Settings window, the Keychain prompt (the only check of
  `KeychainSecretStore` so far; it is type-checked on Linux).
- Typing stays responsive in a 1 MB request; completion and hover appear where expected.
- Layout and light/dark appearance against `docs/gui-draft.html`. Findings become app fixes.
- macOS numbers for PLAN §5.1: `cargo test --release --test validate_perf --test schema_perf
  -- --nocapture` (CI runs debug tests with captured output, so it shows none).

### Coding packages

| Package | Depends on | Owns | Scope |
|---|---|---|---|
| WP-DIST | APP-SHELL (done) | `[package.metadata.packager]` and `icons/` in `crates/washboard-app/`, a release workflow in `.github/workflows/`, `Makefile` | The unsigned `.app` already builds (`make app`, CI artifact) with a placeholder icon. Left: the real icon, DMG via `cargo-packager`, codesign and notarization via `rcodesign` (apple-codesign) or `cargo-packager`'s signing support, configured, not scripted. Check first that both handle Developer ID + hardened runtime + notarytool on macOS 26. |
| WP-FSEVENTS | UI-MODEL (done) | file watching in `crates/washboard-ui-model`, `notify` in its `Cargo.toml`, the binding in `crates/washboard-app/**` | PLAN M5 and §2 (`notify`, FSEvents backend): notice request files edited outside the app. The PLAN does not say what happens to an open buffer; decide that first (proposal: reload a clean buffer, keep a dirty one and ask). |
| WP-A11Y | APP-INTEGRATION (done) | `crates/washboard-app/**` | PLAN M5: VoiceOver labels on toolbar items, sidebar rows and the icon-only buttons, checked in `tests/appkit.rs`. |
| WP-REQUEST-FOLDERS | PROJECT, UI-MODEL, CLI (done) | folders in `crates/washboard-core/src/project/{mod,requests,names}.rs` and `RequestMeta::folder` (additive); the sidebar tree, folder commands and New Request placement in `crates/washboard-ui-model`; request lookup and `request list`/`request mv` in `crates/washboard-cli` | Requests can live in folders, which are real subdirectories of `requests/`, and New Request files a request into its operation's folder, creating it on first use. Below. |
| WP-REQUEST-FOLDERS-APP | WP-REQUEST-FOLDERS; OPERATION-PICKER (done) | `crates/washboard-app/src/sidebar.rs`, the folder items in `menu.rs`, folder tests in `tests/appkit.rs` | The sidebar shows the requests' folder tree and drops the OPERATIONS list; folders get context menus, drag and drop moves. Below. |

### WP-REQUEST-FOLDERS in detail

The sidebar's OPERATIONS list goes (WP-REQUEST-FOLDERS-APP); creating a request is the
operation picker's job, and grouping requests becomes the user's, with folders. Folders are
created when they are first needed, not for every operation up front: a WSDL with 200
operations would get 200 empty folders, git does not keep empty directories, and Replace WSDL
would leave folders for removed operations behind.

- **On disk.** A folder is a subdirectory of `requests/`, nested to any depth, so the project
  stays browsable in Finder and git (PLAN §3). Folder names follow the request name rules
  (`names::validate_request_name`). Folders and files starting with `.` are ignored. The disk is
  the truth for folders: they are not in the database, and an empty folder stays until it is
  deleted.
- **Database.** `request.file_name` already holds a path relative to `requests/`; it now may
  contain `/` (always `/`, whatever the platform). No migration. `RequestMeta` gains
  `folder: String` (`""` for the top level), additively.
- **Names stay unique across the project** (case-insensitive, as now), not per folder, so
  `<Operation> <n>` numbering and the CLI's `<name>` arguments keep working. Rename, Duplicate
  and New Request check against every folder. Two files with the same name in different folders
  can still arrive from Finder or git; reconciliation accepts both, the app shows both, and the
  CLI asks for the path (below).
- **Reconciliation** walks `requests/` recursively. A row whose file is gone is matched to a new
  file with the same name elsewhere, when exactly one such file appeared, and treated as a move:
  same id, same history. That covers a move in Finder; anything else stays as today (removed and
  added).
- **New Request placement.** The model's `new_request` takes the target folder. The app passes
  the folder selected in the sidebar when a folder row is selected; otherwise the request goes
  into its operation's folder, created if missing. That folder is named after the operation
  (`GetCustomer`), or `Port/GetCustomer` when the project has more than one supported port, so
  a typical WSDL with one SOAP 1.1 port gets one level. Only supported ports count, so a SOAP
  1.1 port next to a SOAP 1.2 or rpc/encoded one still gives the flat layout. An existing folder
  is matched case-insensitively; if the user renamed the operation's folder, a new one is
  created. Duplicate puts the copy beside the original.
- **Commands** (core, model): create folder, rename folder, delete folder (with every request in
  it and their history; the model reports the count for the confirmation), move a request to a
  folder, move a folder (refused into itself or its descendants, and when the move would put two
  rows of the same name in one folder). All are renames on disk, atomic like other writes.
  Collapsed folders are kept per project in `ui_state`.
- **Sidebar model.** `Sidebar` gains the folder tree next to today's flat `requests`, sorted
  folders first, then requests, each in Finder order (WP-SIDEBAR-MENU). `Sidebar::services`
  stays: the picker uses it.
- **CLI.** `request list` prints paths (`GetCustomer/GetCustomer 1`). Every `<name>` argument
  also takes a path; a bare name that matches two requests fails and lists their paths.
  `request mv <name> <folder>` moves a request (creating the folder; `/` for the top level).
  `request new` places like New Request.
- **Also affected:** WP-FSEVENTS must watch `requests/` recursively.
- **Tests** (Linux): reconciliation of nested files, a move in Finder keeping history, the
  duplicate-name case; placement with one and with several supported ports (both fixtures and a
  synthetic two-port WSDL), the folder reused across case, a selected folder winning; folder
  rename, move and delete with history; the CLI's paths and ambiguity error.

### WP-REQUEST-FOLDERS-APP in detail

- **Sidebar.** One section, REQUESTS: the folder tree, folders with a folder icon and a
  disclosure triangle, requests as today. The OPERATIONS section is removed, and with it an
  operation row's New Request and double-click; unsupported operations are still shown in the
  picker (PLAN §1).
- **Context menus.** A folder: New Request…, New Folder, Rename, Delete. A request: as today.
  The REQUESTS header: New Request…, New Folder. File ▸ New Folder (⌥⌘N) creates one in the
  selected folder, or the selected request's folder, and starts inline rename.
- **Rename** by Return or double-click on a request or folder (PLAN §4); a double-click on a
  folder's triangle still only toggles it.
- **Delete** of a folder confirms with what goes: "Delete “GetCustomer” and its 3 requests?
  Their history is deleted too." An empty folder goes without asking.
- **Drag and drop** moves requests and folders between folders and to the top level (an
  internal pasteboard type, `NSDragOperation::Move` only). Drops the model refuses do not
  highlight.
- **Tests** (`tests/appkit.rs`): no OPERATIONS rows; New Request from the picker lands in the
  operation's folder and starts rename; New Folder, rename, delete with the confirmation; a
  move by the model's command shows in the tree; collapsed folders survive reopening the
  project.

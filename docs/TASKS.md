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

## Open

Every package below that owns `crates/washboard-app/**` touches the same controllers; run
those one at a time, or split the paths before handing them out in parallel.

### Manual checks on a Mac
Not a coding package: what CI cannot see, done by a person from `make app` or the CI artifact.
- The whole loop on a real project: restore on launch, edit, autosave, validate, send, history,
  HTTP log, New Project, Replace WSDL, Project Settings, the Keychain prompt (the only check of
  `KeychainSecretStore` so far; it is type-checked on Linux).
- Typing stays responsive in a 1 MB request; completion and hover appear where expected.
- Layout and light/dark appearance against `docs/gui-draft.html`. Findings become app fixes.
- macOS numbers for PLAN §5.1: `cargo test --release --test validate_perf --test schema_perf
  -- --nocapture` (CI runs debug tests with captured output, so it shows none).

### Coding packages

| Package | Depends on | Owns | Scope |
|---|---|---|---|
| WP-REPLACE-REPORT | VALIDATE, UI-MODEL (done) | new `crates/washboard-core/src/project/replace_report.rs`, `crates/washboard-cli/src/commands/project.rs`, the outcome code in `crates/washboard-ui-model/src/import.rs` | PLAN §4 "Replace WSDL". The report exists twice today: `ui-model`'s `ReplaceOutcome` (operations added/removed, requests that no longer validate) and the CLI's own diff, which only lists requests whose operation is gone. Move the computation into `core`, use it from both, and have `project replace-wsdl` print requests that no longer validate. Requests are never rewritten. |
| WP-DIST | APP-SHELL (done) | `[package.metadata.packager]` and `icons/` in `crates/washboard-app/`, a release workflow in `.github/workflows/`, `Makefile` | The unsigned `.app` already builds (`make app`, CI artifact) with a placeholder icon. Left: the real icon, DMG via `cargo-packager`, codesign and notarization via `rcodesign` (apple-codesign) or `cargo-packager`'s signing support, configured, not scripted. Check first that both handle Developer ID + hardened runtime + notarytool on macOS 26. |
| WP-FORMAT-XML | UI-MODEL (done) | `indent` parameter in `crates/washboard-core/src/xml/pretty.rs` and its callers; a format command and format settings in `crates/washboard-ui-model`; `request format` in `crates/washboard-cli/src/commands/request.rs`; the menu item, binding and Settings window in `crates/washboard-app/**` | PLAN §4 "Format XML (⌃I)": reformat the selected request. Below. |
| WP-FSEVENTS | UI-MODEL (done) | file watching in `crates/washboard-ui-model`, `notify` in its `Cargo.toml`, the binding in `crates/washboard-app/**` | PLAN M5 and §2 (`notify`, FSEvents backend): notice request files edited outside the app. The PLAN does not say what happens to an open buffer; decide that first (proposal: reload a clean buffer, keep a dirty one and ask). |
| WP-A11Y | APP-INTEGRATION (done) | `crates/washboard-app/**` | PLAN M5: VoiceOver labels on toolbar items, sidebar rows and the icon-only buttons, checked in `tests/appkit.rs`. |
| WP-DARK-MODE | APP-INTEGRATION (done) | `crates/washboard-app/**` | PLAN M5 "Dark Mode check". The app already uses only semantic and `system*` colours, so the work is checking, not porting: the editor's highlighting palette and error underlines for contrast on a dark background, text views' background and text colours, and anything drawn by hand (the ruler). The look is judged by the person doing the manual checks, in both appearances. |
| WP-DRAFT-GAPS | APP-INTEGRATION (done) | `crates/washboard-app/**`; additive API in `crates/washboard-ui-model` if a value is missing | What `docs/gui-draft.html` shows and no step built. A bar above the editor: request name, a "SOAP 1.1 · Operation" chip, and the well-formedness state ("Well-formed" / "XML error, line n") from the diagnostics the model already has. In the sidebar, a "1.1" chip on ports and "1.2 · unsupported" on SOAP 1.2 ones, beside today's greyed rows and tooltips. In the HTTP log, the TLS line ("TLS 1.3 · certificate verification SKIPPED (server setting)") from the exchange's `TlsInfo`, which `LogEntry` already carries. |
| WP-RESPONSE-LAYOUT | APP-INTEGRATION (done); after WP-DRAFT-GAPS, which builds the request bar | `crates/washboard-app/**`; additive API in `crates/washboard-ui-model` | PLAN §4 "Response pane and history" and `docs/gui-draft.html`: response beside the request, History tab replaced by a drawer under the response, an older exchange shown whole (request as sent, read-only) with a titlebar accessory instead of the toolbar items. The model needs: which history entry is shown (none = latest), the sent request's text, Send refused while an older one is shown, Restore Request returning text for the app to apply as one undo step, drawer state in `ui_state`. Tests: model side on Linux (send refused, request switch and send return to latest); in `tests/appkit.rs`, selecting an older row shows the accessory and hides Send, Esc returns, ⌘Z after Restore Request brings back the editor's text. |
| WP-RULER-HOVER | APP-INTEGRATION (done) | the ruler and issue tooltips in `crates/washboard-app/**` | Hovering a gutter marker shows the messages of that line's issues, errors first, as a tooltip, like the underline hover in the text. Warnings get a marker too (orange, errors stay red); today only errors are marked. |
| WP-SENT-HEADERS | WP-RESPONSE-LAYOUT | the Headers tab in `crates/washboard-app/**`; additive API in `crates/washboard-ui-model`; a `request_headers` column in `crates/washboard-core/src/project/history.rs` (additive migration) | The Headers tab shows only the response's headers. Add the request's as sent, from `Exchange::request` (`RawMessage` keeps the start line and headers in send order): a "Request" section with the start line and headers, then "Response". `Authorization` is masked as the HTTP log masks it. History stores only `response_headers` today, so older exchanges (WP-RESPONSE-LAYOUT's drawer) need the new column; rows written before it show "not recorded". |
| WP-SIDEBAR-MENU | APP-INTEGRATION (done) | `crates/washboard-app/src/sidebar.rs`, the sidebar footer in `project_window.rs`; request order in `crates/washboard-ui-model` | Replace the +/−/⋯ buttons under the sidebar with a context menu per row: a request gets Rename, Duplicate, Validate, Delete; an operation gets New Request; the REQUESTS header gets New Request. The Project menu and its shortcuts (⌘N, ⌘D, ⌘⌫, ⌘B) stay. Requests sort by name (Finder order: case-insensitive, numbers by value), not by creation; a renamed or new request moves to its place and stays selected. The `sort_order` column stays in the database, unused. |
| WP-SIDEBAR-FLATTEN | WP-FORMAT-XML (adds the app settings and their window); coordinate with WP-SIDEBAR-MENU, which also changes `sidebar.rs` | the OPERATIONS tree in `crates/washboard-app/src/sidebar.rs`, a pure tree-shaping function in `crates/washboard-app/src/text.rs`, one setting in the app settings (`app_settings.rs`, or `settings_window.rs` once WP-SETTINGS-WINDOW has replaced it) | Most WSDLs have one service, and many have one port, so the OPERATIONS group spends two levels on rows with nothing to choose. Leave out a level that has only one row, behind a setting that is on by default. Below. |
| WP-SETTINGS-WINDOW | APP-INTEGRATION (done); coordinate with WP-FORMAT-XML, which adds the first app setting | new `crates/washboard-app/src/settings_window.rs`; the project settings sheet in `sheets.rs` (replaced); the Settings… and Project Settings… items in `menu.rs` | One Settings window for the app and the open projects, replacing the Project Settings sheet. Below. |

### WP-FORMAT-XML in detail

Reformats a request with `xml::pretty_print`, which the response pane already uses: element-only
content is re-indented; text content, comments, PIs, attribute values and the XML declaration
are kept byte for byte. A request that is not well-formed is not touched; the well-formedness
error is shown as usual. Spaces only, no tabs.

- **Settings** (per app, not per project), in the app's user defaults (`NSUserDefaults`, domain
  `at.deduktiva.washboard`), so they live where macOS keeps app settings and `defaults` can read
  them: `FormatIndent` (integer, default 2, allowed 1–8) and `FormatOnSave` (bool, default off).
  The app passes them to the model at launch and on change; the model stores no settings of its
  own. `pretty_print` takes the indent as a parameter instead of its hard-coded two spaces, and
  `TemplateOptions::indent` and the response pane use the same value, so new requests, formatted
  requests and responses look alike.
- **App:** Format XML (⌃I) in the Edit menu, enabled with a request selected. The model computes
  the new text; the app replaces the whole text through the widget so it is one undo step
  (PLAN §2.1 "Not undo/redo") and the selection stays on the same element where practical.
  The indent width and "Format on save" go in the app section of the Settings window
  (WP-SETTINGS-WINDOW; if that is not built yet, Washboard ▸ Settings…, today disabled, opens a
  small window with the two); changing them reformats nothing by itself.
- **Format on save:** applies to File ▸ Save All (⌘S) only, to the open request, as the same
  undo step as ⌃I. Not to autosave: autosave runs a second after typing stops and would rewrite
  the text under the cursor. Never on send; a request is sent as written.
- **CLI:** `washboard request format <name> [--indent N] [--check]`, default indent 2. It does not
  read the app's settings: the CLI builds and runs on Linux, and reading `NSUserDefaults` would
  need CoreFoundation and `unsafe` outside the crates allowed to have it. Rewrites the request
  file in place (taking the project lock like other writing commands) and prints nothing;
  `--check` writes nothing and exits non-zero if the file would change.
- **Tests:** `pretty_print` with indents 1, 4 and 8, including attributes aligned on their own
  lines and `\r\n` input; the model command (refused on a malformed request, no-op on formatted
  text, editor marked dirty and autosaved); format on save on Save All and not on autosave; the
  CLI's exit codes; in `tests/appkit.rs`, ⌃I then ⌘Z restores the text, and the settings round
  trip through a scratch defaults domain.

### WP-SIDEBAR-FLATTEN in detail

The model's tree stays service › port › operation (`Sidebar::services` is unchanged); only the
app's OPERATIONS group is shaped differently.

- **Rule.** With one service, its row is left out and its ports sit directly under OPERATIONS.
  With one port in a service, that port's row is left out and its operations sit directly under
  the service (or under OPERATIONS, when the service row is gone too). Each service is judged on
  its own: in a WSDL with two services, one with a single port and one with two, the first
  shows its operations directly and the second keeps its port rows.
- **Unsupported ports count.** A SOAP 1.2 port is a port: a service with a SOAP 1.1 and a
  SOAP 1.2 port keeps both port rows, so the 1.2 operations stay visible and greyed (PLAN §1:
  shown, never dropped). Only levels with exactly one row, supported or not, are flattened.
- **What a left-out row said** moves into the operations' tooltips: "Service › Port" in front of
  what the tooltip says today (the unsupported reason, if any). WP-DRAFT-GAPS's port chip
  ("1.1", "1.2 · unsupported") is not shown for a left-out port; with only one port, its
  operations' greyed state already tells.
- **Setting** (per app): "Flatten single services and ports in the sidebar", on by default, in
  the user defaults as `SidebarFlatten` (bool) next to the format settings, and in the app
  section of the Settings window. Changing it reshapes every open project window's sidebar at
  once, keeping the selection and the collapsed rows that still exist.
- **Collapsed state** is kept by row path as today (`service:…`, `port:…/…`); a left-out row has
  no state to keep. Turning the setting off shows the restored rows expanded.
- **Unchanged:** double-click on an operation creates a request; New Request picks the first
  supported operation in sidebar order; the REQUESTS group.
- **Tests:** the tree shaping on Linux (`text.rs`): one service with a 1.1 and an unsupported
  port (both fixtures: `customer` has SOAP 1.2, `legacy-rpc` rpc/encoded; only the service row
  goes), and synthetic trees for one service with one port, two services with mixed port
  counts, and the setting off. In `tests/appkit.rs`: the fixture project's OPERATIONS rows
  with the setting on and off, an operation row's tooltip naming its service and port, and
  the selection surviving the toggle.

### WP-SETTINGS-WINDOW in detail

One window, opened by Washboard ▸ Settings… (⌘,), the standard place for settings on macOS.
Project ▸ Project Settings… opens the same window on that project's pane. It is an ordinary
window, not a sheet: it stays open beside the project window and changes apply as they are made,
so there is no Done button.

- **Layout, macOS 26 style:** a sidebar split view (`NSSplitViewItem` sidebar behaviour, so it
  gets the Liquid Glass sidebar) with a unified, title-only toolbar whose title is the selected
  pane, like System Settings. Panes are grouped forms: rounded inset sections on the window
  background, one setting per row, label on the leading edge, control on the trailing edge, a
  hairline between rows, an explanation in secondary text under a section where needed. No
  `NSTabView`, no bezeled boxes. Built from AppKit views (no SwiftUI).
- **App and project, clearly separated:** the sidebar has two sections. "Washboard" holds the
  app settings (General: indent width and format on save from WP-FORMAT-XML; later ones join
  here), stored in user defaults. Below it, one section per open project, titled with the
  project's name and folder icon, with General (name, folder, WSDL files) and Servers. Project
  settings stay in the project's database as today. Each project pane repeats in its header that
  its settings belong to that project and are saved in its folder, so nobody mistakes them for
  app-wide ones. A closed project's section disappears; with no project open there is only the
  app section.
- **Servers pane:** the server list as a grouped section with the +/− buttons inside it, the
  selected server's form as a second section below (Name, URL, Ignore certificate errors, Auth,
  User, Password, Timeout), and the WSDL's suggested servers as a third section with an Add
  button per row. Same model API and Keychain handling as the sheet.
- **State:** the selected pane is remembered (user defaults); the window's frame autosaves.
- **Tests:** in `tests/appkit.rs`: ⌘, opens the window on the app section; Project Settings…
  selects that project's Servers pane; closing a project removes its section; editing a server
  in the window is saved without a Done button; the existing settings sheet checks move to the
  window. The manual checks on a Mac judge the look against System Settings.

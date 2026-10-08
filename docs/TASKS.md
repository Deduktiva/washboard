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
| WP-RULER-HOVER | APP-INTEGRATION (done) | the ruler and issue tooltips in `crates/washboard-app/**` | Hovering a gutter marker shows the messages of that line's issues, errors first, as a tooltip, like the underline hover in the text. Warnings get a marker too (orange, errors stay red); today only errors are marked. |
| WP-SENT-HEADERS | APP-INTEGRATION (done) | the response pane in `crates/washboard-app/**`; additive API in `crates/washboard-ui-model` if `ResponseView` lacks the request | The Headers tab shows only the response. Add the request as sent, from `Exchange::request` (`RawMessage` already keeps the start line and headers in send order): a "Request" section with the start line and headers, then "Response". `Authorization` is masked as the HTTP log masks it. History entries show theirs the same way. |
| WP-SIDEBAR-MENU | APP-INTEGRATION (done) | `crates/washboard-app/src/sidebar.rs`, the sidebar footer in `project_window.rs`; request order in `crates/washboard-ui-model` | Replace the +/−/⋯ buttons under the sidebar with a context menu per row: a request gets Rename, Duplicate, Validate, Delete; an operation gets New Request; the REQUESTS header gets New Request. The Project menu and its shortcuts (⌘N, ⌘D, ⌘⌫, ⌘B) stay. Requests sort by name (Finder order: case-insensitive, numbers by value), not by creation; a renamed or new request moves to its place and stays selected. The `sort_order` column stays in the database, unused. |

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
  App ▸ Settings… (today disabled) opens a small window with the indent width and "Format on
  save"; changing them reformats nothing by itself.
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

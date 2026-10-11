# Washboard — implementation plan

A native macOS SOAP client written in Rust. AppKit UI via `objc2`, no web views,
no network traffic except to servers the user configured.

Decisions taken so far:

| Topic | Decision |
|---|---|
| UI binding | `objc2` + `objc2-app-kit` / `objc2-foundation`, UI built in code (no nibs) |
| Schema validation | libxml2 XSD validation via FFI, **vendored and statically linked** (system copy is 2.9.13 from 2022, see [`libxml2-sys`](../crates/libxml2-sys/README.md)) |
| WSDL/XSD imports | User supplies the referenced files; local files named by a relative path next to the importing file are picked up automatically. All are copied into the project. Unresolved imports are errors. Never fetched. |
| Basic-auth passwords | macOS Keychain; username and everything else in the project database |
| Minimum OS | macOS 26 (the GitHub macOS runners run 26, so CI can launch the app) |
| Distribution | Developer ID, notarized; sandbox-ready but not sandboxed in v1 |
| SOAP | 1.1 only; `document/literal` and `rpc/literal`. No SOAP 1.2, no `rpc/encoded`. |
| Proxies | None. Connections go directly to the configured server. |
| Expected input size | WSDLs > 500 KB, < 200 operations, < 10 import levels (see [performance](specs/performance.md)) |
| Required schema features | `xsi:type` in requests, abstract elements/types + substitution groups, `xs:any` (see [schema model](specs/schema-model.md)) |
| Required WSDL features | `wsdl:import`, `soap:header` (`use="literal"`), `soapAction`, files with a BOM (see [WSDL](specs/wsdl.md)) |

GUI draft: [`gui-draft.html`](gui-draft.html) (open in a browser), ASCII versions in
[`gui-drafts.md`](gui-drafts.md). How each part behaves is specified in [`specs/`](specs/), below.

---

## Scope

### In scope for v1
- Projects = folders on disk, one WSDL (plus its supporting XSDs) each.
- WSDL 1.1 (multi-file via `wsdl:import`), SOAP 1.1 bindings, `document/literal` (wrapped and
  bare), `rpc/literal` ([WSDL](specs/wsdl.md)), literal `soap:header`s. SOAP 1.2 bindings in a WSDL are listed greyed out as
  unsupported, not hidden.
- Request CRUD (new / rename / duplicate / delete), auto-naming, stored as `.xml`.
- Servers per project: name, endpoint URL, "ignore TLS validation errors", auth = none | basic (preemptive).
- XML editor: syntax highlighting, line numbers, live well-formedness errors,
  schema-aware completion, template generation per operation, on-demand and pre-send XSD validation.
- Send, response viewer, persisted per-request response history.
- HTTP log window (in-memory only).
- Autosave + global "Save All" (⌘S).
- Multiple projects open at once, reopened on next launch.

### Out of scope for v1
- WSDL 2.0, SOAP 1.2, `rpc/encoded`.
- WS-Security, WS-Addressing helpers, MTOM/attachments, NTLM/Kerberos, client certificates.
- Custom per-server HTTP headers.
- HTTP proxies (decided: not needed).
- Following HTTP redirects (disabled: they could leave the configured host).

---

## Specs

One file per topic in [`specs/`](specs/). They describe how Washboard behaves and why; the code
is the reference for the details they leave out.

| Spec | Covers |
|---|---|
| [architecture](specs/architecture.md) | crates, key dependencies, threading, the UI model, packaging |
| [disk layout](specs/disk-layout.md) | the project folder, request files and folders, `washboard.sqlite`, locking |
| [macOS app state](specs/macos-app-state.md) | open projects and restore, app settings |
| [new project](specs/new-project.md) | New Project, the import check, Replace WSDL |
| [requests](specs/requests.md) | request and folder commands, placement, sending, the HTTP log, the CLI |
| [sidebar](specs/sidebar.md) | the project window's source list |
| [editor](specs/editor.md) | text view, checking, completion, Format XML, autosave, outside edits |
| [validation](specs/validation.md) | validation steps, errors and spans |
| [response pane](specs/response-pane.md) | response, history drawer, older exchanges |
| [WSDL viewer](specs/wsdl-viewer.md) | the read-only WSDL/XSD viewer, Jump to Definition |
| [settings](specs/settings.md) | the Settings window, servers |
| [WSDL](specs/wsdl.md) | WSDL details, `rpc/literal` |
| [schema bundle](specs/schema-bundle.md) | turning the WSDL's schemas into what libxml2 compiles |
| [schema model](specs/schema-model.md) | the Rust XSD model: `xsi:type`, abstract, `xs:any`, templates |
| [performance](specs/performance.md) | large schemas, targets and measurements, fixture corpus |
| [network](specs/network.md) | no network except configured servers |

---

## Milestones

Each milestone ends in something runnable.

M0 (libxml2 and ureq spikes), M1 (core library and the `washboard` CLI) and M2–M4 are done;
M5 is under way: Replace WSDL with its report, the Dark Mode check and external-change
detection are done; accessibility, the icon and distribution are left. Follow-up work and what
is left of M5 are in `docs/TASKS.md`.

**M2 — App shell + UI model**
App delegate, main menu, welcome window, project window (toolbar, sidebar, editor, response pane),
new-project sheet. The shell is also the objc2 spike: TextKit 1 editor with ruler and
incremental highlighting on a 1 MB XML file, `NSRulerView` subclass, delegates, data sources.
In parallel, `washboard-ui-model` (Linux-testable): app and window state, request CRUD, editor
buffers, autosave/Save All, reopen projects on launch, with a fake front end in tests. Then wire
the AppKit shell to it.

**M3 — Send loop**
Server popup, send with pre-validation, response pane with tabs, history, HTTP log panel,
project settings sheet (servers + Keychain).

**M4 — Schema-aware editor**
Completion, hover docs, gutter markers, issues bar, format XML, live validation if fast enough.

**M5 — Polish & distribution**
Replace WSDL + report, external-change detection (FSEvents), Dark Mode check, accessibility pass
(VoiceOver labels on toolbar/sidebar), app icon; `.app`, DMG, codesign and notarization from
`cargo-packager` configuration plus `rcodesign` (no hand-written bundling or signing scripts).

### CI
- Linux: fmt, clippy, tests for every crate except the AppKit front end (including
  `washboard-ui-model` with its fake front end), the fixture oracle, `cargo deny check bans`.
- macOS runner (`macos-26`): clippy and tests for everything, including the app's headless
  AppKit checks (`crates/washboard-app/tests/appkit.rs`); an unsigned `Washboard.app` artifact.

---

## Open questions

None blocking. Decided: history retention 20 per request, HTTP log keeps 50 exchanges;
both may become configurable in v2.

## Planned for v2

- **Order-aware completion.** v1 offers every child element the content model allows at the
  cursor, regardless of siblings already present. v2 passes the preceding siblings from the
  cursor context (`xml::cursor_context`) to the schema model, which walks the content model
  (sequence position, `maxOccurs` already reached, chosen `xs:choice` branch) and offers only
  what may legally come next, ranking required elements first.
- **Decision pending: own HTTP/1.1 client instead of `ureq`.** `ureq` does not expose the raw
  response, so the HTTP log shows the canonical reason phrase instead of the server's status
  line, lowercased header names, and repeated headers grouped by name; the TLS version is
  unknown. A small client on `native-tls` + `httparse` (POST only, `Content-Length` and chunked
  responses, no redirects, no proxies) would make the log byte-exact in both directions and
  drop a dependency, at the cost of owning chunked decoding and timeout handling. v1 stays on
  `ureq`.
- **Configurable retention:** history entries per request (v1: 20) and HTTP log size (v1: 50).

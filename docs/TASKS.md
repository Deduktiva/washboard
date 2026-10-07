# Work packages

How the work in `docs/PLAN.md` is split so several agents can work in parallel without
stepping on each other. Each package owns a set of paths. Shared contracts are listed
separately and change only additively unless coordinated.

## Shared contracts (exist now, owned by the orchestrator)

| Contract | Path | Used by |
|---|---|---|
| IDs, `QName`, `OperationRef`, `Server`, `Auth`, `RequestMeta`, `HistoryEntry` | `crates/washboard-core/src/model.rs` | all |
| `SchemaBundle`, `SchemaDoc`, `SchemaOrigin` | `crates/washboard-core/src/model.rs` | WSDL → LIBXML2, SCHEMA |
| `Diagnostic`, `TextPos`, `pos_at_byte` | `crates/washboard-core/src/diag.rs` | XML, LIBXML2, VALIDATE, WSDL, APP |
| `SendRequest`, `Exchange`, `RawMessage`, `TlsInfo` | `crates/washboard-core/src/http/exchange.rs` | HTTP, PROJECT (history), APP |
| `SecretStore`, `MemorySecretStore` | `crates/washboard-core/src/secrets.rs` | PROJECT, HTTP callers |
| `xml::decode`, `xml::encode_utf8` | `crates/washboard-core/src/xml.rs` | everything that reads XML |
| SOAP/WSDL/XSD namespace constants | `crates/washboard-core/src/soap.rs` | all |
| Fixtures + oracle | `fixtures/` | all tests |

Adding a field, variant or function to a contract is fine; say so in your final report.
Renaming or removing anything in a contract: don't — report the need instead.

## Status

Wave 1 merged: WP-LIBXML2, WP-WSDL, WP-SCHEMA, WP-XML, WP-PROJECT, WP-HTTP. WP-APP-SHELL is
handed off for a Mac (`docs/handoff/APP-SHELL.md`).

Wave 2: WP-CLI and WP-VALIDATE are done. `crates/washboard-core/tests/pipeline.rs` runs the
real pipeline (`validate::RequestSchema` + `validate::validate_request`) over every fixture
request and its expectation. WP-UI-MODEL, WP-APP-INTEGRATION and WP-DIST are not started.

## Wave 1 — independent, start in parallel

### WP-LIBXML2 — libxml2 build, FFI, safe schema wrapper
Owns: `crates/libxml2-sys/**`, `crates/washboard-core/src/validate/xsd.rs` (+ `validate/mod.rs`
only to declare the module), `vendor/libxml2` (git submodule).
- Add libxml2 as a git submodule at `vendor/libxml2`, pinned to the latest stable `v2.15.x` tag
  (re-check maintenance/security status; record the reasoning in `crates/libxml2-sys/README.md`).
- `build.rs`: build statically with `cmake` (or `cc`) with HTTP, FTP, Python, iconv-if-possible,
  LZMA, zlib, ICU, legacy, catalog off; schemas, push/reader, regexps, threads on.
  `WASHBOARD_LIBXML2=pkg-config` links a system/Homebrew copy instead (dev only).
- Hand-written or bindgen-at-dev-time FFI for exactly what we use (commit the generated file;
  no bindgen at build time).
- Safe wrapper in `validate/xsd.rs`:
  `CompiledSchema::compile(&SchemaBundle) -> Result<CompiledSchema, Vec<Diagnostic>>` and
  `CompiledSchema::validate(&self, xml: &[u8]) -> Vec<Diagnostic>` (validate a standalone
  document; the caller cuts out header/body blocks). Must be `Send`; document thread rules.
- Resource loading: only URIs in the bundle resolve; everything else fails with a diagnostic.
  Prefer the per-context resource loader API if the pinned version has it for schema parsing;
  otherwise the global entity loader behind a mutex. `XML_PARSE_NONET` regardless.
- **Line quirk:** libxml2 reports an element's line as the line where its *start tag ends*.
  Map back to the start-tag line (e.g. by re-scanning the source for the element's `<` before
  that position) so `Diagnostic.pos` points at the start tag. Test with a multi-line start tag.
- Acceptance: until WP-WSDL lands, build bundles by hand in tests from `fixtures/customer/xsd`
  plus small inline documents; reproduce the oracle's verdicts for the body/header blocks of
  the `customer` requests. Builds and tests on Linux and macOS CI.

### WP-WSDL — WSDL model, import graph, bundle construction
Owns: `crates/washboard-core/src/wsdl/**` (convert `wsdl.rs` into a directory).
- Parse with `roxmltree` after `xml::decode`. Load the `wsdl:import` closure; merge definitions
  by QName. Accept `wsdl:import` of an XSD.
- Model: services, ports (with address), bindings (SOAP 1.1 vs 1.2 vs other; style per
  operation), operations with input/output/fault messages, body parts, `soap:header`s,
  `soapAction`. Mark unsupported operations (SOAP 1.2, rpc/encoded) instead of dropping them.
- Import graph over WSDL + XSD (`wsdl:import`, `xs:import`, `xs:include`, `xs:redefine`),
  transitive, cycle-safe, relative to the importing file. Remote URLs matched to supplied files by
  longest path suffix; ambiguities reported. Detect: namespace split across separately imported
  files, XSD 1.1 constructs (`xs:assert`, `xs:alternative`, `xs:override`, `vc:` namespace).
  Output: an import-check report (`Vec<Diagnostic>` with `DiagSource::Import` plus structured data
  for the New Project sheet).
- Build the `SchemaBundle`: inline schema extraction with namespace carry-over, rewriting every
  `schemaLocation` to bundle URIs, rpc/literal wrapper generation (PLAN §5.4, including the
  `xs:include` wrapper when the namespace is shared), generated root importing each namespace once.
- Operation lookup: body element QName → operation (document: part element; rpc: wrapper QName).
- Structural report for `washboard inspect` (counts and flags, no names).
- Acceptance: fixtures produce the expected operations (incl. unsupported ones), headers,
  soapActions; bundle for `customer` and `legacy-rpc` matches what the oracle builds.

### WP-SCHEMA — Rust XSD model
Owns: `crates/washboard-core/src/schema/**` (convert `schema.rs` into a directory).
- Build from a `SchemaBundle` (in tests: hand-made bundles from fixture XSDs).
- Index: global elements/types/groups/attribute groups by QName; derivation graph (both
  directions, transitive); substitution groups (transitive); wildcards with namespace constraints.
- Queries for completion: given an element path with resolved QNames (and `xsi:type` overrides),
  return allowed child elements (with cardinality, abstract/substitution expansion), attributes,
  enumeration values, concrete derived types for `xsi:type`. Documentation strings for hover.
- Template generation for an element QName (PLAN "Template generation"), bounded.
- Performance target: build < 500 ms for a 2 MB bundle; queries < 50 ms (add a synthetic large
  bundle generator under `crates/washboard-core/tests/` or a bench).
- Acceptance: fixture cases — `party` offers `Person`, `Company`, `PublicCompany` for `xsi:type`;
  `ContactMethod` position offers `Email`, `Phone`; `extensions` offers `audit:AuditInfo`.

### WP-XML — editor-side XML utilities
Owns: `crates/washboard-core/src/xml/**` (convert `xml.rs` into a directory, keep `decode`
and `encode_utf8` API unchanged).
- Tokenizer for highlighting: incremental-friendly (re-tokenize a byte range that starts at a
  tag boundary), token kinds: tag name, attribute name/value, namespace prefix, text, comment,
  CDATA, PI, entity, error.
- Well-formedness check with `quick-xml` → first error as a `Diagnostic` with correct position.
- Cursor context: element path from root to cursor with in-scope namespace map and `xsi:type`
  values; whether the cursor is in a tag name, attribute name, attribute value or text.
- Pretty-printer that keeps comments and does not reflow text content.
- Acceptance: fast enough to re-check a 1 MB document under 20 ms (release build).

### WP-PROJECT — project folders and persistence
Owns: `crates/washboard-core/src/project/**` (convert `project.rs` into a directory),
`KeychainSecretStore` in `secrets.rs` (macOS only, additive).
- Folder layout, `washboard.sqlite` schema and migrations (PLAN §3), `rusqlite` with `bundled`.
- Create project (copies WSDL/XSD files preserving relative structure — input is a list of
  source paths + their project-relative destinations; WP-WSDL decides those), open, lock
  (`flock`), reconcile files vs rows.
- Requests: create with auto-name `<Operation> <n>`, rename, duplicate (`<name> copy`, `copy 2`…),
  delete (with history), read/write with atomic writes, last server.
- Servers CRUD; password via `SecretStore`.
- History: store request + response files, `HistoryEntry` rows, retention (20, project setting).
- App-level state file (open projects, bookmark data placeholder).
- Acceptance: all of it covered by tests on Linux using temp dirs.

### WP-HTTP — sending
Owns: `crates/washboard-core/src/http/**` except `exchange.rs` (contract).
- `send(&SendRequest) -> Exchange`, blocking, on `ureq` 3 with `native-tls`.
  No redirects, no proxies, per-server timeout, preemptive basic auth, `Content-Type:
  text/xml; charset=utf-8`, `SOAPAction` quoted (`""` when none).
- Per-request TLS policy: verification disabled only when `ignore_tls_errors`.
- Capture headers as sent (as far as the client allows; document gaps) and the response.
- SOAP fault detection helper for the response.
- Acceptance: tests against a local HTTPS server with a self-signed cert (verification on → error,
  off → success), basic auth header present without a 401, redirect not followed.
  Add a `cargo deny` config banning other HTTP stacks.

### WP-APP-SHELL — AppKit skeleton (macOS; not started in the cloud — see `docs/handoff/APP-SHELL.md`)
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

## Wave 2 — after the wave 1 packages they depend on

| Package | Depends on | Scope |
|---|---|---|
| WP-VALIDATE | LIBXML2, WSDL, XML | Done. Pipeline from PLAN §4 "Validation semantics": well-formedness → SOAP 1.1 root → dispatch of `Body` children → **one** libxml2 pass over the whole document against the bundle plus a shipped SOAP 1.1 envelope schema with lax `Header`/`Body` wildcards (no block extraction). Must reproduce every fixture expectation; replaces `tests/pipeline.rs`. Wires `request validate` and validation before `request send` in the CLI. |
| WP-CLI | all core | Done; see the WP-CLI section below. `request validate` and validation before `request send` are wired after WP-VALIDATE (one call site: `crates/washboard-cli/src/validation.rs`). |
| WP-UI-MODEL | core, VALIDATE | New crate `washboard-ui-model` (PLAN §2.1): app/window state, commands, editor buffers, autosave, background jobs, events to the front end; front-end traits (`MainThread`, `Timers`, `Dialogs`). Tested on Linux with a fake front end. No toolkit dependency. |
| WP-APP-INTEGRATION | APP-SHELL + UI-MODEL | Implement the front-end traits for AppKit and bind views to the model's events and commands. No app behaviour in the AppKit layer. |
| WP-DIST | APP-SHELL | DMG via `cargo-packager`; codesign and notarization via `rcodesign` (apple-codesign) or `cargo-packager`'s signing support, configured, not scripted. Check first that both handle Developer ID + hardened runtime + notarytool on macOS 27. |

### WP-CLI — `washboard` command-line tool (done)
Owns: `crates/washboard-cli/**`; additive read-only open in `crates/washboard-core/src/project/`.
- Same project folders as the app, through `washboard_core::project` and `wsdl` — no separate
  formats or logic in the CLI.
- Commands, noun-verb throughout; `-C/--project <dir>` (default: cwd) selects the project:
  `inspect <wsdl> [--xsd-dir …]` (works on WSDL files, no project; structural report, no names),
  `project new|replace-wsdl|show`, `operation list|template <op> [--save]`,
  `request list|new|show|rename|duplicate|delete|validate|send [--server NAME]|history`,
  `server list|add|edit|remove`. `request send` records history and prints status line and body.
- `validate` and the validation step before `send` call WP-VALIDATE's pipeline; an invalid
  request exits 1. `send --skip-validation` still validates and prints the errors but sends
  anyway, to test how a server handles broken requests (the app keeps blocking the send).
- Locking: read-only commands (`inspect`, `project show`, `operation list|template` without
  `--save`, `request list|show|history`, `server list`) open the project without the lock, so they work while the app
  has it open. Commands that write take the lock and fail with a clear message if it is held.
- Passwords: `KeychainSecretStore` on macOS; elsewhere `WASHBOARD_PASSWORD` or an interactive
  prompt with no echo. `server add/edit --password-stdin` for scripts.
- Output for humans by default; `--json` on `inspect`, the `list` commands and `request history`. Exit codes:
  0 ok, 1 command-level failure (send transport error, SOAP fault with `--fail-on-fault`),
  2 usage/IO/project errors.
- Integration tests run the binary against copies of `fixtures/` in temp dirs (`assert_cmd`
  or plain `std::process::Command`); `send` tested against a local plain-HTTP server.


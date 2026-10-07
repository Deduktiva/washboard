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

Wave 1 merged into `claude/vigilant-meitner-hrthuh`: WP-LIBXML2, WP-WSDL, WP-SCHEMA, WP-XML,
WP-PROJECT, WP-HTTP. `crates/washboard-core/tests/pipeline.rs` checks WSDL → bundle →
libxml2 against the fixture expectations. WP-APP-SHELL is handed off for a Mac
(`docs/handoff/APP-SHELL.md`). Wave 2 not started.

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
- `xtask` or script to produce an unsigned `.app` bundle with `Info.plist`
  (`LSMinimumSystemVersion` 27.0).
- Acceptance: `cargo clippy -p washboard-app --target aarch64-apple-darwin` clean; CI macOS build
  green; list in the final report exactly what must be eyeballed on a Mac.

## Wave 2 — after the wave 1 packages they depend on

| Package | Depends on | Scope |
|---|---|---|
| WP-VALIDATE | LIBXML2, WSDL, XML | Full pipeline from PLAN §4 "Validation semantics": well-formedness → SOAP 1.1 envelope → dispatch → header/body block validation, positions mapped to the request file. Must reproduce every fixture expectation. |
| WP-CLI | all core | Implement the `washboard` subcommands. |
| WP-APP-INTEGRATION | APP-SHELL + core | Wire project, editor, completion, validation, send, history, log, autosave. |
| WP-DIST | APP-SHELL | Codesign, notarize, DMG via `xtask`. |

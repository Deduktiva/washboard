# Washboard — implementation plan

A native macOS SOAP client written in Rust. AppKit UI via `objc2`, no web views,
no network traffic except to servers the user configured.

Decisions taken so far:

| Topic | Decision |
|---|---|
| UI binding | `objc2` + `objc2-app-kit` / `objc2-foundation`, UI built in code (no nibs) |
| Schema validation | libxml2 XSD validation via FFI, **vendored and statically linked** (system copy is 2.9.13 from 2022, see §5) |
| WSDL/XSD imports | User supplies all referenced files; they are copied into the project. Unresolved imports are errors. Never fetched. |
| Basic-auth passwords | macOS Keychain; username and everything else in the project database |
| Minimum OS | macOS 27 |
| Distribution | Developer ID, notarized; sandbox-ready but not sandboxed in v1 |
| SOAP | 1.1 only; `document/literal` and `rpc/literal`. No SOAP 1.2, no `rpc/encoded`. |
| Proxies | None. Connections go directly to the configured server. |
| Expected input size | WSDLs > 500 KB, < 200 operations, < 10 import levels (see §5.1) |
| Required schema features | `xsi:type` in requests, abstract elements/types + substitution groups, `xs:any` (see §5.2) |
| Required WSDL features | `wsdl:import`, `soap:header` (`use="literal"`), `soapAction`, files with a BOM (see §5.3) |

GUI draft: [`gui-draft.html`](gui-draft.html) (open in a browser), ASCII versions in §8.

---

## 1. Scope

### In scope for v1
- Projects = folders on disk, one WSDL (plus its supporting XSDs) each.
- WSDL 1.1 (multi-file via `wsdl:import`), SOAP 1.1 bindings, `document/literal` (wrapped and
  bare), `rpc/literal` (§5.4), literal `soap:header`s. SOAP 1.2 bindings in a WSDL are listed greyed out as
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

## 2. Architecture

Cargo workspace. The split keeps everything except the AppKit layer testable on Linux CI, and
keeps the door open for a Windows or Linux GUI later (§2.1).

```
washboard/
├─ Cargo.toml                 (workspace)
├─ crates/
│  ├─ libxml2-sys/            thin FFI: parser, xmlschemas, error callbacks, entity loader
│  ├─ washboard-core/         no UI, no objc
│  │   ├─ project/            folder layout, sqlite, migrations, request files, history
│  │   ├─ wsdl/               WSDL 1.1 model (services/ports/bindings/operations/messages)
│  │   ├─ schema/             pure-Rust XSD model for completion + templates
│  │   ├─ validate/           libxml2 schema compile/validate, error → (line,col,msg)
│  │   ├─ xml/                tokenizer (highlighting), well-formedness check, pretty-print
│  │   ├─ soap/               envelope/version detection, SOAPAction, fault parsing
│  │   ├─ http/               client, TLS policy, exchange capture for the log
│  │   └─ secrets/            trait SecretStore { get/set/delete } (Keychain impl behind cfg)
│  ├─ washboard-ui-model/     toolkit-independent app behaviour: windows' state, commands, jobs (§2.1)
│  ├─ washboard-cli/          command-line tool on the same project folders, drives core without UI
│  └─ washboard-app/          objc2 AppKit front end: views, menus, editor widget; bundle, assets
└─ packaging                  `cargo-packager` config (.app, DMG) + `rcodesign` for signing/notarizing
```

### Key crates
| Need | Crate | Notes |
|---|---|---|
| AppKit | `objc2`, `objc2-foundation`, `objc2-app-kit`, `block2`, `dispatch2` | `define_class!` for delegates/controllers, `MainThreadMarker` everywhere |
| SQLite | `rusqlite` (`bundled`) | bundled = known version, no surprises from system sqlite |
| XML parsing | own tolerant tokenizer for highlighting and start tags, `roxmltree` for the WSDL/XSD model, libxml2 for well-formedness and validation, `quick-xml` for escaping | one well-formedness verdict for editor and validation |
| XSD validation | own `libxml2-sys`, building a pinned libxml2 release from source (`cc`/`cmake` in `build.rs`), statically linked, HTTP/FTP support compiled out | see §5 for the gotchas |
| HTTP | `ureq` 3 with `native-tls` (Security.framework) | blocking on a worker thread; native trust store incl. user-installed corp CAs; supports disabling verification. Verify in M0 that raw-ish header capture is adequate. |
| Keychain | `security-framework` | generic password, service `at.deduktiva.washboard` |
| IDs / time | `uuid`, `jiff` | |
| File watching (M5) | `notify` (FSEvents backend) | external edits to request files |

No `tokio`. Threads + channels; results are posted back to the UI thread through the front
end's `MainThread` implementation (§2.1; `dispatch2` main queue on macOS).

### Threading model
- **Main thread**: all AppKit, and the `washboard-ui-model` state, which is only touched there
  (`Rc<RefCell<…>>`, deliberately not `Send`).
- **Validation thread** (one per app): owns all libxml2 schema work. If the pinned release only
  offers the process-global external-entity loader, serializing all schema compiles on one
  thread avoids races; if it has per-context resource loaders, this can become a small pool.
  Compiled schemas are cached per project and invalidated on WSDL replacement.
- **HTTP workers**: one short-lived thread per send. "Cancel" detaches and ignores the result
  (blocking ureq can't be interrupted mid-read; a per-server timeout bounds it).
- Workers receive immutable snapshots (`Arc<…>`), never project state.

### 2.1 `washboard-ui-model`: the app without the toolkit
Everything the app *does*, as opposed to how it *looks*, lives in a crate with no UI toolkit
dependency. The AppKit front end only draws state and forwards user input. A Windows or Linux
GUI later reimplements the front end and reuses this crate unchanged.

**Name:** `washboard-ui-model`, as in "view model": it models the UI's state and behaviour,
and the word "model" keeps it apart from `washboard-core` (domain logic) and from the widgets.

**Owns (all toolkit-independent):**
- App state: open projects, recent projects, restore on launch (`project::AppState`), the
  welcome-window condition, the HTTP log ring buffer (50).
- Per project window: the sidebar tree (requests with dirty/invalid markers, operations
  incl. unsupported ones), selection, the server popup's items and selection, the response
  pane's state, history list, issues list, project settings form state.
- Editor buffers: text, dirty flag, BOM preservation, `xml::TokenBuffer`, diagnostics,
  autosave schedule (1 s after the last edit; flush on switch, send, focus loss, quit),
  completion and hover requests answered from `schema::SchemaModel`.
- **Not undo/redo.** The native text widget owns undo (`NSTextView`'s undo manager, later
  `GtkSourceView`'s or the Windows control's); the model only receives the resulting edits
  like any other edit. Programmatic changes the model makes (format XML, insert template,
  completion) are applied through the widget so they land on its undo stack too.
- Commands: new/rename/duplicate/delete request, validate, send (validate first, refuse on
  errors), save all, new project / replace WSDL with the import check, server CRUD.
- Background jobs: schema compile per project, validation, sends; cancellation and
  "stale result" handling (results for a buffer version that has since changed are dropped).

**Interface to a front end:**
- The front end calls commands and reports input (`edit(range, text)`, `select(request)`,
  `send()`, …). Text positions cross the boundary as UTF-16 offsets, the native unit of AppKit
  and Win32 text APIs (a GTK front end converts to its char offsets); the model converts to
  byte offsets with `xml::utf16`.
- The model reports changes as a list of small, typed events (`SidebarChanged`,
  `TokensChanged(range)`, `DiagnosticsChanged`, `ResponseChanged`, `LogAppended`, …), which
  the front end applies to its widgets. No toolkit types cross the boundary.
- The front end provides three traits: `MainThread` (post a closure to the UI thread;
  `dispatch2` on macOS, `glib::idle_add` on GTK, a window message on Win32), `Timers`
  (one-shot timers for autosave and debounce) and `Dialogs` (open/save panels, confirmations,
  alerts), plus a `SecretStore` (Keychain on macOS).

**Stays per toolkit:** windows, menus and shortcuts, toolbar, split views, the text editor widget
(applying token spans as attributes, the line-number ruler, the completion popup), table and
outline views, sheets, the app bundle and platform integration (recent documents, Dock).

**Why now:** the behaviour has to be written once either way. Writing it into a toolkit-free
crate costs little more than writing it into AppKit controllers, and it buys tests: autosave
timing, validate-before-send, stale-result dropping and restore-on-launch run on Linux CI with a
fake front end. Extracting it later from finished AppKit code would mean rewriting it.

**Rough reuse for a second GUI:** `washboard-core` and `washboard-ui-model` unchanged; the
front end is new. For GTK4 the editor would be `GtkSourceView` (XML highlighting and a line
gutter included); on Windows, Scintilla or a RichEdit-based control.

### Why not NSDocument
NSDocument assumes "one file, dirty flag, save prompts". A project is many files plus a DB with
continuous autosave, so we use our own `ProjectWindowController`. We still call
`NSDocumentController.noteNewRecentDocumentURL` so *File ▸ Open Recent* and the Dock menu work.

---

## 3. On-disk layout

A project is a plain folder (not a package, so it's browsable in Finder and git-friendly).

```
Customer API/
├─ washboard.sqlite             metadata (see below)
├─ wsdl/
│  ├─ CustomerService.wsdl      the WSDL as imported (name preserved)
│  └─ xsd/                      supporting XSDs, relative structure preserved
│     └─ common/types.xsd
├─ requests/
│  ├─ GetCustomer 1.xml         full SOAP envelope, exactly what is sent
│  └─ CreateOrder 1.xml
└─ history/
   └─ <request-uuid>/
      ├─ 20261006T140312Z.request.xml
      └─ 20261006T140312Z.response.xml
```

- **Request name = file stem.** Rename = file rename. Names may not contain `/`, `:` or start with `.`.
- **Request files contain the full envelope.** What you see is what is sent; SOAP headers are editable.
- **History** is keyed by request UUID (not name) so renames don't orphan it. The sent request is
  stored too — without it a response is hard to interpret later. Retention: last 20 per request
  (project setting).
- On WSDL replace the previous set moves to `wsdl/.previous/<timestamp>/` (one level kept).
- All writes are atomic (temp file in same dir + `rename`).

### `washboard.sqlite` schema (v1)
```sql
PRAGMA user_version = 1;                -- migrations keyed off this
CREATE TABLE project  (id TEXT PRIMARY KEY,           -- uuid, also the Keychain key prefix
                       name TEXT NOT NULL,
                       wsdl_path TEXT NOT NULL,       -- relative to project root
                       wsdl_imported_at TEXT NOT NULL,
                       history_limit INTEGER NOT NULL DEFAULT 20);
CREATE TABLE server   (id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL,
                       ignore_tls_errors INTEGER NOT NULL DEFAULT 0,
                       auth_kind TEXT NOT NULL CHECK (auth_kind IN ('none','basic')),
                       username TEXT,                 -- password lives in Keychain
                       timeout_secs INTEGER NOT NULL DEFAULT 60,
                       sort_order INTEGER NOT NULL);
CREATE TABLE request  (id TEXT PRIMARY KEY,
                       file_name TEXT NOT NULL UNIQUE,  -- relative to requests/
                       operation TEXT,                  -- "{ns}Port#Operation" hint, may go stale
                       last_server_id TEXT REFERENCES server(id) ON DELETE SET NULL,
                       created_at TEXT NOT NULL, sort_order INTEGER NOT NULL);
CREATE TABLE history  (id TEXT PRIMARY KEY,
                       request_id TEXT NOT NULL REFERENCES request(id) ON DELETE CASCADE,
                       server_id TEXT, url TEXT NOT NULL, sent_at TEXT NOT NULL,
                       duration_ms INTEGER, http_status INTEGER, soap_fault INTEGER,
                       error TEXT,                       -- transport error, if any
                       response_headers TEXT,            -- JSON
                       file_stem TEXT NOT NULL);
CREATE TABLE ui_state (key TEXT PRIMARY KEY, value TEXT);  -- split positions, selection, …
```
- Rollback journal, not WAL: write volume is tiny and WAL side files behave badly on
  synced/network folders.
- **Reconciliation on open**: request files without a row get a row; rows without a file are
  dropped (history kept until pruned). Handles files added/removed in Finder or via git.
- **Same folder opened twice** (two windows or two app instances): `flock` on
  `washboard.sqlite`; second open is refused with a clear message.

### App-level state
`~/Library/Application Support/Washboard/state.json`: list of open projects (path +
bookmark data, window frame autosave name, last selected request). Bookmark data is stored
from day one so we can enable the App Sandbox later without a migration.

---

## 4. Feature behaviour

### Create project (File ▸ New Project…, ⇧⌘N)
1. Sheet: project name, parent location (`NSOpenPanel` with "Create Folder"), WSDL file,
   additional XSD files or a folder.
2. **Import check** runs immediately (validation thread): parse WSDL, walk every
   `wsdl:import`, `xs:import`, `xs:include`, `xs:redefine`. Each reference is shown as
   resolved (to which supplied file) or unresolved. The walk is transitive (XSD → XSD → …),
   cycle-safe, and resolves relative `schemaLocation`s against the *importing* file, so the
   supplied folder structure is preserved on copy. Remote `http(s)://` locations are never fetched;
   they are matched against supplied files by the longest matching path suffix, and an ambiguous
   match (two `common.xsd` in different folders) is reported, not guessed.
   The check also reports the facts from §5.1 (namespaces split across files, XSD 1.1 constructs,
   unsupported bindings) as warnings.
3. *Create* is enabled when nothing is unresolved and the schema set compiles.
4. Copy files, create DB, default server pre-filled from `soap:address`
   (disabled until the user confirms the URL — no implicit connection).

### Replace WSDL (Project ▸ Replace WSDL…)
Same import sheet. After replacing: recompile, re-validate every request, show a report
(operations added/removed, requests now invalid). Requests are never rewritten automatically.

### Requests
- **New** (⌘N): pick an operation (searchable list grouped by service › port). Body is
  generated from the schema (§5). Name: `<Operation> <n>` with the lowest free `n`
  (`GetCustomer 1`, `GetCustomer 2`, …). The new row enters inline-rename immediately.
- Double-clicking an operation in the sidebar's *Operations* section also creates a request.
- **Rename** (Return / double-click), **Duplicate** (⌘D → `GetCustomer 1 copy`),
  **Delete** (⌘⌫, confirms; history deleted with it).
- Each request remembers `last_server_id`; the toolbar server popup shows it. A new request
  uses the most recently used server of the project.

### Editor
- `NSTextView`; TextKit 1 (explicit `NSLayoutManager`) vs TextKit 2 is decided in the M0 spike.
  macOS 27 as the floor makes TextKit 2 more plausible, but line-number rulers and large documents
  have historically been its weak spots, and we need both.
- Line-number ruler (`NSRulerView` subclass) with error/warning markers in the gutter.
- Highlighting: Rust tokenizer, applied as temporary attributes on the layout manager for the
  edited range expanded to the enclosing tag boundaries. Full pass only on load.
- **Well-formedness**: on every edit (debounced 150 ms), libxml2 parse (same parser and options
  as validation) → red underline at the error position + message in the issues bar. This is the
  "not valid XML is visible" requirement. ~15 ms for 1 MB.
- **Schema validation**: Validate button (⌘B), automatically before send, and optionally live
  (debounced 1 s) once we know it's fast enough on large schemas.
- **Completion**: native `NSTextView` completion (`textView:completions:forPartialWordRange:…`)
  fed by the Rust schema model: element names allowed at the cursor path, attribute names,
  enumeration values. Triggered on `<`, space inside a tag, `="`, and ⌥⎋.
- **Hover** on an element name: type name, cardinality, `xs:documentation` (tooltip).
- Format XML (⌃I) — pretty-print preserving comments.

### Validation semantics
1. Must be well-formed.
2. Root must be a SOAP 1.1 `soap:Envelope`. A SOAP 1.2 envelope gets a specific error.
3. Each `Body` child is matched to a binding operation by QName; unknown → error. (Schema
   validation alone would not catch this: the envelope schema allows any `Body` content.)
4. **One libxml2 pass over the whole document** validates envelope, headers and body together.
   The bundle includes a SOAP 1.1 envelope schema (shipped in the app, never fetched) whose
   `Header` and `Body` contain `xs:any processContents="lax"`: every block with a global
   declaration in the project schema is validated fully, and positions refer directly to the
   user's document. Blocks are never cut out, so there is no namespace re-declaration and no
   line mapping beyond the start-tag adjustment in `validate::xsd`.
   Verified with libxml2 2.14 against all schema-level fixtures, including `xsi:type`, abstract
   elements, substitution groups, `xs:any` and rpc/literal wrappers.
5. Header blocks the binding declares (`soap:header`) but that are missing → warning; header
   blocks the binding doesn't declare are validated if the schema knows them (lax), otherwise
   left alone, as SOAP intends.

Errors: list in the issues bar under the editor (click → jump to line), plus gutter markers.
**Send is blocked** while any error exists; the send attempt itself shows the issues bar.

### Send
- Toolbar: server popup, Send (⌘↩). Sends to `server.url` with `Content-Type: text/xml;
  charset=utf-8` and the `SOAPAction` header from the binding operation.
- Basic auth: `Authorization` header built up front (preemptive, no 401 round-trip).
- TLS: platform verification; if the server has "ignore TLS errors", certificate and hostname
  checks are disabled for that request only. The log records whether verification was skipped.
- Response pane: status line, duration, size; tabs **Response** (pretty-printed, highlighted,
  SOAP Faults called out), **Headers**, **History** (list for this request; select to view,
  "Restore request" to copy the sent request into the editor).

### HTTP log (Window ▸ HTTP Log, ⌥⌘L)
One panel for the app. Shows the last exchange in full: request line, headers, body;
status line, headers, body; timing; TLS info. Keeps the last 50 exchanges in memory in a list
above (small extension of "last request and response"; trivially dropped if unwanted). Not
persisted. Bodies over 5 MB are truncated in the view.

### Save / autosave
- Each editor is a buffer with a dirty flag. Autosave 1 s after the last keystroke, and
  immediately on: switching requests, send, window losing key, app deactivate, quit.
- ⌘S = **Save All** across every open project (toolbar button too). Since autosave is
  aggressive, it mostly acts as "flush now"; the window's edited dot reflects unsaved buffers.
- Metadata changes (servers, renames, last server) go straight to SQLite.

### Multiple projects / restore
One window per project. On quit we write the list of open projects; on launch we reopen them
(missing folders reported once, then dropped from the list). With nothing open, a welcome window
lists recent projects with *New Project…* / *Open Project…*.

---

## 5. Schema handling details (highest-risk area)

Two representations, deliberately:
- **libxml2** compiles the schemas for **validation** (authoritative).
- **Our Rust XSD model** (`schema/`) drives **completion and templates**. It needs elements,
  complex/simple types, sequences/choices/all, min/maxOccurs, extensions/restrictions,
  enumerations, groups, attribute groups, the type derivation graph, substitution groups and
  wildcards (§5.2 — these are required, not best effort). It can still be incomplete in exotic
  corners without causing wrong validation results — the worst case is a missing completion.

libxml2 cannot compile a WSDL directly. Steps:
1. Extract each `wsdl:types/xs:schema` into a standalone document. **Copy in-scope namespace
   declarations from `wsdl:definitions`** onto the extracted `xs:schema` — the classic bug where
   `type="tns:Foo"` breaks after extraction.
2. Inline schemas often `xs:import` each other by namespace without `schemaLocation`. Generate a
   synthetic root schema that imports every schema by a synthetic URI
   (`washboard-inline:/<n>.xsd`).
3. Install a custom `xmlExternalEntityLoader` that resolves only: synthetic URIs, paths inside the
   project's `wsdl/` folder, and the bundled SOAP envelope XSDs. Everything else fails. Parse
   with `XML_PARSE_NONET` as a second line of defence.
4. Use `xmlSchemaSetValidStructuredErrors` to collect `(line, col, message)`.

5. **One namespace split across several files.** libxml2 imports each target namespace only once:
   a second `xs:import` of the same namespace with a different `schemaLocation` is silently
   skipped, and the types in that file go missing. Your WSDLs reportedly don't do this, so the
   import check only *detects* it and warns. The fix (a generated wrapper schema per namespace
   that `xs:include`s all its files, with every import of that namespace pointed at it) is
   built only if a real WSDL needs it — or if `rpc/literal` needs it (§5.4).

### libxml2: vendored, not system
The system library on current macOS is 2.9.13 (Feb 2022), built with HTTP and FTP support.
Reasons to vendor a pinned current release instead:
- Several years of XSD validator fixes and security fixes are missing from 2.9.13, and we cannot
  control when or whether Apple updates it.
- A build with HTTP/FTP compiled out makes "no network from the schema loader" a property of the
  binary, not only of our entity loader and `XML_PARSE_NONET`.
- Newer releases have per-context resource loaders instead of the process-global
  `xmlSetExternalEntityLoader` (verify in M0), which removes the single-validation-thread constraint.
- Same version in CI (Linux) and in the app, so test results carry over.

For faster local builds, `WASHBOARD_LIBXML2=pkg-config` links a Homebrew/distro copy instead;
release builds and CI always use the vendored one (a Homebrew dylib would not exist on users'
Macs, and a static Homebrew copy makes releases depend on the build machine).

Cost: a C build in `build.rs` and tracking upstream security releases (MIT license, static linking
is fine). Because upstream maintenance has been thin at times, pick the release in M0 and record
how we'll watch for advisories.

Known libxml2 limits regardless of version: XSD 1.0 only; some edge cases (complex `xs:redefine`,
certain identity constraints) remain weak.

### 5.1 Large and deeply nested schemas
Inputs are > 500 KB WSDLs with multi-level XSD imports. Consequences:
- **Compile once, off the main thread.** Compiled `xmlSchemaPtr` is cached per project, built on
  open in the background; the window is usable immediately, validation and completion show
  "Loading schema…" until ready. Target: open-to-ready under 2 s for a 2 MB schema set.
- **Rust schema model is indexed**, not walked: global elements/types by QName in hash maps, and
  a lazily built per-type "allowed children" table for completion. Target: completion under 50 ms.
- **Template generation is bounded**: recursion depth limit (default 6) and a node budget; when
  cut, a `<!-- … truncated: type Foo -->` comment is emitted instead of silently stopping.
- **Validation of a request** reuses the compiled schema; cost is proportional to the request,
  not the schema. Live validation is enabled only if M0 measures it under ~100 ms.
- **Fixture corpus**: since real WSDLs can't be shared, CI uses public WSDLs of similar shape plus
  a generator that synthesises large schema sets (N files, depth D, namespaces split across files,
  recursive types, deep derivation chains, substitution groups across namespaces, `xs:any`,
  multi-file `wsdl:import`, BOM-prefixed files). `washboard-cli inspect` (§7, M1) prints the same structural report locally,
  without contents, so real WSDLs can be checked against the assumptions here.

### 5.2 `xsi:type`, abstract declarations, `xs:any`
Validation of all three is libxml2's job and it handles them (derivation checks, `block`/`final`,
abstract-without-`xsi:type` errors, `processContents`). The work is in the Rust model, which
needs a project-wide index built once per schema compile:
- **Derivation graph**: for every complex/simple type, its base and all transitively derived
  types (extension and restriction), across all files and namespaces.
- **Substitution groups**: for every element, the transitive set of members, with abstract
  heads excluded from suggestions.

Behaviour:
- **Element position whose declared type is abstract or has derived types**: completion offers
  the element as usual; inside its start tag, `xsi:type="` completes with the concrete derived
  types (abstract ones excluded, `block` respected). Once `xsi:type` is set, child/attribute
  completion uses the **derived** type's content model, not the declared one.
- **QName values**: `xsi:type` values are QNames. Completion inserts the prefix in scope for the
  type's namespace; if there is none, it adds an `xmlns:nsN` declaration on the element. The
  model resolves prefixes using the editor's in-scope namespace map at the cursor.
- **Abstract element head** (`ref` to an abstract element): completion offers the substitution
  group members instead of the head.
- **`xs:any`**: completion offers global elements from the namespaces the wildcard allows
  (`##other`, `##targetNamespace`, explicit lists); for `processContents="skip"` no suggestions,
  only a hint. `xs:anyAttribute` likewise for attributes.
- **Hover** shows the effective type ("declared `tns:Party`, actual `tns:Company` via xsi:type").
- **Validation errors** from libxml2 for these cases are terse (e.g. "The type definition is
  abstract"); we append the list of allowed concrete types/members from the index.

### 5.3 WSDL-level details
- **`wsdl:import`**: WSDL files are loaded transitively and merged into one definitions model
  keyed by QName (messages, portTypes, bindings, services may live in different files). Some
  real-world WSDLs `wsdl:import` an XSD; that is accepted and treated like `xs:import`. Each
  file's own `wsdl:types` schemas are extracted as in §5 step 1, with that file's namespaces.
- **`soap:header message="…" part="…" use="literal"`**: the header part may come from a
  different message than the body. Headers are emitted in templates and validated (§4,
  validation step 6). Parts used as headers are excluded from the body; `soap:body parts="…"`
  is honoured.
- **Style is per operation**: `soap:operation style` overrides `soap:binding style`; the check
  is done per operation, so a mostly-document binding with one rpc operation still loads.
- **`soapAction`**: sent quoted when set; when absent or empty, `SOAPAction: ""` is sent (SOAP 1.1
  requires the header to be present).
- **BOM / encodings**: all inputs are decoded through one function that honours a UTF-8/UTF-16
  BOM and the XML declaration's `encoding`, then hands `&str` to `roxmltree`/`quick-xml`
  (neither accepts a BOM-prefixed or UTF-16 input as-is). libxml2 gets the original bytes.
  Copied WSDL/XSD files stay byte-identical. Line/column mapping is char-based, so the BOM does
  not shift positions. Request files are written as UTF-8 without BOM; a BOM in a file the
  user edited externally is preserved.

### 5.4 `rpc/literal`
Handled by turning it into the document case: for every rpc operation we **generate a schema**
and feed it to both libxml2 and the Rust model, so validation, completion and templates need no
rpc-specific code paths.
- Request wrapper: a global element named after the operation, in the `namespace` of the
  operation's `soap:body`; response wrapper: `<operation>Response`.
- Content: `xs:sequence` of one **unqualified** local element per message part, in message order
  (or the order of `soap:body parts="…"`), named after the part and typed by the part's `type`.
  WS-I requires `type=` parts for rpc; a part declared with `element=` is accepted as a `ref`.
- Operation dispatch on the request works as for document style: by the wrapper's QName.
- `soap:header` parts in rpc operations stay document-style (they reference elements).
- **Gotcha:** if the `soap:body` namespace equals a `targetNamespace` already used by a real
  schema, the generated schema is a second file for that namespace — the libxml2 import problem
  from §5 step 5. Then the generated declarations go into a wrapper schema that `xs:include`s
  the real one. Plan to need this, because services commonly reuse their tns for rpc wrappers.
- `rpc/encoded` operations still load greyed out as unsupported.

### Template generation
For an operation's input: emit a SOAP 1.1 envelope, declared headers, and
the body element expanded recursively. Required elements emitted, optional ones emitted with
`<!-- optional -->`, `xs:choice` emits the first branch plus a comment listing alternatives,
recursion depth-limited, simple-type placeholders by type (`?` for strings, `0` for numerics,
first enum value, `2026-01-01` for dates). Abstract element heads are replaced by the first
concrete substitution member and abstract types get `xsi:type` with the first concrete derived
type, each followed by a comment listing the alternatives. `xs:any` emits
`<!-- any element from: … -->`.

---

## 6. "No network except configured servers" — enforcement

- The only code that opens sockets is `core::http`, and it only accepts a `Server` value.
- No update checker, analytics, crash reporter, or remote fonts/images. Sparkle etc. excluded.
- libxml2: custom entity loader + `XML_PARSE_NONET` (§5).
- Redirects disabled; a 3xx is shown as the response.
- Proxy: ureq's env proxy detection disabled; system proxy settings are ignored. No proxy support.
- DNS lookups for the server host are, of course, still made.
- CI check: `cargo deny check bans` with a ban list in `deny.toml` (no `reqwest`, `hyper`,
  telemetry crates), and `washboard-core/tests/network_boundary.rs`: network-capable crates
  reach the dependency graph only through core's `ureq` dependency, and socket/HTTP APIs appear
  in source only under `core/src/http/`.
- Optional (later): App Sandbox with only `com.apple.security.network.client`; doesn't restrict
  hosts but limits blast radius.

---

## 7. Milestones

Each milestone ends in something runnable.

**M0 — Spikes (de-risk before committing to structure)**
- objc2: window + `NSSplitView` + `NSOutlineView` source list + `NSTextView` (TextKit 1 and 2) with ruler
  and incremental highlighting on a 1 MB XML file. Goal: confirm objc2 ergonomics for
  subclassing (`NSRulerView`, delegates, data sources).
- libxml2: compile inline schemas from 3–4 real-world WSDLs (with cross imports, external XSDs)
  using the synthetic-root + custom-loader approach; capture line-numbered errors. Include a
  synthetic 2 MB set with a namespace split across files and 4+ import levels. Measure compile
  time and per-request validation time. Build the vendored libxml2 via `build.rs` on macOS and
  Linux; check whether per-context resource loaders are usable for schema compiles.
- ureq + native-tls: self-signed server, toggle verification; check what we can capture for the log.

**M1 — Core library + CLI**
Project create/open, DB + migrations, WSDL import & import-check, request CRUD + auto-naming,
servers, Keychain secrets, template generation, validation (incl. generated rpc/literal schemas), send, history. `washboard-cli`
exercises all of it. Unit tests + fixture WSDL corpus + local HTTPS test server.
`washboard-cli inspect <wsdl> [xsd-dir]` prints the structural report from §5.1 (counts, depths,
flags; no names or contents), so it can be run on WSDLs that can't leave your machine.

**M2 — App shell + UI model**
App delegate, main menu, welcome window, project window (toolbar, sidebar, editor, response pane),
new-project sheet. In parallel, `washboard-ui-model` (Linux-testable): app and window state,
request CRUD, editor buffers, autosave/Save All, reopen projects on launch, with a fake front
end in tests. Then wire the AppKit shell to it.

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
- macOS runner: clippy and tests for everything; later an unsigned `.app` artifact.

---

## 8. GUI draft (ASCII)

Interactive version: [`gui-draft.html`](gui-draft.html).

### Project window
```
┌──────────────────────────────────────────────────────────────────────────────────────────────┐
│ ● ● ●  [⫶]  Customer API          Server: [Staging ▾]  [✓ Validate] [▶ Send]   [💾] [Log]    │
├──────────────────────┬───────────────────────────────────────────────────────────────────────┤
│ REQUESTS          [+]│  GetCustomer 1                         SOAP 1.1 · GetCustomer           │
│  ▸ GetCustomer 1     │ ┌───┬─────────────────────────────────────────────────────────────────┐ │
│    GetCustomer 2  •  │ │  1│<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/…"  │ │
│    CreateOrder 1  ⚠  │ │  2│                  xmlns:cus="urn:example:customer">              │ │
│                      │ │  3│  <soapenv:Header/>                                              │ │
│ OPERATIONS           │ │  4│  <soapenv:Body>                                                 │ │
│  ▾ CustomerService   │ │ ⚠5│    <cus:GetCustomer>                                            │ │
│    ▾ CustomerPort    │ │  6│      <cus:customerId>?</cus:customerId>                         │ │
│        GetCustomer   │ │  7│    </cus:GetCustomer>                                           │ │
│        CreateOrder   │ │  …│                                                                 │ │
│        ListOrders    │ └───┴─────────────────────────────────────────────────────────────────┘ │
│                      │  ⚠ 1 error  line 6: 'customerId': '?' is not a valid xs:long   [Hide] │
│                      ├───────────────────────────────────────────────────────────────────────┤
│                      │  200 OK · 143 ms · 1.2 KB         [Response] [Headers] [History (7)]   │
│                      │  <soap:Envelope …>                                                    │
│                      │    <soap:Body>                                                        │
│ [+] [−] [⋯]          │      <GetCustomerResponse>…                                           │
└──────────────────────┴───────────────────────────────────────────────────────────────────────┘
   • unsaved   ⚠ fails validation
```

### New project sheet
```
┌ New Project ─────────────────────────────────────────────┐
│ Name:      [Customer API                          ]       │
│ Location:  ~/Projects/soap               [Choose…]        │
│ WSDL:      CustomerService.wsdl          [Choose…]        │
│ XSD files: common/types.xsd, faults.xsd  [Add…] [−]       │
│                                                           │
│ References                                                │
│  ✓ xs:import urn:example:common → common/types.xsd        │
│  ✓ xs:import urn:example:faults → faults.xsd              │
│  ✗ xs:include addresses.xsd    — not supplied             │
│                                                           │
│                               [Cancel]  [Create] (disabled)│
└───────────────────────────────────────────────────────────┘
```

### Project settings → Servers
```
┌ Customer API — Settings ──────────[ General | Servers ]──┐
│ ┌──────────────┐  Name:  [Staging                    ]   │
│ │ Production   │  URL:   [https://stg.example.com/ws/c ]  │
│ │▸Staging      │  TLS:   [x] Ignore certificate errors    │
│ │ Local        │  Auth:  (•) None  ( ) Basic              │
│ └──────────────┘  User:  [                ]  (disabled)    │
│  [+] [−]          Pass:  [                ]  (Keychain)    │
│                   Timeout: [60] s                         │
└───────────────────────────────────────────────────────────┘
```

### HTTP log panel
```
┌ HTTP Log ────────────────────────────────────────────────────────┐
│ 14:03:12  Customer API  POST stg.example.com  200  143 ms  ← last │
│ 14:01:55  Customer API  POST stg.example.com  500  98 ms          │
├──────────────────────────────┬───────────────────────────────────┤
│ POST /ws/customer HTTP/1.1   │ HTTP/1.1 200 OK                   │
│ Host: stg.example.com        │ Content-Type: text/xml; charset=… │
│ Content-Type: text/xml; …    │ Content-Length: 1234              │
│ SOAPAction: "urn:…/GetCust…" │                                   │
│ Authorization: Basic ••••••  │ <soap:Envelope …>                 │
│                              │                                   │
│ <soapenv:Envelope …>         │                                   │
│ TLS: verification SKIPPED    │                                   │
└──────────────────────────────┴───────────────────────────────────┘
```
Authorization values are masked in the log by default (click to reveal).

---

## 9. Open questions

None blocking. Decided: history retention 20 per request, HTTP log keeps 50 exchanges;
both may become configurable in v2.

## 10. Planned for v2

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

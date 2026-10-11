# Work packages

How the work in `docs/PLAN.md` is split so several agents can work in parallel without
stepping on each other. Each package owns a set of paths. Shared contracts are listed
separately and change only additively unless coordinated.

Finished packages are removed: their specs live in git history and their behaviour is the code
and its tests. Follow-up work on code a finished package wrote is a new package that names the
paths it owns.

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
| WP-DIST | — | `[package.metadata.packager]` and `icons/` in `crates/washboard-app/`, a release workflow in `.github/workflows/`, `Makefile` | The unsigned `.app` already builds (`make app`, CI artifact) with a placeholder icon. Left: the real icon, DMG via `cargo-packager`, codesign and notarization via `rcodesign` (apple-codesign) or `cargo-packager`'s signing support, configured, not scripted. Check first that both handle Developer ID + hardened runtime + notarytool on macOS 26. |
| WP-FSEVENTS | — | file watching in `crates/washboard-ui-model`, `notify` in its `Cargo.toml`, the binding in `crates/washboard-app/**` | PLAN M5 and §2 (`notify`, FSEvents backend): notice request files edited outside the app. The PLAN does not say what happens to an open buffer; decide that first (proposal: reload a clean buffer, keep a dirty one and ask). |
| WP-A11Y | — | `crates/washboard-app/**` | PLAN M5: VoiceOver labels on toolbar items, sidebar rows and the icon-only buttons, checked in `tests/appkit.rs`. |
| WP-REQUEST-FOLDERS | — | folders in `crates/washboard-core/src/project/{mod,requests,names}.rs` and `RequestMeta::folder` (additive); the sidebar tree, folder commands and New Request placement in `crates/washboard-ui-model`; request lookup and `request list`/`request mv` in `crates/washboard-cli` | Requests can live in folders, which are real subdirectories of `requests/`, and New Request files a request into its operation's folder, creating it on first use. Below. |
| WP-REQUEST-FOLDERS-APP | WP-REQUEST-FOLDERS | `crates/washboard-app/src/sidebar.rs`, the folder items in `menu.rs`, folder tests in `tests/appkit.rs` | The sidebar shows the requests' folder tree and drops the OPERATIONS list; folders get context menus, drag and drop moves. Below. |

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
  folders first, then requests, each in Finder order (`window::finder_order`).
  `Sidebar::services` stays: the picker uses it.
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

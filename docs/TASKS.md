# Work packages

How the work in `docs/PLAN.md` and `docs/specs/` is split so several agents can work in parallel without
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
| `SendRequest`, `Exchange`, `RawMessage` | `crates/washboard-core/src/http/exchange.rs` | HTTP, PROJECT (history), APP |
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
  New Project, Replace WSDL, the Settings window, the Keychain prompt (the only check of
  `KeychainSecretStore` so far; it is type-checked on Linux).
- Typing stays responsive in a 1 MB request; completion and hover appear where expected.
- Layout and light/dark appearance against `docs/gui-draft.html`. Findings become app fixes.
- macOS numbers for `docs/specs/performance.md`: `cargo test --release --test validate_perf --test schema_perf
  -- --nocapture` (CI runs debug tests with captured output, so it shows none).

### Coding packages

| Package | Depends on | Owns | Scope |
|---|---|---|---|
| WP-DIST | — | `[package.metadata.packager]` and `icons/` in `crates/washboard-app/`, a release workflow in `.github/workflows/`, `Makefile` | The unsigned `.app` already builds (`make app`, CI artifact) with a placeholder icon. Left: the real icon, DMG via `cargo-packager`, codesign and notarization via `rcodesign` (apple-codesign) or `cargo-packager`'s signing support, configured, not scripted. Check first that both handle Developer ID + hardened runtime + notarytool on macOS 26. |
| WP-A11Y | — | `crates/washboard-app/**` | PLAN M5: VoiceOver labels on toolbar items, sidebar rows and the icon-only buttons, checked in `tests/appkit.rs`. |
| WP-REQUEST-FOLDERS | — | folders in `crates/washboard-core/src/project/{mod,requests,names}.rs` and `RequestMeta::folder` (additive); the sidebar tree, folder commands and New Request placement in `crates/washboard-ui-model`; request lookup and `request list`/`request mv` in `crates/washboard-cli` | Requests can live in folders, which are real subdirectories of `requests/`, and New Request files a request into its operation's folder, creating it on first use. Below. |
| WP-REQUEST-FOLDERS-APP | WP-REQUEST-FOLDERS | `crates/washboard-app/src/sidebar.rs`, the folder items in `menu.rs`, folder tests in `tests/appkit.rs` | The sidebar shows the requests' folder tree and drops the OPERATIONS list; folders get context menus, drag and drop moves. Below. |

### WP-REQUEST-FOLDERS tests
Behaviour: `docs/specs/disk-layout.md`, `docs/specs/requests.md`. Tests (Linux): reconciliation
of nested files, a move in Finder keeping history, the duplicate-name case; placement with one
and with several supported ports (both fixtures and a synthetic two-port WSDL), the folder
reused across case, a selected folder winning; folder rename, move and delete with history; the
CLI's paths and ambiguity error.

### WP-REQUEST-FOLDERS-APP tests
Behaviour: `docs/specs/sidebar.md`. Tests (`tests/appkit.rs`): no OPERATIONS rows; New Request
from the picker lands in the operation's folder and starts rename; New Folder, rename, delete
with the confirmation; a move by the model's command shows in the tree; collapsed folders
survive reopening the project.

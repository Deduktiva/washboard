# Architecture

Cargo workspace. The split keeps everything except the AppKit layer testable on Linux CI, and
keeps the door open for a Windows or Linux GUI later (see "`washboard-ui-model`" below).

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
│  │   ├─ soap.rs             namespaces, envelope builder, body elements
│  │   ├─ http/               client, TLS policy, exchange capture for the log
│  │   └─ secrets.rs          trait SecretStore { get/set/delete } (Keychain impl behind cfg)
│  ├─ washboard-ui-model/     toolkit-independent app behaviour: windows' state, commands, jobs
│  ├─ washboard-cli/          `washboard` command-line tool on the same project folders, no UI
│  ├─ washboard-app/          objc2 AppKit front end: views, menus, editor widget; bundle, assets
│  └─ washboard-test-support/ dev-dependency only: the fixtures path, a local HTTP server for tests
└─ packaging                  `cargo-packager` config (.app, DMG) + `rcodesign` for signing/notarizing
```

## Key crates
| Need | Crate | Notes |
|---|---|---|
| AppKit | `objc2`, `objc2-foundation`, `objc2-app-kit`, `block2`, `dispatch2` | `define_class!` for delegates/controllers, `MainThreadMarker` everywhere |
| SQLite | `rusqlite` (`bundled`) | bundled = known version, no surprises from system sqlite |
| XML parsing | own tolerant tokenizer for highlighting and start tags, `roxmltree` for the WSDL/XSD model, libxml2 for well-formedness and validation, `quick-xml` for escaping | one well-formedness verdict for editor and validation |
| XSD validation | own `libxml2-sys`, building a pinned libxml2 release from source (`cc`/`cmake` in `build.rs`), statically linked, HTTP/FTP support compiled out | see [schema bundle](schema-bundle.md) and [`libxml2-sys`](../../crates/libxml2-sys/README.md) |
| HTTP | `ureq` 3 with `native-tls` (Security.framework) | blocking on a worker thread; native trust store incl. user-installed corp CAs; supports disabling verification. Header capture is close but not byte-exact (PLAN, "Planned for v2"). |
| Keychain | `security-framework` | generic password, service `at.deduktiva.washboard` |
| IDs / time | `uuid`, `jiff` | |
| File watching | `notify` (FSEvents backend) | external edits to request files |

No `tokio`. Threads + channels; workers queue their results in the model and wake the UI thread
through the front end's `MainThread` implementation (`dispatch2` main queue on macOS).

## Threading model
- **Main thread**: all AppKit, and the `washboard-ui-model` state, which is only touched there
  (`Rc<RefCell<…>>`, deliberately not `Send`).
- **Validation threads**: libxml2 schema work runs off the main thread. Compiles on different
  threads don't interfere (each thread's loader serves only its own bundle, see
  `validate::xsd`), so this can be a small pool. A `CompiledSchema` is `Send`, not `Sync`: one
  thread validates against it at a time. Compiled schemas are cached per project and
  invalidated on WSDL replacement.
- **HTTP workers**: one short-lived thread per send. "Cancel" detaches and ignores the result
  (blocking ureq can't be interrupted mid-read; a per-server timeout bounds it).
- Workers receive immutable snapshots (`Arc<…>`), never project state.

## `washboard-ui-model`: the app without the toolkit
Everything the app *does*, as opposed to how it *looks*, lives in a crate with no UI toolkit
dependency. The AppKit front end only draws state and forwards user input. A Windows or Linux
GUI later reimplements the front end and reuses this crate unchanged.

**Name:** `washboard-ui-model`, as in "view model": it models the UI's state and behaviour,
and the word "model" keeps it apart from `washboard-core` (domain logic) and from the widgets.

**Owns (all toolkit-independent):**
- App state: open projects, recent projects, restore on launch (`project::AppState`), the
  welcome-window condition, the HTTP log ring buffer (50).
- Per project window: the sidebar tree (folders, requests with dirty/invalid markers), the
  operations incl. unsupported ones for the New picker, selection, the server popup's items and
  selection, the response pane's state, history list, issues list, the Settings window's project
  panes.
- Editor buffers: text, dirty flag, BOM preservation, `xml::TokenBuffer`, diagnostics,
  autosave schedule (1 s after the last edit; flush on switch, send, focus loss, quit),
  completion and hover requests answered from `schema::SchemaModel`.
- Diagnostics for the editor ([validation](validation.md), "Errors"): underline
  `Diagnostic::span` when present, else mark `pos`; both are converted to UTF-16 for the front
  end like every other text position. The project's one `SchemaModel` (built with the schema
  compile, shared in an `Arc`) also goes to `RequestSchema::with_model`, so abstract-type errors
  name the alternatives. `Diagnostic::detail` is there for quick fixes later (e.g. "set
  `xsi:type` to …"); none are planned yet.
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
- The front end provides three traits: `MainThread` (wake the UI thread, which then calls
  `App::pump` to apply queued worker results with `&mut App`, so the front end needs no global
  to find the app; `dispatch2` on macOS, `glib::idle_add` on GTK, a window message on Win32),
  `Timers` (one-shot timers for autosave and debounce) and `Dialogs` (open/save panels,
  confirmations, alerts), plus a `SecretStore` (Keychain on macOS).

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

## Packaging
`cargo-packager` builds `Washboard.app` from configuration (`Info.plist`: `CFBundleIdentifier`
`at.deduktiva.washboard`, `LSMinimumSystemVersion` 26.0, `NSHighResolutionCapable`, version
from Cargo); WP-DIST adds the DMG, signing and notarization. The app's Cargo binary stays
`washboard-app`: the CLI is `washboard`, and on case-insensitive APFS a binary named `Washboard`
would overwrite it in `target/release/`. Inside the bundle the executable is `Washboard`
(`CFBundleExecutable`), in `Washboard.app/Contents/MacOS/`. The macOS CI job builds the bundle,
but only a Mac with a display can show it.

## Why not NSDocument
NSDocument assumes "one file, dirty flag, save prompts". A project is many files plus a DB with
continuous autosave, so we use our own `ProjectWindowController`. We still call
`NSDocumentController.noteNewRecentDocumentURL` so *File ▸ Open Recent* and the Dock menu work.

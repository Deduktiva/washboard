# Handoff: WP-APP-SHELL (AppKit skeleton)

For whoever builds the app shell **on a Mac**: a person, or a Claude Code session running
locally on macOS 27. It was deliberately not started in the cloud, because the cloud agents run
on Linux. There they can type-check the app but not link, launch, or look at it.

Read first: `CLAUDE.md`, `docs/PLAN.md` §2 (architecture, threading, "Why not NSDocument"),
§4 (feature behaviour), §8 (ASCII GUI), and `docs/gui-draft.html` (open it in a browser; it is
the agreed visual target). The WP-APP-SHELL section in `docs/TASKS.md` is the scope summary;
this file is the detailed version.

## Where things stand

- `crates/washboard-app/src/main.rs` opens one empty `NSWindow` with plain `NSApplication`
  setup. It has never been run. Expect to replace almost all of it.
- Dependencies: `objc2 0.6`, `objc2-foundation 0.3`, `objc2-app-kit 0.3`, all gated to
  `cfg(target_os = "macos")`. Add `block2`, `dispatch2` and `objc2-core-foundation` as needed,
  with the same gating.
- The core crate has contract types only (`washboard_core::model`, `diag`, `http::exchange`).
  The core logic is being built in parallel by other work packages and isn't ready yet. Use
  static sample data shaped like the contract types.

## Goal

A runnable, clickable skeleton matching the GUI draft. It should prove the objc2 patterns we'll
use everywhere, so the integration work later only fills in data and actions. It must **not**
implement project logic, persistence, validation, or HTTP.

## Build order

Each step ends in something you can launch and look at. Commit after each step.

1. **App lifecycle.** An `AppDelegate` class via `define_class!` (NSObject subclass,
   `NSApplicationDelegate`, main-thread-only ivars). Set it with `setDelegate` and keep it alive
   for the app's lifetime. Handle `applicationDidFinishLaunching`, and return `false` from
   `applicationShouldTerminateAfterLastWindowClosed` (the welcome window takes over instead).
2. **Main menu in code.** No nib.
   - App menu: About, Settings… (disabled), Hide, Quit.
   - File: New Project… ⇧⌘N, Open Project… ⌘O, Open Recent (`NSDocumentController` recent
     items), Close ⌘W, Save All ⌘S.
   - Edit: the standard first-responder items, so the text view works: Undo/Redo, Cut/Copy/Paste,
     Select All, Find.
   - Project: New Request ⌘N, Duplicate ⌘D, Rename, Delete ⌘⌫, Validate ⌘B, Send ⌘↩,
     Replace WSDL…, Project Settings….
   - Window: HTTP Log ⌥⌘L, plus the standard window list.
   - Actions go to the first responder or the window controller; stub handlers log what they
     would do.
3. **Welcome window.** Shown when no project windows are open. Left side: icon, name,
   New/Open buttons. Right side: an `NSTableView` of recent projects (static sample data).
4. **Project window.** One window controller per project. Content from the outside in:
   - `NSToolbar`, built through a delegate: sidebar toggle, server popup (`NSPopUpButton`),
     Validate, Send, Save All, HTTP Log. Use `NSWindowToolbarStyleUnified`.
   - `NSSplitViewController` with a sidebar item (source list) and a content item.
   - Sidebar: `NSOutlineView` in source-list style with two sections, REQUESTS and OPERATIONS
     (service › port › operation), using sample data. Implement the data source and delegate in
     Rust. Rows show the unsaved dot and the ⚠ marker from the draft. Inline rename on Return;
     the edit result is only logged. Add a +/−/⋯ footer.
   - Content: a vertical `NSSplitView` with the editor, a collapsible issues bar, and the
     response pane.
5. **Editor.** `NSTextView` inside an `NSScrollView`, created with a **TextKit 1** layout
   manager. Use `initUsingTextLayoutManager(false)` (macOS 12+), or build the
   `NSTextStorage`/`NSLayoutManager`/`NSTextContainer` stack yourself.
   - Monospaced system font. Smart quotes, dashes and automatic text replacement off.
   - Line numbers: a custom `NSRulerView` subclass set as the vertical ruler. It draws the line
     numbers for the visible glyph range and red markers for a list of error lines.
   - Highlighting hook. Define a small Rust trait in the app crate:
     `fn tokens(text: &str, range: Range<usize>) -> Vec<(Range<usize>, TokenKind)>`. Apply
     tokens as **temporary attributes** on the layout manager, so undo and the saved text stay
     unaffected. A dumb placeholder tokenizer is fine; the real one comes from WP-XML
     (`washboard_core::xml`).
   - Undo/redo stays with `NSTextView`'s undo manager. Don't build an undo stack in Rust; later,
     changes the app makes to the text (format, templates, completion) go through the text view
     so they are undoable too (PLAN §2.1).
   - Re-highlight on `textStorage:didProcessEditing:` (or `textDidChange:`), limited to the
     edited range extended to line boundaries.
   - Measure with a ~1 MB XML file: typing must stay responsive. Note the numbers in your
     report. This backs the TextKit 1 choice in PLAN §4 "Editor". If TextKit 2 turns out
     clearly better with a working ruler, say so; don't silently switch.
   - Issues bar: a list of diagnostics (`washboard_core::diag::Diagnostic`, sample data).
     Clicking one selects that line in the editor and scrolls to it.
6. **Response pane.** A status label (status, duration, size). Segmented tabs
   Response/Headers/History. A read-only `NSTextView` (same highlighting hook) and an
   `NSTableView` for history.
7. **HTTP log panel.** A single `NSPanel` for the whole app (the Window menu toggles it). Top:
   an `NSTableView` of exchanges built from `washboard_core::http::Exchange` sample values.
   Bottom: request and response side by side. Mask `Authorization` values, with a click to
   reveal.
8. **Sheets.** New Project (form fields, a list of references with ✓/✗, Create disabled while
   anything shows ✗) and Project Settings › Servers (list + form). Use static data, and
   present them as sheets on the window.
9. **Threading pattern.** Add one "fake send" path: Send starts a `std::thread` that sleeps
   500 ms, then posts the result back to the main thread with `dispatch2` (main queue) and
   updates the response pane. This is the pattern every later background job will use; prove
   that `MainThreadMarker` and `Retained` work cleanly with it.
10. **Bundle.** Configure `cargo-packager` (in `crates/washboard-app/Cargo.toml` metadata or a
    `Packager.toml`) to build release and assemble `Washboard.app`; no hand-written bundling
    code:
    - `Info.plist` with `CFBundleIdentifier` `at.deduktiva.washboard`,
      `LSMinimumSystemVersion` 27.0, `NSHighResolutionCapable`, and a version taken from Cargo.
    - Keep the app's Cargo binary named `washboard-app`. The CLI's binary is `washboard`, and
      APFS is case-insensitive, so an app binary named `Washboard` would overwrite the CLI in
      `target/release/`. Inside the bundle the executable can be called `Washboard`
      (`CFBundleExecutable`), because it lives in `Washboard.app/Contents/MacOS/`.
    - A placeholder icon.
    - Unsigned for now. WP-DIST adds signing and notarization.

## objc2 notes

- Everything AppKit is main-thread-only. Pass `MainThreadMarker` down explicitly rather than
  calling `MainThreadMarker::new().unwrap()` deep inside.
- Use `define_class!` for every delegate, data source and subclass. Keep Rust state in
  `#[ivars]` behind `RefCell`/`Cell`, and keep `Retained<…>` handles to views you update later.
- Delegate properties are weak in AppKit. The window controller must own its delegates and data
  sources, or they get deallocated.
- `objc2-app-kit` gates every class behind a cargo feature. With the default features
  everything is on; if compile times hurt, switch to an explicit feature list.
- Check method names against the generated docs on docs.rs for the exact crate versions in
  `Cargo.lock`. The selector-to-Rust naming is mechanical but easy to guess wrong.
- Every `unsafe` block gets a `// SAFETY:` comment (CLAUDE.md).

## Done means

- `cargo clippy -p washboard-app -- -D warnings` is clean on macOS. From Linux,
  `--target aarch64-apple-darwin` must stay clean too, because CI checks it.
- `cargo run -p washboard-app` shows the welcome window. Opening a sample project shows the
  project window looking like the GUI draft in light and dark mode. Menus, shortcuts, sidebar,
  rename, editor typing, the ruler, highlighting, issue click-to-line, fake send, the log panel
  and both sheets all work.
- The bundle script produces an app that launches from Finder.
- Report back:
  - Screenshots (light and dark).
  - The 1 MB editor measurements and your TextKit verdict.
  - Any objc2 patterns that turned out awkward.
  - Anything that diverges from the draft, and why.

## Out of scope

Core integration (loading projects, real requests, validation, sending, autosave, reopening
projects on launch), completion popups, hover docs, Keychain. App behaviour will live in the
toolkit-independent `washboard-ui-model` crate (PLAN §2.1); WP-APP-INTEGRATION binds the shell
to it. Keep the shell's controllers thin with that in mind: views, layout and forwarding input,
not state or rules (e.g. no autosave timers or dirty tracking in AppKit classes).

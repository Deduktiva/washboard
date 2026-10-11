# Editor

## Text view
- `NSTextView` on **TextKit 1** (explicit `NSLayoutManager`). Line-number rulers and large
  documents have historically been TextKit 2's weak spots, and we need both. The app shell
  measures typing on a 1 MB file; TextKit 2 is reconsidered only if it is clearly better there
  with a working ruler.
- Line-number ruler (`NSRulerView` subclass) with error/warning markers in the gutter.
- Highlighting: Rust tokenizer, applied as temporary attributes on the layout manager for the
  edited range expanded to the enclosing tag boundaries. Full pass only on load.

## Checking
- **Well-formedness**: on every edit (debounced 150 ms), libxml2 parse (same parser and options
  as validation) → red underline at the error position + message in the issues bar. This is the
  "not valid XML is visible" requirement. ~15 ms for 1 MB.
- **Schema validation**: Validate button (⌘B), automatically before send, and live (debounced
  1 s); fast enough on large schemas per the measurements in [performance](performance.md).
  What is checked and how errors show: [validation](validation.md).

## Completion and hover
- **Completion**: native `NSTextView` completion (`textView:completions:forPartialWordRange:…`)
  fed by the Rust schema model: element names allowed at the cursor path, attribute names,
  enumeration values. Triggered on `<`, space inside a tag, `="`, and ⌥⎋. What the model offers
  for `xsi:type`, abstract declarations and `xs:any`: [schema model](schema-model.md).
- **Hover** on an element name: type name, cardinality, `xs:documentation` (tooltip).

## Format XML
Format XML (⌃I, Edit menu) re-indents element-only content with the app's indent width (1–8
spaces, default 2; also used for new requests and the response pane), keeping text, comments,
PIs and attribute values byte for byte. One undo step; a request that is not well-formed is
left alone.

## Save / autosave
- Each editor is a buffer with a dirty flag. Autosave 1 s after the last keystroke, and
  immediately on: switching requests, send, window losing key, app deactivate, quit.
- ⌘S = **Save All** across every open project (menu only, no toolbar button). Since autosave
  is aggressive, it mostly acts as "flush now"; the window's edited dot reflects unsaved buffers.
  With "Format on save" on (off by default), Save All first formats the open request, as the
  same undo step as ⌃I. Autosave never formats (it would rewrite the text under the cursor),
  and neither does Send: a request is sent as written.
- Metadata changes (servers, renames, last server) go straight to SQLite.

## Edited outside the app
Request files are watched (`notify`, FSEvents on macOS), recursively under `requests/`. A
change outside the app rescans the sidebar. The open request, when its file changed, is
reloaded if it has no unsaved edits; otherwise the user picks Reload (drops the edits) or Keep
Edits (saves them over the file).

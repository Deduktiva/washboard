# WSDL viewer and Jump to Definition

A read-only view of the project's WSDL and XSD files, highlighted like the editor, and a way to
get there from a request or response: ⌘-click an element, attribute or `xsi:type` value and the
viewer shows the declaration that applies at that spot. The point is answering "what is allowed
here, and why" without leaving the app or guessing which of a dozen imported files holds the
type.

## The viewer
- **One window per project**, titled `<file> — <project>`, opened with Window ▸ WSDL (⌥⌘D is taken
  by the Dock, so no shortcut by default) or by a jump. It closes with its project. A separate
  window rather than a fourth column: the project window already has sidebar, request and
  response side by side, and the viewer is reference material that is most useful next to the
  request, on a second screen if there is one.
- **Content**: the files in the project's `wsdl/` folder as they are stored there, decoded with
  `xml::decode` (so a BOM or UTF-16 file shows correctly) and never modified. Files only: the
  generated schemas of the [schema bundle](schema-bundle.md) (root, per-namespace wrappers,
  rpc wrappers) are not shown, and `wsdl/.previous/` is not listed.
- **Text view**: the editor's text view, read-only and selectable: TextKit 1, line-number ruler,
  the same tokenizer and colours, a full highlighting pass on load (the text never changes).
  Find (⌘F, the find bar) and Copy work; nothing else edits it.
- **Top bar**: Back and Forward (⌘[ and ⌘]) through the jump history, a popup of the files (the
  main WSDL first, then the others by path in Finder order, `xsd/common/types.xsd`), and the
  position of the insertion point (`line 42, column 21`).
- **Jump history**: every jump and every file chosen in the popup pushes the previous file and
  selection. Back and Forward behave like a browser's (a new jump drops the forward entries);
  50 entries at most, the oldest dropped first. Kept in memory only.
- **State**: whether the viewer is open, its frame, the file shown and the scroll position are
  kept per project in `ui_state`; a project reopened on launch reopens its viewer as it was.
- **Replace WSDL** reloads the file list, clears the jump history and shows the main WSDL; a
  file that no longer exists is not shown again from history.

## Jump to Definition
- **Where**: every text view that shows a SOAP message: the request editor, the read-only sent
  request of an older exchange, and the Response tab of the response pane. Not the HTTP log and
  not the Headers tab.
- **How**: ⌘-click on a name, or View ▸ Jump to Definition (⌃⌘J, Xcode's) at the insertion
  point. While ⌘ is held, the name under the mouse that has a definition is underlined and the
  cursor becomes the pointing hand, so the user sees before clicking whether a click would go
  anywhere. A ⌘-click anywhere else is the text view's ordinary ⌘-click; there is no beep and no
  alert for a name without a definition.
- **View ▸ Jump to Type Definition** (no shortcut) goes to the element's or attribute's type
  instead of its declaration. A modifier-click for it is deliberately left out: from the
  declaration, the type is one more ⌘-click away in the viewer.
- **Window**: the viewer opens if needed and comes to the front; the project window stays key,
  so typing continues in the request. The target is selected, scrolled to the middle and
  flashed with the find indicator (`showFindIndicatorForRange:`), so the eye finds it in a long
  file.
- Nothing is jumpable until the schema is ready (`SchemaState::Ready`); before that, ⌘ shows no
  underline.

### What a name jumps to

| Under the mouse | Declaration | Type definition |
|---|---|---|
| Element name (start or end tag) inside a `Header` or `Body` block | the `xs:element` that applies at that path: the local declaration in its parent's type, or the global one (also for `ref=`, substitution group members and elements matched by a lax or strict wildcard) | its named or anonymous type, the `xsi:type` in effect if one is written |
| Attribute name | the `xs:attribute`: local, global (via `ref=`), or inside an attribute group | its simple type |
| `xsi:type` value | the type definition | same |
| rpc/literal wrapper (Body child of an rpc operation) | the binding's `wsdl:operation` | — |
| Part element inside an rpc wrapper | the `wsdl:part` of the message | the part's `type` |
| Child of `soapenv:Fault/detail` | the global element of the fault part | its type |

No definition: the SOAP envelope's own elements and attributes (`Envelope`, `Header`, `Body`,
`Fault`, `mustUnderstand`, …; the envelope schema is shipped, not part of the project),
`xmlns` declarations, `xsi:*` attribute names, built-in XSD types (`xs:string`), elements
matched by a `skip` wildcard, and anything the schema model cannot resolve (an element the
schema does not allow there, a typo). The last case is the one where a jump would be most
welcome, but going somewhere plausible-but-wrong is worse than going nowhere; the validation
error already says what is wrong.

The landing spot is the value of the declaration's `name` attribute (the start tag for an
anonymous type), so the selection shows the name that was clicked.

### Jumps inside the viewer
⌘-click works in the viewer itself on references, so a chain like element → type → base type
needs no searching:
- XSD: `type`, `ref`, `base`, `itemType`, each QName of `memberTypes`, `substitutionGroup`, and
  `schemaLocation` (opens that file at the top).
- WSDL: `message` (of `wsdl:input`, `wsdl:output`, `wsdl:fault`, `soap:header`), `element` and
  `type` of `wsdl:part`, `type` of `wsdl:binding` (the portType), `binding` of `wsdl:port`, and
  `location` of `wsdl:import`.

QNames are resolved with the namespaces in scope at that spot in the original file. A
no-namespace (chameleon) schema has no namespace of its own for its unprefixed references; they
resolve in the namespace of the first schema that includes it, which is right for every real
case we know of (one includer).

## Positions
Positions are in the original file, as the viewer shows it: `diag::TextPos` in the decoded
text, converted to UTF-16 for the front end like every other position (`xml::utf16`).

The schema model is built from the `SchemaBundle`, whose texts are not the files: inline schemas
are cut out of the WSDL (padded so their lines and columns match, [schema bundle](schema-bundle.md)
step 1), but `schemaLocation`s are rewritten to `washboard:/` URIs, missing ones inserted,
namespace declarations added to the extracted `xs:schema` tag and a non-UTF-8 encoding
declaration replaced. Each of these moves the rest of its line. So:
- **`SchemaDoc`** gains the file it came from (`wsdl/`-relative, `None` for generated documents)
  and the list of edits applied to it, enough to map an offset in `text` back to the file
  (additive change to the shared contract). libxml2 errors about inline schemas get the same
  mapping and stop being off by a few columns on edited lines.
- **The schema model** records a source location (file and span) for every element and
  attribute declaration, type definition, group and attribute group, global or local, mapped
  back to the file at build time. It answers `definition(path)`, `attribute_definition(path,
  name)`, `type_definition(qname)` and the type-definition variants with that location.
  Generated declarations have none.
- **`wsdl::Definitions`** records the location of each message, part, portType, portType
  operation, binding, binding operation and service. rpc wrappers and their parts resolve
  through these instead of the generated schema.

The file list, the files' decoded text and both indexes are built with the schema compile, off
the main thread, and shared in the same `Arc` as the `SchemaModel`. A jump is a cursor-context
scan of the clicked text plus a few hash lookups, like hover ([editor](editor.md)).

## Layering
- `washboard-core`: source locations in `schema` and `wsdl`, the bundle's offset map, QName
  reference resolution inside a schema or WSDL file (for jumps in the viewer).
- `washboard-ui-model`: the viewer's state per project window (open, files, file shown,
  selection, jump history), `App::definition(key, source, at, kind)` where `source` names the
  editor, the sent request or the response and `kind` is declaration or type, and the commands
  to open the viewer, show a file, go back and forward. A jump emits a `ViewerChanged` event
  with the file and the UTF-16 range to select.
- `washboard-app`: the window, the top bar, ⌘-tracking (underline, cursor) in the three text
  views, the menu items, frame autosave.

## CLI
`washboard request definition <name> <line>:<col> [--type]` prints where the name at that
position is defined (`wsdl/xsd/common/types.xsd:42:21`) or nothing, exit code 1. It exercises the
same core functions, so the tests and a quick check need no app.

## Not planned
- Showing the generated schemas, or the merged schema as one document.
- An outline of the WSDL (services › ports › operations) or of a schema. Possible later in the
  viewer's top bar; the file popup and Find cover v1.
- Find References (where is this type used). Needs a reverse index; nothing asks for it yet.
- Jumps from completion items or hover popovers.

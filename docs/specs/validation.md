# Validation

When it runs (live, ⌘B, before send): [editor](editor.md), "Checking".

## Steps
0. `${name}` placeholders are filled in from the parameter set, and every later step runs on
   that text; diagnostics are mapped back to the placeholders. A placeholder that cannot be
   filled in is an error, and the schema steps are skipped until none is left
   ([parameter sets](parameters.md), "Validation").
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
   Checked by `washboard-core/tests/pipeline.rs` against every fixture request, including
   `xsi:type`, abstract elements, substitution groups, `xs:any` and rpc/literal wrappers.
5. Header blocks the binding declares (`soap:header`) but that are missing → warning; header
   blocks the binding doesn't declare are validated if the schema knows them (lax), otherwise
   left alone, as SOAP intends.
6. `soapenv:mustUnderstand`, `actor` and `encodingStyle` are allowed on every header block,
   as SOAP 1.1 §4.2 says. A block's declared type rarely allows foreign attributes, so these
   three are removed from libxml2's parsed tree before the pass (no error filtering, positions
   unchanged) and `mustUnderstand` is checked for `0`/`1` in Rust instead. Other attributes in
   the SOAP namespace stay errors.

Where libxml2 must not complain about something the protocol allows, remove it from the parsed
tree before validating (`CompiledSchema::validate` with `strip`, used for the SOAP
header-block attributes) rather than filtering errors afterwards.

## Errors
Errors: counted in the editor's validity widget ([editor](editor.md)), listed in the
collapsible issues list under the editor (click → jump to line), plus gutter markers.
**Send is blocked** while any error exists; the send attempt itself shows the issues list.

- libxml2's messages for `xsi:type`, abstract elements and abstract types are terse (e.g. "The
  type definition is abstract"); we append the list of allowed concrete types/members from the
  schema model's index ([schema model](schema-model.md)).
- This works on structured data, not on libxml2's message text. `validate::xsd` puts the
  libxml2 error code, the element's document-order index and, for attribute errors, the
  attribute's QName into `Diagnostic::detail` (`diag::DiagDetail`).
  `validate::request` maps codes 1846 (abstract element) and 1876 (abstract type) to the
  element, resolves its path in the request with `schema::SchemaModel` and appends the
  substitution group members or the `xsi:type` candidates. This needs a model attached
  with `RequestSchema::with_model`; the app shares the one it builds for completion.
- Exception, attributes: libxml2 2.15.4 never hands us the `xmlAttr`. `xmlVUpdateError`
  (error.c) replaces every node with its element before the error is raised. The attribute's
  name survives only in the message prefix libxml2 builds from the node's QName
  (`Element '…', attribute '{ns}a': `), so the wrapper reads that prefix and accepts the
  name only if the element has such an attribute (`xmlHasNsProp`). A changed format loses
  the attribute; it never names a wrong one. Revisit if libxml2 starts keeping the node.
- **Spans**: `Diagnostic::span` (start and end `TextPos`, end exclusive) sits next to `pos`
  and is chosen by error code: the attribute (name to closing quote) for attribute errors,
  the element's text (whitespace trimmed) for simple-type and facet errors, the start tag
  for everything else. `pos` stays at the element's `<`. Diagnostics without a span get a
  single caret at `pos`.
- The CLI (`request validate`, `request send`) renders diagnostics with `annotate-snippets`,
  the renderer rustc uses, so spans are underlined across characters and lines without
  layout code of our own.

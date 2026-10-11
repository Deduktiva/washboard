# WSDL

## WSDL-level details
- **`wsdl:import`**: WSDL files are loaded transitively and merged into one definitions model
  keyed by QName (messages, portTypes, bindings, services may live in different files). Some
  real-world WSDLs `wsdl:import` an XSD; that is accepted and treated like `xs:import`. Each
  file's own `wsdl:types` schemas are extracted as in [schema bundle](schema-bundle.md) step 1,
  with that file's namespaces.
- **`soap:header message="…" part="…" use="literal"`**: the header part may come from a
  different message than the body. Headers are emitted in templates and validated
  ([validation](validation.md), steps 4–6). Parts used as headers are excluded from the body;
  `soap:body parts="…"` is honoured.
- **Style is per operation**: `soap:operation style` overrides `soap:binding style`; the check
  is done per operation, so a mostly-document binding with one rpc operation still loads.
- **Unsupported bindings and operations** (SOAP 1.2, `rpc/encoded`) load and are listed greyed
  out with their reason, never hidden.
- **`soapAction`**: sent quoted when set; when absent or empty, `SOAPAction: ""` is sent (SOAP 1.1
  requires the header to be present).
- **BOM / encodings**: all inputs are decoded through one function that honours a UTF-8/UTF-16
  BOM and the XML declaration's `encoding`, then hands `&str` to `roxmltree`/`quick-xml`
  (neither accepts a BOM-prefixed or UTF-16 input as-is). libxml2 gets the decoded text as
  UTF-8 too, so its line and column numbers match ours.
  Copied WSDL/XSD files stay byte-identical. Line/column mapping is char-based, so the BOM does
  not shift positions. Request files are written as UTF-8 without BOM; a BOM in a file the
  user edited externally is preserved.

## `rpc/literal`
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
  from [schema bundle](schema-bundle.md) step 5. Then the generated declarations go into a
  wrapper schema that `xs:include`s the real one. Plan to need this, because services commonly
  reuse their tns for rpc wrappers.
- `rpc/encoded` operations still load greyed out as unsupported.

# Schema bundle

Two representations of the project's schemas, deliberately:
- **libxml2** compiles the schemas for **validation** (authoritative). See `crates/libxml2-sys/README.md`.
- **Our Rust XSD model** (`schema/`) drives **completion and templates**
  ([schema model](schema-model.md)). It can be incomplete in exotic corners without causing
  wrong validation results — the worst case is a missing completion.

Both are built from one `SchemaBundle`: every schema document of the project, and nothing else.
libxml2 cannot compile a WSDL directly, so `wsdl` builds the bundle in these steps:
1. Extract each `wsdl:types/xs:schema` into a standalone document. **Copy in-scope namespace
   declarations from `wsdl:definitions`** onto the extracted `xs:schema` — the classic bug where
   `type="tns:Foo"` breaks after extraction.
2. Inline schemas often `xs:import` each other by namespace without `schemaLocation`. Generate a
   synthetic root schema that imports every schema by a synthetic URI
   (`washboard:/inline/<n>.xsd`).
3. Resolve only documents in the `SchemaBundle` (project XSDs, extracted and generated schemas,
   the shipped SOAP envelope XSD); everything else fails. libxml2 2.15.4 has a per-context
   resource loader but doesn't pass it to nested imports, so a process-global loader backed by a
   thread-local bundle covers those (`validate::xsd`). Instance documents are parsed with
   `XML_PARSE_NONET | XML_PARSE_NO_XXE` and a loader that refuses everything.
4. Use `xmlSchemaSetValidStructuredErrors` to collect `(line, col, message)`.
5. **One namespace split across several files.** libxml2 imports each target namespace only once:
   a second `xs:import` of the same namespace with a different `schemaLocation` is silently
   skipped, and the types in that file go missing. Your WSDLs reportedly don't do this, so the
   import check detects it and warns, and the bundle builder generates a wrapper schema per
   split namespace that `xs:include`s all its files, with every import of that namespace pointed
   at it (also used for `rpc/literal`, see [WSDL](wsdl.md)).

Known libxml2 limits regardless of version: XSD 1.0 only; some edge cases (complex `xs:redefine`,
certain identity constraints) remain weak.

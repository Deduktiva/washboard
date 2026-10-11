# Large and deeply nested schemas

Inputs are > 500 KB WSDLs with multi-level XSD imports. Consequences:
- **Compile once, off the main thread.** Compiled `xmlSchemaPtr` is cached per project, built on
  open in the background; the window is usable immediately, validation and completion show
  "Loading schema…" until ready. Target: open-to-ready under 2 s for a 2 MB schema set.
- **Rust schema model is indexed**, not walked: global elements/types by QName in hash maps, and
  a lazily built per-type "allowed children" table for completion. Target: completion under 50 ms.
- **Template generation is bounded**: recursion depth limit (default 6) and a node budget; when
  cut, a `<!-- … truncated: type Foo -->` comment is emitted instead of silently stopping.
- **Validation of a request** reuses the compiled schema; cost is proportional to the request,
  not the schema. Live validation is enabled only if it measures under ~100 ms. Measured
  (`crates/washboard-core/tests/validate_perf.rs`, release build, median of 5, 4-core 2.1 GHz
  Xeon on Linux, vendored libxml2 2.15.4) on the synthetic 2.1 MB set: compile 340–370 ms;
  `validate_request` 0.1 ms for 1 KB, 0.7 ms for 14 KB, 3.6 ms for 81 KB, 12 ms for 257 KB,
  80 ms for 1.6 MB, and 9 ms for 81 KB with 500 errors. Live validation is therefore on; both
  targets are asserted in release builds. Not yet measured on a Mac.
- **Typing** stays responsive in a 1 MB request (TextKit 1, incremental highlighting;
  [editor](editor.md)).

## Fixture corpus
Since real WSDLs can't be shared, CI uses public WSDLs of similar shape plus a generator that
synthesises large schema sets (N files, depth D, namespaces split across files, recursive types,
deep derivation chains, substitution groups across namespaces, `xs:any`, multi-file
`wsdl:import`, BOM-prefixed files). `washboard inspect` prints the same structural report
locally, without contents, so real WSDLs can be checked against the assumptions here.

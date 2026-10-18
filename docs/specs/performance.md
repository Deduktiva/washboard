# Large and deeply nested schemas

Inputs are > 500 KB WSDLs with multi-level XSD imports. Consequences:
- **Compile once, off the main thread.** Compiled `xmlSchemaPtr` is cached per project, built on
  open in the background; the window is usable immediately, validation and completion show
  "Loading schema…" until ready. Target: open-to-ready under 2 s for a 2 MB schema set.
- **Rust schema model is indexed**, not walked: global elements/types by QName in hash maps, and
  a lazily built per-type "allowed children" table for completion. Target: completion under 50 ms.
- **Template generation is bounded**: recursion depth limit (default 6) and a node budget; when
  cut, a `<!-- … truncated: type Foo -->` comment is emitted instead of silently stopping.
 **Validation of a request** reuses the compiled schema; cost is proportional to the request,
  not the schema. Measurements from `crates/washboard-core/tests/validate_perf.rs`, release build:

  | Host                     | compile    | 1KB    | 14KB  | 81KB  | 257KB | 1.7MB | 81KB+500 errors |
  |--------------------------|------------|--------|-------|-------|-------|-------|-----------------|
  | Linux 4-core Xeon 2.1GHz | 340–370 ms | 0.1ms  | 0.7ms | 3.6ms | 12ms  | 80ms  | 9ms             |
  | macOS 27 m3              | 170 ms     | 0.03ms | 0.4ms | 1.7ms | 5ms   | 32ms  | 3ms             |

  Targets are asserted in release builds.
- **Typing** stays responsive in a 1 MB request (TextKit 1, incremental highlighting;
  [editor](editor.md)).

## Fixture corpus
CI uses public WSDLs of expected shapes plus generated synthetic large schema sets.

# Schema model

The Rust XSD model (`schema/`) drives completion, hover and templates; libxml2 alone decides
validity ([schema bundle](schema-bundle.md)). It needs elements, complex/simple types,
sequences/choices/all, min/maxOccurs, extensions/restrictions, enumerations, groups, attribute
groups, the type derivation graph, substitution groups and wildcards (these are required, not
best effort). It is built once per schema compile and indexed, not walked
([performance](performance.md)).

## `xsi:type`, abstract declarations, `xs:any`
Validation of all three is libxml2's job and it handles them (derivation checks, `block`/`final`,
abstract-without-`xsi:type` errors, `processContents`). The work is in the Rust model, which
needs a project-wide index built once per schema compile:
- **Derivation graph**: for every complex/simple type, its base and all transitively derived
  types (extension and restriction), across all files and namespaces.
- **Substitution groups**: for every element, the transitive set of members, with abstract
  heads excluded from suggestions.

Behaviour:
- **Element position whose declared type is abstract or has derived types**: completion offers
  the element as usual; inside its start tag, `xsi:type="` completes with the concrete derived
  types (abstract ones excluded, `block` respected). Once `xsi:type` is set, child/attribute
  completion uses the **derived** type's content model, not the declared one.
- **QName values**: `xsi:type` values are QNames. Completion inserts the prefix in scope for the
  type's namespace; if there is none, it adds an `xmlns:nsN` declaration on the element. The
  model resolves prefixes using the editor's in-scope namespace map at the cursor.
- **Abstract element head** (`ref` to an abstract element): completion offers the substitution
  group members instead of the head.
- **`xs:any`**: completion offers global elements from the namespaces the wildcard allows
  (`##other`, `##targetNamespace`, explicit lists); for `processContents="skip"` no suggestions,
  only a hint. `xs:anyAttribute` likewise for attributes.
- **Hover** shows the effective type ("declared `tns:Party`, actual `tns:Company` via xsi:type").
- **Validation errors** for these cases name the allowed concrete types or members from the
  index ([validation](validation.md), "Errors").

## Templates
For an operation's input: emit a SOAP 1.1 envelope, declared headers, and
the body element expanded recursively. Required elements emitted, optional ones emitted with
`<!-- optional -->`, `xs:choice` emits the first branch plus a comment listing alternatives,
recursion depth-limited, simple-type placeholders by type (`?` for strings, `0` for numerics,
first enum value, `2026-01-01` for dates). Abstract element heads are replaced by the first
concrete substitution member and abstract types get `xsi:type` with the first concrete derived
type, each followed by a comment listing the alternatives. `xs:any` emits
`<!-- any element from: … -->`.

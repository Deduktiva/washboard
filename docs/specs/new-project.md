# New Project and Replace WSDL

## Create project (File ▸ New Project…, ⇧⌘N)
1. New Project window (its own window, not a sheet): project name, parent location
   (`NSOpenPanel` with "Create Folder"), WSDL file, additional XSD files or a folder.
2. **Import check** runs immediately (validation thread): parse WSDL, walk every
   `wsdl:import`, `xs:import`, `xs:include`, `xs:redefine`. Each reference is shown as
   resolved (to which supplied file) or unresolved. The walk is transitive (XSD → XSD → …),
   cycle-safe, and resolves relative `schemaLocation`s against the *importing* file, so the
   supplied folder structure is preserved on copy. Remote `http(s)://` locations are never fetched;
   they are matched against supplied files by the longest matching path suffix, and an ambiguous
   match (two `common.xsd` in different folders) is reported, not guessed.
   A relative location that is not among the supplied files but exists on disk relative to the
   importing file is read and added, transitively, and listed as found rather than supplied:
   real WSDLs ship with their XSDs beside them. Absolute paths are not followed this way.
   Each reference row names the file it comes from, since the same file is often imported from
   several places.
   The check also reports, as warnings, namespaces split across files ([wsdl](wsdl.md),
   "Schema bundle"), XSD 1.1 constructs and unsupported bindings.
3. *Create* is enabled when nothing is unresolved and the schema set compiles.
4. Copy files, create DB. The WSDL's SOAP 1.1 `soap:address`es are not servers: Settings ▸
   Servers always lists them as read-only "from WSDL" rows, and Add as Server… turns one into a
   server once the user has saved it (no implicit connection).

## Replace WSDL (Settings ▸ the project's General ▸ Replace WSDL…)
Same import sheet, on the Settings window. After replacing: recompile, re-validate every
request, show a report (operations added/removed, requests now invalid). Requests are never
rewritten automatically.

# Washboard — implementation plan

A native macOS SOAP client written in Rust. AppKit UI via `objc2`, no web views,
no network traffic except to servers the user configured.


| Topic | Decision |
|---|---|
| UI binding | `objc2` + `objc2-app-kit` / `objc2-foundation`, UI built in code (no nibs) |
| Schema validation | libxml2 XSD validation via FFI, vendored and statically linked, see [`libxml2-sys`](../crates/libxml2-sys/README.md)) |
| WSDL/XSD imports | User supplies the referenced files; local files named by a relative path next to the importing file are picked up automatically. All are copied into the project. Unresolved imports are errors. Never fetched. |
| Basic-auth passwords | macOS Keychain; username and everything else in the project database |
| Minimum OS | macOS 26 (the GitHub macOS runners run 26, so CI can launch the app) |
| Distribution | Developer ID, notarized; sandbox-ready but not sandboxed in v1 |
| SOAP | 1.1 only; `document/literal` and `rpc/literal`. No SOAP 1.2, no `rpc/encoded`. |
| Proxies | None. Connections go directly to the configured server. |
| Expected input size | WSDLs > 500 KB, < 200 operations, < 10 import levels (see [performance](specs/performance.md)) |
| Required schema features | `xsi:type` in requests, abstract elements/types + substitution groups, `xs:any` (see [schema model](specs/schema-model.md)) |
| Required WSDL features | `wsdl:import`, `soap:header` (`use="literal"`), `soapAction`, files with a BOM (see [WSDL](specs/wsdl.md)) |

GUI draft: [`gui-draft.html`](gui-draft.html) (open in a browser), ASCII versions in
[`gui-drafts.md`](gui-drafts.md). How each part behaves is specified in [`specs/`](specs/), below.

## Specs

One file per topic in [`specs/`](specs/). They describe how Washboard behaves and why; the code
is the reference for the details they leave out.

| Spec | Covers |
|---|---|
| [architecture](specs/architecture.md) | crates, key dependencies, threading, the UI model, packaging |
| [project disk layout](specs/projects-disk-layout.md) | project folder with database and request files and folders, locking |
| [macOS app state](specs/macos-app-state.md) | open projects and restore, app settings |
| [new project](specs/new-project.md) | New Project, the import check, Replace WSDL |
| [requests](specs/requests.md) | request and folder commands, placement, sending, the CLI |
| [sidebar](specs/sidebar.md) | the project window's source list |
| [editor](specs/editor.md) | text view, checking, completion, Format XML, autosave, outside edits |
| [validation](specs/validation.md) | validation steps, errors and spans |
| [response pane](specs/response-pane.md) | response, history drawer, older exchanges |
| [settings](specs/settings.md) | the Settings window, servers, parameters |
| [parameter sets](specs/parameters.md) | `${name}` placeholders, sets, project-wide selection and pins |
| [WSDL](specs/wsdl.md) | WSDL details, `rpc/literal` |
| [schema bundle](specs/schema-bundle.md) | turning the WSDL's schemas into what libxml2 compiles |
| [schema model](specs/schema-model.md) | the Rust XSD model: `xsi:type`, abstract, `xs:any`, templates |
| [performance](specs/performance.md) | large schemas, targets and measurements, fixture corpus |
| [network](specs/network.md) | no network except configured servers |

### CI
See `.github/workflows/ci.yml`.

## Outlook

- **Order-aware completion.** v1 offers every child element the content model allows at the
  cursor, regardless of siblings already present. v2 passes the preceding siblings from the
  cursor context (`xml::cursor_context`) to the schema model, which walks the content model
  (sequence position, `maxOccurs` already reached, chosen `xs:choice` branch) and offers only
  what may legally come next, ranking required elements first.
- HTTP proxies

### Out of scope for v1
- WSDL 2.0, SOAP 1.2, `rpc/encoded`.
- WS-Security, WS-Addressing helpers, MTOM/attachments, NTLM/Kerberos, client certificates.
- Custom per-server HTTP headers.

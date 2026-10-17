# Requests

A request is one `.xml` file holding the full envelope, in `requests/` or a folder below it
([disk layout](disk-layout.md)). Where requests and folders are listed: [sidebar](sidebar.md);
what a send returns: [response pane](response-pane.md).

## Commands
- **New** (⌘N): pick an operation (searchable list grouped by service › port; unsupported
  operations greyed with their reason). Body is generated from the schema
  ([schema model](schema-model.md), "Templates"). Name: `<Operation> <n>` with the lowest free
  `n` (`GetCustomer 1`, `GetCustomer 2`, …). It goes into a folder (see "Placement").
  The new row enters inline-rename immediately.
- **Rename** (Return / double-click), **Duplicate** (⌘D → `GetCustomer 1 copy`, beside the
  original), **Delete** (⌘⌫, confirms; history deleted with it).
- The server and the parameter set are chosen for the whole project; a request can pin either
  ([parameter sets](parameters.md), "Selection"). New requests have no pins; Duplicate copies
  them.

## Send
- Toolbar: server popup, parameter set popup (while the project has sets), Send (⌘↩). Sends to
  `server.url` with `Content-Type: text/xml; charset=utf-8` and the `SOAPAction` header from the
  binding operation ([WSDL](wsdl.md)).
- Send fills in the `${name}` placeholders once, validates that text and sends exactly it
  ([parameter sets](parameters.md)). **Send is blocked** while it has errors
  ([validation](validation.md)), unresolvable placeholders included. Apart from the
  placeholders it is sent as written: Send never formats it.
- Basic auth: `Authorization` header built up front (preemptive, no 401 round-trip).
- TLS: platform verification; if the server has "ignore TLS errors", certificate and hostname
  checks are disabled for that request only. The log records whether verification was skipped.
- No redirects and no proxies ([network](network.md)).
- The exchange goes into the request's history ([response pane](response-pane.md)) and the
  app's HTTP log (below).

## HTTP log (Window ▸ HTTP Log, ⌥⌘L)
One panel for the app, across projects. Shows the last exchange in full: request line, headers,
body; status line, headers, body; timing; TLS info. Keeps the last 50 exchanges in memory in a
list above (small extension of "last request and response"; trivially dropped if unwanted). Not
persisted. Bodies over 5 MB are truncated in the view. Authorization values are masked by
default (click to reveal).

## Placement
The model's `new_request` takes the target folder. The app passes the folder selected in the
sidebar when a folder row is selected; otherwise the request goes into its operation's folder,
created if missing. That folder is named after the operation (`GetCustomer`), or
`Port/GetCustomer` when the project has more than one supported port, so a typical WSDL with one
SOAP 1.1 port gets one level. Only supported ports count, so a SOAP 1.1 port next to a SOAP 1.2
or rpc/encoded one still gives the flat layout. An existing folder is matched
case-insensitively; if the user renamed the operation's folder, a new one is created. Duplicate
puts the copy beside the original.

## Folder commands
Create folder, rename folder, delete folder (with every request in it and their history; the
model reports the count for the confirmation), move a request to a folder, move a folder
(refused into itself or its descendants, and when the move would put two rows of the same name
in one folder). All are renames on disk, atomic like other writes.

## CLI
`request list` prints paths (`GetCustomer/GetCustomer 1`). Every `<name>` argument also takes a
path; a bare name that matches two requests fails and lists their paths.
`request mv <name> <folder>` moves a request (creating the folder; `/` for the top level).
`request new` places like New Request.
`request send` and `request validate` take `--set <name>`, and `request send` `--server
<name>`, for that run only; without them the request's pin, else the project's choice, is used.
`request show --resolved` prints the text as it would be sent. `set …` and `param …` edit the
sets and parameters ([parameter sets](parameters.md), "CLI").

# Requests

A request is one `.xml` file holding the full envelope, in `requests/` or a folder below it
([disk layout](disk-layout.md)). Where requests and folders are listed: [sidebar](sidebar.md).

## Commands
- **New** (⌘N): pick an operation (searchable list grouped by service › port; unsupported
  operations greyed with their reason). Body is generated from the schema
  ([schema model](schema-model.md), "Templates"). Name: `<Operation> <n>` with the lowest free
  `n` (`GetCustomer 1`, `GetCustomer 2`, …). It goes into a folder (see "Placement").
  The new row enters inline-rename immediately.
- **Rename** (Return / double-click), **Duplicate** (⌘D → `GetCustomer 1 copy`, beside the
  original), **Delete** (⌘⌫, confirms; history deleted with it).
- Each request remembers `last_server_id`; the toolbar server popup shows it. A new request
  uses the most recently used server of the project.

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

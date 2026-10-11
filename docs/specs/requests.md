# Requests

A request is one `.xml` file holding the full envelope ([disk layout](disk-layout.md)).
Where requests are listed: [sidebar](sidebar.md); grouping them:
[request folders](request-folders.md).

## Commands
- **New** (⌘N): pick an operation (searchable list grouped by service › port; unsupported
  operations greyed with their reason). Body is generated from the schema
  ([schema model](schema-model.md), "Templates"). Name: `<Operation> <n>` with the lowest free
  `n` (`GetCustomer 1`, `GetCustomer 2`, …). It goes into the selected folder, else into its
  operation's folder, created on first use ([request folders](request-folders.md)). The new row enters inline-rename
  immediately.
- **Rename** (Return / double-click), **Duplicate** (⌘D → `GetCustomer 1 copy`, beside the
  original), **Delete** (⌘⌫, confirms; history deleted with it).
- Each request remembers `last_server_id`; the toolbar server popup shows it. A new request
  uses the most recently used server of the project.

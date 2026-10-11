# Request folders

Grouping requests is the user's job, with folders. Folders are created when they are first
needed, not for every operation up front: a WSDL with 200 operations would get 200 empty
folders, git does not keep empty directories, and Replace WSDL would leave folders for removed
operations behind.

- **On disk.** A folder is a subdirectory of `requests/`, nested to any depth, so the project
  stays browsable in Finder and git ([disk layout](disk-layout.md)). Folder names follow the
  request name rules (`names::validate_request_name`). Folders and files starting with `.` are
  ignored. The disk is the truth for folders: they are not in the database, and an empty folder
  stays until it is deleted.
- **Database.** `request.file_name` holds a path relative to `requests/` and may contain `/`
  (always `/`, whatever the platform). `RequestMeta::folder` is `""` for the top level.
- **Names stay unique across the project** (case-insensitive), not per folder, so
  `<Operation> <n>` numbering and the CLI's `<name>` arguments keep working. Rename, Duplicate
  and New Request check against every folder. Two files with the same name in different folders
  can still arrive from Finder or git; reconciliation accepts both, the app shows both, and the
  CLI asks for the path.
- **Reconciliation** walks `requests/` recursively. A row whose file is gone is matched to a new
  file with the same name elsewhere, when exactly one such file appeared, and treated as a move:
  same id, same history. That covers a move in Finder; anything else is a removal and an
  addition.
- **New Request placement.** The model's `new_request` takes the target folder. The app passes
  the folder selected in the sidebar when a folder row is selected; otherwise the request goes
  into its operation's folder, created if missing. That folder is named after the operation
  (`GetCustomer`), or `Port/GetCustomer` when the project has more than one supported port, so
  a typical WSDL with one SOAP 1.1 port gets one level. Only supported ports count, so a SOAP
  1.1 port next to a SOAP 1.2 or rpc/encoded one still gives the flat layout. An existing folder
  is matched case-insensitively; if the user renamed the operation's folder, a new one is
  created. Duplicate puts the copy beside the original.
- **Commands** (core, model): create folder, rename folder, delete folder (with every request in
  it and their history; the model reports the count for the confirmation), move a request to a
  folder, move a folder (refused into itself or its descendants, and when the move would put two
  rows of the same name in one folder). All are renames on disk, atomic like other writes.
- **CLI.** `request list` prints paths (`GetCustomer/GetCustomer 1`). Every `<name>` argument
  also takes a path; a bare name that matches two requests fails and lists their paths.
  `request mv <name> <folder>` moves a request (creating the folder; `/` for the top level).
  `request new` places like New Request.

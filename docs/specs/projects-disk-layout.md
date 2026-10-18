# Projects: Disk layout

A project is a plain folder (not a package, so it's browsable in Finder and git-friendly).

```
Customer API/
├─ washboard.sqlite             metadata (see below)
├─ .washboard.lock              flock target, see "Same folder opened twice"
├─ wsdl/
│  ├─ CustomerService.wsdl      the WSDL as imported (name preserved)
│  └─ xsd/                      supporting XSDs, relative structure preserved
│     └─ common/types.xsd
├─ requests/
│  ├─ GetCustomer/              a folder: a plain subdirectory, any depth
│  │  └─ GetCustomer 1.xml      full SOAP envelope, exactly what is sent
│  └─ CreateOrder 1.xml
└─ history/
   └─ <request-uuid>/
      ├─ 20261006T140312Z.request.xml
      └─ 20261006T140312Z.response.xml
```

- **Request name = file stem.** Rename = file rename. Names may not contain `/`, `:` or start
  with `.` (`names::validate_request_name`).
- **Names are unique across the project** (case-insensitive), not per folder, so
  `<Operation> <n>` numbering and the CLI's `<name>` arguments keep working. Rename, Duplicate
  and New Request check against every folder. Two files with the same name in different folders
  can still arrive from Finder or git; reconciliation accepts both, the app shows both, and the
  CLI asks for the path.
- **Folders** are subdirectories of `requests/`, nested to any depth, named by the same rules.
  The disk is the truth for folders: they are not in the database, and an empty folder stays
  until it is deleted. They are created when first needed, not for every operation up front: a
  WSDL with 200 operations would get 200 empty folders, git does not keep empty directories, and
  Replace WSDL would leave folders for removed operations behind. Folders and files starting
  with `.` are ignored.
- **Request files contain the full envelope.** What you see is what is sent; SOAP headers are editable.
- **History** is keyed by request UUID (not name) so renames don't orphan it. The sent request is
  stored too — without it a response is hard to interpret later. Retention: last 20 per request
  (project setting).
- On WSDL replace the previous set moves to `wsdl/.previous/<timestamp>/` (one level kept).
- All writes are atomic (temp file in same dir + `rename`).

## `washboard.sqlite`
Tables for the project, its servers, request metadata, history and UI state; the columns are
the migrations in `crates/washboard-core/src/project/db.rs`, keyed off `PRAGMA user_version`.
What the schema decides:
- The project id is a UUID and the Keychain key prefix; passwords live only in the Keychain.
  It is in the database, not derived from the path, so passwords survive moving or renaming
  the folder. Nothing else uses it: recent projects and "already open" go by path.
- **Known issue, deliberately not addressed: copying a project folder** (Finder, `cp`, a
  second checkout on the same Mac) copies the project id and the server ids, so both projects
  share the same Keychain items. Changing a password in one changes it in the other, and
  deleting a server or switching it to no auth deletes the password the other still uses.
  Servers added after the copy get their own ids. The fix considered and not done: remember id →
  folder in the app state and give a copy a new id on open, copying its passwords over.
- A request row names its file by a path relative to `requests/`, always `/`-separated whatever
  the platform; its operation is a hint that may go stale when the WSDL changes.
- History rows point at their `history/` files and keep the request's start line and headers
  and the response's headers as JSON, with `Authorization` masked before it is stored.
- Rollback journal, not WAL: write volume is tiny and WAL side files behave badly on
  synced/network folders.
- **Reconciliation on open** walks `requests/` recursively: request files without a row get a
  row; rows without a file are dropped (history kept until pruned). Handles files added/removed
  in Finder or via git. A row whose file is gone is matched to a new file with the same name
  elsewhere, when exactly one such file appeared, and treated as a move: same id, same history.
  That covers a move in Finder; anything else is a removal and an addition.
- **Same folder opened twice** (two windows or two app instances): `flock` on
  `.washboard.lock` beside it (not on the database: on macOS `flock` and SQLite's own `fcntl`
  locks interfere); the second open is refused with a clear message. Read-only CLI commands
  open without the lock.

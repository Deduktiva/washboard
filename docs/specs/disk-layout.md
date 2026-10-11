# Disk layout

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

- **Request name = file stem.** Rename = file rename. Names may not contain `/`, `:` or start with `.`.
  Names are unique across the project (case-insensitive), not per folder. Folders follow the
  same rules; they exist only on disk, not in the database, and an empty one stays until deleted.
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
- A request row names its file by a `/`-separated path relative to `requests/`; its operation
  is a hint that may go stale when the WSDL changes.
- History rows point at their `history/` files and keep the request's start line and headers
  and the response's headers as JSON, with `Authorization` masked before it is stored.
- Rollback journal, not WAL: write volume is tiny and WAL side files behave badly on
  synced/network folders.
- **Reconciliation on open**: request files without a row get a row; rows without a file are
  dropped (history kept until pruned). Handles files added/removed in Finder or via git.
- **Same folder opened twice** (two windows or two app instances): `flock` on
  `.washboard.lock` beside it (not on the database: on macOS `flock` and SQLite's own `fcntl`
  locks interfere); the second open is refused with a clear message. Read-only CLI commands
  open without the lock.

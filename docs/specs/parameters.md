# Parameter sets

**Status: planned, not implemented.** When it lands, the specs listed under "Changes elsewhere"
are updated with it.

A parameter set is a named group of values, such as one test customer: `customer_id` = `4711`,
`customer_name` = `Jane Doe`. Request files refer to them as `${customer_id}`; the values of the
active set are filled in when the request is validated and sent. The file on disk keeps the
placeholder.

## Data model
- A project has one list of **variables** (the columns) and any number of **sets** (the rows),
  each with a value per variable. Settings edits them as one grid. A shared list makes a typo in
  a request an "undefined parameter" error rather than a silent miss, and renaming a variable
  renames it in every set.
- A cell can be **unset**; using an unset value is an error. An empty string is a value.
- **Names**: `[A-Za-z_][A-Za-z0-9_]*`, unique case-sensitively. Names containing `.` are
  reserved for built-in generated values (see "Reserved").
- Values are plain text in the database. There are no secret values; passwords stay with
  servers in the Keychain.
- Sets are independent of servers: any set can be used with any server.

## Storage
In `washboard.sqlite`, beside the servers ([disk layout](disk-layout.md)):

```sql
CREATE TABLE parameter_set (id TEXT PRIMARY KEY, name TEXT NOT NULL,
                            sort_order INTEGER NOT NULL);
CREATE TABLE parameter     (name TEXT PRIMARY KEY, sort_order INTEGER NOT NULL);
CREATE TABLE parameter_value (
  set_id TEXT NOT NULL REFERENCES parameter_set(id) ON DELETE CASCADE,
  name   TEXT NOT NULL REFERENCES parameter(name) ON DELETE CASCADE ON UPDATE CASCADE,
  value  TEXT NOT NULL,
  PRIMARY KEY (set_id, name));
ALTER TABLE request ADD COLUMN pinned_server_id TEXT REFERENCES server(id) ON DELETE SET NULL;
ALTER TABLE request ADD COLUMN pinned_parameter_set_id TEXT
  REFERENCES parameter_set(id) ON DELETE SET NULL;
ALTER TABLE history ADD COLUMN parameter_set_id TEXT;
```

A missing `parameter_value` row is an unset cell. The active server and active set are
`ui_state` keys.

## Placeholders
- `${name}` with a valid name is a placeholder.
- Anything else between `${` and the next `}` (`${1x}`, `${now:yyyy}`, `${ }`) is an error,
  "invalid placeholder", never literal text: the syntax stays free for later extensions without
  changing what existing requests send. A bare `$name`, or `${` with no closing `}`, is literal
  text.
- `$${` is a literal `${`.
- Values are inserted XML-escaped (`&`, `<`, `>`, `"`, `'`), which is correct in both text and
  attribute values. A value is never parsed as XML, and placeholders inside a value are not
  expanded.
- The whole envelope is interpolated, SOAP headers included. The server URL and HTTP headers
  are not: a set must not be able to send traffic anywhere but a configured server
  ([network](network.md)).

## Resolving
`core` resolves a request's text through a lookup ("the value of `name`?") rather than a fixed
map, so built-in values can be added later. The result is the resolved text, the problems
found, and a map from each substitution's span in the resolved text to the placeholder's span
in the source.

The send path resolves **once**, validates that text and sends exactly that text. Nothing
resolves separately for validation and send.

## Validation
Validation runs on the resolved text ([validation](validation.md)); libxml2 never sees a
`${...}`, since `${customer_id}` inside an `xs:int` element would be a real error and errors
are never filtered afterwards.

- Every placeholder must resolve first. These are errors at the placeholder's span and block
  send:

  | Case | Message |
  |---|---|
  | Not a valid name | invalid placeholder: `${…}` |
  | No such variable (dotted names included) | undefined parameter: customer_id |
  | No set active | parameter customer_id has no value: no parameter set selected |
  | Unset in the active set | parameter customer_id has no value in “Test customer A” |

  While any of these exist, the schema pass is skipped; well-formedness still runs.
- Diagnostics are mapped back to the source with the substitution map. One inside a substituted
  value covers the whole `${customer_id}` span and its message ends with
  `(customer_id = "abc" in “Test customer A”)`; positions after a substitution shift back.
- Results depend on the active set, so changing it re-validates the open request.
- Completion, hover, Format XML and templates work on the source text and are unchanged.
  Format XML keeps `${...}` as it is, being ordinary text.

## Selection
The server and the parameter set are chosen for the **whole project**; a request can **pin**
either or both. For a send, each is taken from, in order:

1. the CLI flag (`--server`, `--set`), in the CLI only;
2. the request's pin;
3. the project's active choice;
4. the first server; no set.

- **Toolbar**: a server popup and a parameter set popup before Send. The set popup is hidden
  while the project has no sets and offers "No Parameter Set". Each popup ends with a "Pin to
  This Request" item with a checkmark. Not pinned, choosing an item changes the project's
  choice; pinned, it changes the pin. Unchecking returns the request to the project's choice.
- **A pin is very noticeable**: the pinned popup shows the `pin.fill` symbol and a bezel tinted
  with `colors::warning`.
- New request: no pins. Duplicate: copies the pins. Deleting a server or set clears the pins on
  it, and those requests follow the project's choice again.
- **Upgrade**: the project's last used server (`washboard.last_server`) becomes its active
  server. `request.last_server_id` values are not turned into pins: they record last use, not a
  choice, and pinning them would leave the project-wide choice with nothing to do.
  `last_server_id` stays in the table and `RequestMeta`, but selection no longer reads it.

## History
The stored request body is the resolved text, so a history entry shows the values that were
sent. The entry also records `parameter_set_id`, and the response pane names the set beside the
server.

## Settings
Each project gets a **Parameters** pane ([settings](settings.md)): a grid with a row per set
and a column per variable; add, rename, reorder and delete both; an unset cell is drawn
differently from an empty one. Deleting a variable or set asks first when values would be lost.

## CLI
- `set list`, `set add <name>`, `set rm <name>`, `set rename <old> <new>`.
- `param list`, `param add <name>`, `param rm <name>`, `param rename <old> <new>`,
  `param set <set> <name> <value>`, `param unset <set> <name>`.
- `request send` and `request validate` take `--set <name>`; `request show --resolved` prints
  what would be sent.

## Reserved
- Names containing `.` are for built-in generated values (`${uuid.v4}`, `${now.iso}`); until
  those exist, such a name is "undefined parameter".
- Decided when built-ins are added, not now: whether two `${uuid.v4}` in one request share a
  value, and whether a set's value may contain a generator.

## Changes elsewhere
- [requests](requests.md): selection replaces "each request remembers `last_server_id`"; Send
  resolves before validating.
- [disk layout](disk-layout.md): the new tables; "what you see is what is sent" becomes "what
  you see, with placeholders filled in, is what is sent".
- [validation](validation.md): runs on the resolved text; the placeholder errors.
- [settings](settings.md): the Parameters pane.
- [response pane](response-pane.md): the set in the status line and history table.

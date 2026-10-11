# Settings (Washboard ▸ Settings…, ⌘,)

One ordinary window (not a sheet) for the app and every open project, laid out like System
Settings: a sidebar of panes, a title-only toolbar naming the pane, grouped forms. Changes apply
as they are made; there is no Done button. A closed project's section leaves the window.

## Panes
- **Washboard** (the app): General, with the indent width and format on save. Stored in the
  user defaults ([macOS app state](macos-app-state.md)).
- **One section per open project**: General (name, folder, WSDL files with Show in Finder and
  Replace WSDL…, see [new project](new-project.md)) and Servers. Project panes say that their
  settings belong to the project and are saved in its folder.

## Servers
The project's servers, then one read-only "From WSDL" row per SOAP 1.1 `soap:address` of the
loaded WSDL, read from the WSDL whenever it loads and never stored. Its only action is Add as
Server…, which opens the server sheet on a new server named after the port, with the address as
its URL. The rows stay when a server has the same address, and they are never sendable: the
toolbar's popup lists servers only.

A server is edited in a sheet (Edit… on its row, Add Server… under the list) with Save, Cancel
and Delete Server, which asks first. User and Password show only with Basic auth, the password
field says whether one is stored, and switching auth off removes it.

New Project opens the window on the new project's Servers pane when its WSDL has an address.

## Layout
Layout follows System Settings, decided once in `crates/washboard-app/src/form.rs`: groups
20 pt from the content edges and at most 640 pt wide, every header, footnote and row label on
one leading line inside the group, controls on the trailing edge, text fields one width.

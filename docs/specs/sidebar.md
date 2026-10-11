# Sidebar

The project window's source list.

One section, REQUESTS: the folder tree, folders with a folder icon and a disclosure triangle,
then requests, each by name in Finder order (case-insensitive, numbers by value,
`window::finder_order`); a new or renamed row moves to its place and stays selected. There is no
list of the WSDL's operations in the sidebar; the New picker is where they are shown, the
unsupported ones too.

- **Context menus.** A folder: New Request…, New Folder, Rename, Delete. A request: the
  commands above, acting on the clicked row. The REQUESTS header: New Request…, New Folder.
  File ▸ New Folder (⌥⌘N) creates one in the selected folder, or the selected request's folder,
  and starts inline rename.
- **Rename** by Return or double-click on a request or folder; a double-click on a folder's
  triangle still only toggles it.
- **Delete** of a folder confirms with what goes: "Delete “GetCustomer” and its 3 requests?
  Their history is deleted too." An empty folder goes without asking.
- **Drag and drop** moves requests and folders between folders and to the top level (an
  internal pasteboard type, `NSDragOperation::Move` only). Drops the model refuses do not
  highlight.
- Collapsed folders are kept per project in `ui_state`.

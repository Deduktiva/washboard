# Response pane and history

- **Layout.** The content area is one vertical `NSSplitView`: the request on the left (request
  bar, editor, issues bar), the response on the right. This replaces the earlier stacked layout;
  there is no setting to get it back. The request bar and the response's status line have the
  same height, so the two halves start on one band. Each half keeps a minimum width, and the
  window's minimum width leaves room for the sidebar and both.
- **Response side**, top to bottom: the status line (status · duration · size [· SOAP Fault] ·
  server [· parameter set] · sent time, the URL as its tooltip), tabs **Response** (pretty-printed, highlighted)
  and **Headers**, the body, and the History drawer.
- **History drawer.** A second split under the response body. Its header line is always shown:
  a disclosure triangle, "History", the number of earlier exchanges, and one dot per stored
  exchange (oldest left; red for a SOAP Fault or a transport failure; a ring around the one
  shown). Clicking the header opens or closes the table (Sent | Server | Status | Duration,
  newest first; Server names the parameter set too, as `Staging · Customer A`). Whether it is open and its height are kept per project in `ui_state`.
- **Older exchange.** Selecting any row but the newest shows that whole exchange:
  - The request side shows the request as sent (`history/…/<stamp>.request.xml`), read-only, in a
    separate text view on the window background colour, with "Sent <time> · read-only" in the
    request bar. The editor keeps its buffer, selection and undo stack; the issues bar is
    hidden, since its results are about the editor's text.
  - A titlebar accessory (`NSTitlebarAccessoryViewController`, layout attribute bottom) appears
    under the toolbar: "Older exchange · <time> · <server>[ · <set>]", **Restore Request**, and **Show
    Latest** (default button, Esc). Its background is a yellow tint that works in light and dark.
    While it shows, the toolbar's own items (server and set popups, Send, HTTP Log) are hidden. A unified
    toolbar cannot be coloured itself; the accessory is AppKit's way to attach a bar to it.
  - Send (⌘↩), Edit ▸ Validate (⌘B) and Format XML are disabled, in the menu too, and the
    model refuses to send in this state, so an old exchange is never sent by a shortcut.
- **Back to the latest.** Show Latest, Esc, selecting the newest row, switching requests and a
  finished send all return to the newest exchange and the editor. Restore Request puts the sent
  request into the editor as one undo step (applied through the widget, like Format XML) and
  returns to it; the editor's previous text is one ⌘Z away.

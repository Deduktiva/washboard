# HTTP log (Window ▸ HTTP Log, ⌥⌘L)

One panel for the app. Shows the last exchange in full: request line, headers, body;
status line, headers, body; timing; TLS info. Keeps the last 50 exchanges in memory in a list
above (small extension of "last request and response"; trivially dropped if unwanted). Not
persisted. Bodies over 5 MB are truncated in the view.

Authorization values are masked in the log by default (click to reveal).

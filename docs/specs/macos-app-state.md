# macOS app state: `AppState`

What the app keeps outside project folders, and how it reopens projects.

## Open projects
`~/Library/Application Support/com.deduktiva.Washboard/state.json`: list of open projects (path +
bookmark data, window frame autosave name, last selected request). Bookmark data is stored
from day one so we can enable the App Sandbox later without a migration.

One window per project. On quit we write the list of open projects; on launch we reopen them
(missing folders reported once, then dropped from the list). With nothing open, a welcome window
lists recent projects with *New Project…* / *Open Project…*.

## App settings
App settings (not per project) live in the user defaults (`NSUserDefaults`, domain
`com.deduktiva.washboard`), where macOS keeps them and `defaults` can read them: `FormatIndent`
and `FormatOnSave` so far. The app reads them and passes them to the model at launch and on
change; the model stores no settings of its own, and the CLI reads none. The Settings window's
selected pane (`SettingsPane`) is kept there too.

## Project files: status messages of new, open and save, and the reasons a file fails (`project_io.rs`).

# New project created (S10).
project-new-created = Neues Projekt angelegt · Strg T legt eine MIDI-Spur an

# Same on macOS: ⌘ instead of Strg.
project-new-created-mac = Neues Projekt angelegt · ⌘T legt eine MIDI-Spur an

# A project file could not be opened; `$file` is the file name, `$reason` one of the `project-reason-*` texts.
project-open-failed = „{ $file }“ lässt sich nicht öffnen: { $reason }

# A project opened; the plugin check is still pending.
project-opened = { $name } geöffnet

# A project opened and nothing is missing.
project-opened-ready = { $name } geöffnet · Leertaste spielt

# A project opened with missing plugins or samples; `$problems` says which (dialog title text).
project-opened-problems = { $name } geöffnet · { $problems }

# The project was saved; `$path` is the shortened path.
project-saved = Gespeichert · { $path }

# Saving failed; `$reason` is one of the `project-reason-*` texts.
project-save-failed = Speichern fehlgeschlagen: { $reason }
project-save-plugin-pending = Zustand von { $device } ist noch nicht verfügbar. Beende gegebenenfalls den temporären Automations-Eingriff und speichere erneut.

## Reasons (lower case, they follow a colon). The system's own text goes to the log only.

# The file does not exist.
project-reason-not-found = Datei nicht gefunden

# The file or folder is not accessible.
project-reason-denied = keine Berechtigung

# Any other read or write error.
project-reason-io = Datei lässt sich nicht lesen oder schreiben

# The file is JSON or something else, but not a komponist project.
project-reason-not-project = keine komponist-Projektdatei

# The file was written by a newer komponist.
project-reason-too-new = Dateiversion { $found } ist neuer als dieses komponist

# The file is a project but its content is wrong.
project-reason-damaged = Datei ist beschädigt

# A migration of an older file failed.
project-reason-migration = Datei lässt sich nicht aktualisieren

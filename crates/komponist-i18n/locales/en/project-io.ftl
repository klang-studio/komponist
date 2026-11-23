## Project files: status messages of new, open and save, and the reasons a file fails (`project_io.rs`).

# New project created (S10).
project-new-created = New project created · Ctrl+T adds a MIDI track

# Same on macOS: ⌘ instead of Ctrl.
project-new-created-mac = New project created · ⌘T adds a MIDI track

# A project file could not be opened; `$file` is the file name, `$reason` one of the `project-reason-*` texts.
project-open-failed = “{ $file }” could not be opened: { $reason }

# A project opened; the plugin check is still pending.
project-opened = { $name } opened

# A project opened and nothing is missing.
project-opened-ready = { $name } opened · Space plays

# A project opened with missing plugins or samples; `$problems` says which (dialog title text).
project-opened-problems = { $name } opened · { $problems }

# The project was saved; `$path` is the shortened path.
project-saved = Saved · { $path }

# Saving failed; `$reason` is one of the `project-reason-*` texts.
project-save-failed = Saving failed: { $reason }
project-save-plugin-pending = The state of { $device } is still pending. End any temporary automation override and save again.
project-save-plugin-cached = Saved Previous Plugin State: { $devices }
project-save-plugin-missing-state = No Plugin State Available: { $devices }

# The project file is saved, but the system did not confirm that it reached the disk (shown after the save status).
project-save-not-durable = Not yet flushed to disk

## Reasons (lower case, they follow a colon). The system's own text goes to the log only.

# The file does not exist.
project-reason-not-found = file not found

# The file or folder is not accessible.
project-reason-denied = permission denied

# Any other read or write error.
project-reason-io = file could not be read or written

# The file is JSON or something else, but not a komponist project.
project-reason-not-project = not a komponist project file

# The file was written by a newer komponist.
project-reason-too-new = file version { $found } is newer than this komponist

# The file is a project but its content is wrong.
project-reason-damaged = file is damaged

# A migration of an older file failed.
project-reason-migration = file could not be upgraded

## S09 start screen (`dialogs/start.rs`).

# S09 button "Neues Projekt".
start-new = New Project

# S09 button "Projekt öffnen …" (needs more input).
start-open = Open Project…

# S09 button "Aus Vorlage"; the template's name follows as a hint (`new-project-template-name`).
start-template = From Template

# S09 label of the system block at the bottom left.
start-system = System

# System row: audio.
start-row-audio = Audio

# System row: MIDI.
start-row-midi = MIDI

# System row: plugins.
start-row-plugins = Plugins

# S09 header of the recent list (name of the list).
start-recent = Recent

# S09 filter field: name and hint.
start-filter = Filter

# S09 keyboard hint under the list.
start-hint = ↑ ↓ choose · Enter opens · Del removes from the list · You can also open projects by drag and drop

# S09 empty list.
start-no-recent = No projects opened yet.

# S09 list with a filter that matches nothing.
start-no-match = No project matches the filter.

# S09 recent row of a file that is gone.
start-file-missing = File missing

# S09 version line under the wordmark; `$version` is "0.1.0".
start-version = { $version } · Linux · CLAP and VST3

# Same on macOS; the platform is part of the line.
start-version-mac = { $version } · macOS · CLAP and VST3

# Same on Windows.
start-version-windows = { $version } · Windows · CLAP and VST3

# S09 recent row: tempo and track count; `$bpm` is "124", `$tracks` a count.
start-meta = { $bpm } BPM · { $tracks ->
        [one] 1 track
       *[other] { $tracks } tracks
    }

# System row audio without a device.
start-audio-none = No audio device

# System row audio while the stream is stopped.
start-audio-stopped = Audio stopped

# System row MIDI without a port.
start-midi-none = No MIDI device found

# System row plugins during a scan; `$shortcut` opens the settings.
start-plugins-scanning = Scan running · Settings { $shortcut }

# System row plugins after a scan; `$found` is the plugin count.
start-plugins-found = { $found } found · Settings { $shortcut }

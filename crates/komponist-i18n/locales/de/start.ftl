## S09 start screen (`dialogs/start.rs`).

# S09 button "Neues Projekt".
start-new = Neues Projekt

# S09 button "Projekt öffnen …" (needs more input).
start-open = Projekt öffnen …

# S09 button "Aus Vorlage"; the template's name follows as a hint (`new-project-template-name`).
start-template = Aus Vorlage

# S09 label of the system block at the bottom left.
start-system = System

# System row: audio.
start-row-audio = Audio

# System row: MIDI.
start-row-midi = MIDI

# System row: plugins.
start-row-plugins = Plugins

# S09 header of the recent list (name of the list).
start-recent = Zuletzt geöffnet

# S09 filter field: name and hint.
start-filter = Filtern

# S09 keyboard hint under the list.
start-hint = ↑ ↓ wählen · Enter öffnet · Entf entfernt aus der Liste · Projekte lassen sich auch per Drag and Drop öffnen

# S09 empty list.
start-no-recent = Noch keine Projekte geöffnet.

# S09 list with a filter that matches nothing.
start-no-match = Kein Projekt passt zum Filter.

# S09 recent row of a file that is gone.
start-file-missing = Datei fehlt

# S09 version line under the wordmark; `$version` is "0.1.0".
start-version = { $version } · Linux · CLAP und VST3

# Same on macOS; the platform is part of the line.
start-version-mac = { $version } · macOS · CLAP und VST3

# Same on Windows.
start-version-windows = { $version } · Windows · CLAP und VST3

# S09 recent row: tempo and track count; `$bpm` is "124", `$tracks` a count.
start-meta = { $bpm } BPM · { $tracks ->
        [one] 1 Spur
       *[other] { $tracks } Spuren
    }

# System row audio without a device.
start-audio-none = Kein Audio-Gerät

# System row audio while the stream is stopped.
start-audio-stopped = Audio angehalten

# System row MIDI without a port.
start-midi-none = kein MIDI-Gerät erkannt

# System row plugins during a scan; `$shortcut` opens the settings.
start-plugins-scanning = Scan läuft · Einstellungen { $shortcut }

# System row plugins after a scan; `$found` is the plugin count.
start-plugins-found = { $found } gefunden · Einstellungen { $shortcut }

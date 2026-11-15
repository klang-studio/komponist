## S07 settings (`dialogs/settings.rs`).

# S07 title of the dialog.
settings-title = Einstellungen

# S07 navigation tab: audio.
settings-nav-audio = Audio

# S07 navigation tab: MIDI.
settings-nav-midi = MIDI

# S07 navigation tab: plugins.
settings-nav-plugins = Plugins

# S07 navigation tab: appearance.
settings-nav-display = Darstellung

# S07 navigation tab: keyboard.
settings-nav-keyboard = Tastatur

# S07 section "Audio".
settings-audio = Audio

# S07 section "MIDI-Eingänge".
settings-midi-inputs = MIDI-Eingänge

# S07 section "Plugin-Ordner".
settings-plugin-dirs = Plugin-Ordner

# S07 section "Darstellung".
settings-display = Darstellung

# S07 section "Tastatur".
settings-keyboard = Tastatur

# S07 row label and name of the driver control.
settings-driver = Treiber

# S07 row label and name of the output device dropdown.
settings-device = Ausgabegerät

# S07 row label and name of the sample rate dropdown.
settings-rate = Abtastrate

# S07 row label and name of the buffer size control.
settings-buffer = Puffergröße

# S07 first entry of the output device dropdown: the driver's default.
settings-default-device = Standardgerät

# S07 notice without a device.
settings-no-device-notice = Gerät nicht verbunden. Wiedergabe und Aufnahme sind angehalten, das Projekt bleibt bearbeitbar.

# S07 button of that notice.
settings-retry = Erneut suchen

# S07 hint when the audio thread got no real-time priority.
settings-no-realtime = Audio läuft ohne Echtzeit-Priorität; Aussetzer sind möglich. RTKit oder die Gruppe „audio“ erlauben sie.

# S07 button: add a plugin folder.
settings-add-folder = + Ordner

# S07 button: scan the plugin folders again.
settings-rescan = Neu scannen

# S07 name of the × button of an added plugin folder.
settings-remove-folder = Ordner entfernen

# S07 text without MIDI inputs.
settings-no-midi = Keine MIDI-Eingänge gefunden

# S07 MIDI row, input switched off.
settings-midi-off = aus

# S07 MIDI row, input on and connected.
settings-midi-active = aktiv · Kanal 1–16

# S07 MIDI row, input on but not present.
settings-midi-disconnected = nicht verbunden

# S07 row label and name of the UI scale control.
settings-scale = UI-Skalierung

# S07 UI scale segment: follow the display.
settings-scale-auto = Automatisch

# S07 UI scale segment; `$percent` is 100, 125, 150 or 200.
settings-scale-percent = { $percent } %

# S07 label of the computer keyboard switch.
settings-keyboard-midi = Computer-Tastatur spielt Noten

# S07 hint under that switch (prototype start screen "Tastatur").
settings-keyboard-hint = A W S E D F T G Z H U J K spielen Noten, sobald eine Spur scharf ist oder dieser Schalter an ist.

# S07 scan summary while a scan runs.
settings-scan-running = Scan läuft …

# S07 scan summary before the first scan.
settings-scan-none = Noch kein Scan

# S07 scan summary: plugins found.
settings-scan-found = { $n ->
        [one] 1 Plugin gefunden
       *[other] { $n } Plugins gefunden
    }

# S07 scan summary: when the last scan ended; `$when` is a day and time ("heute 09:12").
settings-scan-last = letzter Scan { $when }

# S07 scan summary: plugins that could not be loaded.
settings-scan-skipped = { $n ->
        [one] 1 Plugin übersprungen (lässt sich nicht laden)
       *[other] { $n } Plugins übersprungen (lassen sich nicht laden)
    }

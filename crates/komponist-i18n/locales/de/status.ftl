## Status bar (`Screens.dc.html` `stt`, `ks-view.js` `statusView`) and the messages the app shows in it (`App::set_status`).

# Right side of the status bar: the audio output. `$host` is the driver, `$rate` e.g. "48 kHz", `$ms` the latency.
status-output = { $host } · { $rate } · { $frames } Samples · { $ms } ms

# Appended to the audio output when the audio thread got no real-time priority.
status-no-realtime = ohne Echtzeit-Priorität

# Right side while the audio stream is not running.
status-audio-stopped = Audio angehalten

# Largest number of frames missing at a single delay line.
status-pdc-shortfall = Latenzausgleich unvollständig · Es fehlen bis zu { $frames } { $frames ->
        [one] Frame
       *[other] Frames
    } je Verzögerungsleitung

# Left side while recording; `$tracks` are the armed track names. The leading "● " is painted as a dot.
status-recording = ● Aufnahme läuft · { $tracks }

# Middle text while recording.
status-recording-hint = R oder Leertaste beendet · Esc verwirft den Take

# Left side during the count-in.
status-count-in = Vorzähler …

# Middle text while a device parameter is dragged.
status-param-hint = Alt-Klick: Standard · Rechtsklick: Wert eingeben

# Same on macOS: ⌥ instead of Alt.
status-param-hint-mac = ⌥-Klick: Standard · Rechtsklick: Wert eingeben

# Left side in the piano roll; German keeps "Noten" for one note (verbatim S01).
status-notes-selected = { $clip } · { $selected } von { $total } { $total ->
        [one] Noten
       *[other] Noten
    } ausgewählt

# Middle text in the piano roll.
status-notes-hint = Strg-Ziehen auf Note: Velocity · Alt-Ziehen: Kopie

# Same on macOS: ⌘ instead of Strg, ⌥ instead of Alt.
status-notes-hint-mac = ⌘-Ziehen auf Note: Velocity · ⌥-Ziehen: Kopie

# Left side in the step sequencer with a selected step; `$step` counts from 1.
status-step-selected = { $clip } · Step { $step } von { $length }

# Middle text in the step sequencer with a selected step.
status-step-hint = Klick setzt Step · Ziehen ↕ auf Step: Velocity · Alt-Klick: Wahrscheinlichkeit

# Same on macOS: ⌥ instead of Alt.
status-step-hint-mac = Klick setzt Step · Ziehen ↕ auf Step: Velocity · ⌥-Klick: Wahrscheinlichkeit

# Left side in the step sequencer without a selected step; German keeps "Steps" for one step.
status-steps-set = { $clip } · { $count } { $count ->
        [one] Steps
       *[other] Steps
    } gesetzt

# Middle text in the step sequencer without a selected step.
status-steps-hint = Leertaste spielt das Pattern

# Left side with a selected track (also in the maximized mixer).
status-track-selected = { $name } ausgewählt

# Middle text in the maximized mixer.
status-mixer-hint = Tab: zurück zum Arrangement · Klick auf rote Anzeige setzt zurück

# Left side in the device chain.
status-devices = { $name } · { $count ->
        [one] 1 Gerät
       *[other] { $count } Geräte
    }

# Middle text in the device chain.
status-devices-hint = Gerät ziehen: umsortieren · Alt-Klick auf Schalter: Bypass

# Same on macOS: ⌥ instead of Alt.
status-devices-hint-mac = Gerät ziehen: umsortieren · ⌥-Klick auf Schalter: Bypass

# Left side with selected clips.
status-clips-selected = { $count ->
        [one] 1 Clip
       *[other] { $count } Clips
    } ausgewählt

# Middle text with selected clips.
status-clips-hint = Ziehen verschiebt · Alt-Ziehen kopiert · rechte Kante kürzt · Strg D dupliziert · Entf löscht

# Same on macOS: ⌘ instead of Strg, ⌥ instead of Alt.
status-clips-hint-mac = Ziehen verschiebt · ⌥-Ziehen kopiert · rechte Kante kürzt · ⌘D dupliziert · Entf löscht

# Middle text with a selected track.
status-track-hint = Doppelklick in die Spur legt einen Clip an · R nimmt auf

# Left side of a project that is new or has unsaved changes.
status-project-unsaved = { $name } · nicht gespeichert

# Middle text of a project without tracks.
status-empty-hint = Strg T: MIDI-Spur · Strg Shift T: Audio-Spur · Strg S: speichern

# Same on macOS: ⌘ instead of Strg, ⌥ instead of Alt.
status-empty-hint-mac = ⌘T: MIDI-Spur · ⌘⇧T: Audio-Spur · ⌘S: speichern

## Messages (`App::set_status`).

# The engine's worker threads run without real-time priority.
status-workers-no-realtime = Audio-Threads laufen ohne Echtzeit-Priorität

# The engine's worker threads could not join the audio device's workgroup (macOS).
status-workers-no-workgroup = Audio-Threads konnten der Audio-Workgroup nicht beitreten

# The audio engine panicked in the audio callback; a new one was built from the open project (#210).
status-engine-rebuilt = Audio-Engine abgestürzt · aus dem Projekt neu aufgebaut, die Wiedergabe ist gestoppt

# Same while a take was running: it ended at the crash and keeps what was recorded.
status-engine-rebuilt-take = Audio-Engine abgestürzt · die Aufnahme bis dahin bleibt erhalten, aus dem Projekt neu aufgebaut

# A MIDI input appeared; `$name` as S07 shows it.
status-midi-connected = { $name } verbunden

# A MIDI input vanished.
status-midi-disconnected = { $name } getrennt

# A plugin bridge exited.
status-device-crashed = { $name } abgestürzt · die Spur läuft ohne dieses Gerät weiter

# A plugin bridge does not answer.
status-device-unresponsive = { $name } hängt · die Spur läuft ohne dieses Gerät weiter

# A plugin could not be loaded.
status-device-load-failed = { $name } lässt sich nicht laden

# Name of a device that is gone, for messages.
status-device-unnamed = Gerät { $id }

# Source of a plugin log line without a device.
status-plugin-scan = Plugin-Scan
status-bundled-devices-unavailable = Mitgelieferte Geräte fehlen.

# A plugin could not restore its saved state.
status-state-load-failed = { $name }: Zustand konnte nicht geladen werden

# One plugin's process runs without real-time priority.
status-plugin-no-realtime = { $name } läuft ohne Echtzeit-Priorität

# A MIDI effect dropped where no track is selected.
status-midi-fx-need-track = MIDI-Effekte brauchen eine Spur

# An effect dropped where no track is selected.
status-fx-need-track = Effekte brauchen eine Spur

# Collect and save: nothing had to be copied.
status-collected-none = Gespeichert · keine Samples zu sammeln ({ $folder })

# Collect and save: `$count` samples were copied into `$folder`.
status-collected = Gesammelt und gespeichert · { $count ->
        [one] 1 Sample
       *[other] { $count } Samples
    } in „{ $folder }“

# Collecting the samples failed; `$reason` is the system's text.
status-collect-failed = Sammeln fehlgeschlagen: { $reason }

# Editing cannot create an object once the project ID counter is exhausted.
status-id-space-exhausted = Keine freien Objekt-IDs im Projekt verfügbar.
# Audio clip editing and refused drops.
status-audio-clips-hint = Ziehen verschiebt · Alt-Ziehen kopiert · Kanten kürzen · Ecken: Fades · Linie: Gain · Strg E teilt · Strg D dupliziert · Entf löscht
status-audio-clips-hint-mac = Ziehen verschiebt · ⌥-Ziehen kopiert · Kanten kürzen · Ecken: Fades · Linie: Gain · ⌘ E teilt · ⌘ D dupliziert · Entf löscht
status-drop-refused-midi = MIDI-Spuren nehmen keine Audio-Dateien auf.
status-drop-refused-audio = Audio-Spuren nehmen keine MIDI-Clips auf.

# Linked audio import and browsing (epic #8).
status-collect-skipped = { $count } Dateien nicht gesammelt: keine unterstützten Audio-Dateien · { $files }
status-collect-unapproved = Plugin-Dateien wurden nicht gesammelt. Wähle sie im Gerät aus, um das Kopieren zu erlauben. { $files }

status-macros-mapping = Zuordnen: Parameter bewegen ordnet sie { $source } zu · Klick auf ein anderes Makro wechselt · Esc beendet
status-macros-locked = { $param } wird von { $source } gesteuert
status-macros-moved = { $param }: jetzt { $source }

# #227: rejected edits and failed device starts reach the status bar.
status-edit-routing-cycle = Nicht möglich: Das würde eine Schleife im Routing bilden
status-edit-not-a-bus = Nicht möglich: „{ $track }“ ist kein Bus
status-edit-automation-too-large = Nicht möglich: Die Automation würde zu viele Punkte erzeugen
status-edit-macro-full = Nicht möglich: Spur-Makro { $n } hat schon 16 Zuordnungen
# Errors only a bug can cause; the details go to the log.
status-edit-failed = Bearbeitung nicht möglich
status-midi-enable-failed = MIDI-Eingang „{ $device }“ lässt sich nicht öffnen
status-audio-input-failed = Audio-Eingang lässt sich nicht starten · Einstellungen { $shortcut }

## Crash recovery (#227, crash-recovery proposal (internal notes)).

# A recovered copy opened; `$time` is "heute 14:32", `$shortcut` the save shortcut ("Strg S"), `$file` the file name.
status-recovered = Änderungen von { $time } wiederhergestellt · { $shortcut } speichert sie in „{ $file }“

# Same for a project that was never saved.
status-recovered-untitled = Änderungen von { $time } wiederhergestellt · { $shortcut } speichert sie als neues Projekt

# Same for a project whose file is gone: saving asks for a location.
status-recovered-elsewhere = Änderungen von { $time } wiederhergestellt · { $shortcut } fragt nach einem Ort zum Speichern

# The recovery copy could not be written (once per session); `$reason` is one of the `project-reason-*` texts.
status-recovery-write-failed = Sicherungskopie lässt sich nicht schreiben: { $reason }

# The recovery folder could not be read at start; `$reason` is one of the `project-reason-*` texts.
status-recovery-scan-failed = Sicherungskopien lassen sich nicht lesen: { $reason }

# Audio clips in the arrangement and their editing handles.
audio-clip-missing = ⚠ Datei fehlt: { $file }
audio-clip-missing-short = Datei fehlt
audio-clip-peaks = Wellenform wird berechnet …
audio-clip-end-of-file = Ende der Datei erreicht
audio-clip-cuts-below = schneidet den Clip darunter
audio-clip-repitch-badge = RP
audio-clip-repitch-tip = Re-Pitch: Original { $orig } BPM, Projekt { $project } BPM
audio-clip-fade-in = Fade ein
audio-clip-fade-out = Fade aus
audio-clip-gain-line = Clip-Gain
audio-clip-start = Clip-Anfang
audio-clip-end = Clip-Ende
a11y-audio-missing-suffix = , Datei fehlt
a11y-audio-repitch-suffix = , Re-Pitch { $tempo } BPM

# Audio source editor and file inspector.
dock-waveform = Wellenform
inspector-clip-audio = AUDIO
audio-clip-source-start = Quell-Start
audio-clip-gain = Gain
audio-clip-repitch = Re-Pitch
audio-clip-original-tempo = Originaltempo
audio-clip-bar-count = Quelle auf
audio-clip-bars = { $n } Takte
audio-waveform-title = Wellenform, { $name }
audio-source-start = Dateianfang
audio-source-end = Dateiende
audio-channel-left = L
audio-channel-right = R
audio-channel-mono = mono
audio-channel-stereo = Stereo
audio-amplitude-zero = 0 dB
audio-amplitude-silence = −∞
audio-file = Datei
audio-file-replace = Datei ersetzen …
audio-file-copy-path = Pfad kopieren
audio-file-show = Im Ordner zeigen
audio-file-actions = Dateiaktionen
audio-file-external = extern verknüpft
audio-file-in-project = im Projekt
audio-file-path = Dateipfad
audio-file-metadata = { $rate } kHz · { $channels } · { $duration }
audio-file-bit-depth = { $bits } Bit
audio-factor = { $pct } · { $st } st
audio-file-replace-shortened = „{ $name }“ ersetzt · Clip bis zum Dateiende gekürzt
audio-file-replace-failed = Datei konnte nicht ersetzt werden: { $reason }
audio-file-no-audio = Keine lesbare Audio-Datei
audio-file-too-short = Die Datei endet vor dem Quell-Start des Clips.
audio-zoom-in = Wellenform vergrößern
audio-zoom-out = Wellenform verkleinern
audio-waveform-pending = Wellenform wird berechnet …
audio-waveform-controls = Wellenform-Werkzeuge
# Linked audio import and browsing (epic #8).
menu-import-audio = Audio-Datei importieren …
menu-new-audio-track = Neue Audio-Spur
audio-import-confirm = Importieren
status-audio-import-failed = Audio-Datei lässt sich nicht importieren · { $file } · { $why }
status-audio-imported =
    { $count ->
        [one] 1 Audio-Datei verknüpft
       *[other] { $count } Audio-Dateien verknüpft
    }
audio-file-empty = Die Audiodatei ist leer.
missing-audio-files = Dateien fehlen
missing-plugins-and-files = Plugins und Dateien fehlen
missing-audio-text = Im Projekt „{ $project }“ fehlen verknüpfte Audio-Dateien. Öffne einen Clip und wähle „Datei ersetzen …“, um seine Datei wiederzufinden. Die Clips bleiben erhalten.
missing-continue-files = Fortfahren
missing-audio-open-clip = Clip öffnen
missing-audio-chip = AUDIO
missing-audio-place = Spur { $track } · Clip { $clip }

# Audio input, monitoring and recording UI (approved audio proposal).
audio-input-none = Kein Eingang
audio-input-mono = Eingang { $n }
audio-input-pair = Eingang { $a }+{ $b }
audio-input-bus = Bus { $name }
audio-input-missing-bus = nicht verfügbar
audio-input-unavailable = { $name } · nicht verfügbar
audio-input-tip = Eingang: { $name }
audio-input-tip2 = Klick wählt den Eingang
audio-input-title = Eingang · { $track }
audio-input-sources = Quellen
audio-input-section-bus = Busse
audio-input-clear = Kein Eingang
audio-input-loop = würde eine Schleife bilden
audio-input-no-device = Kein Audio-Eingang verfügbar
audio-input-open-settings = Audio-Einstellungen öffnen …
audio-arm-no-input = Kein Eingang gewählt · Klick auf die Eingangszeile
audio-arm-unavailable = { $name } · nicht verfügbar
audio-input-open-failed = Audio-Eingang lässt sich nicht öffnen · { $why }
audio-input-meter = Eingangspegel { $track }
audio-monitoring = Monitoring
ui-monitor-off = aus
ui-monitor-auto = auto
ui-monitor-on = an
ui-monitor-menu-off = Aus
ui-monitor-menu-auto = Auto · bei Record-Arm
ui-monitor-menu-on = An
ui-monitor-tip2 = Rechtsklick: Auto
ui-monitor-auto-mark = A
audio-monitor-auto-armed = überwacht durch Record-Arm
audio-bus-record-short = REC
routing-record-bus = Bus aufnehmen
routing-record-bus-name = Bus aufnehmen, { $bus }
routing-record-bus-tip = Legt eine Audio-Spur an, die diesen Bus aufnimmt
routing-record-bus-track = { $bus } Aufnahme
status-record-bus-created = { $track } angelegt · Eingang Bus { $bus } · R nimmt auf
mixer-audio = Audio
audio-recordings-provisional = Aufnahmen vorläufig
audio-recordings-provisional-tip = Aufnahmen liegen vorläufig in { $folder } und wandern beim Speichern ins Projekt.
audio-file-provisional = vorläufig
audio-file-provisional-note = Liegt vorläufig in { $folder } und wandert beim Speichern ins Projekt.
audio-take-remove = Take „{ $name }“ entfernen
audio-take-rename = Take umbenennen
a11y-audio-takes-suffix = , Take { $take } von { $count }
audio-input-change-recording = Beende die Aufnahme, bevor du den Eingang wechselst.
audio-clip-context-menu = Audio-Clip
audio-clip-context-mute = Stumm
audio-clip-context-duplicate = Duplizieren
audio-clip-context-split = Teilen

# Live recording waveform toolbar.
audio-live-waveform = Wellenform · live
audio-live-input = Eingang { $input }
audio-live-peak = Peak { $db } dBFS
audio-live-duration = Dauer { $time }
# Audio recording lifecycle and file failures.
audio-record-input-unavailable = Kein verfügbarer Audio-Eingang für „{ $track }“ · Eingang wählen
audio-record-finalizing = Aufnahme wird abgeschlossen …
audio-record-folder-unavailable = Kein Ordner für Aufnahmen verfügbar
audio-record-failed = Aufnahme fehlgeschlagen: { $reason }
audio-record-move-failed = Aufnahme bleibt vorläufig gespeichert: { $reason }
audio-record-complete = Aufnahme abgeschlossen · { $count } Takes
audio-record-apply-failed = Aufnahme konnte nicht ins Projekt übernommen werden · Dateien bleiben erhalten
audio-monitor-tip = Monitoring: { $mode }
status-recording-input = ● Aufnahme läuft · { $tracks } · { $input }

# Approved #14 Warp UI: exclusive playback mode and full-source interpretation.
audio-clip-mode = Modus
audio-clip-fixed = Fix
audio-clip-warp = Warp
audio-clip-mode-help = Fix: Originalgeschwindigkeit. Re-Pitch: Tempo folgen, Tonhöhe ändern. Warp: Tempo folgen, Tonhöhe beibehalten.
audio-clip-source-bars-help = Berechnet das Originaltempo der gesamten aktiven Quelldatei aus der Taktzahl im aktuellen Taktmaß.
audio-clip-duration = Dauer
audio-clip-duration-value = Dauer { $ratio }×
audio-clip-duration-help = Ausgabedauer / Quelldauer: { $ratio }× · Originaltempo / Projekttempo
audio-clip-preserve-pitch = Tonhöhe beibehalten
audio-clip-original-speed = Originalgeschwindigkeit
audio-source-ruler = Quellzeit
audio-source-time = Quelle { $source } ↔ Song { $song }
audio-warp-error = Warp-Fehler aufgetreten: { $reason }
audio-source-ruler-short = Quelle
audio-song-ruler = Song

# Audio file analysis and explicit original-tempo confirmation.
audio-tempo-detect = Tempo erkennen
audio-tempo-analyzing = Tempo wird analysiert …
audio-tempo-unknown = Kein stabiles Tempo erkannt
audio-tempo-failed = Tempoanalyse fehlgeschlagen
audio-tempo-estimate = { $bpm } BPM · { $confidence } % Konfidenz
audio-tempo-use = { $bpm } BPM übernehmen
audio-tempo-use-half = { $bpm } BPM übernehmen (halb)
audio-tempo-use-double = { $bpm } BPM übernehmen (doppelt)

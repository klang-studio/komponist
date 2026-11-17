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
audio-clip-bar-count = Takte
audio-clip-bars = = { $n } Takte
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
audio-clip-context-menu = Audio-Clip
audio-clip-context-mute = Stumm
audio-clip-context-duplicate = Duplizieren
audio-clip-context-split = Teilen

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

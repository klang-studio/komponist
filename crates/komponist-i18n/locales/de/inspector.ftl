## Inspector (`Screens.dc.html` `sec`, `F`, `trackSec`, the inspector markup; `ks-view.js` `inspView`; S01, S02, S03, S10; `views/inspector`). Key prefix is the section or field group; the file is the area.

# Section "Clip".
inspector-section-clip = Clip

# Section "Spur".
inspector-section-track = Spur

# Section "Bus" of a selected bus or group (routing proposal 07-inspektor C).
inspector-section-bus = Bus

# Section "Geräte".
inspector-section-devices = Geräte

# Section "Parameter" (a touched device parameter, WP-89).
inspector-section-parameter = Parameter

# Parameter field "Wert".
inspector-param-value = Wert

# Parameter field "Standard".
inspector-param-default = Standard

# Parameter field "Bereich".
inspector-param-range = Bereich

# Section "Kanal".
inspector-section-channel = Kanal

# Section "Projekt" (nothing selected).
inspector-section-project = Projekt

# Section "Auswahl" (nothing selected).
inspector-section-selection = Auswahl

# Text of the empty selection (S10).
inspector-nothing-selected = Nichts ausgewählt. Wenn nichts gewählt ist, zeigt der Inspector die Projekt-Einstellungen.

# Clip kind of a MIDI clip (`meta:'MIDI'`).
inspector-clip-midi = MIDI

# Clip kind of a step clip (`meta:'STEP'`).
inspector-clip-step = STEP

# The clip being recorded (`meta:'nimmt auf'`).
inspector-clip-recording = nimmt auf

# Clip field "Start".
inspector-clip-start = Start

# Clip field "Länge".
inspector-clip-length = Länge

# Clip field "Loop".
inspector-clip-loop = Loop

# Clip field "Loop-Länge".
inspector-clip-loop-length = Loop-Länge

# Clip field "Transponieren".
inspector-clip-transpose = Transponieren

# Clip field "Velocity".
inspector-clip-velocity = Velocity

# Clip field "Pattern" (step clips).
inspector-clip-pattern = Pattern

# Unit of the pattern length.
inspector-clip-steps = Steps

# Clip field "Auflösung" (step clips).
inspector-clip-resolution = Auflösung

# Clip field "Swing" (step clips).
inspector-clip-swing = Swing

# Clip field "Noten" (recording).
inspector-clip-notes = Noten

# Clip field "Quantisieren" (recording).
inspector-clip-quantize = Quantisieren

# Value "an".
inspector-value-on = an

# Value "aus".
inspector-value-off = aus

# The takes list (S02 "Jeder Durchlauf wird ein eigener Take im selben Clip, wählbar im Inspector").
inspector-clip-takes = Takes

# Dropdown "Instrument".
inspector-track-instrument = Instrument

# Its entry for a track without instrument (not in the design).
inspector-track-no-instrument = Kein Instrument

# Dropdown "Eingang".
inspector-track-input = Eingang

# Its default entry.
inspector-track-all-inputs = Alle MIDI-Eingänge

# Dropdown "Kanal" (MIDI channel).
inspector-track-midi-channel = Kanal

# Its default entry.
inspector-track-omni = Omni

# Meta action of "Geräte" (`meta:'+ Effekt'`).
inspector-devices-add-effect = + Effekt

# Meta action of "Kanal" (`meta:'im Mixer zeigen'`).
inspector-channel-show-in-mixer = im Mixer zeigen

# The level in the compact strip.
inspector-channel-volume = Lautstärke

# The pan knob.
inspector-channel-pan = Pan

# The meter of the compact strip.
inspector-channel-meter = Pegel

# Project field "Tempo".
inspector-project-tempo = Tempo

# Project field "Taktart".
inspector-project-meter = Taktart

# Project field "Abtastrate" (S10).
inspector-project-sample-rate = Abtastrate

# Project field "Spuren".
inspector-project-tracks = Spuren

# Unit of the project length.
inspector-project-bars = Takte

# Accessible name of the power square of a device row (as the device card's switch).
inspector-device-active = Aktiv

# Tooltip detail of the power square of a bypassed device.
inspector-device-bypass = Bypass

# Status of a loading device (`ks-view.js` `fmt:'lädt'`).
inspector-device-loading = lädt

# Status of a crashed device (S05 `D('Airwindows Cons.','abgestürzt',{err:1})`).
inspector-device-crashed = abgestürzt

# Status of a device that failed to load. Short on purpose (owner decision Q5, 2026-11-14); the card and the plugin frame keep "Lässt sich nicht laden".
inspector-device-load-failed = lädt nicht

# Status of a device that stopped answering (as the card's "Hängt").
inspector-device-unresponsive = hängt

# Status of a missing device (by analogy, S06 "Plugin fehlt").
inspector-device-missing = fehlt

# Accessible name of the name field while renaming.
inspector-rename = Name

# Accessible name of a swatch of the track colour.
inspector-swatch = Farbe { $hue }

# Accessible name of a device row.
inspector-device = Gerät { $name }

# Notes of a take.
inspector-take-notes =
    { $n ->
        [one] 1 Note
       *[other] { $n } Noten
    }

# Name of a take without a name (a hand-drawn clip's only take has none); `$n` counts from 1.
inspector-take = Take { $n }

# Accessible name of an input row of a bus.
inspector-input-row = Eingang { $name }, { $detail }

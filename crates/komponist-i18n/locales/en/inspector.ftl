## Inspector (`Screens.dc.html` `sec`, `F`, `trackSec`, the inspector markup; `ks-view.js` `inspView`; S01, S02, S03, S10; `views/inspector`). Key prefix is the section or field group; the file is the area.

# Section "Clip".
inspector-section-clip = Clip

# Section "Spur".
inspector-section-track = Track

# Section "Bus" of a selected bus or group (routing proposal 07-inspektor C).
inspector-section-bus = Bus

# Section "Geräte".
inspector-section-devices = Devices

# Section "Parameter" (a touched device parameter, WP-89).
inspector-section-parameter = Parameter

# Parameter field "Wert".
inspector-param-value = Value

# Parameter field "Standard".
inspector-param-default = Default

# Parameter field "Bereich".
inspector-param-range = Range

# Section "Kanal".
inspector-section-channel = Channel

# Section "Projekt" (nothing selected).
inspector-section-project = Project

# Section "Auswahl" (nothing selected).
inspector-section-selection = Selection

# Text of the empty selection (S10).
inspector-nothing-selected = Nothing selected. With nothing selected, the inspector shows the project settings.

# Clip kind of a MIDI clip (`meta:'MIDI'`).
inspector-clip-midi = MIDI

# Clip kind of a step clip (`meta:'STEP'`).
inspector-clip-step = STEP

# The clip being recorded (`meta:'nimmt auf'`).
inspector-clip-recording = recording

# Clip field "Start".
inspector-clip-start = Start

# Clip field "Länge".
inspector-clip-length = Length

# Clip field "Loop".
inspector-clip-loop = Loop

# Clip field "Loop-Länge".
inspector-clip-loop-length = Loop length

# Clip field "Transponieren".
inspector-clip-transpose = Transpose

# Clip field "Velocity".
inspector-clip-velocity = Velocity

# Clip field "Pattern" (step clips).
inspector-clip-pattern = Pattern

# Unit of the pattern length.
inspector-clip-steps = Steps

# Clip field "Auflösung" (step clips).
inspector-clip-resolution = Resolution

# Clip field "Swing" (step clips).
inspector-clip-swing = Swing

# Clip field "Noten" (recording).
inspector-clip-notes = Notes

# Clip field "Quantisieren" (recording).
inspector-clip-quantize = Quantize

# Value "an".
inspector-value-on = on

# Value "aus".
inspector-value-off = off

# The takes list (S02 "Jeder Durchlauf wird ein eigener Take im selben Clip, wählbar im Inspector").
inspector-clip-takes = Takes

# Dropdown "Instrument".
inspector-track-instrument = Instrument

# Its entry for a track without instrument (not in the design).
inspector-track-no-instrument = No Instrument

# Dropdown "Eingang".
inspector-track-input = Input

# Its default entry.
inspector-track-all-inputs = All MIDI inputs

# Dropdown "Kanal" (MIDI channel).
inspector-track-midi-channel = Channel

# Its default entry.
inspector-track-omni = Omni

# Meta action of "Geräte" (`meta:'+ Effekt'`).
inspector-devices-add-effect = + Effect

# Meta action of "Kanal" (`meta:'im Mixer zeigen'`).
inspector-channel-show-in-mixer = show in mixer

# The level in the compact strip.
inspector-channel-volume = Volume

# The pan knob.
inspector-channel-pan = Pan

# The meter of the compact strip.
inspector-channel-meter = Level

# Project field "Tempo".
inspector-project-tempo = Tempo

# Project field "Taktart".
inspector-project-meter = Time signature

# Project field "Abtastrate" (S10).
inspector-project-sample-rate = Sample rate

# Project field "Spuren".
inspector-project-tracks = Tracks

# Unit of the project length.
inspector-project-bars = Bars

# Accessible name of the power square of a device row (as the device card's switch).
inspector-device-active = Active

# Tooltip detail of the power square of a bypassed device.
inspector-device-bypass = Bypass

# Status of a loading device (`ks-view.js` `fmt:'lädt'`).
inspector-device-loading = loading

# Status of a crashed device (S05 `D('Airwindows Cons.','abgestürzt',{err:1})`).
inspector-device-crashed = crashed

# Status of a device that failed to load. Short on purpose (owner decision Q5, 2026-11-14); the card and the plugin frame keep "Lässt sich nicht laden".
inspector-device-load-failed = won't load

# Status of a device that stopped answering (as the card's "Hängt").
inspector-device-unresponsive = not responding

# Status of a missing device (by analogy, S06 "Plugin fehlt").
inspector-device-missing = missing

# Accessible name of the name field while renaming.
inspector-rename = Name

# Accessible name of a swatch of the track colour.
inspector-swatch = Color { $hue }

# Accessible name of a device row.
inspector-device = Device { $name }

# Notes of a take.
inspector-take-notes =
    { $n ->
        [one] 1 note
       *[other] { $n } notes
    }

# Name of a take without a name (a hand-drawn clip's only take has none); `$n` counts from 1.
inspector-take = Take { $n }

# Accessible name of an input row of a bus.
inspector-input-row = Input { $name }, { $detail }

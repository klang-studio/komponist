## Track toggles M S R I (`widgets/toggle.rs`; Designsystem track toggle).

# Name and tooltip of the M toggle.
ui-toggle-mute = Mute

# Name and tooltip of the S toggle.
ui-toggle-solo = Solo

# Name and tooltip of the R toggle.
ui-toggle-arm = Record-Arm

# Name and tooltip of the I toggle.
ui-toggle-monitor = Monitoring

# Second tooltip line of M while another track's solo mutes it.
ui-toggle-mute-implied = stumm durch Solo

# Second tooltip line of S.
ui-toggle-solo-detail = Alt-Klick: nur diese Spur solo

# Same on macOS: ⌥ instead of Alt.
ui-toggle-solo-detail-mac = ⌥-Klick: nur diese Spur solo

# Second tooltip line of R.
ui-toggle-arm-detail = Alt-Klick exklusiv

# Same on macOS: ⌥ instead of Alt.
ui-toggle-arm-detail-mac = ⌥-Klick exklusiv

# Second tooltip line of I while the armed track monitors.
ui-toggle-monitor-implied = durch Record-Arm

# AccessKit value of an I toggle that is off but monitors because the track is armed.
ui-toggle-monitor-implied-value = überwacht durch Record-Arm

## Value menu of knobs and faders (`widgets/value_menu.rs`; owner decision 2026-11-15).

# Accessible name of the right-click menu.
ui-value-menu = Wert

# Menu entry: back to the default value.
ui-value-to-default = Auf Standard

# Shortcut column of "Auf Standard".
ui-value-to-default-shortcut = Doppelklick

# Menu entry: open the text entry.
ui-value-enter = Wert eingeben

## Number field and meters (`widgets/number_field.rs`, `meter.rs`, `device/meter_gr.rs`).

# Accessible name of the text entry of a number field, knob or fader; `$name` is the control's name.
ui-number-entry = { $name } Eingabe

# AccessKit value of a meter that clipped; `$db` is the formatted peak ("−0.5").
ui-meter-value-clipped = { $db } dB, Clip

# Accessible name of the compressor's In / GR / Out meter.
ui-meter-gr-name = Pegel und Gain Reduction

# AccessKit value of the GR meter; `$readout` is the formatted reduction ("−4.1").
ui-meter-gr-value = größte Reduktion { $readout } dB

# AccessKit description of the GR meter.
ui-meter-gr-hint = Klick setzt die Anzeige zurück

## FM display and pad grid (`widgets/device/fm_algorithm.rs`, `pad_grid.rs`).

# Accessible name of the FM algorithm display.
ui-fm-algorithm = Algorithmus { $n }

# Accessible name of a carrier operator.
ui-fm-carrier = Operator { $n }, Träger

# Accessible name of a modulator operator.
ui-fm-modulator = Operator { $n }, Modulator

# Accessible name of a pad of the pad grid.
ui-pad-name = Pad { $name }

# AccessKit description of a pad; `$note` is the note name ("C1").
ui-pad-note = Note { $note }

## Timeline and canvas names for AccessKit (`timeline/`; CLAUDE.md rule 12).

# Bar range of a clip (en dash).
a11y-bars = Takt { $first }–{ $last }

# One bar of a clip.
a11y-bar = Takt { $n }

# Accessible name of a clip; `$span` is `a11y-bars` or `a11y-bar`.
a11y-clip = Clip { $name }, { $span }

# Accessible name of a muted clip.
a11y-clip-muted = Clip { $name }, { $span }, stumm

# Accessible name of a track header.
a11y-track = Spur { $name }

# Accessible name of a track header with its instrument.
a11y-track-instrument = Spur { $name }, { $instrument }

# Accessible name of the ruler (value: the playhead position).
a11y-ruler = Lineal

# Accessible name of a note; `$pitch` is the note name ("C4"), `$pos` the position ("9.1.000").
a11y-note = Note { $pitch }, { $pos }

# Accessible name of a muted note.
a11y-note-muted = Note { $pitch }, { $pos }, stumm

## Graph widget texts: titles, handle and axis names (`widgets/device/graph/configs.rs`).

# Title and accessible name of the envelope graph.
ui-graph-envelope = Hüllkurve

# Title and accessible name of the EQ graph.
ui-graph-eq = Parametrischer EQ

# Title and accessible name of surf's wavetable graph.
ui-graph-wavetable = Wellentabelle

# Title and handle name of the filter graphs.
ui-graph-filter = Filter

# Title of the transfer and compressor graphs.
ui-graph-transfer = Kennlinie

# Title of the gain reduction history (the 4 s are the displayed span).
ui-graph-gr-history = Reduktion · 4 s

# Accessible name of the sample waveform graph.
ui-graph-waveform = Wellenform

# Accessible name of the XY graph and its handle.
ui-graph-xy = XY-Feld

# Title of the LFO shape graphs.
ui-graph-lfo-shape = LFO · Form

# Title of the oscillator signal graph.
ui-graph-signal = Signal

# Envelope handle.
ui-graph-attack = Attack

# Envelope handle.
ui-graph-decay = Decay

# Envelope handle.
ui-graph-sustain = Sustain

# Envelope handle.
ui-graph-release = Release

# Axis name of envelope times.
ui-graph-time = Zeit

# Wheel binding: bend of the envelope curve.
ui-graph-curve = Krümmung

# Y binding of the decay handle.
ui-graph-sustain-level = Sustain-Pegel

# Y binding of the sustain handle.
ui-graph-level = Pegel

# EQ band handle.
ui-graph-band = Band { $n }

# X binding of an EQ band.
ui-graph-frequency = Frequenz

# Y binding of an EQ band.
ui-graph-gain = Gain

# Wheel binding of an EQ band.
ui-graph-q = Q

# X binding of the filter handle.
ui-graph-cutoff = Cutoff

# Y binding of the filter handle.
ui-graph-resonance = Resonanz

# Compressor handle.
ui-graph-knee = Knie

# X binding of the knee handle.
ui-graph-threshold = Threshold

# Y binding of the knee handle.
ui-graph-ratio = Ratio

# Sample start handle.
ui-graph-sample-start = Sample-Start

# Sample end handle.
ui-graph-sample-end = Sample-Ende

# Binding of the sample start and end.
ui-graph-position = Position

# LFO shape point handle.
ui-graph-point = Punkt { $n }

# X binding of an LFO point.
ui-graph-phase = Phase

# Y binding of an LFO point.
ui-graph-value = Wert

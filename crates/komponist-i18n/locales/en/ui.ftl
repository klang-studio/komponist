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
ui-toggle-mute-implied = muted by solo

# Second tooltip line of S.
ui-toggle-solo-detail = Alt+click: solo this track only

# Same on macOS: ⌥ instead of Alt.
ui-toggle-solo-detail-mac = ⌥+click: solo this track only

# Second tooltip line of R.
ui-toggle-arm-detail = Alt+click: exclusive

# Same on macOS: ⌥ instead of Alt.
ui-toggle-arm-detail-mac = ⌥+click: exclusive

# Second tooltip line of I while the armed track monitors.
ui-toggle-monitor-implied = through Record-Arm

# AccessKit value of an I toggle that is off but monitors because the track is armed.
ui-toggle-monitor-implied-value = monitoring through Record-Arm

## Value menu of knobs and faders (`widgets/value_menu.rs`; owner decision 2026-11-15).

# Accessible name of the right-click menu.
ui-value-menu = Value

# Menu entry: back to the default value.
ui-value-to-default = Reset to Default

# Shortcut column of "Auf Standard".
ui-value-to-default-shortcut = Double-click

# Menu entry: open the text entry.
ui-value-enter = Enter Value

## Number field and meters (`widgets/number_field.rs`, `meter.rs`, `device/meter_gr.rs`).

# Accessible name of the text entry of a number field, knob or fader; `$name` is the control's name.
ui-number-entry = { $name } entry

# AccessKit value of a meter that clipped; `$db` is the formatted peak ("−0.5").
ui-meter-value-clipped = { $db } dB, clip

# Accessible name of the compressor's In / GR / Out meter.
ui-meter-gr-name = Level and gain reduction

# AccessKit value of the GR meter; `$readout` is the formatted reduction ("−4.1").
ui-meter-gr-value = maximum reduction { $readout } dB

# AccessKit description of the GR meter.
ui-meter-gr-hint = Click to reset the display

## FM display and pad grid (`widgets/device/fm_algorithm.rs`, `pad_grid.rs`).

# Accessible name of the FM algorithm display.
ui-fm-algorithm = Algorithm { $n }

# Accessible name of a carrier operator.
ui-fm-carrier = Operator { $n }, carrier

# Accessible name of a modulator operator.
ui-fm-modulator = Operator { $n }, modulator

# Accessible name of a pad of the pad grid.
ui-pad-name = Pad { $name }

# AccessKit description of a pad; `$note` is the note name ("C1").
ui-pad-note = Note { $note }

## Timeline and canvas names for AccessKit (`timeline/`; CLAUDE.md rule 12).

# Bar range of a clip (en dash).
a11y-bars = bars { $first }–{ $last }

# One bar of a clip.
a11y-bar = bar { $n }

# Accessible name of a clip; `$span` is `a11y-bars` or `a11y-bar`.
a11y-clip = Clip { $name }, { $span }

# Accessible name of a muted clip.
a11y-clip-muted = Clip { $name }, { $span }, muted

# Accessible name of a track header.
a11y-track = Track { $name }

# Accessible name of a track header with its instrument.
a11y-track-instrument = Track { $name }, { $instrument }

# Accessible name of the ruler (value: the playhead position).
a11y-ruler = Ruler

# Accessible name of a note; `$pitch` is the note name ("C4"), `$pos` the position ("9.1.000").
a11y-note = Note { $pitch }, { $pos }

# Accessible name of a muted note.
a11y-note-muted = Note { $pitch }, { $pos }, muted

## Graph widget texts: titles, handle and axis names (`widgets/device/graph/configs.rs`).

# Title and accessible name of the envelope graph.
ui-graph-envelope = Envelope

# Title and accessible name of the EQ graph.
ui-graph-eq = Parametric EQ

# Title and handle name of the filter graphs.
ui-graph-filter = Filter

# Title of the transfer and compressor graphs.
ui-graph-transfer = Transfer curve

# Title of the gain reduction history (the 4 s are the displayed span).
ui-graph-gr-history = Reduction · 4 s

# Accessible name of the sample waveform graph.
ui-graph-waveform = Waveform

# Accessible name of the XY graph and its handle.
ui-graph-xy = XY pad

# Title of the LFO shape graphs.
ui-graph-lfo-shape = LFO · Shape

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
ui-graph-time = Time

# Wheel binding: bend of the envelope curve.
ui-graph-curve = Curve

# Y binding of the decay handle.
ui-graph-sustain-level = Sustain level

# Y binding of the sustain handle.
ui-graph-level = Level

# EQ band handle.
ui-graph-band = Band { $n }

# X binding of an EQ band.
ui-graph-frequency = Frequency

# Y binding of an EQ band.
ui-graph-gain = Gain

# Wheel binding of an EQ band.
ui-graph-q = Q

# X binding of the filter handle.
ui-graph-cutoff = Cutoff

# Y binding of the filter handle.
ui-graph-resonance = Resonance

# Compressor handle.
ui-graph-knee = Knee

# X binding of the knee handle.
ui-graph-threshold = Threshold

# Y binding of the knee handle.
ui-graph-ratio = Ratio

# Sample start handle.
ui-graph-sample-start = Sample start

# Sample end handle.
ui-graph-sample-end = Sample end

# Binding of the sample start and end.
ui-graph-position = Position

# LFO shape point handle.
ui-graph-point = Point { $n }

# X binding of an LFO point.
ui-graph-phase = Phase

# Y binding of an LFO point.
ui-graph-value = Value

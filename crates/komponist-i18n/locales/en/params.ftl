## Value texts (`komponist_model::params::ValueTexts`): switch states and the percent suffix.

# A switch that is on.
value-on = On

# A switch that is off, and the lowest value of a time that means off.
value-off = Off

# Suffix of a percent value (German with a space, English without).
value-percent = %

# hexa algorithm 0: free routing.
value-free = Free

# Sampler end point at its maximum: plays to the end of the sample.
value-end = End

# volt glide and fade at their lowest value (lower case in the German UI).
value-off-lower = off

# volt octave offsets: the unit after the number.
value-unit-octave = oct

## Parameter names shared by several devices.

# Attack time.
param-attack = Attack

# Decay time.
param-decay = Decay

# Sustain level.
param-sustain = Sustain

# Release time.
param-release = Release

# Feedback.
param-feedback = Feedback

# Dry/wet mix.
param-mix = Mix

# Saturation.
param-drive = Drive

# Portamento time.
param-glide = Glide

# Stereo spread.
param-spread = Spread

# Tuning in semitones.
param-tune = Tune

# Unison detune.
param-detune = Detune

# Unison voices.
param-unison = Unison

# Mono instead of poly.
param-mono = Mono

# Master volume.
param-volume = Volume

# Stereo width.
param-width = Width

# Filter cutoff.
param-cutoff = Cutoff

# Filter resonance.
param-resonance = Resonance

# Frequency ratio (hexa) or compressor ratio.
param-ratio = Ratio

# Lookahead switch.
param-lookahead = Lookahead

# Rate of an LFO or the arpeggiator.
param-rate = Rate

# Gate length of the arpeggiator.
param-gate = Gate

# A mode choice.
param-mode = Mode

# A type choice.
param-type = Type

# volt key tracking and per-key LFO restart.
param-key = Key

# Tempo sync switch.
param-sync = Sync

# Fade-in time.
param-fade = Fade

# Velocity sensitivity.
param-velocity = Velocity

# Pan of a Sampler pad.
param-pan = Pan

# Mute of a Sampler pad.
param-mute = Mute

# Solo of a Sampler pad.
param-solo = Solo

# Choke group of a Sampler pad.
param-choke = Choke

# volt sub oscillator level.
param-sub = Sub

# volt analog drift.
param-analog = Analog

# volt pitch-bend range.
param-bend = Bend

# Compressor threshold.
param-threshold = Threshold

# Compressor makeup gain.
param-makeup = Makeup

# Compressor stereo link.
param-link = Link

# Limiter ceiling.
param-ceiling = Ceiling

# Reverb diffusion.
param-diffusion = Diffusion

# Delay time.
param-time = Time

# Reverb pre-delay.
param-pre-delay = Pre-Delay

# Delay low cut.
param-low-cut = Low Cut

# Delay high cut.
param-high-cut = High Cut

# volt LFO unipolar switch.
param-lfo-uni = Uni

## hexa: macros, globals and the operator parameters (`device.rs`, `macros.rs`).

# hexa macro: fixed algorithm 1–32, 0 = free routing.
param-hexa-algorithm = Algorithm

# hexa macro: frequency ratio of operator 2.
param-hexa-ratio2 = Ratio 2

# hexa macro: level of operator 2.
param-hexa-index = Index

# hexa card: the level of an operator; the number is appended („Op 1“).
param-hexa-op-level = Op

# hexa card: envelope delay of an operator; the number is appended.
param-hexa-op-delay = Delay

# hexa card: envelope hold of an operator; the number is appended.
param-hexa-op-hold = Hold

# hexa card: envelope loop of an operator; the number is appended.
param-hexa-op-loop = Loop

# hexa card: attack curve of an operator; the number is appended.
param-hexa-op-attack-curve = Curve A

# hexa card: decay curve of an operator; the number is appended.
param-hexa-op-decay-curve = Curve D

# hexa card: release curve of an operator; the number is appended.
param-hexa-op-release-curve = Curve R

# hexa card: level scaling above the break point; the number is appended.
param-hexa-op-key-scale = Key Scale

# hexa card: whether the operator is heard (a carrier); the number is appended.
param-hexa-op-output = Carrier

# hexa card, page 1.
page-hexa-operators = Operators

# hexa card, page 2.
page-hexa-envelopes = Envelopes

# hexa card, page 3 (empty for now).
page-hexa-lfo = LFO

# hexa card, page 4 (empty for now).
page-hexa-global = Global

# Title of hexa's envelope graph; `$op` is the placeholder of the selected operator.
graph-title-hexa-amp = Op { $op } · Amplitude

## EQ (`internal/eq.rs`).

# EQ output gain.
param-eq-out = Output

# EQ: band switch; `$n` is the band number.
param-eq-band-on = Band { $n } On

# EQ: filter type of a band; `$n` is the band number.
param-eq-band-type = Type { $n }

# EQ: frequency of a band; `$n` is the band number.
param-eq-band-freq = Freq { $n }

# EQ: gain of a band; `$n` is the band number.
param-eq-band-gain = Gain { $n }

# EQ: Q of a band; `$n` is the band number.
param-eq-band-q = Q { $n }

# EQ: live spectrum switch.
param-eq-analyse = Analysis

# EQ band type: low cut.
choice-eq-type-low-cut = LC

# EQ band type: low shelf.
choice-eq-type-low-shelf = LS

# EQ band type: bell (German „Glocke“).
choice-eq-type-bell = Bell

# EQ band type: high shelf.
choice-eq-type-high-shelf = HS

# EQ band type: high cut.
choice-eq-type-high-cut = HC

## Delay (`internal/delay.rs`). The note values (1/8, 1/4T …) are notation and stay in code.

# Delay: ping-pong switch.
param-delay-ping-pong = Ping-Pong

# Delay: note value of the tempo sync.
param-delay-note = Note Value

## Hall / Reverb (`internal/hall.rs`).

# Reverb: room size in metres.
param-hall-size = Size

# Reverb: damping frequency.
param-hall-damping = Damping

# Reverb: modulation depth (short, a knob label).
param-hall-modulation = Mod.

# Reverb: the room character.
param-hall-room = Space

# Reverb: freeze switch.
param-hall-freeze = Freeze

# Reverb: wet signal only.
param-hall-wet-only = Wet Only

# Reverb room: smallest.
choice-hall-room-chamber = Room

# Reverb room: medium.
choice-hall-room-hall = Hall

# Reverb room: largest.
choice-hall-room-church = Church

## Limiter (`internal/limiter.rs`).

# Limiter: input gain.
param-limiter-gain = Input

## presse (`internal/presse.rs`).

# Compressor: knee width.
param-presse-knee = Knee

# Compressor: level detector.
param-presse-detector = Detector

# Compressor: sidechain high-pass.
param-presse-sc-hpf = SC HP

# Compressor: listen to the sidechain.
param-presse-sc-listen = SC Listen

# Compressor: auto release (short).
param-presse-auto-release = Auto Rel.

# Compressor: auto makeup (short).
param-presse-auto-makeup = Auto Mkp.

## Arpeggiator (`internal/arpeggiator.rs`). The rates (1/8, 1/16T …) are notation and stay in code.

# Arpeggiator: pattern mode.
param-arp-pattern = Pattern

# Arpeggiator: octave count.
param-arp-octaves = Octaves

# Arpeggiator: latch the held keys.
param-arp-hold = Hold

# Arpeggiator pattern: upwards.
choice-arp-up = Up

# Arpeggiator pattern: downwards.
choice-arp-down = Down

# Arpeggiator pattern: up and down.
choice-arp-up-down = Up/Down

# Arpeggiator pattern: random order.
choice-arp-random = Random

# Arpeggiator pattern: in the order the keys went down.
choice-arp-as-played = As Played

## Tonleiter / Scale (`internal/tonleiter.rs`). Note names C D E F G A B are the same in every language.

# Scale: the root note.
param-scale-root = Root

# Scale: the scale type.
param-scale-scale = Scale

# Scale type.
choice-scale-major = Major

# Scale type.
choice-scale-minor = Minor

# Scale type.
choice-scale-harmonic-minor = Harmonic Minor

# Scale type.
choice-scale-melodic-minor = Melodic Minor

# Scale type.
choice-scale-dorian = Dorian

# Scale type.
choice-scale-phrygian = Phrygian

# Scale type.
choice-scale-lydian = Lydian

# Scale type.
choice-scale-mixolydian = Mixolydian

# Scale type.
choice-scale-locrian = Locrian

# Scale type.
choice-scale-pentatonic-major = Pentatonic Major

# Scale type.
choice-scale-pentatonic-minor = Pentatonic Minor

# Scale type.
choice-scale-blues = Blues

# Scale type.
choice-scale-whole-tone = Whole Tone

# Scale type.
choice-scale-chromatic = Chromatic

# Scale: a key outside the scale becomes the nearest allowed one.
choice-scale-mode-nearest = Nearest

# Scale: a key outside the scale moves up.
choice-scale-mode-up = Up

# Scale: a key outside the scale moves down.
choice-scale-mode-down = Down

# Scale: a key outside the scale is dropped.
choice-scale-mode-filter = Filter

## Sampler (`sampler/mod.rs`); the number of the pad is appended where the name says `$pad`.

# Sampler: tuning of the whole kit.
param-sampler-kit-tune = Kit Tune

# Sampler: the selected pad follows the MIDI input.
param-sampler-follow = Pad follows input

# Sampler: sample start of pad `$pad`.
param-sampler-start = Start { $pad }

# Sampler: sample end of pad `$pad`.
param-sampler-end = End { $pad }

# Sampler: playback mode of pad `$pad`.
param-sampler-mode = Mode { $pad }

# Sampler: tuning of pad `$pad`.
param-sampler-tune = Tune { $pad }

# Sampler: decay of pad `$pad`.
param-sampler-decay = Decay { $pad }

# Sampler: drive of pad `$pad`.
param-sampler-drive = Drive { $pad }

# Sampler: pan of pad `$pad`.
param-sampler-pan = Pan { $pad }

# Sampler: level of pad `$pad`.
param-sampler-level = Level { $pad }

# Sampler: mute of pad `$pad`.
param-sampler-mute = Mute { $pad }

# Sampler: solo of pad `$pad`.
param-sampler-solo = Solo { $pad }

# Sampler: choke group of pad `$pad`.
param-sampler-choke = Choke { $pad }

# Sampler pad mode: plays to the end.
choice-sampler-mode-one-shot = 1×

# Sampler pad mode: plays while the key is held.
choice-sampler-mode-gate = Gate

# Title of the Sampler's waveform graph; `$pad` is the placeholder of the selected pad.
graph-title-sampler-pad = Pad { $pad }

## volt (`internal/volt/`).

# volt: waveform of oscillator A.
param-volt-osc-a = Osc A

# volt: waveform of oscillator B.
param-volt-osc-b = Osc B

# volt: pulse width of oscillator A.
param-volt-width-a = Width A

# volt: pulse width of oscillator B.
param-volt-width-b = Width B

# volt: octave of oscillator B.
param-volt-b-oct = B Oct.

# volt: fine tune of oscillator B.
param-volt-b-fine = B Fine

# volt: balance of oscillator A and B.
param-volt-osc-mix = Mix A/B

# volt: noise level.
param-volt-noise = Noise

# volt: high-pass frequency.
param-volt-hp = HP

# volt: how far the filter envelope moves the cutoff.
param-volt-env-amount = Env Amt.

# volt: LFO waveform.
param-volt-lfo-shape = Shape

# volt: LFO divider when synced to the tempo.
param-volt-lfo-div = Divider

# volt matrix: source of slot `$n`.
param-volt-source = Source { $n }

# volt matrix: destination of slot `$n`.
param-volt-target = Target { $n }

# volt matrix: amount of slot `$n`.
param-volt-amount = Amount { $n }

# volt: velocity to filter envelope amount.
param-volt-vel-filter = Vel → Env

# volt: mod wheel to vibrato.
param-volt-wheel-vib = Wheel → Vib.

# volt: channel pressure to cutoff.
param-volt-at-cut = Press → Cut

# volt: glide only between legato notes.
param-volt-glide-legato = Glide Legato

# volt: number of voices.
param-volt-voices = Voices

# volt: octave shift.
param-volt-octave = Oct.

# volt: envelope attack (one letter).
param-volt-envelope-a = A

# volt: envelope decay (one letter).
param-volt-envelope-d = D

# volt: envelope sustain (one letter).
param-volt-envelope-s = S

# volt: envelope release (one letter).
param-volt-envelope-r = R

# Waveform.
choice-volt-wave-sine = Sine

# Waveform.
choice-volt-wave-triangle = Triangle

# Waveform.
choice-volt-wave-saw = Saw

# Waveform.
choice-volt-wave-square = Square

# LFO shape: sample and hold.
choice-volt-lfo-random = Random

# LFO shape: smoothed random.
choice-volt-lfo-smooth = Smooth

# LFO divider.
choice-volt-div-4-bars = 4 Bars

# LFO divider.
choice-volt-div-2-bars = 2 Bars

# LFO divider.
choice-volt-div-1-bar = 1 Bar

# volt matrix source: the third envelope.
choice-volt-source-env3 = Env 3

# volt matrix source: mod wheel.
choice-volt-source-wheel = Mod Wheel

# volt matrix source: channel pressure.
choice-volt-source-pressure = Pressure

# volt matrix destination.
choice-volt-dest-pitch = Pitch

# volt matrix destination.
choice-volt-dest-pitch-b = Osc B Pitch

# volt matrix destination.
choice-volt-dest-lfo1-rate = LFO 1 Rate

# volt matrix destination.
choice-volt-dest-lfo2-rate = LFO 2 Rate

# volt card, page 1.
page-volt-sound = Sound

# volt card, page 2.
page-volt-envelopes = Envelopes

# volt card, page 3.
page-volt-matrix = Matrix

# Name above the envelope switch on volt's page 2.
label-volt-envelope = Envelope

# Name above the LFO switch on volt's page 2.
label-volt-lfo = LFO

# Title of volt's envelope graph; `$name` is the name of the chosen envelope.
graph-title-volt-envelope = { $name } Env.

# General sampler
param-general-sampler-start = Start
param-general-sampler-end = End
param-general-sampler-fade-in = Fade in
param-general-sampler-fade-out = Fade out
param-general-sampler-mode = Playback
param-general-sampler-direction = Direction
param-general-sampler-forward = Forward
param-general-sampler-reverse = Reverse
param-general-sampler-root = Root note
param-general-sampler-slices = Slices
param-general-sampler-coarse = Coarse
param-general-sampler-fine = Fine
param-general-sampler-sync = Tempo sync
param-general-sampler-source-bpm = Source tempo (BPM)
param-general-sampler-gain = Gain
param-general-sampler-pan = Pan
param-general-sampler-attack = Attack
param-general-sampler-decay = Decay
param-general-sampler-sustain = Sustain
param-general-sampler-release = Release
param-general-sampler-filter = Filter
param-general-sampler-off = Off
param-general-sampler-low-pass = Low-pass
param-general-sampler-high-pass = High-pass
param-general-sampler-band-pass = Band-pass
param-general-sampler-cutoff = Cutoff
param-general-sampler-resonance = Resonance
param-general-sampler-filter-env = Env Amt.
param-general-sampler-filter-decay = Filter Dec.
param-general-sampler-lfo-shape = LFO shape
param-general-sampler-sine = Sine
param-general-sampler-triangle = Triangle
param-general-sampler-saw-up = Rising saw
param-general-sampler-saw-down = Falling saw
param-general-sampler-square = Square
param-general-sampler-sample-hold = Sample & hold
param-general-sampler-lfo-rate = LFO rate
param-general-sampler-lfo-target = LFO target
param-general-sampler-lfo-depth = LFO depth
param-general-sampler-velocity-target = Velocity target
param-general-sampler-velocity-depth = Velocity depth
param-general-sampler-pitch = Pitch

## surf (`builtin-devices/surf/`), the wavetable instrument (#107). Oscillator B's octave and fine tune are volt's.

# surf: the wavetable of oscillator A.
param-surf-table-a = Table A

# surf: the wavetable of oscillator B.
param-surf-table-b = Table B

# surf: the position in the table of oscillator A; also a matrix destination.
param-surf-position-a = Position A

# surf: the position in the table of oscillator B; also a matrix destination.
param-surf-position-b = Position B

# surf: octave of oscillator A.
param-surf-a-oct = A Oct.

# surf: fine tune of oscillator A.
param-surf-a-fine = A Fine

# surf: the name above the switch between oscillator A and B.
label-surf-osc = Oscillator

# surf: factory wavetable.
choice-surf-table-basic = Basic Shapes

# surf: factory wavetable.
choice-surf-table-pulse = Pulse Width

# surf: factory wavetable.
choice-surf-table-sync = Sync

# surf: factory wavetable.
choice-surf-table-harmonics = Harmonics

# surf: factory wavetable.
choice-surf-table-odd-even = Odd–Even

# surf: factory wavetable.
choice-surf-table-organ = Organ

# surf: factory wavetable.
choice-surf-table-vowels = Vowels

# surf: factory wavetable.
choice-surf-table-choir = Choir

# surf: factory wavetable.
choice-surf-table-fm = FM

# surf: factory wavetable.
choice-surf-table-fold = Fold

# surf: factory wavetable.
choice-surf-table-bits = Bits

# surf: factory wavetable.
choice-surf-table-comb = Comb

# surf: factory wavetable.
choice-surf-table-glass = Glass

# surf: factory wavetable.
choice-surf-table-hollow = Hollow

# surf: factory wavetable.
choice-surf-table-resonance = Resonance

# surf: factory wavetable.
choice-surf-table-random = Random

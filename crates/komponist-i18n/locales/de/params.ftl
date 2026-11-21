## Value texts (`komponist_model::params::ValueTexts`): switch states and the percent suffix.

# A switch that is on.
value-on = Ein

# A switch that is off, and the lowest value of a time that means off.
value-off = Aus

# Suffix of a percent value (German with a space, English without).
value-percent = { " " }%

# hexa algorithm 0: free routing.
value-free = Frei

# Sampler end point at its maximum: plays to the end of the sample.
value-end = Ende

# volt glide and fade at their lowest value (lower case in the German UI).
value-off-lower = aus

# volt octave offsets: the unit after the number.
value-unit-octave = Okt.

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
param-volume = Lautstärke

# Stereo width.
param-width = Breite

# Filter cutoff.
param-cutoff = Cutoff

# Filter resonance.
param-resonance = Resonanz

# Frequency ratio (hexa) or compressor ratio.
param-ratio = Ratio

# Lookahead switch.
param-lookahead = Lookahead

# Rate of an LFO or the arpeggiator.
param-rate = Rate

# Gate length of the arpeggiator.
param-gate = Gate

# A mode choice.
param-mode = Modus

# A type choice.
param-type = Typ

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
param-time = Zeit

# Reverb pre-delay.
param-pre-delay = Pre-Delay

# Delay low cut.
param-low-cut = Low Cut

# Delay high cut.
param-high-cut = High Cut

# volt LFO unipolar switch.
param-lfo-uni = Uni

## hexa: macros, globals and the operator parameters (`device.rs`, `macros.rs`).

param-hexa-global-page = Global
param-hexa-operator-page = Op { $first } / { $second }

# hexa macro: fixed algorithm 1–32, 0 = free routing.
param-hexa-algorithm = Algorithmus

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
param-hexa-op-attack-curve = Kurve A

# hexa card: decay curve of an operator; the number is appended.
param-hexa-op-decay-curve = Kurve D

# hexa card: release curve of an operator; the number is appended.
param-hexa-op-release-curve = Kurve R

# hexa card: level scaling above the break point; the number is appended.
param-hexa-op-key-scale = Key Scale

# hexa card: whether the operator is heard (a carrier); the number is appended.
param-hexa-op-output = Träger

# hexa card, page 1.
page-hexa-operators = Operatoren

# hexa card, page 2.
page-hexa-envelopes = Hüllkurven

# hexa card, page 3 (empty for now).
page-hexa-lfo = LFO

# hexa card, page 4 (empty for now).
page-hexa-global = Global

# Title of hexa's envelope graph; `$op` is the placeholder of the selected operator.
graph-title-hexa-amp = Op { $op } · Amplitude

## EQ (`internal/eq.rs`).

# EQ output gain.
param-eq-out = Ausgang

# EQ: band switch; `$n` is the band number.
param-eq-band-on = Band { $n } an

# EQ: filter type of a band; `$n` is the band number.
param-eq-band-type = Typ { $n }

# EQ: frequency of a band; `$n` is the band number.
param-eq-band-freq = Frequenz { $n }

# EQ: gain of a band; `$n` is the band number.
param-eq-band-gain = Gain { $n }

# EQ: Q of a band; `$n` is the band number.
param-eq-band-q = Q { $n }

# EQ: live spectrum switch.
param-eq-analyse = Analyse

# EQ band type: low cut.
choice-eq-type-low-cut = LC

# EQ band type: low shelf.
choice-eq-type-low-shelf = LS

# EQ band type: bell (German „Glocke“).
choice-eq-type-bell = Gl.

# EQ band type: high shelf.
choice-eq-type-high-shelf = HS

# EQ band type: high cut.
choice-eq-type-high-cut = HC

## Delay (`internal/delay.rs`). The note values (1/8, 1/4T …) are notation and stay in code.

# Delay: ping-pong switch.
param-delay-ping-pong = Ping-Pong

# Delay: note value of the tempo sync.
param-delay-note = Notenwert

## Hall / Reverb (`internal/hall.rs`).

# Reverb: room size in metres.
param-hall-size = Größe

# Reverb: damping frequency.
param-hall-damping = Dämpfung

# Reverb: modulation depth (short, a knob label).
param-hall-modulation = Modul.

# Reverb: the room character.
param-hall-room = Raum

# Reverb: freeze switch.
param-hall-freeze = Einfrieren

# Reverb: wet signal only.
param-hall-wet-only = Nur Hall

# Reverb room: smallest.
choice-hall-room-chamber = Zimmer

# Reverb room: medium.
choice-hall-room-hall = Halle

# Reverb room: largest.
choice-hall-room-church = Kirche

## Limiter (`internal/limiter.rs`).

# Limiter: input gain.
param-limiter-gain = Eingang

## presse (`internal/presse.rs`).

# Compressor: knee width.
param-presse-knee = Knie

# Compressor: level detector.
param-presse-detector = Detektor

# Compressor: sidechain high-pass.
param-presse-sc-hpf = SC-HP

# Compressor: listen to the sidechain.
param-presse-sc-listen = SC hören

# Compressor: auto release (short).
param-presse-auto-release = Auto-Rel.

# Compressor: auto makeup (short).
param-presse-auto-makeup = Auto-Mkp.

## Arpeggiator (`internal/arpeggiator.rs`). The rates (1/8, 1/16T …) are notation and stay in code.

# Arpeggiator: pattern mode.
param-arp-pattern = Muster

# Arpeggiator: octave count.
param-arp-octaves = Oktaven

# Arpeggiator: latch the held keys.
param-arp-hold = Halten

# Arpeggiator pattern: upwards.
choice-arp-up = Auf

# Arpeggiator pattern: downwards.
choice-arp-down = Ab

# Arpeggiator pattern: up and down.
choice-arp-up-down = Auf/Ab

# Arpeggiator pattern: random order.
choice-arp-random = Zufall

# Arpeggiator pattern: in the order the keys went down.
choice-arp-as-played = Gespielt

## Tonleiter / Scale (`internal/tonleiter.rs`). Note names C D E F G A B are the same in every language.

# Scale: the root note.
param-scale-root = Grundton

# Scale: the scale type.
param-scale-scale = Tonleiter

# Scale type.
choice-scale-major = Dur

# Scale type.
choice-scale-minor = Moll

# Scale type.
choice-scale-harmonic-minor = Harmonisch Moll

# Scale type.
choice-scale-melodic-minor = Melodisch Moll

# Scale type.
choice-scale-dorian = Dorisch

# Scale type.
choice-scale-phrygian = Phrygisch

# Scale type.
choice-scale-lydian = Lydisch

# Scale type.
choice-scale-mixolydian = Mixolydisch

# Scale type.
choice-scale-locrian = Lokrisch

# Scale type.
choice-scale-pentatonic-major = Pentatonik Dur

# Scale type.
choice-scale-pentatonic-minor = Pentatonik Moll

# Scale type.
choice-scale-blues = Blues

# Scale type.
choice-scale-whole-tone = Ganzton

# Scale type.
choice-scale-chromatic = Chromatisch

# Scale: a key outside the scale becomes the nearest allowed one.
choice-scale-mode-nearest = Nächster Ton

# Scale: a key outside the scale moves up.
choice-scale-mode-up = Aufwärts

# Scale: a key outside the scale moves down.
choice-scale-mode-down = Abwärts

# Scale: a key outside the scale is dropped.
choice-scale-mode-filter = Filtern

## Sampler (`sampler/mod.rs`); the number of the pad is appended where the name says `$pad`.

# Sampler: tuning of the whole kit.
param-sampler-kit-tune = Kit-Tune

# Sampler: the selected pad follows the MIDI input.
param-sampler-follow = Pad folgt Eingang

# Sampler: sample start of pad `$pad`.
param-sampler-start = Start { $pad }

# Sampler: sample end of pad `$pad`.
param-sampler-end = Ende { $pad }

# Sampler: playback mode of pad `$pad`.
param-sampler-mode = Modus { $pad }

# Sampler: tuning of pad `$pad`.
param-sampler-tune = Tune { $pad }

# Sampler: decay of pad `$pad`.
param-sampler-decay = Decay { $pad }

# Sampler: drive of pad `$pad`.
param-sampler-drive = Drive { $pad }

# Sampler: pan of pad `$pad`.
param-sampler-pan = Pan { $pad }

# Sampler: level of pad `$pad`.
param-sampler-level = Pegel { $pad }

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
param-volt-osc-a = Osz A

# volt: waveform of oscillator B.
param-volt-osc-b = Osz B

# volt: pulse width of oscillator A.
param-volt-width-a = Breite A

# volt: pulse width of oscillator B.
param-volt-width-b = Breite B

# volt: octave of oscillator B.
param-volt-b-oct = B Okt.

# volt: fine tune of oscillator B.
param-volt-b-fine = B Fein

# volt: balance of oscillator A and B.
param-volt-osc-mix = Mix A/B

# volt: noise level.
param-volt-noise = Rauschen

# volt: high-pass frequency.
param-volt-hp = HP

# volt: how far the filter envelope moves the cutoff.
param-volt-env-amount = Hk. Menge

# volt: LFO waveform.
param-volt-lfo-shape = Form

# volt: LFO divider when synced to the tempo.
param-volt-lfo-div = Teiler

# volt matrix: source of slot `$n`.
param-volt-source = Quelle { $n }

# volt matrix: destination of slot `$n`.
param-volt-target = Ziel { $n }

# volt matrix: amount of slot `$n`.
param-volt-amount = Menge { $n }

# volt: velocity to filter envelope amount.
param-volt-vel-filter = Vel → Hk.

# volt: mod wheel to vibrato.
param-volt-wheel-vib = Rad → Vibr.

# volt: channel pressure to cutoff.
param-volt-at-cut = Druck→Cut

# volt: glide only between legato notes.
param-volt-glide-legato = Glide Legato

# volt: number of voices.
param-volt-voices = Stimmen

# volt: octave shift.
param-volt-octave = Okt.

# volt: envelope attack (one letter).
param-volt-envelope-a = A

# volt: envelope decay (one letter).
param-volt-envelope-d = D

# volt: envelope sustain (one letter).
param-volt-envelope-s = S

# volt: envelope release (one letter).
param-volt-envelope-r = R

# Waveform.
choice-volt-wave-sine = Sinus

# Waveform.
choice-volt-wave-triangle = Dreieck

# Waveform.
choice-volt-wave-saw = Säge

# Waveform.
choice-volt-wave-square = Rechteck

# LFO shape: sample and hold.
choice-volt-lfo-random = Zufall

# LFO shape: smoothed random.
choice-volt-lfo-smooth = Glatt

# LFO divider.
choice-volt-div-4-bars = 4 Takte

# LFO divider.
choice-volt-div-2-bars = 2 Takte

# LFO divider.
choice-volt-div-1-bar = 1 Takt

# volt matrix source: the third envelope.
choice-volt-source-env3 = Hk. 3

# volt matrix source: mod wheel.
choice-volt-source-wheel = Modrad

# volt matrix source: channel pressure.
choice-volt-source-pressure = Druck

# volt matrix destination.
choice-volt-dest-pitch = Tonhöhe

# volt matrix destination.
choice-volt-dest-pitch-b = Osz B Tonhöhe

# volt matrix destination.
choice-volt-dest-lfo1-rate = LFO 1 Rate

# volt matrix destination.
choice-volt-dest-lfo2-rate = LFO 2 Rate

# volt card, page 1.
page-volt-sound = Klang

# volt card, page 2.
page-volt-envelopes = Hüllkurven

# volt card, page 3.
page-volt-matrix = Matrix

# Name above the envelope switch on volt's page 2.
label-volt-envelope = Hüllkurve

# Name above the LFO switch on volt's page 2.
label-volt-lfo = LFO

# Title of volt's envelope graph; `$name` is the name of the chosen envelope.
graph-title-volt-envelope = { $name }-Hk.

# General sampler
param-general-sampler-start = Start
param-general-sampler-end = Ende
param-general-sampler-fade-in = Einblendung
param-general-sampler-fade-out = Ausblendung
param-general-sampler-mode = Wiedergabe
param-general-sampler-direction = Richtung
param-general-sampler-forward = Vorwärts
param-general-sampler-reverse = Rückwärts
param-general-sampler-root = Grundton
param-general-sampler-slices = Slices
param-general-sampler-coarse = Grob
param-general-sampler-fine = Fein
param-general-sampler-sync = Tempo-Sync
param-general-sampler-source-bpm = Quelltempo (BPM)
param-general-sampler-gain = Pegel
param-general-sampler-pan = Panorama
param-general-sampler-attack = Attack
param-general-sampler-decay = Decay
param-general-sampler-sustain = Sustain
param-general-sampler-release = Release
param-general-sampler-filter = Filter
param-general-sampler-off = Aus
param-general-sampler-low-pass = Tiefpass
param-general-sampler-high-pass = Hochpass
param-general-sampler-band-pass = Bandpass
param-general-sampler-cutoff = Grenzfrequenz
param-general-sampler-resonance = Resonanz
param-general-sampler-filter-env = Hüllkurve
param-general-sampler-filter-decay = Filterabkl.
param-general-sampler-lfo-shape = LFO-Form
param-general-sampler-sine = Sinus
param-general-sampler-triangle = Dreieck
param-general-sampler-saw-up = Aufsteigender Sägezahn
param-general-sampler-saw-down = Absteigender Sägezahn
param-general-sampler-square = Rechteck
param-general-sampler-sample-hold = Sample & Hold
param-general-sampler-lfo-rate = LFO-Frequenz
param-general-sampler-lfo-target = LFO-Ziel
param-general-sampler-lfo-depth = LFO-Stärke
param-general-sampler-velocity-target = Velocity-Ziel
param-general-sampler-velocity-depth = Velocity-Stärke
param-general-sampler-pitch = Tonhöhe

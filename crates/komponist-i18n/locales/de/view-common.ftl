## Texts that several views share: the grid dropdown, snap and zoom buttons, the "zu spät" indicator (`views/grid.rs`, `views/late.rs`), the compile diagnostics (`diagnostics.rs`).

# Accessible name and label of the grid dropdown (arrangement header, piano roll toolbar).
view-grid = Raster

# Snap toggle (magnet icon) of the arrangement header and the piano roll toolbar.
view-snap = Snap

# The "−" of the arrangement header and the piano roll toolbar.
view-zoom-out = Herauszoomen

# The "+" of the arrangement header and the piano roll toolbar.
view-zoom-in = Hineinzoomen

## Grid dropdown entries

# Grid of one bar.
grid-bar = 1 Takt

# Grid of a half note.
grid-half = 1/2

# Grid of a quarter note.
grid-quarter = 1/4

# Grid of an eighth note.
grid-eighth = 1/8

# Grid of a sixteenth note.
grid-sixteenth = 1/16

# Grid of a thirty-second note.
grid-thirty-second = 1/32

# Grid of a quarter-note triplet.
grid-quarter-triplet = 1/4 Triole

# Grid of an eighth-note triplet.
grid-eighth-triplet = 1/8 Triole

# Grid of a sixteenth-note triplet.
grid-sixteenth-triplet = 1/16 Triole

## Late indicator

# The indicator of a device whose blocks recently missed their deadline (not in the design).
late-label = zu spät

# Tooltip of the late indicator.
late-blocks =
    { $n ->
        [one] 1 Block zu spät
       *[other] { $n } Blöcke zu spät
    }

## Compile diagnostics: tracks and devices the engine leaves out (#92; `diagnostics.rs`, not in the design)

# Status of a track or device the engine leaves out: the inspector's device row and track section.
not-played-label = spielt nicht

# Tooltip detail of a track or device the engine leaves out; `$reason` is one of the `not-played-reason-*` texts.
not-played-detail = Spielt nicht: { $reason }

# Reason: the track comes after the most tracks the engine holds.
not-played-reason-track-limit = mehr als { $limit } Spuren

# Reason: no processor slot is left (the most devices the engine holds).
not-played-reason-slot-limit = mehr als { $limit } Geräte

# Reason: more MIDI effects in front of an instrument than the engine runs.
not-played-reason-midi-effect-limit = mehr als { $limit } MIDI-Effekte vor dem Instrument

# Reason: the project file holds this track or device id twice (the later one is left out).
not-played-reason-duplicate-id = ID doppelt in der Projektdatei

# Reason: a MIDI effect behind an instrument or audio effect.
not-played-reason-misplaced-midi-effect = MIDI-Effekt nicht am Anfang der Kette

# Reason: a second instrument, or an instrument behind an audio effect.
not-played-reason-misplaced-instrument = Instrument nicht direkt hinter den MIDI-Effekten

# Reason: an instrument or MIDI effect on an audio track, a bus or the master.
not-played-reason-wrong-chain = Instrumente und MIDI-Effekte nur auf MIDI-Spuren

# Status bar and export summary: tracks the engine leaves out, with the reason (a `not-played-reason-*` text).
not-played-tracks =
    { $count ->
        [one] 1 Spur spielt nicht: { $reason }
       *[other] { $count } Spuren spielen nicht: { $reason }
    }

# Status bar and export summary: devices the engine leaves out, with the reason.
not-played-devices =
    { $count ->
        [one] 1 Gerät spielt nicht: { $reason }
       *[other] { $count } Geräte spielen nicht: { $reason }
    }

## Track hue names (accessible names and tooltips of the colour swatches, in the order of `komponist_tokens::track::HUES`)

# Track hue 1.
hue-red = Rot

# Track hue 2.
hue-vermilion = Zinnober

# Track hue 3.
hue-orange = Orange

# Track hue 4.
hue-yellow = Gelb

# Track hue 5.
hue-lime = Limette

# Track hue 6.
hue-green = Grün

# Track hue 7.
hue-emerald = Smaragd

# Track hue 8.
hue-teal = Petrol

# Track hue 9.
hue-cyan = Cyan

# Track hue 10.
hue-blue = Blau

# Track hue 11.
hue-indigo = Indigo

# Track hue 12.
hue-violet = Violett

# Track hue 13.
hue-magenta = Magenta

# Track hue 14.
hue-pink = Rosa

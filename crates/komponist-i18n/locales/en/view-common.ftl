## Texts that several views share: the grid dropdown, snap and zoom buttons, the "zu spät" indicator (`views/grid.rs`, `views/late.rs`), the compile diagnostics (`diagnostics.rs`).

# Accessible name and label of the grid dropdown (arrangement header, piano roll toolbar).
view-grid = Grid

# Snap toggle (magnet icon) of the arrangement header and the piano roll toolbar.
view-snap = Snap

# The "−" of the arrangement header and the piano roll toolbar.
view-zoom-out = Zoom Out

# The "+" of the arrangement header and the piano roll toolbar.
view-zoom-in = Zoom In

## Grid dropdown entries

# Grid of one bar.
grid-bar = 1 Bar

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
grid-quarter-triplet = 1/4 Triplet

# Grid of an eighth-note triplet.
grid-eighth-triplet = 1/8 Triplet

# Grid of a sixteenth-note triplet.
grid-sixteenth-triplet = 1/16 Triplet

## Late indicator

# The indicator of a device whose blocks recently missed their deadline (not in the design).
late-label = late

# Tooltip of the late indicator.
late-blocks =
    { $n ->
        [one] 1 block late
       *[other] { $n } blocks late
    }

## Compile diagnostics: tracks and devices the engine leaves out (#92; `diagnostics.rs`, not in the design)

# Status of a track or device the engine leaves out: the inspector's device row and track section.
not-played-label = not played

# Tooltip detail of a track or device the engine leaves out; `$reason` is one of the `not-played-reason-*` texts.
not-played-detail = Not played: { $reason }

# Reason: the track comes after the most tracks the engine holds.
not-played-reason-track-limit = more than { $limit } tracks

# Reason: no processor slot is left (the most devices the engine holds).
not-played-reason-slot-limit = more than { $limit } devices

# Reason: more MIDI effects in front of an instrument than the engine runs.
not-played-reason-midi-effect-limit = more than { $limit } MIDI effects before the instrument

# Reason: the project file holds this track or device id twice (the later one is left out).
not-played-reason-duplicate-id = duplicate ID in the project file

# Reason: a MIDI effect behind an instrument or audio effect.
not-played-reason-misplaced-midi-effect = MIDI effect not at the start of the chain

# Reason: a second instrument, or an instrument behind an audio effect.
not-played-reason-misplaced-instrument = instrument not right after the MIDI effects

# Reason: an instrument or MIDI effect on an audio track, a bus or the master.
not-played-reason-wrong-chain = instruments and MIDI effects only on MIDI tracks

# Status bar and export summary: tracks the engine leaves out, with the reason (a `not-played-reason-*` text).
not-played-tracks =
    { $count ->
        [one] 1 track not played: { $reason }
       *[other] { $count } tracks not played: { $reason }
    }

# Status bar and export summary: devices the engine leaves out, with the reason.
not-played-devices =
    { $count ->
        [one] 1 device not played: { $reason }
       *[other] { $count } devices not played: { $reason }
    }

## Track hue names (accessible names and tooltips of the colour swatches, in the order of `komponist_tokens::track::HUES`)

# Track hue 1.
hue-red = Red

# Track hue 2.
hue-vermilion = Vermilion

# Track hue 3.
hue-orange = Orange

# Track hue 4.
hue-yellow = Yellow

# Track hue 5.
hue-lime = Lime

# Track hue 6.
hue-green = Green

# Track hue 7.
hue-emerald = Emerald

# Track hue 8.
hue-teal = Teal

# Track hue 9.
hue-cyan = Cyan

# Track hue 10.
hue-blue = Blue

# Track hue 11.
hue-indigo = Indigo

# Track hue 12.
hue-violet = Violet

# Track hue 13.
hue-magenta = Magenta

# Track hue 14.
hue-pink = Pink

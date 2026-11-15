## Texts that several views share: the grid dropdown, snap and zoom buttons, the "zu spät" indicator (`views/grid.rs`, `views/late.rs`).

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

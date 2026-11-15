## Piano roll (`Screens.dc.html` S01 editor, `Prototyp.dc.html` `pr`; `views/piano_roll`). Grid, snap and zoom buttons are in `view-common.ftl`.

# Accessible name of the tool switch.
piano-roll-tools = Tool

# Tool 1.
piano-roll-pointer = Pointer

# Tool 2.
piano-roll-pencil = Pencil

# Tool 3.
piano-roll-eraser = Eraser

# The quantise button.
piano-roll-quantize = Quantize

# Key of the velocity field and title of the velocity lane (`Vel`).
piano-roll-velocity-key = Vel

# Accessible name of the velocity field.
piano-roll-velocity = Velocity

# The length field (key and accessible name).
piano-roll-length = Length

# The key field (key and accessible name).
piano-roll-note = Note

# Accessible name of the note grid (group of the notes).
piano-roll-notes = Notes

# Accessible name of the keyboard.
piano-roll-keyboard = Keyboard

# Accessible name of the velocity lane.
piano-roll-velocity-lane = Velocity lane

# Shown in an empty clip (`Prototyp.dc.html` `pr.emptyS`).
piano-roll-empty-hint = Pencil: click adds a note · Pointer: double-click · Keys A–K play

# Accessible name of a key of the keyboard; `$note` is the note name ("C4").
piano-roll-key = Key { $note }

# Status text while the velocity is dragged.
piano-roll-velocity-status = Velocity { $velocity }

# The selection info of the toolbar: the selected notes, else all notes of the clip (`selT`).
piano-roll-selection =
    { $n ->
        [one] 1 note
       *[other] { $n } notes
    }

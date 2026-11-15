## Step sequencer (`Screens.dc.html` S03, `Prototyp.dc.html` `stepView`; `views/step_seq`). Resolution entries reuse `grid-*` of `view-common.ftl`.

# Toolbar: pattern length key (`Länge 16 Steps`) and its accessible name.
step-seq-length = Länge

# Unit of the pattern length.
step-seq-steps = Steps

# Toolbar: resolution dropdown (label and accessible name).
step-seq-resolution = Auflösung

# Toolbar: swing key and accessible name.
step-seq-swing = Swing

# Step info and lane tab: velocity key.
step-seq-velocity-key = Vel

# Accessible name of the velocity field.
step-seq-velocity = Velocity

# Step info and lane tab: probability key (the screens write "Wahr.").
step-seq-probability-key = Wahr.

# Accessible name of the probability field.
step-seq-probability = Wahrscheinlichkeit

# Step info and lane tab: ratchet key.
step-seq-ratchet-key = Ratch.

# Accessible name of the ratchet field.
step-seq-ratchet = Ratchet

# The button that turns the pattern into a MIDI clip.
step-seq-to-midi = Als MIDI-Clip

# Its tooltip (S03 notes).
step-seq-to-midi-detail = Wandelt das Pattern in einen normalen MIDI-Clip um.

# Accessible name of the lane tabs.
step-seq-lane = Lane

# Without an open step clip (the shell normally shows its own empty state).
step-seq-no-clip = Kein Clip geöffnet

# The lane's row and a row header's accessible name; `$name` is the row name ("Kick").
step-seq-row = Zeile { $name }

# The step info (shown in `type.label` capitals); `$step` counts from 1.
step-seq-step-info = Step { $step } · { $row }

# Accessible name of a lane bar; `$tab` is the lane tab key ("Vel").
step-seq-lane-step = { $tab } Step { $step }

# Accessible name of a step that is off.
step-seq-step-off = Step { $step }, { $row }, aus

# Accessible name of a step that is on.
step-seq-step-on = Step { $step }, { $row }, an, Velocity { $velocity }

# Appended to the name of a step that is on when its probability is below 100 %.
step-seq-step-probability = , Wahrscheinlichkeit { $probability } %

# Appended to the name of a step that is on when it ratchets.
step-seq-step-ratchet = , Ratchet { $ratchet }

# Status text while the velocity of a step changes.
step-seq-status-velocity = Velocity { $velocity }

# Status text while the probability of a step changes.
step-seq-status-probability = Wahrscheinlichkeit { $probability } %

# Status text while the ratchet of a step changes.
step-seq-status-ratchet = Ratchet ×{ $ratchet }

## Step sequencer (`Screens.dc.html` S03, `Prototyp.dc.html` `stepView`; `views/step_seq`). Resolution entries reuse `grid-*` of `view-common.ftl`.

# Toolbar: pattern length key (`Länge 16 Steps`) and its accessible name.
step-seq-length = Length

# Unit of the pattern length.
step-seq-steps = Steps

# Toolbar: resolution dropdown (label and accessible name).
step-seq-resolution = Resolution

# Toolbar: swing key and accessible name.
step-seq-swing = Swing

# Step info and lane tab: velocity key.
step-seq-velocity-key = Vel

# Accessible name of the velocity field.
step-seq-velocity = Velocity

# Step info and lane tab: probability key (the screens write "Wahr.").
step-seq-probability-key = Prob.

# Accessible name of the probability field.
step-seq-probability = Probability

# Step info and lane tab: ratchet key.
step-seq-ratchet-key = Ratch.

# Accessible name of the ratchet field.
step-seq-ratchet = Ratchet

# The button that turns the pattern into a MIDI clip.
step-seq-to-midi = As MIDI Clip

# Its tooltip (S03 notes).
step-seq-to-midi-detail = Converts the pattern into a regular MIDI clip.

# Accessible name of the lane tabs.
step-seq-lane = Lane

# Without an open step clip (the shell normally shows its own empty state).
step-seq-no-clip = No clip open

# The lane's row and a row header's accessible name; `$name` is the row name ("Kick").
step-seq-row = Row { $name }

# The step info (shown in `type.label` capitals); `$step` counts from 1.
step-seq-step-info = Step { $step } · { $row }

# Accessible name of a lane bar; `$tab` is the lane tab key ("Vel").
step-seq-lane-step = { $tab } step { $step }

# Accessible name of a step that is off.
step-seq-step-off = Step { $step }, { $row }, off

# Accessible name of a step that is on.
step-seq-step-on = Step { $step }, { $row }, on, velocity { $velocity }

# Appended to the name of a step that is on when its probability is below 100 %.
step-seq-step-probability = , probability { $probability }%

# Appended to the name of a step that is on when it ratchets.
step-seq-step-ratchet = , ratchet { $ratchet }

# Status text while the velocity of a step changes.
step-seq-status-velocity = Velocity { $velocity }

# Status text while the probability of a step changes.
step-seq-status-probability = Probability { $probability }%

# Status text while the ratchet of a step changes.
step-seq-status-ratchet = Ratchet ×{ $ratchet }

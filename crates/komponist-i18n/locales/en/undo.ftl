## Undo and redo step labels. The app stores the message id (`undo-quantize`) in the document and resolves it when it shows the label (`crate::undo_label`), so a language switch relabels old steps.

# Undo step: Spur scharf schalten.
undo-arm-track = Arm Track

# Undo step: Clips duplizieren.
undo-duplicate-clips = Duplicate Clips

# Undo step: Clips teilen.
undo-split-clips = Split Clips

# Undo step: Clips stumm schalten.
undo-mute-clips = Mute Clips

# Undo step: Spuren stumm schalten.
undo-mute-tracks = Mute Tracks

# Undo step: Quantisieren.
undo-quantize = Quantize

# Undo step: Noten transponieren.
undo-transpose-notes = Transpose Notes

# Undo step: Noten verschieben.
undo-move-notes = Move Notes

# Undo step: Noten kopieren.
undo-copy-notes = Copy Notes

# Undo step: Noten-Länge.
undo-note-length = Note Length

# Undo step: Noten hinzufügen.
undo-add-notes = Add Notes

# Undo step: Noten löschen.
undo-delete-notes = Delete Notes

# Undo step: Velocity.
undo-velocity = Velocity

# Undo step: Projekt ändern.
undo-change-project = Change Project

# Undo step: Master ändern.
undo-change-master = Change Master

# Undo step: Spur hinzufügen.
undo-add-track = Add Track

# Undo step: Neue MIDI-Spur.
undo-new-midi-track = New MIDI Track

# Undo step: Spuren anlegen (the "Neue Spur" dialog with several tracks).
undo-add-tracks = Add Tracks

# Undo step: Spuren löschen.
undo-delete-tracks = Delete Tracks

# Undo step: Spuren verschieben.
undo-move-tracks = Move Tracks

# Undo step: Spur ändern.
undo-change-track = Change Track

# Undo step: Exklusiv schalten.
undo-exclusive = Make Exclusive

# Undo step: Clip anlegen.
undo-add-clip = Add Clip

# Undo step: Clips löschen.
undo-delete-clips = Delete Clips

# Undo step: Clips verschieben.
undo-move-clips = Move Clips

# Undo step: Clips kopieren.
undo-copy-clips = Copy Clips

# Undo step: Clip-Länge.
undo-clip-length = Clip Length

# Undo step: Clip-Loop.
undo-clip-loop = Clip Loop

# Undo step: Clip ändern.
undo-change-clip = Change Clip

# Undo step: Take wählen.
undo-select-take = Choose Take

# Undo step: Step.
undo-step = Step

# Undo step: Pattern ändern.
undo-change-pattern = Change Pattern

# Undo step: Zeile ändern.
undo-change-row = Change Row

# Undo step: Als MIDI-Clip.
undo-as-midi-clip = Convert to MIDI Clip

# Undo step: Gerät entfernen.
undo-remove-device = Remove Device

# Undo step: Geräte entfernen.
undo-remove-devices = Remove Devices

# Undo step: Gerät verschieben.
undo-move-device = Move Device

# Undo step: Gerät kopieren.
undo-copy-device = Copy Device

# Undo step: Geräte einfügen.
undo-paste-devices = Paste Devices

# Undo step: Geräte duplizieren.
undo-duplicate-devices = Duplicate Devices

# Undo step: Bypass.
undo-bypass = Bypass

# Undo step: Sidechain-Quelle oder -Abgriff eines Geräts setzen.
undo-set-sidechain = Set Sidechain

# Undo step: Preset laden.
undo-load-preset = Load Preset

# Undo step: Instrument ersetzen.
undo-replace-instrument = Replace Instrument

# Undo step: MIDI-Effekt hinzufügen.
undo-add-midi-effect = Add MIDI Effect

# Undo step: Effekt hinzufügen.
undo-add-effect = Add Effect

# Undo step: Sample laden.
undo-sample-load = Load Sample

# Undo step: Sample entfernen.
undo-sample-clear = Remove Sample

# Undo step: Tempo.
undo-tempo = Tempo

## Routing (wave 9)

# Undo step: group tracks (Ctrl G).
undo-group-tracks = Group Tracks

# Undo step: create a send to a new bus.
undo-send-to-new-bus = Send to New Bus

# Undo step: create a bus (“New Track” dialog).
undo-add-bus = Add Bus

# Undo step: change the output of a track.
undo-set-output = Change Output

# Undo step: create a send.
undo-add-send = Add Send

# Undo step: change a send (level, tap, target, on/off).
undo-change-send = Change Send

# Undo step: remove a send.
undo-remove-send = Remove Send

# Undo step: drag the send level.
undo-send-level = Send Level

# Undo step: fold or unfold a group.
undo-fold-group = Collapse or Expand Group

## Labels with a name in them (the name is plugin-provided text).

# A card parameter changed; `$name` is the parameter's name ("Cutoff").
undo-param = Change { $name }

# A parameter without a name; `$plugin` is the plugin's name.
undo-param-unnamed = { $plugin }: Change Parameter

# A state the plugin reports on its own (its GUI).
undo-plugin-state = { $plugin }: Change State

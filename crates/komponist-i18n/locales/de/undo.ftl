## Undo and redo step labels. The app stores the message id (`undo-quantize`) in the document and resolves it when it shows the label (`crate::undo_label`), so a language switch relabels old steps.

# Undo step: Spur scharf schalten.
undo-arm-track = Spur scharf schalten

# Undo step: Clips duplizieren.
undo-duplicate-clips = Clips duplizieren

# Undo step: Clips teilen.
undo-split-clips = Clips teilen

# Undo step: Clips stumm schalten.
undo-mute-clips = Clips stumm schalten

# Undo step: Spuren stumm schalten.
undo-mute-tracks = Spuren stumm schalten

# Undo step: Quantisieren.
undo-quantize = Quantisieren

# Undo step: Noten transponieren.
undo-transpose-notes = Noten transponieren

# Undo step: Noten verschieben.
undo-move-notes = Noten verschieben

# Undo step: Noten kopieren.
undo-copy-notes = Noten kopieren

# Undo step: Noten-Länge.
undo-note-length = Noten-Länge

# Undo step: Noten hinzufügen.
undo-add-notes = Noten hinzufügen

# Undo step: Noten löschen.
undo-delete-notes = Noten löschen

# Undo step: Velocity.
undo-velocity = Velocity

# Undo step: Projekt ändern.
undo-change-project = Projekt ändern

# Undo step: Master ändern.
undo-change-master = Master ändern

# Undo step: Spur hinzufügen.
undo-add-track = Spur hinzufügen

# Undo step: Neue MIDI-Spur.
undo-new-midi-track = Neue MIDI-Spur

# Undo step: Spuren anlegen (the "Neue Spur" dialog with several tracks).
undo-add-tracks = Spuren anlegen

# Undo step: Spuren löschen.
undo-delete-tracks = Spuren löschen

# Undo step: Spuren verschieben.
undo-move-tracks = Spuren verschieben

# Undo step: Spur ändern.
undo-change-track = Spur ändern

# Undo step: Exklusiv schalten.
undo-exclusive = Exklusiv schalten

# Undo step: Clip anlegen.
undo-add-clip = Clip anlegen

# Undo step: Clips löschen.
undo-delete-clips = Clips löschen

# Undo step: Clips verschieben.
undo-move-clips = Clips verschieben

# Undo step: Clips kopieren.
undo-copy-clips = Clips kopieren

# Undo step: Clip-Länge.
undo-clip-length = Clip-Länge

# Undo step: Clip-Loop.
undo-clip-loop = Clip-Loop

# Undo step: Clip ändern.
undo-change-clip = Clip ändern

# Undo step: Take wählen.
undo-select-take = Take wählen

# Undo step: Step.
undo-step = Step

# Undo step: Pattern ändern.
undo-change-pattern = Pattern ändern

# Undo step: Zeile ändern.
undo-change-row = Zeile ändern

# Undo step: Als MIDI-Clip.
undo-as-midi-clip = Als MIDI-Clip

# Undo step: Gerät entfernen.
undo-remove-device = Gerät entfernen

# Undo step: Geräte entfernen.
undo-remove-devices = Geräte entfernen

# Undo step: Gerät verschieben.
undo-move-device = Gerät verschieben

# Undo step: Gerät kopieren.
undo-copy-device = Gerät kopieren

# Undo step: Geräte einfügen.
undo-paste-devices = Geräte einfügen

# Undo step: Geräte duplizieren.
undo-duplicate-devices = Geräte duplizieren

# Undo step: Bypass.
undo-bypass = Bypass

# Undo step: Sidechain-Quelle oder -Abgriff eines Geräts setzen.
undo-set-sidechain = Sidechain setzen

# Undo step: Preset laden.
undo-load-preset = Preset laden

# Undo step: Instrument ersetzen.
undo-replace-instrument = Instrument ersetzen

# Undo step: MIDI-Effekt hinzufügen.
undo-add-midi-effect = MIDI-Effekt hinzufügen

# Undo step: Effekt hinzufügen.
undo-add-effect = Effekt hinzufügen

# Undo step: Sample laden.
undo-sample-load = Sample laden

# Undo step: Sample entfernen.
undo-sample-clear = Sample entfernen

# Undo step: Tempo.
undo-tempo = Tempo

## Routing (wave 9)

# Undo step: Spuren in eine Gruppe zusammenfassen (Strg G).
undo-group-tracks = Spuren gruppieren

# Undo step: Send zu einem neuen Bus anlegen.
undo-send-to-new-bus = Send zu neuem Bus

# Undo step: einen Bus anlegen (Dialog „Neue Spur“).
undo-add-bus = Bus anlegen

# Undo step: den Ausgang einer Spur ändern.
undo-set-output = Ausgang ändern

# Undo step: einen Send anlegen.
undo-add-send = Send anlegen

# Undo step: einen Send ändern (Pegel, Abgriff, Ziel, ein/aus).
undo-change-send = Send ändern

# Undo step: einen Send entfernen.
undo-remove-send = Send entfernen

# Undo step: den Send-Pegel ziehen.
undo-send-level = Send-Pegel

# Undo step: eine Gruppe falten oder entfalten.
undo-fold-group = Gruppe falten

## Labels with a name in them (the name is plugin-provided text).

# A card parameter changed; `$name` is the parameter's name ("Cutoff").
undo-param = { $name } ändern

# A parameter without a name; `$plugin` is the plugin's name.
undo-param-unnamed = { $plugin }: Parameter ändern

# A state the plugin reports on its own (its GUI).
undo-plugin-state = { $plugin }: Zustand ändern

# Audio clip envelope gestures.
undo-clip-gain = Clip-Gain ändern
undo-clip-fade = Fade ändern
undo-clip-repitch = Re-Pitch ändern
undo-clip-original-tempo = Originaltempo ändern
undo-clip-source-start = Quell-Start ändern
undo-clip-replace-file = Datei ersetzen

# Audio source window offset.

# Linked audio import and browsing (epic #8).
undo-import-audio = Audio importieren
undo-add-audio-track = Audio-Spur hinzufügen

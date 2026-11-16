## Device chain (`Screens.dc.html` `card()`, the `isDevices` markup, S05/S06 notes; `views/devices`): card texts, the add-device menu, the sidechain chip and popover, the preset menu.

# The open button of a card (`openT` default).
devices-open = Öffnen

# The open button while the plugin window is open (`openT:'Geöffnet'`).
devices-opened = Geöffnet

# The open button of a plugin without own GUI (`openT:'Parameter'`): opens the generic view.
devices-params = Parameter

# Title of a loading card (`msg`).
devices-loading = Lädt …

# Its text (`msg2`).
devices-loading-text = Plugin wird gescannt und instanziiert.

# Title of a crashed card (`msg`).
devices-crashed = Abgestürzt

# Its text (`msg2`).
devices-crashed-text = Das Plugin wurde beendet, die Spur läuft ohne dieses Gerät weiter. Die letzte Einstellung ist gesichert.

# Title when loading failed (the status bar's "… lässt sich nicht laden"; not in S05).
devices-load-failed = Lässt sich nicht laden

# The text when loading failed (the status bar's "… lässt sich nicht laden" plus the rest of `msg2`).
devices-load-failed-text = Das Plugin lässt sich nicht laden, die Spur läuft ohne dieses Gerät weiter. Die letzte Einstellung ist gesichert.

# Title of a plugin that stopped answering ("hängt" as in the status bar; not in S05).
devices-unresponsive = Hängt

# Its text.
devices-unresponsive-text = Das Plugin antwortet nicht mehr, die Spur läuft ohne dieses Gerät weiter. Die letzte Einstellung ist gesichert.

# The button of a failed card (`btnT`) and the context menu entry.
devices-reload = Neu laden

# Title of a missing plugin (S06 label "Plugin fehlt").
devices-missing = Plugin fehlt

# Its text, from the S06 dialog.
devices-missing-text = Nicht installiert oder nicht in einem Plugin-Ordner. Die Einstellungen bleiben im Projekt gespeichert und werden wieder geladen, sobald das Plugin gefunden wird.

# A state that failed to load (WP-45 note, the status bar's text).
devices-state-warning = Zustand konnte nicht geladen werden

# The drop area after the last card.
devices-drop-hint = Effekt aus dem Browser hierher ziehen

# Accessible name of the drop box, which also opens the add menu.
devices-add-device = Gerät hinzufügen

# The add menu's search field: hint and accessible name.
devices-add-search = Gerät suchen …

# The add menu with no match.
devices-add-no-match = Keine Treffer

# Accessible name of the add menu's list.
devices-add-list = Geräte

# Accessible name and tooltip of the ⋯ button (handoff `aria-label="Weitere Aktionen"`).
devices-more = Weitere Aktionen

# The preset button without a matching factory preset, and the menu's name.
devices-preset = Preset

# The page switch's accessible name.
devices-pages = Seiten

# The accessible name of the operator switch on hexa's envelope page.
devices-operator = Operator

# Context menu of a parameter: set it to its default (knobs and faders: the double-click).
devices-to-default = Auf Standard

# Its shortcut column.
devices-to-default-shortcut = Doppelklick

# Its shortcut column for the controls where only the Alt-click resets (number fields, choices).
devices-to-default-alt-shortcut = Alt-Klick

# Context menu of a parameter: open the text entry.
devices-enter-value = Wert eingeben

# Accessible name of a parameter's context menu.
devices-parameter = Parameter

# Title of the FM display with free routing.
devices-fm-free = Algorithmus Frei

# Band of an EQ, in the place of a parameter (`EQ · Band 2 · Spur 1`).
devices-band = Band

# The master channel in the place of a parameter.
devices-master = Master

# Context menu: collapse the frame to its 28 px strip.
devices-collapse = Einklappen

# Context menu: expand a collapsed frame.
devices-expand = Aufklappen

# Context menu: copy the device behind itself.
devices-duplicate = Duplizieren

# Accessible name of the chain's horizontal scrollbar.
devices-scroll-h = Geräte-Kette horizontal scrollen

# Accessible name of the chain's vertical scrollbar.
devices-scroll-v = Geräte-Kette vertikal scrollen

# The header button (`+ Gerät`).
devices-add = + Gerät

# Accessible name and tooltip of the bypass switch (on = the device runs).
devices-active = Aktiv

# Tooltip detail of the switch (S05 note "Schalter (Bypass)") and context menu entry.
devices-bypass = Bypass

# Context menu: remove the device.
devices-remove = Entfernen

# Sampler pad context menu: pick a sample file.
devices-load-sample = Sample laden …

# Sampler pad context menu for a missing sample: look for the file.
devices-search = Suchen …

# Sampler pad context menu: empty the pad.
devices-remove-sample = Sample entfernen

# Under a pad whose sample file is gone.
devices-pad-missing = fehlt

# Under a pad whose sample is loading.
devices-pad-loading = Lädt …

# Under a pad whose sample could not be read.
devices-pad-failed = Fehler

# The waveform's title for a pad without a sample.
devices-no-sample = kein Sample

# Accessible name of the card's context menu.
devices-device = Gerät

# Accessible name of the card row.
devices-chain = Geräte-Kette

# Readout name in the compressor graph's strip while an external sidechain source is set (shown over `devices-detector-external`; reads "Detektor: Extern").
devices-detector = Detektor

# Readout value for `devices-detector`.
devices-detector-external = Extern

# The title of volt's filter graph (followed by the type).
devices-filter = Filter

# The title of volt's LFO graph (followed by the number and shape).
devices-lfo = LFO

# The sidechain chip's text (`SC`).
devices-sc = SC

# Tooltip of the sidechain chip.
devices-sc-tip = Sidechain: Das Signal einer anderen Spur steuert dieses Gerät.

# Accessible name of the chip without a source.
devices-sc-off = Sidechain: aus

# Accessible name and name of the sidechain popover's list.
devices-sc-source = Sidechain-Quelle

# The popover's first row: the device's own input.
devices-sc-internal = — (intern: Eingangssignal)

# The popover's section of the tap.
devices-sc-tap = Abgriff

# Hint under the tap while it is pre fader.
devices-sc-pre-hint = Vor Fader: Mute der Quelle ändert nichts

# Hint under the tap while it is post fader.
devices-sc-post-hint = Nach Fader: folgt Fader und Stumm, nicht Solo

# Context menu of the chip: back to the internal input.
devices-sc-remove = Quelle entfernen

# "Bass (diese Spur)": the track that holds the device.
devices-sc-this-track = diese Spur

# Placeholder of the graph: widget kind (handoff §4, WP-84).
devices-widget-knob = Regler

# Placeholder: widget kind.
devices-widget-knob-large = Großer Regler

# Placeholder: widget kind.
devices-widget-number = Zahlenfeld

# Placeholder: widget kind.
devices-widget-segment = Umschalter

# Placeholder: widget kind.
devices-widget-toggle-button = Schalt-Button

# Placeholder: widget kind.
devices-widget-dropdown = Dropdown

# Placeholder: widget kind.
devices-widget-mini-fader = Mini-Fader

# Placeholder: widget kind.
devices-widget-meter-gr = GR-Meter

# Placeholder: widget kind.
devices-widget-pad-grid = Pad-Raster

# Placeholder: widget kind.
devices-widget-fm-algorithm = FM-Algorithmus

# Placeholder: widget kind.
devices-widget-column-select = Spaltenwahl

# Placeholder: widget kind.
devices-widget-graph = Graph

# The accessible name of the preset button: "Preset: Glas-Glocke" (handoff `aria-label`).
devices-preset-name = Preset: { $name }

# The accessible name of a collapsed frame (handoff `collapsed()`: "… aufklappen").
devices-expand-name = { $name } aufklappen

# The accessible name of a chain head entry: "Gerät Hexa anzeigen".
devices-reveal = Gerät { $name } anzeigen

# The accessible name of operator column `n`: "Operator 3".
devices-operator-n = Operator { $n }

# An empty sampler pad: "Pad 3".
devices-pad = Pad { $n }

# The accessible name of a pad's context menu: "Pad Kick".
devices-pad-menu = Pad { $name }

# The waveform's title: "Pad 1 · Kick" or "Pad 1 · kein Sample" (`$sample` is the sample name or `devices-no-sample`).
devices-waveform-title = Pad { $pad } · { $sample }

# The sidechain chip's accessible name: "Sidechain: Drums".
devices-sc-chip = Sidechain: { $source }

# The reason for the tooltip when the source is the track that holds the device.
devices-sc-reason-self = Nicht möglich: „{ $name }“ ist die Spur dieses Geräts (Rückkopplung).

# … when the source is a group that contains it.
devices-sc-reason-group = Nicht möglich: „{ $name }“ enthält diese Spur (Rückkopplung).

# … when the source is fed by the device's track; `$holder` is the track that holds the device.
devices-sc-reason-fed = Nicht möglich: „{ $holder }“ gibt schon an „{ $source }“ aus (Rückkopplung).

# The accessible name of a card: "Gerät Surge XT".
devices-card = Gerät { $name }

# The text of a parameter's range: "20 Hz bis 20 kHz".
devices-range = { $min } bis { $max }

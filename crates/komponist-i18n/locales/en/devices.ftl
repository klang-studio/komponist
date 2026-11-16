## Device chain (`Screens.dc.html` `card()`, the `isDevices` markup, S05/S06 notes; `views/devices`): card texts, the add-device menu, the sidechain chip and popover, the preset menu.

# The open button of a card (`openT` default).
devices-open = Open

# The open button while the plugin window is open (`openT:'Geöffnet'`).
devices-opened = Opened

# The open button of a plugin without own GUI (`openT:'Parameter'`): opens the generic view.
devices-params = Parameters

# Title of a loading card (`msg`).
devices-loading = Loading…

# Its text (`msg2`).
devices-loading-text = The plugin is being scanned and instantiated.

# Title of a crashed card (`msg`).
devices-crashed = Crashed

# Its text (`msg2`).
devices-crashed-text = The plugin was terminated. The track keeps playing without this device. The last setting is saved.

# Title when loading failed (the status bar's "… lässt sich nicht laden"; not in S05).
devices-load-failed = Failed to Load

# The text when loading failed (the status bar's "… lässt sich nicht laden" plus the rest of `msg2`).
devices-load-failed-text = The plugin failed to load. The track keeps playing without this device. The last setting is saved.

# Title of a plugin that stopped answering ("hängt" as in the status bar; not in S05).
devices-unresponsive = Not Responding

# Its text.
devices-unresponsive-text = The plugin stopped responding. The track keeps playing without this device. The last setting is saved.

# The button of a failed card (`btnT`) and the context menu entry.
devices-reload = Reload

# Title of a missing plugin (S06 label "Plugin fehlt").
devices-missing = Plugin Missing

# Its text, from the S06 dialog.
devices-missing-text = Not installed, or not in a plugin folder. The settings stay saved in the project and load again as soon as the plugin is found.

# A state that failed to load (WP-45 note, the status bar's text).
devices-state-warning = State could not be loaded

# The drop area after the last card.
devices-drop-hint = Drag an effect from the browser here

# Accessible name of the drop box, which also opens the add menu.
devices-add-device = Add device

# The add menu's search field: hint and accessible name.
devices-add-search = Search devices…

# The add menu with no match.
devices-add-no-match = No matches

# Accessible name of the add menu's list.
devices-add-list = Devices

# Accessible name and tooltip of the ⋯ button (handoff `aria-label="Weitere Aktionen"`).
devices-more = More actions

# The preset button without a matching factory preset, and the menu's name.
devices-preset = Preset

# The page switch's accessible name.
devices-pages = Pages

# The accessible name of the operator switch on hexa's envelope page.
devices-operator = Operator

# Context menu of a parameter: set it to its default (knobs and faders: the double-click).
devices-to-default = Reset to Default

# Its shortcut column.
devices-to-default-shortcut = Double-click

# Its shortcut column for the controls where only the Alt-click resets (number fields, choices).
devices-to-default-alt-shortcut = Alt+click

# Same on macOS: ⌥ instead of Alt.
devices-to-default-alt-shortcut-mac = ⌥+click

# Context menu of a parameter: open the text entry.
devices-enter-value = Enter Value

# Accessible name of a parameter's context menu.
devices-parameter = Parameter

# Title of the FM display with free routing.
devices-fm-free = Algorithm Free

# Band of an EQ, in the place of a parameter (`EQ · Band 2 · Spur 1`).
devices-band = Band

# The master channel in the place of a parameter.
devices-master = Master

# Context menu: collapse the frame to its 28 px strip.
devices-collapse = Collapse

# Context menu: expand a collapsed frame.
devices-expand = Expand

# Context menu: copy the device behind itself.
devices-duplicate = Duplicate

# Accessible name of the chain's horizontal scrollbar.
devices-scroll-h = Scroll device chain horizontally

# Accessible name of the chain's vertical scrollbar.
devices-scroll-v = Scroll device chain vertically

# The header button (`+ Gerät`).
devices-add = + Device

# Accessible name and tooltip of the bypass switch (on = the device runs).
devices-active = Active

# Tooltip detail of the switch (S05 note "Schalter (Bypass)") and context menu entry.
devices-bypass = Bypass

# Context menu: remove the device.
devices-remove = Remove

# Sampler pad context menu: pick a sample file.
devices-load-sample = Load Sample…

# Sampler pad context menu for a missing sample: look for the file.
devices-search = Locate…

# Sampler pad context menu: empty the pad.
devices-remove-sample = Remove Sample

# Under a pad whose sample file is gone.
devices-pad-missing = missing

# Under a pad whose sample is loading.
devices-pad-loading = Loading…

# Under a pad whose sample could not be read.
devices-pad-failed = Error

# The waveform's title for a pad without a sample.
devices-no-sample = no sample

# Accessible name of the card's context menu.
devices-device = Device

# Accessible name of the card row.
devices-chain = Device chain

# Readout name in the compressor graph's strip while an external sidechain source is set (shown over `devices-detector-external`; reads "Detektor: Extern").
devices-detector = Detector

# Readout value for `devices-detector`.
devices-detector-external = External

# The title of volt's filter graph (followed by the type).
devices-filter = Filter

# The title of volt's LFO graph (followed by the number and shape).
devices-lfo = LFO

# The sidechain chip's text (`SC`).
devices-sc = SC

# Tooltip of the sidechain chip.
devices-sc-tip = Sidechain: the signal of another track controls this device.

# Accessible name of the chip without a source.
devices-sc-off = Sidechain: off

# Accessible name and name of the sidechain popover's list.
devices-sc-source = Sidechain source

# The popover's first row: the device's own input.
devices-sc-internal = — (internal: input signal)

# The popover's section of the tap.
devices-sc-tap = Tap

# Hint under the tap while it is pre fader.
devices-sc-pre-hint = Pre fader: muting the source changes nothing

# Hint under the tap while it is post fader.
devices-sc-post-hint = Post fader: follows fader and mute, not solo

# Context menu of the chip: back to the internal input.
devices-sc-remove = Remove Source

# "Bass (diese Spur)": the track that holds the device.
devices-sc-this-track = this track

# Placeholder of the graph: widget kind (handoff §4, WP-84).
devices-widget-knob = Knob

# Placeholder: widget kind.
devices-widget-knob-large = Large Knob

# Placeholder: widget kind.
devices-widget-number = Number Field

# Placeholder: widget kind.
devices-widget-segment = Segmented Switch

# Placeholder: widget kind.
devices-widget-toggle-button = Toggle Button

# Placeholder: widget kind.
devices-widget-dropdown = Dropdown

# Placeholder: widget kind.
devices-widget-mini-fader = Mini Fader

# Placeholder: widget kind.
devices-widget-meter-gr = GR Meter

# Placeholder: widget kind.
devices-widget-pad-grid = Pad Grid

# Placeholder: widget kind.
devices-widget-fm-algorithm = FM Algorithm

# Placeholder: widget kind.
devices-widget-column-select = Column Select

# Placeholder: widget kind.
devices-widget-graph = Graph

# The accessible name of the preset button: "Preset: Glas-Glocke" (handoff `aria-label`).
devices-preset-name = Preset: { $name }

# The accessible name of a collapsed frame (handoff `collapsed()`: "… aufklappen").
devices-expand-name = Expand { $name }

# The accessible name of a chain head entry: "Gerät Hexa anzeigen".
devices-reveal = Show device { $name }

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
devices-sc-reason-self = Not possible: “{ $name }” is the track of this device (feedback loop).

# … when the source is a group that contains it.
devices-sc-reason-group = Not possible: “{ $name }” contains this track (feedback loop).

# … when the source is fed by the device's track; `$holder` is the track that holds the device.
devices-sc-reason-fed = Not possible: “{ $holder }” already outputs to “{ $source }” (feedback loop).

# The accessible name of a card: "Gerät Surge XT".
devices-card = Device { $name }

# The text of a parameter's range: "20 Hz bis 20 kHz".
devices-range = { $min } to { $max }

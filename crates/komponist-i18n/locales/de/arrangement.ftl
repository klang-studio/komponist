## Arrangement (`Screens.dc.html` S01, S10; design system `headers`; `views/arrangement`). Routing texts of the buses are in `routing.ftl`, the grid and zoom buttons in `view-common.ftl`.

# The area below the tracks (S01).
arrangement-new-track = Neue Spur

# Accessible name of the box that opens the dialog for new tracks.
arrangement-new-track-button = Neue Spur …

# Its hint, before the shortcut of a new MIDI track (`$shortcut` is empty when none is bound).
arrangement-new-track-hint = Instrument hierher ziehen oder { $shortcut }

# S10: title of the empty-project hint.
arrangement-empty-title = Leeres Projekt

# S10: its text.
arrangement-empty-text = Ziehe ein Instrument aus dem Browser hierher. Das legt eine MIDI-Spur mit diesem Instrument an.

# S10: the primary action.
arrangement-add-midi-track = MIDI-Spur hinzufügen

# S10: the secondary action.
arrangement-open-project = Projekt öffnen

# Accessible name of the text field of a track being renamed.
arrangement-rename = Spurname

# Instrument line of a track without instrument (design system `headers`).
arrangement-no-instrument = Instrument hierher ziehen

# Instrument line of a track muted by a solo elsewhere.
arrangement-implied-mute = stumm durch Solo

# Name of a new MIDI clip from a double-click (prototype `newClip`); content, not retranslated afterwards.
arrangement-new-midi-clip = MIDI-Clip

# Name of a new step clip on a sampler track (prototype `newClip`); content, not retranslated afterwards.
arrangement-new-pattern = Pattern { $n }

# Accessible name of the bottom edge of a header.
arrangement-height-grip = Spurhöhe · { $track }

# Accessible name of the header meter.
arrangement-meter = Pegel { $track }

# Instrument line: the plugin is missing (design system `headers`, "plugin fehlt").
arrangement-plugin-missing = ⚠ Plugin fehlt: { $plugin }

# Instrument line: the plugin crashed (S05 "abgestürzt").
arrangement-plugin-crashed = ⚠ Plugin abgestürzt: { $plugin }

# Instrument line: loading failed, no crash.
arrangement-plugin-load-failed = ⚠ Plugin lässt sich nicht laden: { $plugin }

# Instrument line: the plugin stopped answering.
arrangement-plugin-unresponsive = ⚠ Plugin hängt: { $plugin }

# Instrument line while the plugin loads (prototype `arrView`).
arrangement-plugin-loading = { $plugin } · lädt …

# Instrument line of a bypassed instrument (prototype `arrView`).
arrangement-plugin-bypassed = { $plugin } · aus

# The count box: tracks that are no group and no bus.
arrangement-count-tracks =
    { $n ->
        [one] 1 Spur
       *[other] { $n } Spuren
    }

# The count box: groups, after the tracks (`05-arrangement`).
arrangement-count-groups =
    { $n ->
        [one] 1 Gruppe
       *[other] { $n } Gruppen
    }

# The count box: effect buses, after the groups.
arrangement-count-buses =
    { $n ->
        [one] 1 Bus
       *[other] { $n } Busse
    }

# Accessible name of the fold button of a group; `$action` is `expand` or `collapse`.
arrangement-group-fold = Gruppe { $name } { $action ->
        [expand] ausklappen
       *[collapse] einklappen
    }

# Tooltip of the fold button of a folded group.
arrangement-group-expand = Gruppe ausklappen

# Tooltip of the fold button of an open group.
arrangement-group-collapse = Gruppe einklappen

# Header context menu: delete the track.
arrangement-delete = Löschen

# Accessible name of the header context menu of a track.
arrangement-track-menu = Spur

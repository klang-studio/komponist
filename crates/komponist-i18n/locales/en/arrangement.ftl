## Arrangement (`Screens.dc.html` S01, S10; design system `headers`; `views/arrangement`). Routing texts of the buses are in `routing.ftl`, the grid and zoom buttons in `view-common.ftl`.

# The area below the tracks (S01).
arrangement-new-track = New Track

# Accessible name of the box that opens the dialog for new tracks.
arrangement-new-track-button = New Track…

# Its hint, before the shortcut of a new MIDI track (`$shortcut` is empty when none is bound).
arrangement-new-track-hint = Drag an instrument here or { $shortcut }

# S10: title of the empty-project hint.
arrangement-empty-title = Empty Project

# S10: its text.
arrangement-empty-text = Drag an instrument from the browser here. This creates a MIDI track with that instrument.

# S10: the primary action.
arrangement-add-midi-track = Add MIDI Track

# S10: the secondary action.
arrangement-open-project = Open Project

# Accessible name of the text field of a track being renamed.
arrangement-rename = Track name

# Instrument line of a track without instrument (design system `headers`).
arrangement-no-instrument = Drag an instrument here

# Instrument line of a track muted by a solo elsewhere.
arrangement-implied-mute = muted by solo

# Name of a new MIDI clip from a double-click (prototype `newClip`); content, not retranslated afterwards.
arrangement-new-midi-clip = MIDI Clip

# Name of a new step clip on a sampler track (prototype `newClip`); content, not retranslated afterwards.
arrangement-new-pattern = Pattern { $n }

# Accessible name of the bottom edge of a header.
arrangement-height-grip = Track height · { $track }

# Accessible name of the header meter.
arrangement-meter = Level { $track }

# Instrument line: the plugin is missing (design system `headers`, "plugin fehlt").
arrangement-plugin-missing = ⚠ Plugin missing: { $plugin }

# Instrument line: the plugin crashed (S05 "abgestürzt").
arrangement-plugin-crashed = ⚠ Plugin crashed: { $plugin }

# Instrument line: loading failed, no crash.
arrangement-plugin-load-failed = ⚠ Plugin failed to load: { $plugin }

# Instrument line: the plugin stopped answering.
arrangement-plugin-unresponsive = ⚠ Plugin not responding: { $plugin }

# Instrument line while the plugin loads (prototype `arrView`).
arrangement-plugin-loading = { $plugin } · loading…

# Instrument line of a bypassed instrument (prototype `arrView`).
arrangement-plugin-bypassed = { $plugin } · off

# The count box: tracks that are no group and no bus.
arrangement-count-tracks =
    { $n ->
        [one] 1 track
       *[other] { $n } tracks
    }

# The count box: groups, after the tracks (`05-arrangement`).
arrangement-count-groups =
    { $n ->
        [one] 1 group
       *[other] { $n } groups
    }

# The count box: effect buses, after the groups.
arrangement-count-buses =
    { $n ->
        [one] 1 bus
       *[other] { $n } buses
    }

# Accessible name of the fold button of a group; `$action` is `expand` or `collapse`.
arrangement-group-fold = { $action ->
        [expand] Expand
       *[collapse] Collapse
    } group { $name }

# Tooltip of the fold button of a folded group.
arrangement-group-expand = Expand group

# Tooltip of the fold button of an open group.
arrangement-group-collapse = Collapse group

# Header context menu: delete the track.
arrangement-delete = Delete

# Accessible name of the header context menu of a track.
arrangement-track-menu = Track
arrangement-flatten-track = Flatten to Audio

arrangement-new-track-hint-audio = Drag an instrument or audio file here · { $midi } · { $audio }

## Status bar (`Screens.dc.html` `stt`, `ks-view.js` `statusView`) and the messages the app shows in it (`App::set_status`).

# Right side of the status bar: the audio output. `$host` is the driver, `$rate` e.g. "48 kHz", `$ms` the latency.
status-output = { $host } · { $rate } · { $frames } samples · { $ms } ms

# Appended to the audio output when the audio thread got no real-time priority.
status-no-realtime = without real-time priority

# Right side while the audio stream is not running.
status-audio-stopped = Audio stopped

# Left side while recording; `$tracks` are the armed track names. The leading "● " is painted as a dot.
status-recording = ● Recording · { $tracks }

# Middle text while recording.
status-recording-hint = R or Space stops · Esc discards the take

# Left side during the count-in.
status-count-in = Count-in…

# Middle text while a device parameter is dragged.
status-param-hint = Alt+click: default · Right-click: enter value

# Left side in the piano roll; German keeps "Noten" for one note (verbatim S01).
status-notes-selected = { $clip } · { $selected } of { $total } { $total ->
        [one] note
       *[other] notes
    } selected

# Middle text in the piano roll.
status-notes-hint = Ctrl+drag a note: velocity · Alt+drag: copy

# Same on macOS: ⌘ instead of Ctrl.
status-notes-hint-mac = ⌘+drag a note: velocity · Alt+drag: copy

# Left side in the step sequencer with a selected step; `$step` counts from 1.
status-step-selected = { $clip } · Step { $step } of { $length }

# Middle text in the step sequencer with a selected step.
status-step-hint = Click sets a step · Drag ↕ on a step: velocity · Alt+click: probability

# Left side in the step sequencer without a selected step; German keeps "Steps" for one step.
status-steps-set = { $clip } · { $count ->
        [one] 1 step
       *[other] { $count } steps
    } set

# Middle text in the step sequencer without a selected step.
status-steps-hint = Space plays the pattern

# Left side with a selected track (also in the maximized mixer).
status-track-selected = { $name } selected

# Middle text in the maximized mixer.
status-mixer-hint = Tab: back to the arrangement · Click a red indicator to reset it

# Left side in the device chain.
status-devices = { $name } · { $count ->
        [one] 1 device
       *[other] { $count } devices
    }

# Middle text in the device chain.
status-devices-hint = Drag a device: reorder · Alt+click a switch: bypass

# Left side with selected clips.
status-clips-selected = { $count ->
        [one] 1 clip
       *[other] { $count } clips
    } selected

# Middle text with selected clips.
status-clips-hint = Drag moves · Alt+drag copies · right edge trims · Ctrl+D duplicates · Del deletes

# Same on macOS: ⌘ instead of Ctrl.
status-clips-hint-mac = Drag moves · Alt+drag copies · right edge trims · ⌘D duplicates · Del deletes

# Middle text with a selected track.
status-track-hint = Double-click in the track to add a clip · R records

# Left side of a project that is new or has unsaved changes.
status-project-unsaved = { $name } · unsaved

# Middle text of a project without tracks.
status-empty-hint = Ctrl+T: MIDI track · Ctrl+S: save

# Same on macOS: ⌘ instead of Ctrl.
status-empty-hint-mac = ⌘T: MIDI track · ⌘S: save

## Messages (`App::set_status`).

# The engine's worker threads run without real-time priority.
status-workers-no-realtime = Audio threads run without real-time priority

# The engine's worker threads could not join the audio device's workgroup (macOS).
status-workers-no-workgroup = Audio threads could not join the audio workgroup

# A MIDI input appeared; `$name` as S07 shows it.
status-midi-connected = { $name } connected

# A MIDI input vanished.
status-midi-disconnected = { $name } disconnected

# A plugin bridge exited.
status-device-crashed = { $name } crashed · the track keeps running without this device

# A plugin bridge does not answer.
status-device-unresponsive = { $name } is not responding · the track keeps running without this device

# A plugin could not be loaded.
status-device-load-failed = { $name } could not be loaded

# Name of a device that is gone, for messages.
status-device-unnamed = Device { $id }

# Source of a plugin log line without a device.
status-plugin-scan = Plugin scan

# A plugin could not restore its saved state.
status-state-load-failed = { $name }: state could not be loaded

# One plugin's process runs without real-time priority.
status-plugin-no-realtime = { $name } runs without real-time priority

# A MIDI effect dropped where no track is selected.
status-midi-fx-need-track = MIDI effects need a track

# An effect dropped where no track is selected.
status-fx-need-track = Effects need a track

# A sample was linked to a pad (`$pad` counts from 1).
status-sample-on-pad = “{ $name }” is on pad { $pad }

# Name of a sample without a name.
status-sample-unnamed = Sample

# Collect and save: nothing had to be copied.
status-collected-none = Saved · no samples to collect ({ $folder })

# Collect and save: `$count` samples were copied into `$folder`.
status-collected = Collected and saved · { $count ->
        [one] 1 sample
       *[other] { $count } samples
    } in “{ $folder }”

# Collecting the samples failed; `$reason` is the system's text.
status-collect-failed = Collecting failed: { $reason }

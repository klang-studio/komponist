## Status bar (`Screens.dc.html` `stt`, `ks-view.js` `statusView`) and the messages the app shows in it (`App::set_status`).

# Right side of the status bar: the audio output. `$host` is the driver, `$rate` e.g. "48 kHz", `$ms` the latency.
status-output = { $host } · { $rate } · { $frames } samples · { $ms } ms

# Appended to the audio output when the audio thread got no real-time priority.
status-no-realtime = without real-time priority

# Right side while the audio stream is not running.
status-audio-stopped = Audio stopped

# Largest number of frames missing at a single delay line.
status-pdc-shortfall = Latency compensation incomplete · Up to { $frames } { $frames ->
        [one] frame
       *[other] frames
    } short per delay line

# Left side while recording; `$tracks` are the armed track names. The leading "● " is painted as a dot.
status-recording = ● Recording · { $tracks }

# Middle text while recording.
status-recording-hint = R or Space stops · Esc discards the take

# Left side during the count-in.
status-count-in = Count-in…

# Middle text while a device parameter is dragged.
status-param-hint = Alt+click: default · Right-click: enter value

# Same on macOS: ⌥ instead of Alt.
status-param-hint-mac = ⌥+click: default · Right-click: enter value

# Left side in the piano roll; German keeps "Noten" for one note (verbatim S01).
status-notes-selected = { $clip } · { $selected } of { $total } { $total ->
        [one] note
       *[other] notes
    } selected

# Middle text in the piano roll.
status-notes-hint = Ctrl+drag a note: velocity · Alt+drag: copy

# Same on macOS: ⌘ instead of Ctrl.
status-notes-hint-mac = ⌘+drag a note: velocity · ⌥+drag: copy

# Left side in the step sequencer with a selected step; `$step` counts from 1.
status-step-selected = { $clip } · Step { $step } of { $length }

# Middle text in the step sequencer with a selected step.
status-step-hint = Click sets a step · Drag ↕ on a step: velocity · Alt+click: probability

# Same on macOS: ⌥ instead of Alt.
status-step-hint-mac = Click sets a step · Drag ↕ on a step: velocity · ⌥+click: probability

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

# Same on macOS: ⌥ instead of Alt.
status-devices-hint-mac = Drag a device: reorder · ⌥+click a switch: bypass

# Left side with selected clips.
status-clips-selected = { $count ->
        [one] 1 clip
       *[other] { $count } clips
    } selected

# Middle text with selected clips.
status-clips-hint = Drag moves · Alt+drag copies · right edge trims · Ctrl+D duplicates · Del deletes

# Same on macOS: ⌘ instead of Ctrl.
status-clips-hint-mac = Drag moves · ⌥+drag copies · right edge trims · ⌘D duplicates · Del deletes

# Middle text with a selected track.
status-track-hint = Double-click in the track to add a clip · R records

# Left side of a project that is new or has unsaved changes.
status-project-unsaved = { $name } · unsaved

# Middle text of a project without tracks.
status-empty-hint = Ctrl+T: MIDI track · Ctrl+Shift+T: Audio track · Ctrl+S: save

# Same on macOS: ⌘ instead of Ctrl.
status-empty-hint-mac = ⌘T: MIDI track · ⌘⇧T: Audio track · ⌘S: save

## Messages (`App::set_status`).

# The engine's worker threads run without real-time priority.
status-workers-no-realtime = Audio threads run without real-time priority

# The engine's worker threads could not join the audio device's workgroup (macOS).
status-workers-no-workgroup = Audio threads could not join the audio workgroup

# The audio engine panicked in the audio callback; a new one was built from the open project (#210).
status-engine-rebuilt = Audio engine crashed · rebuilt from the project, playback stopped

# Same while a take was running: it ended at the crash and keeps what was recorded.
status-engine-rebuilt-take = Audio engine crashed · the recording so far is kept, rebuilt from the project

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

# A sample was linked in the general-purpose Sampler.
status-sampler-loaded = Loaded “{ $name }” into Sampler

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

# Editing cannot create an object once the project ID counter is exhausted.
status-id-space-exhausted = No unused object IDs remain in this project.
# Audio clip editing and refused drops.
status-audio-clips-hint = Drag moves · Alt+drag copies · Edges trim · Corners: fades · Line: gain · Ctrl+E splits · Ctrl+D duplicates · Del deletes
status-audio-clips-hint-mac = Drag moves · ⌥+drag copies · Edges trim · Corners: fades · Line: gain · ⌘ E splits · ⌘ D duplicates · Del deletes
status-drop-refused-midi = MIDI tracks don’t take audio files.
status-drop-refused-audio = Audio tracks don’t take MIDI clips.

# Linked audio import and browsing (epic #8).
status-collect-skipped = { $count } files were not collected: unsupported audio files · { $files }
status-collect-unapproved = Plugin files were not collected. Select them in the device to approve copying. { $files }

status-macros-mapping = Map: move parameters to map them to { $source } · Click another macro to switch · Esc ends
status-macros-locked = { $param } is controlled by { $source }
status-macros-moved = { $param }: now { $source }

# #227: rejected edits and failed device starts reach the status bar.
status-edit-routing-cycle = Not possible: this would create a routing loop
status-edit-not-a-bus = Not possible: “{ $track }” is not a bus
status-edit-automation-too-large = Not possible: the automation would create too many points
status-edit-macro-full = Not possible: track macro { $n } already has 16 mappings
# Errors only a bug can cause; the details go to the log.
status-edit-failed = Edit not possible
status-midi-enable-failed = Cannot open MIDI input “{ $device }”
status-audio-input-failed = Cannot start audio input · Settings { $shortcut }

## Crash recovery (#227, crash-recovery proposal (internal notes)).

# A recovered copy opened; `$time` is "today 14:32", `$shortcut` the save shortcut ("Ctrl+S"), `$file` the file name.
status-recovered = Recovered changes from { $time } · { $shortcut } saves them to “{ $file }”

# Same for a project that was never saved.
status-recovered-untitled = Recovered changes from { $time } · { $shortcut } saves them as a new project

# Same for a project whose file is gone: saving asks for a location.
status-recovered-elsewhere = Recovered changes from { $time } · { $shortcut } asks where to save them

# The recovery copy could not be written (once per session); `$reason` is one of the `project-reason-*` texts.
status-recovery-write-failed = Cannot write the recovery copy: { $reason }

# The recovery folder could not be read at start; `$reason` is one of the `project-reason-*` texts.
status-recovery-scan-failed = Cannot read the recovery copies: { $reason }

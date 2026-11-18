# Audio clips in the arrangement and their editing handles.
audio-clip-missing = ⚠ File missing: { $file }
audio-clip-missing-short = File missing
audio-clip-peaks = Calculating waveform…
audio-clip-end-of-file = End of file reached
audio-clip-cuts-below = cuts the clip below
audio-clip-repitch-badge = RP
audio-clip-repitch-tip = Re-Pitch: original { $orig } BPM, project { $project } BPM
audio-clip-fade-in = Fade in
audio-clip-fade-out = Fade out
audio-clip-gain-line = Clip gain
audio-clip-start = Clip start
audio-clip-end = Clip end
a11y-audio-missing-suffix = , file missing
a11y-audio-repitch-suffix = , Re-Pitch { $tempo } BPM

# Audio source editor and file inspector.
dock-waveform = Waveform
inspector-clip-audio = AUDIO
audio-clip-source-start = Source start
audio-clip-gain = Gain
audio-clip-repitch = Re-Pitch
audio-clip-original-tempo = Original tempo
audio-clip-bar-count = Set source to
audio-clip-bars = { $n } bars
audio-waveform-title = Waveform, { $name }
audio-source-start = File start
audio-source-end = File end
audio-channel-left = L
audio-channel-right = R
audio-channel-mono = mono
audio-channel-stereo = stereo
audio-amplitude-zero = 0 dB
audio-amplitude-silence = −∞
audio-file = File
audio-file-replace = Replace File…
audio-file-copy-path = Copy Path
audio-file-show = Show in Folder
audio-file-actions = File actions
audio-file-external = external link
audio-file-in-project = in project
audio-file-path = File path
audio-file-metadata = { $rate } kHz · { $channels } · { $duration }
audio-file-bit-depth = { $bits } bit
audio-factor = { $pct } · { $st } st
audio-file-replace-shortened = “{ $name }” replaced · clip trimmed to the end of the file
audio-file-replace-failed = Could not replace the file: { $reason }
audio-file-no-audio = No readable audio file
audio-file-too-short = The file ends before the clip’s source start.
audio-zoom-in = Zoom In Waveform
audio-zoom-out = Zoom Out Waveform
audio-waveform-pending = Calculating waveform…
audio-waveform-controls = Waveform controls
# Linked audio import and browsing (epic #8).
menu-import-audio = Import Audio File…
menu-new-audio-track = New Audio Track
audio-import-confirm = Import
status-audio-import-failed = Could not import audio file · { $file } · { $why }
status-audio-imported =
    { $count ->
        [one] 1 audio file linked
       *[other] { $count } audio files linked
    }
audio-file-empty = The audio file is empty.
missing-audio-files = Files Missing
missing-plugins-and-files = Plugins and Files Missing
missing-audio-text = Linked audio files are missing from “{ $project }”. Open a clip and choose “Replace File…” to locate its source. The clips are kept.
missing-continue-files = Continue
missing-audio-open-clip = Open Clip
missing-audio-chip = AUDIO
missing-audio-place = Track { $track } · Clip { $clip }

# Audio input, monitoring and recording UI (approved audio proposal).
audio-input-none = No input
audio-input-device-source = Input: { $name }
audio-input-device-monitor = Output monitor: { $name }
audio-input-mono = Input { $n }
audio-input-pair = Input { $a }+{ $b }
audio-input-bus = Bus { $name }
audio-input-missing-bus = unavailable
audio-input-unavailable = { $name } · unavailable
audio-input-tip = Input: { $name }
audio-input-tip2 = Click to choose the input
audio-input-title = Input · { $track }
audio-input-sources = Sources
audio-input-section-bus = Buses
audio-input-clear = No Input
audio-input-loop = would create a loop
audio-input-no-device = No audio input available
audio-input-open-settings = Open Audio Settings…
audio-arm-no-input = No input selected · click the input line
audio-arm-unavailable = { $name } · unavailable
audio-input-open-failed = Cannot open audio input · { $why }
audio-input-meter = Input level { $track }
audio-monitoring = Monitoring
ui-monitor-off = off
ui-monitor-auto = auto
ui-monitor-on = on
ui-monitor-menu-off = Off
ui-monitor-menu-auto = Auto (When Armed)
ui-monitor-menu-on = On
ui-monitor-tip2 = Right-click: Auto
ui-monitor-auto-mark = A
audio-monitor-auto-armed = monitored while armed
audio-bus-record-short = REC
routing-record-bus = Record Bus
routing-record-bus-name = Record bus, { $bus }
routing-record-bus-tip = Creates an audio track that records this bus
routing-record-bus-track = { $bus } Recording
status-record-bus-created = { $track } created · input bus { $bus } · R records
mixer-audio = Audio
audio-recordings-provisional = Temporary recordings
audio-recordings-provisional-tip = Recordings are kept in { $folder } for now and move into the project when you save.
audio-file-provisional = provisional
audio-file-provisional-note = Kept in { $folder } for now and moves into the project when you save.
audio-take-remove = Remove take “{ $name }”
audio-take-rename = Rename Take
a11y-audio-takes-suffix = , take { $take } of { $count }
audio-input-change-recording = Finish recording before changing the input.
audio-clip-context-menu = Audio Clip
audio-clip-context-mute = Mute
audio-clip-context-duplicate = Duplicate
audio-clip-context-split = Split

# Live recording waveform toolbar.
audio-live-waveform = Waveform · live
audio-live-input = Input { $input }
audio-live-peak = Peak { $db } dBFS
audio-live-duration = Duration { $time }
# Audio recording lifecycle and file failures.
audio-record-input-unavailable = No audio input available for “{ $track }” · choose an input
audio-record-finalizing = Finalizing recording…
audio-record-folder-unavailable = No recording folder available
audio-record-failed = Recording failed: { $reason }
audio-record-move-failed = Recording remains provisional: { $reason }
audio-record-complete = Recording finished · { $count } takes
audio-record-apply-failed = Could not add recording to the project · files are preserved
audio-monitor-tip = Monitoring: { $mode }
status-recording-input = ● Recording · { $tracks } · { $input }

# Approved #14 Warp UI: exclusive playback mode and full-source interpretation.
audio-clip-mode = Mode
audio-clip-fixed = Fixed
audio-clip-warp = Warp
audio-clip-mode-help = Fixed: original speed. Re-Pitch: follow tempo, change pitch. Warp: follow tempo, preserve pitch.
audio-clip-source-bars-help = Entire active source file in the current meter, not the trimmed clip. No tempo detection.
audio-clip-duration = Duration
audio-clip-duration-value = Duration { $ratio }×
audio-clip-duration-help = Output duration / source duration: { $ratio }× · original tempo / project tempo
audio-clip-preserve-pitch = Preserve pitch
audio-clip-original-speed = Original speed
audio-source-ruler = Source time
audio-source-time = Source { $source } ↔ Song { $song }
audio-warp-error = Warp error observed: { $reason }
audio-source-ruler-short = Source
audio-song-ruler = Song

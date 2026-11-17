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

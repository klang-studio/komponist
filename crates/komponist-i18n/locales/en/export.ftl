## S08 WAV export: settings, device wait, progress and the status texts (`dialogs/export.rs`, `export.rs`).

# S08 title of the settings dialog.
export-title = Export WAV
flatten-title = Rendering { $track }
flatten-waiting = Waiting for track devices…
flatten-done = Flattened { $track } to audio
flatten-cancelled = Flatten cancelled
flatten-failed = Could not flatten track · { $error }
flatten-device-failed = Device unavailable · { $device }
flatten-no-clips = Choose a MIDI track with clips.
flatten-track-changed = The track or its signal sources changed during rendering.
flatten-worker-failed = Rendering was interrupted.
flatten-missing-sample = Sample missing · { $file }
flatten-pad-failed = Could not load { $device } pad { $pad } “{ $sample }”.
flatten-state-failed = Could not capture the current state of “{ $device }” for undo.
flatten-warp-failed = Warp in audio clip “{ $clip }” · { $reason }

# S08 row label and name of the range control.
export-range = Range

# S08 range segment: the whole song.
export-song = Whole Song

# S08 range segment without a current loop; with one it is `export-loop-bars`.
export-loop = Loop

# S08 range segment with a loop; `$from` and `$to` are bar numbers.
export-loop-bars = Loop { $from }–{ $to }

# S08 range segment: the selection.
export-selection = Selection

# S08 row label of the range's start and end.
export-from-to = From – to

# S08 name of the range start.
export-from = From

# S08 name of the range end.
export-to = To

# S08 name of the range's length.
export-length = Length

# S08 row label and name of the tail field.
export-tail = Tail

# S08 hint next to the tail field.
export-tail-hint = for reverb and delay

# S08 row label and name of the sample rate dropdown.
export-rate = Sample rate

# S08 row label and name of the bit depth control.
export-bit-depth = Bit depth

# S08 bit depth segment.
export-bits-16 = 16 bit

# S08 bit depth segment.
export-bits-24 = 24 bit

# S08 bit depth segment.
export-bits-32 = 32 bit float

# S08 row label and name of the normalise switch.
export-normalize = Normalize

# S08 next to the switch when on.
export-normalize-on = to −1.0 dBFS

# S08 next to the switch when off.
export-normalize-off = off

# S08 row label and name of the dither switch.
export-dither = Dithering

# S08 row label and name of the file name field.
export-file = File

# S08 row label and name of the folder box.
export-folder = Folder

# S08 button next to the folder: choose another one.
export-change = Change

# S08 primary button.
export-export = Export

# S08 name of the progress bar.
export-progress = Progress

# S08 progress row: render speed.
export-speed = Speed

# S08 progress row: sample peak so far.
export-peak = Peak so far

# S08 progress row: time left.
export-remaining = Time left

# Text of the device wait list (no screen).
export-waiting = The export waits until all devices are running.

# A device that is still loading (prototype "· lädt …").
export-loading = loading…

# A device whose plugin is not installed.
export-missing = missing

# A device whose bridge crashed.
export-crashed = crashed

# A device that failed to load (as the device card's "Lässt sich nicht laden").
export-load-failed = cannot be loaded

# A device that stopped answering (as the device card's "Hängt").
export-unresponsive = not responding

# Button of the device wait list: go on without the devices.
export-without = Export Without Them

# Link of the status bar message after an export (S08 "fertig · Statuszeile").
export-show-in-folder = Show in Folder

# Title of the file browser for "Ändern".
export-folder-title = Choose Export Folder

# Status: nothing to export (prototype `openExport`).
export-empty = Empty project · nothing to export

# Status: the export was cancelled.
export-cancelled = Export canceled

# Title of the progress and device dialogs; `$file` is the file name.
export-exporting = Exporting { $file }

# S08 rendered time of the total; `$done` and `$total` are times.
export-of = { $done } of { $total }

# S08 render speed; `$factor` is "6.2".
export-speed-value = { $factor } × real time

# S08 time left in seconds, rounded up.
export-remaining-seconds = about { $s } s

# S08 time left in minutes, from one minute on.
export-remaining-minutes = about { $m } min

# Status after an export (prototype); `$file` is the file name, `$length` "00:33", `$bits` 16, 24 or 32.
export-done = { $file } exported · { $length } · { $bits } bit

# Added to `export-done` when one device dropped out of the render.
export-dropped-one = { $name } dropped out

# Added to `export-done` when several devices dropped out; `$names` is a comma separated list.
export-dropped-many = { $names } dropped out

# Status: the export failed; `$why` is the cause.
export-failed = Export failed · { $why }

# Status: an audio clip's linked file could not be opened; `$file` is its path.
export-missing-audio = Export failed · Audio file is missing or cannot be opened: { $file }

# Status: Warp failed on the named audio clip during export.
export-warp-failed = Export failed · Warp in audio clip “{ $clip }” · { $why }
export-warp-settings = The Warp settings are unsupported
export-warp-input = The audio contains invalid samples
export-warp-position = The Warp position is out of range
export-warp-backend = Time stretching failed
export-warp-source-read = The audio file cannot be read
export-warp-source-seek = The position in the audio file cannot be reached
export-warp-source-resample = The audio sample rate cannot be converted

# Status: "Im Ordner zeigen" failed; `$why` is the cause.
export-open-failed = Cannot open the folder · { $why }

## S08 WAV export: settings, device wait, progress and the status texts (`dialogs/export.rs`, `export.rs`).

# S08 title of the settings dialog.
export-title = WAV exportieren

# S08 row label and name of the range control.
export-range = Bereich

# S08 range segment: the whole song.
export-song = Ganzer Song

# S08 range segment without a current loop; with one it is `export-loop-bars`.
export-loop = Loop

# S08 range segment with a loop; `$from` and `$to` are bar numbers.
export-loop-bars = Loop { $from }–{ $to }

# S08 range segment: the selection.
export-selection = Auswahl

# S08 row label of the range's start and end.
export-from-to = Von – bis

# S08 name of the range start.
export-from = Von

# S08 name of the range end.
export-to = Bis

# S08 name of the range's length.
export-length = Länge

# S08 row label and name of the tail field.
export-tail = Nachlauf

# S08 hint next to the tail field.
export-tail-hint = für Hall und Delay

# S08 row label and name of the sample rate dropdown.
export-rate = Abtastrate

# S08 row label and name of the bit depth control.
export-bit-depth = Bittiefe

# S08 bit depth segment.
export-bits-16 = 16 bit

# S08 bit depth segment.
export-bits-24 = 24 bit

# S08 bit depth segment.
export-bits-32 = 32 bit float

# S08 row label and name of the normalise switch.
export-normalize = Normalisieren

# S08 next to the switch when on.
export-normalize-on = auf −1.0 dBFS

# S08 next to the switch when off.
export-normalize-off = aus

# S08 row label and name of the dither switch.
export-dither = Dithering

# S08 row label and name of the file name field.
export-file = Datei

# S08 row label and name of the folder box.
export-folder = Ordner

# S08 button next to the folder: choose another one.
export-change = Ändern

# S08 primary button.
export-export = Exportieren

# S08 name of the progress bar.
export-progress = Fortschritt

# S08 progress row: render speed.
export-speed = Geschwindigkeit

# S08 progress row: sample peak so far.
export-peak = Spitze bisher

# S08 progress row: time left.
export-remaining = Restzeit

# Text of the device wait list (no screen).
export-waiting = Der Export wartet, bis alle Geräte laufen.

# A device that is still loading (prototype "· lädt …").
export-loading = lädt …

# A device whose plugin is not installed.
export-missing = fehlt

# A device whose bridge crashed.
export-crashed = abgestürzt

# A device that failed to load (as the device card's "Lässt sich nicht laden").
export-load-failed = lässt sich nicht laden

# A device that stopped answering (as the device card's "Hängt").
export-unresponsive = hängt

# Button of the device wait list: go on without the devices.
export-without = Ohne sie exportieren

# Link of the status bar message after an export (S08 "fertig · Statuszeile").
export-show-in-folder = Im Ordner zeigen

# Title of the file browser for "Ändern".
export-folder-title = Export-Ordner wählen

# Status: nothing to export (prototype `openExport`).
export-empty = Leeres Projekt · nichts zu exportieren

# Status: the export was cancelled.
export-cancelled = Export abgebrochen

# Title of the progress and device dialogs; `$file` is the file name.
export-exporting = Exportiere { $file }

# S08 rendered time of the total; `$done` and `$total` are times.
export-of = { $done } von { $total }

# S08 render speed; `$factor` is "6.2".
export-speed-value = { $factor } × Echtzeit

# S08 time left in seconds, rounded up.
export-remaining-seconds = ca. { $s } s

# S08 time left in minutes, from one minute on.
export-remaining-minutes = ca. { $m } min

# Status after an export (prototype); `$file` is the file name, `$length` "00:33", `$bits` 16, 24 or 32.
export-done = { $file } exportiert · { $length } · { $bits } bit

# Added to `export-done` when one device dropped out of the render.
export-dropped-one = { $name } ist ausgefallen

# Added to `export-done` when several devices dropped out; `$names` is a comma separated list.
export-dropped-many = { $names } sind ausgefallen

# Status: the export failed; `$why` is the cause.
export-failed = Export fehlgeschlagen · { $why }

# Status: an audio clip's linked file could not be opened; `$file` is its path.
export-missing-audio = Export fehlgeschlagen · Audiodatei fehlt oder lässt sich nicht öffnen: { $file }

# Status: Warp failed on the named audio clip during export.
export-warp-failed = Export fehlgeschlagen · Warp im Audioclip „{ $clip }“ · { $why }
export-warp-settings = Die Warp-Einstellungen werden nicht unterstützt
export-warp-input = Die Audiodaten enthalten ungültige Samples
export-warp-position = Die Warp-Position liegt außerhalb des gültigen Bereichs
export-warp-backend = Die Zeitdehnung ist fehlgeschlagen
export-warp-source-read = Die Audiodatei lässt sich nicht lesen
export-warp-source-seek = Die Position in der Audiodatei lässt sich nicht ansteuern
export-warp-source-resample = Die Samplerate der Audiodatei lässt sich nicht umrechnen

# Status: "Im Ordner zeigen" failed; `$why` is the cause.
export-open-failed = Ordner lässt sich nicht öffnen · { $why }

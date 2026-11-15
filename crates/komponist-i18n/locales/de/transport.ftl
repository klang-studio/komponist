## Transport bar (`Screens.dc.html` shell markup and `tp`): names of the controls, tooltips, readouts and the "⋯" menu.

# Play button: name and tooltip.
transport-play = Wiedergabe

# Stop button: name and tooltip.
transport-stop = Stopp

# Record button: name and tooltip.
transport-record = Aufnahme

# Loop toggle.
transport-loop = Loop

# Metronome toggle.
transport-metronome = Metronom

# Count-in toggle (one bar).
transport-count-in = Vorzähler 1

# Name of the position readout (10.2.120).
transport-position = Position

# Name of the time readout (00:17.902).
transport-time = Zeit

# Name of the tempo field.
transport-tempo = Tempo

# Name of the meter readout ("4/4", as in the inspector, S06).
transport-meter = Taktart

# Name of the CPU readout.
transport-cpu = CPU

# CPU load as shown, `$pct` is the rounded number.
transport-cpu-value = { $pct } %

# Name of the master meter.
transport-master = Pegel Master

# Name of the "⋯" menu button.
transport-menu = Menü

# Chip without an audio device (S07) and the status bar's text for it.
transport-no-device = Kein Audio-Gerät

# AccessKit text of the project name while the project has unsaved changes.
transport-project-unsaved = { $name }, nicht gespeichert

## The "⋯" menu (prototype `menu`).

# Menu item: new project.
transport-menu-new = Neues Projekt

# Menu item: open a project (needs more input).
transport-menu-open = Projekt öffnen …

# Menu item: save.
transport-menu-save = Speichern

# Menu item: save under another name (needs more input).
transport-menu-save-as = Speichern unter …

# Menu item: copy the samples next to the project, then save.
transport-menu-collect = Sammeln und speichern

# Menu item: export a WAV file (needs more input).
transport-menu-export = WAV exportieren …

# Menu item: back to the start screen.
transport-menu-start = Zum Startbildschirm

## Tooltips of the icon toggles (the names above stay the AccessKit names).

# Tooltip of the count-in toggle while it is on, `$bars` is the number of bars.
transport-count-in-tip = Vorzähler: { $bars } { $bars ->
        [one] Takt
       *[other] Takte
    }

# Tooltip of the count-in toggle while it is off.
transport-count-in-off-tip = Vorzähler: aus

## The master display left of the CPU readout (#34, master-scope proposal, variant A): its accessible name says
## the picture, the tooltip (also its accessible description) what it shows and how to switch.

# Name of the master display while it shows the waveform.
transport-scope-oscilloscope = Master-Anzeige: Oszilloskop

# Name of the master display while it shows the spectrum.
transport-scope-spectrum = Master-Anzeige: Spektrum

# Tooltip and description of the master display while it shows the waveform.
transport-scope-oscilloscope-tip = Zeigt den Master-Ausgang als Wellenform. Klick wechselt zur Spektrum-Anzeige.

# Tooltip and description of the master display while it shows the spectrum.
transport-scope-spectrum-tip = Zeigt das Frequenzspektrum des Master-Ausgangs. Klick wechselt zur Wellenform.

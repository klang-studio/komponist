## Mixer (`Screens.dc.html` `mixer()`, the `isMixer` markup, S04 notes; `views/mixer`). Sends and buses are in `routing.ftl`.

# Section label of the inserts (shown upper case by `type.label`).
mixer-inserts = Inserts

# Name of the master strip.
mixer-master = Master

# Second line of the master strip (`inst:'Ausgang 1–2'`).
mixer-master-output = Ausgang 1–2

# Accessible name and tooltip of the "+" slot ("„+“ öffnet die Gerätesuche").
mixer-add-effect = Effekt hinzufügen

# Tooltip detail of the "+" slot.
mixer-add-effect-detail = Öffnet die Gerätesuche für diesen Kanal

# Tooltip detail of an insert slot (S04 "Insert-Slot").
mixer-slot-detail = Klick öffnet · Ziehen sortiert um · Alt-Klick Bypass

# Tooltip of the peak value (S04 "Peak-Wert / LED").
mixer-peak-tip = Peak und Clip zurücksetzen

# Tooltip detail of the peak value.
mixer-peak-detail = Alt-Klick für alle Kanäle

# Status of a loading insert.
mixer-slot-loading = lädt …

# Status of a crashed insert.
mixer-slot-crashed = abgestürzt

# Status of an insert that failed to load (as the device card's "Lässt sich nicht laden").
mixer-slot-load-failed = lässt sich nicht laden

# Status of an insert that stopped answering (as the card's "Hängt").
mixer-slot-unresponsive = hängt

# Status of a missing insert (S06 "Plugin fehlt").
mixer-slot-missing = Plugin fehlt

# Status of a bypassed insert.
mixer-slot-bypass = Bypass

# Accessible name of a strip (rule 12).
mixer-strip = Kanal { $name }

# Accessible name of the fader.
mixer-volume = Lautstärke { $name }

# Accessible name of the level under the fader.
mixer-volume-value = Lautstärke { $name }, Wert

# Accessible name of the pan knob.
mixer-pan = Pan { $name }

# Accessible name of the meter.
mixer-meter = Pegel { $name }

# Accessible name of the peak value.
mixer-peak = Peak { $name }

# Accessible name of an insert slot.
mixer-insert = Insert { $plugin }

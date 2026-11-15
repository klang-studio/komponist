## Mixer (`Screens.dc.html` `mixer()`, the `isMixer` markup, S04 notes; `views/mixer`). Sends and buses are in `routing.ftl`.

# Section label of the inserts (shown upper case by `type.label`).
mixer-inserts = Inserts

# Name of the master strip.
mixer-master = Master

# Second line of the master strip (`inst:'Ausgang 1–2'`).
mixer-master-output = Output 1–2

# Accessible name and tooltip of the "+" slot ("„+“ öffnet die Gerätesuche").
mixer-add-effect = Add effect

# Tooltip detail of the "+" slot.
mixer-add-effect-detail = Opens the device search for this channel

# Tooltip detail of an insert slot (S04 "Insert-Slot").
mixer-slot-detail = Click opens · Drag reorders · Alt+click bypass

# Tooltip of the peak value (S04 "Peak-Wert / LED").
mixer-peak-tip = Reset peak and clip

# Tooltip detail of the peak value.
mixer-peak-detail = Alt+click for all channels

# Status of a loading insert.
mixer-slot-loading = loading…

# Status of a crashed insert.
mixer-slot-crashed = crashed

# Status of an insert that failed to load (as the device card's "Lässt sich nicht laden").
mixer-slot-load-failed = failed to load

# Status of an insert that stopped answering (as the card's "Hängt").
mixer-slot-unresponsive = not responding

# Status of a missing insert (S06 "Plugin fehlt").
mixer-slot-missing = Plugin missing

# Status of a bypassed insert.
mixer-slot-bypass = Bypass

# Accessible name of a strip (rule 12).
mixer-strip = Channel { $name }

# Accessible name of the fader.
mixer-volume = Volume { $name }

# Accessible name of the level under the fader.
mixer-volume-value = Volume { $name }, value

# Accessible name of the pan knob.
mixer-pan = Pan { $name }

# Accessible name of the meter.
mixer-meter = Level { $name }

# Accessible name of the peak value.
mixer-peak = Peak { $name }

# Accessible name of an insert slot.
mixer-insert = Insert { $plugin }

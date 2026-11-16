## Routing UI shared by the mixer, the arrangement, the inspector and the dock popover (routing proposal "Texte" and "AccessKit-Namen", internal notes; `views/routing`).

# Section label of the sends.
routing-sends = Sends

# Section label of the buses in the mixer, and of the arrangement section below "Neue Spur".
routing-buses = Busse

# Section label of a bus's inputs in the inspector.
routing-inputs = Eingänge

# The output dropdown and its row label in the popover.
routing-output = Ausgang

# The master as an output target.
routing-master = Master

# Output menu: a new effect bus around the track.
routing-new-bus = Neuer Bus …

# Send popover: a new effect bus with a send to it.
routing-new-effect-bus = Neuer Effekt-Bus

# Output menu: a new group around the track.
routing-new-group = Neue Gruppe …

# The "+" row of a send list, and its tooltip.
routing-add-send = Send hinzufügen

# Accessible name of the "+" row of a send list.
routing-add-send-name = Send hinzufügen, { $track }

# Pre fader as a menu item.
routing-pre = Vor Fader

# Post fader as a menu item.
routing-post = Nach Fader

# Pre fader as the tooltip of the "pre" chip.
routing-pre-tip = Vor Fader

# Post fader as the tooltip of the "post" chip.
routing-post-tip = Nach Fader

# Short form of pre fader in the send row (lit chip).
routing-pre-short = pre

# Short form of post fader in the send row.
routing-post-short = post

# The value of a disabled send.
routing-off = aus

# Context menu of a track: group the selection (Strg G).
routing-group-tracks = In Gruppe zusammenfassen

# Context menu of a track: an effect bus fed by this track.
routing-send-to-new-bus = Send zu neuem Bus

# Context menu of a track: the sends submenu, and the title of the popover with the bus list.
routing-send-to = Send an …

# Context menu of a track: the output submenu.
routing-output-to = Ausgang

# Context menu of a send: remove it; tooltip of its "×".
routing-remove = Entfernen

# Context menu of a send: switch it off.
routing-switch-off = Ausschalten

# Context menu of a send: switch it on.
routing-switch-on = Einschalten

# Context menu of a group: dissolve it, the tracks stay.
routing-ungroup = Gruppe auflösen

# Context menu of an effect bus.
routing-delete-bus = Bus löschen

# Context menu: rename.
routing-rename = Umbenennen

# Context menu: colour.
routing-color = Farbe

# The inspector section and the tooltip of the dock chip.
routing-routing = Routing

# The inspector's sidechain line.
routing-sidechain-for = Sidechain-Quelle für:

# Kind of a group (line under a strip name, target row suffix).
routing-kind-group = Gruppe

# Kind of an effect bus.
routing-kind-effect-bus = Effekt-Bus

# The "Bus" type of the new-track dialog.
routing-kind-bus = Bus

# The reason a bus takes no clips.
routing-no-clips = Busse nehmen keine Clips auf.

# The reason a drop of an instrument on a bus is refused.
routing-effects-only = Busse nehmen nur Effekte auf.

# Tooltip of a strip's "S" that only follows a solo.
routing-solo-follows-tip = Läuft mit, weil eine solo geschaltete Spur auf diesen Bus ausgibt.

# Detail line of the dock chip's tooltip.
routing-popover-hint = Strg+Klick auf den Chip: ganze Liste im Inspektor

# Same on macOS: ⌘ instead of Strg.
routing-popover-hint-mac = ⌘+Klick auf den Chip: ganze Liste im Inspektor

# Inspector: nothing routes to this bus.
routing-no-inputs = Noch nichts geroutet

# Accessible name of a bus strip.
routing-bus-strip = Busspur { $name }

# Accessible name of a send row with a level; `$tap` is `pre` or `post`.
routing-send-on = Send { $target }, { $db } dB, { $tap ->
        [pre] vor Fader
       *[post] nach Fader
    }

# Accessible name of a disabled send row; `$tap` is `pre` or `post`.
routing-send-off = Send { $target }, aus, { $tap ->
        [pre] vor Fader
       *[post] nach Fader
    }

# Accessible name of the pre/post switch of a send; `$tap` is `pre` or `post`.
routing-send-tap = Send { $track } an { $target }: { $tap ->
        [pre] Vor Fader
       *[post] Nach Fader
    }

# Accessible name of the output button.
routing-output-name = Ausgang: { $target }

# Accessible name of the output list.
routing-output-list = Ausgang { $track }

# Accessible name of the dock chip.
routing-chip =
    { $sends ->
        [0] Routing { $track }: Ausgang { $output }
        [one] Routing { $track }: Ausgang { $output }, 1 Send
       *[other] Routing { $track }: Ausgang { $output }, { $sends } Sends
    }

# Title of the dock popover.
routing-popover-title = Routing · { $track }

# Accessible name of a group header; `$state` is `folded` or `unfolded`.
routing-group-name = Gruppe { $name }, { $state ->
        [folded] eingeklappt
       *[unfolded] ausgeklappt
    }

# Accessible name of an effect bus header.
routing-effect-bus-name = Effekt-Bus { $name }

# The line under a group's name.
routing-tracks =
    { $n ->
        [one] 1 Spur
       *[other] { $n } Spuren
    }

# The line under an effect bus's name.
routing-sends-count =
    { $n ->
        [one] 1 Send
       *[other] { $n } Sends
    }

# The lane of an effect bus in the arrangement.
routing-bus-lane =
    Effekt-Bus · { $sends ->
        [one] 1 Send
       *[other] { $sends } Sends
    } · keine Clips

# The lane of a group in the arrangement.
routing-group-lane = Summe der Spuren (nur Anzeige)

# Accessible name of a menu entry that is blocked by a feedback loop.
routing-blocked = { $label }, nicht möglich: Rückkopplung

# Why an output is not possible: the target already feeds the source.
routing-reason-feedback = Nicht möglich: „{ $target }“ speist „{ $source }“ schon (Rückkopplung).

# Why an output is not possible: a track cannot output to itself.
routing-reason-self = Nicht möglich: „{ $track }“ kann nicht an sich selbst ausgeben.

# Why a send target is not possible: the send exists.
routing-reason-used = Schon verwendet: { $target }

# Why a send target is not possible: a group is an output, not a send target.
routing-reason-group = „{ $target }“ ist eine Gruppe: Eine Gruppe nimmt man als Ausgang, nicht als Send.

# Tooltip of the output button.
routing-output-tip = Ausgang von „{ $track }“

# The mixer's accessible description of a bus strip that only follows a solo.
routing-solo-follows = Solo { $name }, läuft mit

# Accessible name of the "×" of a send row.
routing-remove-send = Send an { $name } entfernen

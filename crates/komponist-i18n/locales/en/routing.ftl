## Routing UI shared by the mixer, the arrangement, the inspector and the dock popover (routing proposal "Texte" and "AccessKit-Namen", internal notes; `views/routing`).

# Section label of the sends.
routing-sends = Sends

# Section label of the buses in the mixer, and of the arrangement section below "Neue Spur".
routing-buses = Buses

# Section label of a bus's inputs in the inspector.
routing-inputs = Inputs

# The output dropdown and its row label in the popover.
routing-output = Output

# The master as an output target.
routing-master = Master

# Output menu: a new effect bus around the track.
routing-new-bus = New Bus…

# Send popover: a new effect bus with a send to it.
routing-new-effect-bus = New Effect Bus

# Output menu: a new group around the track.
routing-new-group = New Group…

# The "+" row of a send list, and its tooltip.
routing-add-send = Add Send

# Accessible name of the "+" row of a send list.
routing-add-send-name = Add send, { $track }

# Pre fader as a menu item.
routing-pre = Pre Fader

# Post fader as a menu item.
routing-post = Post Fader

# Pre fader as the tooltip of the "pre" chip.
routing-pre-tip = Pre fader

# Post fader as the tooltip of the "post" chip.
routing-post-tip = Post fader

# Short form of pre fader in the send row (lit chip).
routing-pre-short = pre

# Short form of post fader in the send row.
routing-post-short = post

# The value of a disabled send.
routing-off = off

# Context menu of a track: group the selection (Strg G).
routing-group-tracks = Group Tracks

# Context menu of a track: an effect bus fed by this track.
routing-send-to-new-bus = Send to New Bus

# Context menu of a track: the sends submenu, and the title of the popover with the bus list.
routing-send-to = Send To…

# Context menu of a track: the output submenu.
routing-output-to = Output

# Context menu of a send: remove it; tooltip of its "×".
routing-remove = Remove

# Context menu of a send: switch it off.
routing-switch-off = Switch Off

# Context menu of a send: switch it on.
routing-switch-on = Switch On

# Context menu of a group: dissolve it, the tracks stay.
routing-ungroup = Ungroup

# Context menu of an effect bus.
routing-delete-bus = Delete Bus

# Context menu: rename.
routing-rename = Rename

# Context menu: colour.
routing-color = Color

# The inspector section and the tooltip of the dock chip.
routing-routing = Routing

# The inspector's sidechain line.
routing-sidechain-for = Sidechain source for:

# Kind of a group (line under a strip name, target row suffix).
routing-kind-group = Group

# Kind of an effect bus.
routing-kind-effect-bus = Effect bus

# The "Bus" type of the new-track dialog.
routing-kind-bus = Bus

# The reason a bus takes no clips.
routing-no-clips = Buses hold no clips.

# The reason a drop of an instrument on a bus is refused.
routing-effects-only = Buses take effects only.

# Tooltip of a strip's "S" that only follows a solo.
routing-solo-follows-tip = Follows solo because a soloed track feeds this bus.

# Detail line of the dock chip's tooltip.
routing-popover-hint = Ctrl+click the chip: full list in the inspector

# Inspector: nothing routes to this bus.
routing-no-inputs = Nothing routed yet

# Accessible name of a bus strip.
routing-bus-strip = Bus strip { $name }

# Accessible name of a send row with a level; `$tap` is `pre` or `post`.
routing-send-on = Send { $target }, { $db } dB, { $tap ->
        [pre] pre fader
       *[post] post fader
    }

# Accessible name of a disabled send row; `$tap` is `pre` or `post`.
routing-send-off = Send { $target }, off, { $tap ->
        [pre] pre fader
       *[post] post fader
    }

# Accessible name of the pre/post switch of a send; `$tap` is `pre` or `post`.
routing-send-tap = Send { $track } to { $target }: { $tap ->
        [pre] Pre fader
       *[post] Post fader
    }

# Accessible name of the output button.
routing-output-name = Output: { $target }

# Accessible name of the output list.
routing-output-list = Output { $track }

# Accessible name of the dock chip.
routing-chip =
    { $sends ->
        [0] Routing { $track }: output { $output }
        [one] Routing { $track }: output { $output }, 1 send
       *[other] Routing { $track }: output { $output }, { $sends } sends
    }

# Title of the dock popover.
routing-popover-title = Routing · { $track }

# Accessible name of a group header; `$state` is `folded` or `unfolded`.
routing-group-name = Group { $name }, { $state ->
        [folded] folded
       *[unfolded] expanded
    }

# Accessible name of an effect bus header.
routing-effect-bus-name = Effect bus { $name }

# The line under a group's name.
routing-tracks =
    { $n ->
        [one] 1 track
       *[other] { $n } tracks
    }

# The line under an effect bus's name.
routing-sends-count =
    { $n ->
        [one] 1 send
       *[other] { $n } sends
    }

# The lane of an effect bus in the arrangement.
routing-bus-lane =
    Effect bus · { $sends ->
        [one] 1 send
       *[other] { $sends } sends
    } · no clips

# The lane of a group in the arrangement.
routing-group-lane = Sum of the tracks (display only)

# Accessible name of a menu entry that is blocked by a feedback loop.
routing-blocked = { $label }, not possible: feedback loop

# Why an output is not possible: the target already feeds the source.
routing-reason-feedback = Not possible: “{ $target }” already feeds “{ $source }” (feedback loop).

# Why an output is not possible: a track cannot output to itself.
routing-reason-self = Not possible: “{ $track }” cannot output to itself.

# Why a send target is not possible: the send exists.
routing-reason-used = Already used: { $target }

# Why a send target is not possible: a group is an output, not a send target.
routing-reason-group = “{ $target }” is a group: use a group as an output, not as a send.

# Tooltip of the output button.
routing-output-tip = Output of “{ $track }”

# The mixer's accessible description of a bus strip that only follows a solo.
routing-solo-follows = Solo { $name }, follows

# Accessible name of the "×" of a send row.
routing-remove-send = Remove send to { $name }

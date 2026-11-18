## Automation lanes and clip automation.

automation-title = Automation
automation-show = Show automation
automation-hide = Hide automation
automation-show-menu = Show Automation
automation-hide-menu = Hide Automation
automation-add-lane = + Automation Lane
automation-add-lane-menu = Add Automation Lane
automation-hide-lane = Hide Lane
automation-delete-lane = Delete Lane
automation-clear-lane = Delete All Points
automation-choose = Choose Parameter…
automation-search = Search parameters
automation-recent = Recently Moved
automation-mixer = Mixer
automation-send = Send { $bus }
automation-mapped-by = via { $mapping }
automation-no-params = No automatable parameters
automation-mode = Automation mode
automation-mode-lane = Automation mode of { $param }
automation-mode-all = Mode for All Lanes
automation-mode-off = Off
automation-mode-read = Read
automation-mode-touch = Touch
automation-mode-latch = Latch
automation-mode-off-hint = Automation does not play; controls apply
automation-mode-read-hint = Plays the automation
automation-mode-touch-hint = Writes while you hold a control
automation-mode-latch-hint = Writes from the first touch until stop
automation-writing = writing
automation-overridden = by clip automation
automation-shape = Shape
automation-shape-linear = Linear
automation-shape-curve = Curve
automation-shape-hold = Hold
automation-delete-points = Delete Points
automation-tooltip = { $param } · { $value } · { $pos }
automation-param-missing = ⚠ Parameter missing
automation-not-automatable = ⚠ not automatable
automation-not-on-track = ⚠ Device not on this track
automation-clip = Automation
automation-clip-remove = Remove clip automation
automation-lane-hint = Double-click: point · Ctrl+drag: draw · Alt+drag: copy · Shift: fine
automation-lane-hint-mac = Double-click: point · ⌘+drag: draw · ⌥+drag: copy · Shift: fine
automation-override-hint = Automated: applies until playback · Touch or Latch writes
automation-lane-empty = Double-click adds a point
automation-points = { $n ->
    [one] 1 point
    *[other] { $n } points
    }
automation-master = Master
automation-master-summary = Master · { $n ->
    [0] no effects
    [one] 1 effect
    *[other] { $n } effects
    }
automation-inspector-row = { $points }
automation-selection = { $track } · { $param } · { $n ->
    [one] 1 point selected
    *[other] { $n } points selected
    }
automation-clip-selection = { $clip } · Clip automation { $param } · { $n ->
    [one] 1 point selected
    *[other] { $n } points selected
    }
automation-chooser-status = { $track } · Automation lane
automation-chooser-hint = Arrows choose · Enter takes · Esc closes
automation-writing-status = { $track } · writing { $param } ({ $mode })
automation-writing-hint-touch = Release: back to the curve in 250 ms · Esc discards the pass
automation-writing-hint-latch = Holds the value until stop · Esc discards the pass
a11y-automation-lane = Automation { $param }, { $track }
a11y-automation-lane-missing = Automation { $param }, { $track }, { $problem }
a11y-automation-point = Point { $pos }, { $value }
a11y-automation-master = Master
a11y-automation-chooser = Lane parameter
a11y-automation-clip-lane = Clip automation { $param }
a11y-automation-curve-handle = Curve { $pos }

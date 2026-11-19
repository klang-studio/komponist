## Automation lanes and clip automation.

automation-title = Automation
automation-show = Automation zeigen
automation-hide = Automation ausblenden
automation-show-menu = Automation zeigen
automation-hide-menu = Automation ausblenden
automation-add-lane = + Automationsspur
automation-add-lane-menu = Automationsspur hinzufügen
automation-hide-lane = Automationsspur ausblenden
automation-delete-lane = Automationsspur löschen
automation-clear-lane = Alle Punkte löschen
automation-choose = Parameter wählen …
automation-search = Parameter suchen
automation-recent = Zuletzt bewegt
automation-mixer = Mixer
automation-send = Send { $bus }
automation-mapped-by = über { $mapping }
automation-no-params = Keine automatisierbaren Parameter
automation-mode = Automationsmodus
automation-mode-lane = Automationsmodus von { $param }
automation-mode-all = Modus für alle Automationsspuren
automation-mode-off = Aus
automation-mode-read = Lesen
automation-mode-touch = Touch
automation-mode-latch = Latch
automation-mode-off-hint = Automation spielt nicht, Regler gelten
automation-mode-read-hint = Spielt die Automation ab
automation-mode-touch-hint = Schreibt, solange du einen Regler hältst
automation-mode-latch-hint = Schreibt ab dem ersten Griff bis zum Stopp
automation-writing = schreibt
automation-overridden = durch Clip-Automation
automation-shape = Form
automation-shape-linear = Linear
automation-shape-curve = Kurve
automation-shape-hold = Halten
automation-delete-points = Punkte löschen
automation-tooltip = { $param } · { $value } · { $pos }
automation-param-missing = ⚠ Parameter fehlt
automation-not-automatable = ⚠ nicht automatisierbar
automation-not-on-track = ⚠ Gerät nicht auf dieser Spur
automation-clip = Automation
automation-clip-remove = Clip-Automation entfernen
automation-lane-hint = Doppelklick: Punkt · Strg-Ziehen: zeichnen · Alt-Ziehen: Kopie · Shift: fein
automation-lane-hint-mac = Doppelklick: Punkt · ⌘-Ziehen: zeichnen · ⌥-Ziehen: Kopie · Shift: fein
automation-override-hint = Automatisiert: gilt bis zur nächsten Wiedergabe · Touch oder Latch schreibt
automation-lane-empty = Doppelklick setzt einen Punkt
automation-points = { $n ->
    [one] 1 Punkt
    *[other] { $n } Punkte
    }
automation-master = Master
automation-master-summary = Master · { $n ->
    [0] keine Effekte
    [one] 1 Effekt
    *[other] { $n } Effekte
    }
automation-inspector-row = { $points }
automation-selection = { $track } · { $param } · { $n ->
    [one] 1 Punkt ausgewählt
    *[other] { $n } Punkte ausgewählt
    }
automation-clip-selection = { $clip } · Clip-Automation { $param } · { $n ->
    [one] 1 Punkt ausgewählt
    *[other] { $n } Punkte ausgewählt
    }
automation-chooser-status = { $track } · Automationsspur
automation-chooser-hint = Pfeiltasten wählen · Enter übernimmt · Esc schließt
automation-writing-status = { $track } · schreibt { $param } ({ $mode })
automation-writing-hint-touch = Loslassen: zurück zur Kurve in 250 ms · Esc verwirft den Durchgang
automation-writing-hint-latch = Hält den Wert bis zum Stopp · Esc verwirft den Durchgang
a11y-automation-lane = Automation { $param }, { $track }
a11y-automation-lane-missing = Automation { $param }, { $track }, { $problem }
a11y-automation-point = Punkt { $pos }, { $value }
a11y-automation-master = Master
a11y-automation-chooser = Parameter der Automationsspur
a11y-automation-clip-lane = Clip-Automation { $param }
a11y-automation-curve-handle = Kurve { $pos }

# Automation — UI terms and texts (de source, en translation)

Updated after the owner's decisions of 2026-11-16 (`DECISIONS.md`): mode per lane, master row.

Proposal for #64. German is the source (CLAUDE.md rule 1), English follows `docs/I18N.md` §6 (US English, Title Case
for titles, buttons, tabs, menu items; sentence case for labels, tooltips, accessible names, status texts and hints;
"35%" without a space; "Ctrl+drag"). Names stay as they are (komponist, Hexa, Volt, Presse, plugin and parameter names).
Values, positions and units are formatters passed in as variables (`$value` = "1.2 kHz", `$pos` = "10.1.000").

**New catalog file:** `automation.ftl` (de and en) for everything of the lanes, the chooser, the modes and the clip
editor; undo labels go to `undo-views.ftl` (static messages, `Lang::by_id`). Keys reuse existing messages where the
text is the same concept (`arrangement-plugin-missing`, `devices-enter-value`, `param-volume`, `param-pan`).

## 1. Glossary additions (I18N.md §6)

| German | English | Note |
|---|---|---|
| Automation | Automation | the feature, the toggle, the clip-editor button |
| Automationsspur | Automation Lane | follows the existing „Velocity-Spur“; never just „Spur“ (that is the track) |
| Clip-Automation | Clip Automation | not „Hüllkurve“ / "Envelope": those are the devices' ADSRs (Volt „Hüllkurven“) |
| Punkt | Point | a breakpoint |
| Automationsmodus | Automation Mode | |
| Aus / Lesen / Touch / Latch | Off / Read / Touch / Latch | Touch and Latch are loan words in German DAWs, like Mute and Solo (I18N §5) |
| schreibt | writing | state badge |
| Form: Linear / Kurve / Halten | Shape: Linear / Curve / Hold | segment shapes; not „Stufe“/"Step" (clashes with the step sequencer) |
| Zuletzt bewegt | Recently Moved | chooser section |
| Spur-Makro(s) | Track macro(s) / Track Macros | #72's terms (`macros-title`, `macros-slot`, `macros-ref`); the chooser and the lane labels use #72's keys, never a bare „Makro“ |

## 2. Messages (`automation.ftl`)

| Key | de | en | Where |
|---|---|---|---|
| `automation-title` | Automation | Automation | context-menu group, inspector row label, chooser a11y parent |
| `automation-show` | Automation zeigen | Show automation | track-header toggle tooltip + a11y name (sentence case) |
| `automation-hide` | Automation ausblenden | Hide automation | same toggle when lanes are shown |
| `automation-show-menu` | Automation zeigen | Show Automation | knob value menu item, track context menu (Title Case) |
| `automation-hide-menu` | Automation ausblenden | Hide Automation | track context menu |
| `automation-add-lane` | + Automationsspur | + Automation Lane | last row of the chooser |
| `automation-add-lane-menu` | Automationsspur hinzufügen | Add Automation Lane | track context menu |
| `automation-hide-lane` | Automationsspur ausblenden | Hide Lane | lane context menu |
| `automation-delete-lane` | Automationsspur löschen | Delete Lane | lane context menu |
| `automation-clear-lane` | Alle Punkte löschen | Delete All Points | lane context menu |
| `automation-choose` | Parameter wählen … | Choose Parameter… | chooser of an empty lane |
| `automation-search` | Parameter suchen | Search parameters | chooser search placeholder |
| `automation-recent` | Zuletzt bewegt | Recently Moved | chooser section (drawn upper case by `type.label`) |
| `automation-mixer` | Mixer | Mixer | chooser section |
| `automation-send` | Send { $bus } | Send { $bus } | chooser row and lane label of a send level |
| `automation-mapped-by` | über { $macro } | via { $macro } | chooser hint behind a parameter driven by a track macro (`$macro` = #72's `macros-ref`: „über Spur-Makro 1 „Open““) |
| `automation-no-params` | Keine automatisierbaren Parameter | No automatable parameters | chooser section of a device without any |
| `automation-mode` | Automationsmodus | Automation mode | mode chip tooltip + a11y name |
| `automation-mode-lane` | Automationsmodus von { $param } | Automation mode of { $param } | mode chip a11y name (the mode is per lane) |
| `automation-mode-all` | Modus für alle Automationsspuren | Mode for All Lanes | track-header and lane context menu, submenu with the four modes (bulk) |
| `automation-mode-off` | Aus | Off | mode chip / menu |
| `automation-mode-read` | Lesen | Read | |
| `automation-mode-touch` | Touch | Touch | |
| `automation-mode-latch` | Latch | Latch | |
| `automation-mode-off-hint` | Automation spielt nicht, Regler gelten | Automation does not play; controls apply | menu item subtitle |
| `automation-mode-read-hint` | Spielt die Automation ab | Plays the automation | |
| `automation-mode-touch-hint` | Schreibt, solange du einen Regler hältst | Writes while you hold a control | |
| `automation-mode-latch-hint` | Schreibt ab dem ersten Griff bis zum Stopp | Writes from the first touch until stop | |
| `automation-writing` | schreibt | writing | lane-header badge while a write runs |
| `automation-overridden` | durch Clip-Automation | by clip automation | label of a hatched clip span in a lane |
| `automation-shape` | Form | Shape | point/segment context submenu |
| `automation-shape-linear` | Linear | Linear | |
| `automation-shape-curve` | Kurve | Curve | |
| `automation-shape-hold` | Halten | Hold | |
| `automation-delete-points` | Punkte löschen | Delete Points | point context menu (`devices-enter-value` „Wert eingeben“ is reused above it) |
| `automation-tooltip` | { $param } · { $value } · { $pos } | { $param } · { $value } · { $pos } | value tooltip („Feedback · 64 % · 10.1.000“) |
| `automation-param-missing` | ⚠ Parameter fehlt | ⚠ Parameter missing | lane header line 2 |
| `automation-not-automatable` | ⚠ nicht automatisierbar | ⚠ not automatable | lane header line 2 |
| `automation-not-on-track` | ⚠ Gerät nicht auf dieser Spur | ⚠ Device not on this track | clip-automation lane |
| `automation-clip` | Automation | Automation | clip-editor toolbar toggle (icon + text) |
| `automation-clip-remove` | Clip-Automation entfernen | Remove clip automation | × at the top right of a clip lane (tooltip + a11y) |
| `automation-lane-hint` | Doppelklick: Punkt · Strg-Ziehen: zeichnen · Alt-Ziehen: Kopie · Shift: fein | Double-click: point · Ctrl+drag: draw · Alt+drag: copy · Shift: fine | status-bar hint over a lane |
| `automation-lane-hint-mac` | Doppelklick: Punkt · ⌘-Ziehen: zeichnen · ⌥-Ziehen: Kopie · Shift: fein | Double-click: point · ⌘+drag: draw · ⌥+drag: copy · Shift: fine | macOS variant (I18N §6) |
| `automation-override-hint` | Automatisiert: gilt bis zur nächsten Wiedergabe · Touch oder Latch schreibt | Automated: applies until playback · Touch or Latch writes | status bar after a manual change of an automated target (§6.6) |
| `automation-lane-empty` | Doppelklick setzt einen Punkt | Double-click adds a point | centred hint in an empty lane (`text.3`) |
| `automation-points` | { $n -> [one] 1 Punkt *[other] { $n } Punkte } | { $n -> [one] 1 point *[other] { $n } points } | inspector parameter row, status selection |
| `automation-master` | Master | Master | name of the master row (same word as the mixer's master strip) |
| `automation-master-summary` | Master · { $n -> [0] keine Effekte [one] 1 Effekt *[other] { $n } Effekte } | Master · { $n -> [0] no effects [one] 1 effect *[other] { $n } effects } | body text of the master row, like a bus row's summary |
| `automation-inspector-row` | { $points } | { $points } | inspector parameter section: „14 Punkte“, followed by the lane's mode chip |
| `automation-selection` | { $track } · { $param } · { $n -> [one] 1 Punkt ausgewählt *[other] { $n } Punkte ausgewählt } | { $track } · { $param } · { $n -> [one] 1 point selected *[other] { $n } points selected } | status bar, left part (lane focused) |
| `automation-clip-selection` | { $clip } · Clip-Automation { $param } · { $n -> [one] 1 Punkt ausgewählt *[other] { $n } Punkte ausgewählt } | { $clip } · Clip automation { $param } · { $n -> [one] 1 point selected *[other] { $n } points selected } | status bar, clip editor |
| `automation-chooser-status` | { $track } · Automationsspur | { $track } · Automation lane | status bar, left part while the chooser is open |
| `automation-chooser-hint` | Pfeiltasten wählen · Enter übernimmt · Esc schließt | Arrows choose · Enter takes · Esc closes | status hint while the chooser is open |
| `automation-writing-status` | { $track } · schreibt { $param } ({ $mode }) | { $track } · writing { $param } ({ $mode }) | status bar, left part, in `state.record-ink` while a write runs |
| `automation-writing-hint-touch` | Loslassen: zurück zur Kurve in 250 ms · Esc verwirft den Durchgang | Release: back to the curve in 250 ms · Esc discards the pass | status hint while Touch writes |
| `automation-writing-hint-latch` | Hält den Wert bis zum Stopp · Esc verwirft den Durchgang | Holds the value until stop · Esc discards the pass | status hint while Latch writes |

Reused from #72 (track macros): `macros-title` (chooser section), `macros-slot` / `macros-slot-named` (lane labels),
`macros-entry` (chooser rows), `macros-ref` (inside `automation-mapped-by`), `macros-lane-overridden` (lane header of a
driven parameter, after „⚠ “).

Reused, unchanged: `arrangement-plugin-missing` („⚠ Plugin fehlt: { $plugin }“) for a lane of a missing plugin,
`devices-enter-value` („Wert eingeben“), `param-volume` („Lautstärke“) and `param-pan` („Pan“) as lane labels and
chooser rows.

## 3. Accessible names (`automation.ftl`, `a11y-` prefix kept for search)

| Key | de | en | Node |
|---|---|---|---|
| `a11y-automation-lane` | Automation { $param }, { $track } | Automation { $param }, { $track } | lane group |
| `a11y-automation-lane-missing` | Automation { $param }, { $track }, { $problem } | Automation { $param }, { $track }, { $problem } | lane of a missing target (`$problem` = one of the warnings without „⚠ “) |
| `a11y-automation-point` | Punkt { $pos }, { $value } | Point { $pos }, { $value } | point (`Slider`), e.g. „Punkt 10.1.000, 64 %“ |
| `a11y-automation-master` | Master | Master | master-row group |
| `a11y-automation-chooser` | Parameter der Automationsspur | Lane parameter | chooser combo box |
| `a11y-automation-clip-lane` | Clip-Automation { $param } | Clip automation { $param } | clip-editor lane group |
| `a11y-automation-curve-handle` | Kurve { $pos } | Curve { $pos } | curve handle of a segment (drag target) |

## 4. Undo labels (`undo-views.ftl`, Title Case in English)

| Key | de | en |
|---|---|---|
| `undo-automation-add-point` | Automationspunkt setzen | Add Automation Point |
| `undo-automation-move-points` | Automationspunkte verschieben | Move Automation Points |
| `undo-automation-copy-points` | Automationspunkte kopieren | Copy Automation Points |
| `undo-automation-delete-points` | Automationspunkte löschen | Delete Automation Points |
| `undo-automation-draw` | Automation zeichnen | Draw Automation |
| `undo-automation-bend` | Kurve biegen | Bend Curve |
| `undo-automation-shape` | Kurvenform ändern | Change Curve Shape |
| `undo-automation-value` | Automationswert | Automation Value |
| `undo-automation-paste` | Automation einfügen | Paste Automation |
| `undo-automation-add-lane` | Automationsspur hinzufügen | Add Automation Lane |
| `undo-automation-delete-lane` | Automationsspur löschen | Delete Automation Lane |
| `undo-automation-target` | Automationsziel ändern | Change Automation Target |
| `undo-automation-clear` | Automationsspur leeren | Clear Automation Lane |
| `undo-automation-lane-height` | Höhe der Automationsspur | Automation Lane Height |
| `undo-automation-show` | Automation ein- oder ausblenden | Show or Hide Automation |
| `undo-automation-mode` | Automationsmodus | Automation Mode |
| `undo-automation-modes` | Automationsmodi | Automation Modes |
| `undo-automation-record` | Automation aufnehmen | Record Automation |
| `undo-automation-clip-add` | Clip-Automation hinzufügen | Add Clip Automation |
| `undo-automation-clip-remove` | Clip-Automation entfernen | Remove Clip Automation |

## 5. Length check (English must fit, rule 7)

- Mode chip: the widest text is „Lesen“ / "Touch"/"Latch" (5 letters) — the chip is sized for 5 letters at
  `type.ui` 12 plus the caret (≈ 52 px).
- Lane header row 2: "⚠ Parameter missing" (19 letters) fits in 190 px at `type.small` 11.
- Chooser width 280 (mockup 02 `-en`): the longest hint is "via track macro 1 “Open”"; parameter names truncate with
  "…" and show the full name as a tooltip, as the inspector does.
- English renderings of every mockup: `mockups/*-en.png` (new texts in English; the snapshot panels around them stay
  German). Nothing is cut off; "Choose Parameter…" fits the lane chooser (142 px).
- Status hint: the English lane hint is 64 characters, shorter than today's longest piano-roll hint.

# Track macros — spec and design proposal (#72, epic #57)

Status: **decided by the owner 2026-11-16** (`DECISIONS.md`; Q3 deviates from the proposal: every mapping has a bend).
Nothing here is built. Files: this spec, `DECISIONS.md`, `OPEN-QUESTIONS.md` (the questions as asked),
`mockups/*.html` + `*.png` (German; `*-en.png` the same in English; `mockups/render.sh` renders them with headless
Chrome). Visual reference: the app's Linux snapshots and `design/tokens.json` / `device-tokens.json`
(the design handoff is obsolete, owner 2026-11-16). Parallel spec: automation (#64) in `../automation/`; §10 is the
interface between the two, and it matches what `../automation/SPEC.md` §3.4 expects.

Mockups:

| File | Shows |
|---|---|
| `01-macros.png` | The 8 macros at the head of the device chain and in the inspector; driven parameters (badge + ring) |
| `02-mapping-mode.png` | Mapping mode: macro 1 selected, candidates highlighted, the mapping list (range, curve, invert) in the inspector |
| `03-driven-parameter.png` | A driven parameter: its value menu, the status text after a drag, the inspector's „Gesteuert von“ |
| `04-mapping-list.png` | Component sheet (2x): mapping list with the curve control per row (bending, tooltip), empty state, macro value menu, hover text, other widgets, collapsed card, EQ handle |
| `05-missing-target.png` | Component sheet (2x): missing plugin, missing parameter, error dot and tooltip, the missing device's card |

---

## 1. Goals and non-goals

**Goals (v1, milestone v0.4)**

1. Every track (MIDI tracks and buses) has **8 track macros**: freely assignable controls, each with a name, a value
   and a list of **mappings**.
2. A mapping drives one **target** of the same track — a parameter of a device in its chain (built-in devices, Hexa,
   CLAP and VST3 plugins, including parameters that are *device macros*) and the track's volume, pan and send levels
   (owner, Q2) — through its own range (minimum, maximum), direction (inverted) and bend (owner, Q3).
3. Assigning is direct: a **mapping mode** (choose a macro, move parameters), plus a value-menu entry for keyboard and
   screen-reader users.
4. It is always visible what a macro drives and what drives a parameter.
5. Macros are **automation targets** (#56) and **MIDI-learn targets** (#15); their changes reach the targets
   sample-accurately, allocation-free, in live playback and in the export alike.
6. Undo, file format with migration, AccessKit, `de` and `en` catalogs, the demo project (rule 15).

**Non-goals (v1)**

- Project-wide or master macros (owner: per track). **The master channel has no macros and is never a mapping
  target**, although it is an automation target in v1 (automation spec, owner 2026-11-16): a mapping only targets the
  owning track's devices, volume, pan and sends, and the master is not a track. Master insert parameters and the master
  volume are automated with the master row's own lanes, not with a track macro.
- Macro-to-macro mappings, macro presets/racks, saving a chain with its macros as a preset, randomise.
- Modulation (LFOs, envelopes driving parameters) — that is #15. A macro is not a modulator: it sets values.
- Several macros driving one parameter (§3.5).
- Changing the device macros (Hexa's 8 macros, CLAP parameters flagged as macros, ARCHITECTURE §8.5): they stay as
  they are; they are ordinary mapping targets.

## 2. Terms

| Concept | de (UI) | en (UI) | Meaning |
|---|---|---|---|
| **Track macro** | Spur-Makro, pl. Spur-Makros | Track macro, pl. Track macros (title: Track Macros) | One of the 8 controls of a track defined here |
| Device macro | Geräte-Makro | Device macro | A device's own macro parameter (Hexa's 8, a CLAP parameter flagged as macro). komponist names the concept only where it must (a heading); the parameters themselves keep their own names („Algorithmus“, a plugin's „Macro 1“) |
| Mapping | Zuordnung | Mapping | One macro → one target, with range and direction |
| Map (mode, action) | Zuordnen | Map | The mapping mode and its button |
| Target | Ziel | Target | What a mapping drives (as in the automation spec) |
| Driven | gesteuert | controlled | A parameter that a mapping drives |

**Naming rule (the point of the owner's request):** komponist's own texts never say a bare „Makro“ / "Macro". The
track concept is always „Spur-Makro“ / "Track macro"; the device concept, where komponist has to name it, is
„Geräte-Makro“ / "Device macro". A plugin's own parameter names („Makro 1“ in the Surge XT snapshot) are plugin text
and stay as they are; next to them our macros are recognisable by the word *Spur-* and by the **number badge**
(§8.1), which no device parameter has. An unnamed track macro is never called „Makro 5“: it shows its badge and
„frei“ / "unused" (§3.1).

## 3. Model (`komponist-model`, #73)

### 3.1 Data

```rust
pub struct Track {                          // existing fields unchanged
    #[serde(default, skip_serializing_if = "TrackMacros::is_default")]
    pub macros: TrackMacros,
}
/// Exactly 8 slots, never inserted, removed or reordered: the slot index is the macro's identity.
pub struct TrackMacros(pub [TrackMacro; 8]);
pub struct TrackMacro {
    pub name: String,                       // user content; "" = unnamed (display rule below)
    pub value: f32,                         // normalised 0.0 ..= 1.0, the base value (default 0.0)
    pub mappings: Vec<Mapping>,             // at most MAX_MAPPINGS (16), in creation order (= list order)
}
pub struct Mapping {
    pub target: AutomationTarget,           // shared with automation (§10); never `Macro`
    pub label: String,                      // last known "Device · Parameter" (display only, like a lane's label)
    pub min: f32,                           // normalised 0 ..= 1 in the target's own scale
    pub max: f32,                           // min ≤ max
    pub invert: bool,
    pub bend: f32,                          // −1.0 ..= 1.0, 0 = linear (owner, Q3)
}
```

- **Identity of a macro:** `(TrackId, index)`, index 0 ..= 7 (shown as 1 ..= 8). This is exactly the automation spec's
  `AutomationTarget::Macro { index }` (relative to the owning track). No macro ids: slots are fixed.
- **Identity of a target:** the automation spec's `AutomationTarget` (`../automation/SPEC.md` §3.1): `Param { device:
  DeviceId, param: ParamId }` with the stable `ParamId` (readable keys for internal devices and Hexa, `p<id>` for
  bridged plugins), `Volume`, `Pan`, `Send { send: SendId }`. One type for both features: whichever of #65 / #73
  lands first adds it (module `komponist_model::target`), the other uses it. A mapping with `Macro` is rejected.
- **Device macros are ordinary targets:** Hexa's 8 macros are parameters with stable keys (`algorithm`, `ratio2`,
  `index`, `feedback`, `attack`, `decay`, `sustain`, `release`, `komponist_builtin::Macro::key`); a CLAP parameter with
  `param_flags::MACRO` is `p<id>` like any other.
- **Value domain:** normalised 0–1 everywhere (macro value, `min`, `max`), as the automation spec decided for points,
  for the same reasons (one domain, no `ParamInfo` needed to edit or keep a mapping of a missing plugin, curves and
  ranges follow the parameter's own `Scale`: a cutoff range sweeps evenly in pitch). `f32` like automation points.
- **Display name** of a macro (computed, not stored): `name` if not empty; else the name of its first mapping's
  parameter (live name, else the `label`'s parameter part); else „frei“ / "unused". The automation lane label and the
  accessible name use the same rule with the slot name in front („Spur-Makro 1 · Open“).

### 3.2 The mapping function

`t = invert ? 1 − v : v`, `s = bend(t)`, `n = min + (max − min) · s`, then `plain = PlainMap::to_plain(n)` of the target (the automation
spec's `PlainMap`: scale, range, steps; stepped, enum and bool parameters snap, bool at 0.5). Pure, in
`komponist-model` next to `PlainMap`, used by the engine, the UI's displayed values and the tests.

`bend(t)` is the automation spec's `Curve { tension }` shape with `tension = bend`: `bend(t) = (e^(k·t) − 1) / (e^k − 1)`,
`k = 6 · bend`; `bend = 0` (|bend| < 1e-3) is the identity. Monotone, `bend(0) = 0`, `bend(1) = 1`, so the range ends
stay where the handles are; a positive bend starts slowly and ends fast, a negative one the other way round. The bend
works in the macro's direction (after the inversion), so „slow start“ always means „slow at the macro's 0 % end“. One
shared function in `komponist-model::automation` (or `target`) serves lanes and mappings, so they cannot differ.

### 3.3 Bend per mapping — owner decision: **yes, in v1** (Q3)

Each mapping has a `bend` (−1 … +1) on top of the parameter's normalised scale (which already makes frequency and time
logarithmic and levels follow the fader taper). The bend is the automation spec's curve shape, so the two features use
one formula, one look (a curve dragged up or down) and one word („Kurve“ / "Curve"). Uses: a filter that should open
mostly in the last third of the macro's travel, a reverb mix that should jump in early, two targets that should move
"against" each other with different speeds.

### 3.4 Invariants (enforced by the edits, checked by property tests)

1. Exactly 8 macros; `value`, `min`, `max` finite and in 0..=1, `bend` finite and in −1..=1; `min ≤ max` (dragging a handle past the other one
   stops there; „Umkehren“ is the way to reverse).
2. A target is a device of **this** track's chain (or this track's volume, pan, send), never `Macro`.
3. **One driver per target:** a target appears in at most one mapping of the track, across all 8 macros (§3.5).
4. At most `MAX_MAPPINGS = 16` mappings per macro (RT budget, §6.5); the edit refuses the 17th with a status text.
5. Only **automatable** targets (`ParamInfo::automatable`; plugins: `param_flags::AUTOMATABLE`, never read-only,
   hidden or the plugin's bypass parameter) — the same rule as automation, checked by the app when it maps (the model
   has no `ParamInfo`); a parameter that stops being automatable after a plugin update is kept and marked (§7).

### 3.5 One driver per parameter — decision

Mapping a parameter that another macro of the track already drives **moves** the mapping to the selected macro (one
undo step, status text „Cutoff: jetzt Spur-Makro 1“). Two macros on one parameter would need a combination rule (sum?
last writer? multiply?) that the user cannot see on the knob; Ableton allows it with "last wins" and it is a common
source of confusion. One driver keeps the badge on the knob unambiguous. owner decision Q6.

### 3.6 Edits (`komponist-model::edits::macros`, pure, unit-tested)

| Edit | Undo label (de / en) |
|---|---|
| `set_macro_value(track, index, v)` (knob drag = one gesture, as a fader) | „Spur-Makro „Open“ ändern“ / Change Track Macro “Open” |
| `rename_macro(track, index, name)` | Spur-Makro umbenennen / Rename Track Macro |
| `add_mapping(track, index, target, label, min, max, invert)` — moves an existing mapping of the target from another macro | Zuordnung hinzufügen / Add Mapping |
| `remove_mapping(track, index, target)` | Zuordnung entfernen / Remove Mapping |
| `clear_macro(track, index)` (all mappings; name and value stay) | Alle Zuordnungen entfernen / Remove All Mappings |
| `set_mapping_range(track, index, target, min, max)` (handle drag = one gesture) | Zuordnungsbereich ändern / Change Mapping Range |
| `set_mapping_invert(track, index, target, invert)` | Zuordnung umkehren / Invert Mapping |
| `set_mapping_bend(track, index, target, bend)` (drag = one gesture; Alt-click / ⌥-click resets to 0) | Zuordnungskurve ändern / Change Mapping Curve |

**Repairs in the same undo step as the edit that causes them** (the routing precedent: deleting a track repairs
every reference): `remove_device` and `replace_instrument` drop the mappings of that device; removing a send drops its
mappings; `remove_tracks` drops the whole track anyway. Undo brings device and mappings back together. A device move
within the chain changes nothing (ids). Should devices ever move between tracks (the automation spec mentions it),
the mappings to it are dropped (macros belong to their track), with the status text „2 Zuordnungen entfernt“. A
future track duplicate copies the macros with remapped device and send ids.

**Unmapping keeps the value:** removing a mapping (or the macro's mappings, or moving one) leaves the parameter at the
value the macro gave it. For built-in devices and Hexa the app writes that value into the device state in the same
undo step (`edits::set_device_state` through the device's `Field::set` / internal spec, as a card edit does); a
bridged plugin already holds it, and its next snapshot is stored outside the history (§9). owner decision Q9.

---

## 4. A direct edit of a driven parameter — decision: **the macro wins**

A driven parameter **follows its macro only**. Its knob, fader, switch or field on the device card, in the plugin
frame's generic view and in the inspector does not take a drag, wheel, arrow key, double-click or text entry; the
widget shows the effective value, the badge and the mapped range (§8.2). A drag attempt puts the status text
„Frequenz 5 wird von Spur-Makro 1 „Open“ gesteuert“ up and selects that macro (its card cell and the inspector show
it), so the way forward is one click away. The value menu offers „Spur-Makro zeigen“ and „Zuordnung entfernen“ instead
of „Auf Standard“ / „Wert eingeben“ (mockup 03).

Edits komponist cannot block:

- **The plugin's own GUI** changes a driven parameter: the change is heard until the gesture ends (CLAP
  `PARAM_GESTURE_END`, VST3 `endEdit`), or 250 ms after the last change of a plugin without gestures; then the
  engine **re-asserts** the macro's value. At most one re-assert per parameter per 250 ms, so a plugin that reports
  internally modulated values cannot start a fight. Such GUI edits of driven parameters make no undo step (§9).
- **A preset or state load** of the device (preset menu, undo to an older state, „Neu laden“ after a crash, a fresh
  processor): the engine re-asserts every mapping of that device at the first block after it (§6.3).
- **An automation lane on the parameter** is a direct edit in the automation spec's sense (`../automation/SPEC.md`
  §3.4): it does not play while the mapping exists, and its lane says so (§10.1).

Why not the alternatives:

- **The edit detaches the mapping** (the parameter leaves the macro's control): silent, destructive and invisible
  during playback — a nudge on a knob would break an automated macro the user set up minutes ago, and the next macro
  move would no longer do what the macro's name says. An undo-able detach is still a surprise.
- **Offset** (the edit moves a base value; the macro adds ±range around it, Bitwig's modulation model): it contradicts
  the owner's model of a mapping (absolute minimum and maximum), needs a stored base per mapping and per plugin
  parameter, makes "what value does the plugin get" depend on two numbers the knob cannot both show, and is
  modulation — #15's territory, where it belongs (the knob token already has the `mod_ring` for it).
- **Macro wins** is what users know from Ableton's racks (mapped parameters are locked there), keeps one source of
  truth (so an automated macro renders the same every time), and makes a mapping's effect predictable: the knob always
  shows `f(macro)`.

owner decision Q1.

---

## 5. UI (#75)

### 5.1 Where the 8 controls live — decision: **head of the device chain + inspector section**

1. **„Spur-Makros“ card at the head of the device chain** (Geräte dock tab), mockup 01. A frame of the device grid,
   4 columns wide (`device_w = 16 + 60 · 4 − 4 = 252`, `body_h` 216, head 28 in `surface.3` like an instrument's),
   pinned at the left: it does not scroll with the chain, a 1 px `line.2` divider in the gap separates it from the
   devices. Head: the track's colour bar (3 × 14), the title, the „Zuordnen“ toggle (20 px button, `accent` when on,
   like „Geöffnet“), the ⋯ menu (Einklappen). Body: rows 0 and 1 are the 8 macros as knob cells (`widget.knob`);
   row 2 is the selected macro in words (badge, name, „4 Ziele“, the target names), or the hint in mapping mode.
   Collapsible to the 28 px strip like a device (mockup 04). Why here: mapping happens where the targets are, and the
   chain is per track — the same reasons Ableton (rack macros) and Bitwig (remote controls) put them there.
2. **„Spur-Makros“ section in the inspector** (between „Geräte“ and „Kanal“; meta action „in Geräten zeigen“, like
   „im Mixer zeigen“): 8 small knobs (`size.knob-small` 20) in 2 rows of 4 with badge, name and value, so the macros are
   at hand from the arrangement without opening the dock. Clicking one selects it, and the section expands with its
   detail: name field, value field, the mapping list (§5.4). The inspector is the place for the list because it has
   the width (256 px) and already hosts the "Parameter" detail of a touched control.
3. **Not** in the track header (56 px default height: no room for 8 controls; a later option for tall tracks) and
   **not** in the mixer strip (80 px wide; it would need a second strip mode). owner decision Q4.

Selection: one macro per track is selected (UI state, not undone, default macro 1); clicking a macro knob or its
name selects it in both places.

### 5.2 The macro control

- Knob cell as on device cards: name (`type.device-label`), value (`mono 10`, „40 %“). The **value arc in the track
  colour** (`track.base`) — the track colour means "macro" everywhere (§8.1); a macro without mappings draws its arc
  and value in `text.3` and its name „frei“.
- **Number badge** 12 × 12, `radius.s`, `type.mono-xxs`, top right of the cell (in the inspector's small cells top
  left, beside the 20 px knob): `surface.4` / `text.2`; the selected
  macro's badge in `accent` / `accent.ink` with the `select.ring` around the cell; in mapping mode the selected cell gets
  the `knob.learn` ground (`accent.surface` + inset 1 px `accent`).
- Interaction as every knob (drag, wheel, arrows, double-click = default 0 %, value menu, text entry in %). Double-click
  on the **name** renames in place (also „Umbenennen“ in the value menu, and the inspector's name field).
- Value menu: Auf Standard · Wert eingeben · — · Umbenennen · Alle Zuordnungen entfernen (mockup 04).
- While a macro is hovered or dragged, its targets on the visible cards get a 1 px ring in the track colour, so the
  user sees what moves.

### 5.3 Mapping workflow

**Mapping mode** (mockup 02):

1. Turn on „Zuordnen“ (card head, inspector detail, or `Strg Shift M` / `⇧⌘M`, owner decision Q10). The selected
   macro is the one that receives mappings; clicking another macro switches.
2. Every **mappable** control of the track gets the light candidate ground (`accent.surface`, no outline): knobs,
   mini faders, switches, segments, number fields and dropdowns of automatable parameters on the cards, in the plugin
   frame's generic view and the track's volume, pan and send controls in the inspector and mixer. Controls
   the selected macro already drives carry the full `knob.learn` style and their badge; controls of another macro keep
   their badge (moving one moves it to the selected macro, §3.5). Not mappable and dimmed (opacity 0.4): graphs and XY
   pads (their parameters are reachable through their own controls), collapsed devices (tooltip „Aufklappen, um
   Parameter zuzuordnen“), missing or crashed devices, read-only displays.
3. **Moving** a candidate (drag, wheel, arrow key, a switch click) maps it to the selected macro — the move itself is
   not applied, so mapping never changes the sound by accident. A parameter moved in the **plugin's own GUI** is
   mapped on its first gesture or value change (the bridge already reports both; the plugin frame header shows the
   same accent chip as the chain head). Each new mapping is one undo step.
4. The chain head shows the chip „Zuordnen · Spur-Makro 1 „Open““; the status bar says „Zuordnen: Parameter bewegen
   ordnet sie Spur-Makro 1 zu · Klick auf ein anderes Makro wechselt · Esc beendet“.
5. **Esc**, the toggle, or switching the dock tab / the selected track ends mapping mode. It is UI state, not undone.

**Default range of a new mapping — no jump.** The mapping is created so the parameter keeps its current value `p`
(normalised) at the macro's current value `v`: the widest range anchored at one end that maps `v` to `p` —
`p ≥ v`: `max = 1`, `min = (p − v) / (1 − v)` (`v < 1`); `p < v`: `min = 0`, `max = p / v`. A fresh macro is at 0, so
its mappings start at the parameter's current value and open upwards (`min = p`, `max = 1`). If that range would be
degenerate (`max − min < 0.01`, e.g. `p = 1` at `v = 0`), the mapping gets the full range, inverted when `p > 0.5`.
owner decision Q8.

**Without mapping mode** (keyboard, screen reader, quick one-off): the value menu of every mappable control gets
„Spur-Makro zuordnen“ with a submenu of the 8 macros („1 · Open“, „4 · frei“ …); the moved/driven state follows.

### 5.4 The mapping list (inspector, mockups 02 and 04)

Below the selected macro's name and value fields: „Zuordnungen · 4“ and the „Zuordnen“ toggle; one row per mapping
in creation order:

- Line 1: device name (`text.3`, 11 px, truncated first) · parameter name (`text.1`, 12 px 500); right: the **„Kurve“
  control** (a 28 × 18 field in `surface.2` with the mapping's curve from the 0 % end to the 100 % end drawn in
  `track.base`; drag up/down bends, one gesture; Alt-click / ⌥-click back to linear; double-click opens the text entry,
  −1 … +1; tooltip „Kurve: ziehen biegt · Alt-Klick: linear“ with the value, „linear“ or „+0.40“ / „−0.40“), the
  „Umkehren“ icon
  toggle (18 px; on = `knob.learn` style) and „Zuordnung entfernen“ (×).
- Line 2: the **range bar** in the parameter's own scale (the bend does not change it: it shapes the way between
  the handles, which the curve control shows): track 4 px `surface.0`, the range filled in `track.dim`,
  handles 6 × 12 `text.2` at min and max, the current effective value as a 2 × 8 mark in `track.base`. Drag a handle =
  one gesture („Zuordnungsbereich ändern“); a tooltip shows the plain value („Minimum · 320 Hz“); double-click on a
  handle opens the text entry in the parameter's units (the knob's parser); Alt-drag on the fill moves the range.
- Line 3: the values from macro 0 % to macro 100 % with the existing `devices-range` message („320 Hz bis 4.8 kHz“, an
  inverted mapping reads high to low: „30 % bis 10 %“), right the current value in `track.base`.
- Clicking a row reveals the parameter in the chain (scroll + the 1 px ring, the existing `Reveal`).
- Empty: „Keine Zuordnungen. „Zuordnen“ einschalten und Parameter bewegen.“

### 5.5 Showing who drives a parameter (mockups 01, 03, 04)

- **Badge** with the macro's number, top right of the control's cell, `track.base` ground with `surface.0` digits.
- **Knobs** also draw the mapped range as the existing modulation ring (`widget.knob.mod_ring`: r + 2.5, 1.5 px,
  `track.base`) from `min` to `max` — this uses the prepared `KnobCellOptions::modulation` / `ModArc`, no new token.
  Other widgets (faders, switches, segments, fields, dropdowns) show the badge only. EQ graph handles of a driven band
  get a ring in the track colour (r 8, 1.5 px).
- **Hover text:** „Gesteuert von Spur-Makro 1 „Open““ and the range on a second line.
- **Inspector „Parameter“ section:** a new row „Gesteuert von“ with badge, macro name and range; a click selects the
  macro. The value field is shown read-only (`surface.1`, inset `line.1`, `text.2`).
- The **automation lane chooser** (#68) lists driven parameters greyed with the automation spec's hint
  „über Spur-Makro 1 „Open““ (§10.1).
- The **collapsed device strip** shows nothing extra (28 px has no room); the chain head's chips do not either.

### 5.6 Missing targets in the UI (mockup 05)

- The row's parameter (and, for a missing device, the device name) in `state.error`, the status word right of it
  („fehlt“; „abgestürzt“, „lädt nicht“, „hängt“ as the inspector's device rows say), the bar dimmed, and one line of
  explanation instead of the values: „Die Zuordnung bleibt gespeichert und wirkt wieder, sobald das Plugin geladen
  ist.“ / „Das Plugin hat diesen Parameter nicht mehr. Die Zuordnung bleibt gespeichert.“
- The macro's badge gets a 6 px `state.error` dot; its tooltip: „Spur-Makro 2 „Space“ · 2 Ziele fehlen“.
- A missing device's card (S06 placeholder) gets a footer: badge + „1 Zuordnung bleibt gespeichert“.

---

## 6. Engine (#74)

### 6.1 Program

`compile::build` adds to each `TrackProgram` a `MacroProgram`, rebuilt only when the track's `Arc` changed (macros
live in the track):

```rust
pub struct MacroProgram {
    pub base: [f32; 8],                          // document values
    pub spans: [(u16, u16); 8],                  // macro i's mappings: targets[spans[i].0 .. spans[i].1]
    pub targets: Arc<[CompiledMapping]>,
}
pub struct CompiledMapping {
    pub target: EngineTarget,                    // automation's: Slot { slot, wire } | Gain | Pan | Send(idx)
    pub map: PlainMap,                           // automation's normalised → plain
    pub min: f32, pub max: f32, pub invert: bool,
    pub k: f32,                                  // 6 · bend, 0 for linear
    pub inv_den: f32,                            // 1 / (e^k − 1), precomputed off-thread (unused for k = 0)
}
```

- `EngineTarget` and `PlainMap` are the automation spec's (§7.1 there); wire ids as there (`ParamId` → wire id through
  `komponist-builtin` for internal devices and Hexa — Hexa's card parameters through `device::param`, its 8 device
  macros through `Macro` (keys `index`, `ratio2` …, wire ids 0–7) — `ParamId::plugin_id()` for plugins). The engine
  never sees a `ParamId`.
- Mappings the track cannot resolve compile to nothing (§7): device missing, crashed, failed or not yet loaded (a
  bridged plugin's `PlainMap` needs its parameter list; the existing `ParamsChanged` event recompiles the track),
  parameter gone or not automatable.

### 6.2 Inputs of a macro value, per block (phase 1, events)

1. **Fast path** `Command::SetMacro { track, index, value }` during a knob gesture (like `SetTrackGain`): applied at
   offset 0 of the next block; the program compiled at the gesture's end carries the new base value.
2. **Automation**: the automation stage evaluates `Macro(index)` targets on its ramp grid (64 frames at 48 kHz,
   aligned to the song position) unless held (`Command::Touch`, Read override) or the macro's lane is Off (the automation mode is per lane), and hands the
   values with their offsets to the macro stage instead of emitting events (`../automation/SPEC.md` §7.2 item 5).
3. **MIDI learn** (#15): a learned CC sets the macro at the event's offset (`value = cc / 127`); a learned 14-bit pair
   or pitch bend at full resolution. Takeover rules and touch semantics are #15's and the automation spec's
   (`MIDI_TOUCH_TIMEOUT`).
4. **Base value** from the program (no other source active).

Later sources in a block override earlier ones at the same offset in this order: MIDI and fast path (a person's hand)
over automation (the automation spec's touch rule), automation over the base value.

### 6.3 Fan-out (the macro stage)

Per track, after the inputs: for each macro whose value changed in the block, in offset order, at most once per ramp
grid step (the last value of the step wins), for each of its compiled mappings:

1. `t = invert ? 1 − v : v`; `s = k == 0 ? t : (exp(k·t) − 1) · inv_den`; `n = min + (max − min) · s` — one `f32::exp`
   per emitted value of a bent mapping, no allocation, no table, no branch beyond the linear case (RT rule 4; the same
   evaluation the automation stage does for `Curve` segments). Skip if `|n − last_n| ≤ 1e-5` (per mapping `last_n`,
   preallocated).
2. `Slot` targets: `EventKind::Param { id: wire, value: map.to_plain(n) }` at the offset into **the slot's sorted
   event list** — the per-slot sample-accurate parameter path that #66 introduces (today effects get parameters only
   at offset 0 through the slot's `ParamQueue`); for bridged plugins `ExternalProcessor` already turns it into a wire
   `ParamValue` with the frame offset, the bridge into CLAP `CLAP_EVENT_PARAM_VALUE` / VST3 `IParameterChanges`, and
   #67's VST3 controller sync (`setParamNormalized`, echo guard) keeps the plugin's GUI in step.
3. `Gain` / `Pan` / `Send` targets (owner, Q2): a ramp point for the gain stage at the offset, exactly as an automation value
   of that target (§7.2 item 4 there).

**Re-assert** (emit every mapping's current value at offset 0, regardless of `last_n`): for the mappings a new
program added or changed; for every mapping of a slot whose processor was (re)inserted or re-activated; after
`Command::MacroReassert { track }`, which the app sends after handing a device a state (`SetState`: preset, undo,
reload); after a plugin's gesture end on a driven parameter (§4; the engine sees it once #67 moves the output-ring
consumer into `ExternalProcessor::process`; before #67 the app sends `MacroReassert` on `ParamValuesChanged` of a
driven parameter).

### 6.4 Telemetry

`Telemetry::macro_values`: 8 `AtomicU32` (f32 bits) per track slot, preallocated for `MAX_TRACKS`, written at the end
of a block when changed. The UI reads them every frame, so the knobs, the driven parameters' displayed values
(`f(macro)`, computed in the UI with §3.2) and the range marks follow automation and MIDI without a document change.

### 6.5 RT rules and budget

- Everything preallocated when the program or the track state is built off-thread: `last_n` per mapping, current
  values per macro, a per-macro change buffer of `MAX_BLOCK / RAMP_STEP + 2` entries. `Engine::process` stays
  allocation-free; every macro test runs `process` inside `assert_no_alloc` (rule 4).
- Budget: 8 macros × 16 mappings = at most 128 compiled mappings per track. A ramping macro adds its mappings to the
  automation spec's per-track event budget (§14 there: `frames / RAMP_STEP + 2` per target and block, 1024 events per
  list); overflow follows the same rule (drop intermediate ramp steps, keep jumps and the block's last value per
  target, count it). A test drives 8 macros × 16 mappings at the largest block.
- Perf: the `engine_perf` suite gets a case "16 tracks, 8 automated macros × 4 mappings each, 256 frames" with a
  budget of ≤ 2 % of the block period on the audio thread (PERFORMANCE.md row).

### 6.6 Offline render, export, determinism

The export renders through the same program (the fast path is not used there). Automated macros follow the
automation grid, so live playback and export produce the same events at the same samples; the determinism test of the
automation spec includes a project with automated macros.

---

## 7. Missing and moved targets

| Situation | Mapping | Drives | Shown as |
|---|---|---|---|
| Plugin missing (S06), crashed, failed to load, hangs | kept unchanged | no; again from the first block after the device is back (re-assert) | row in `state.error` + status word, badge dot (§5.6) |
| Plugin still loading | kept | from the moment its parameter list arrives (recompile + re-assert) | normal |
| Parameter gone or no longer automatable (plugin update) | kept (`label` keeps its name) | no | „Parameter fehlt“ |
| Device bypassed | kept | yes (the values wait in the device as a card turn does; heard when bypass ends) | normal |
| Device moved within the chain | unchanged (ids) | yes | — |
| Device removed / instrument replaced / send removed | **deleted in the same undo step** | — | status „2 Zuordnungen entfernt“ when any were dropped |
| Device moved to another track (if ever possible) | deleted in the same step | — | as above |
| Track deleted | gone with the track | — | — |
| Track duplicated (when it exists) | copied with remapped device and send ids | yes | — |

Deleting with the device follows the automation spec (lanes die with their device) and the routing precedent; keeping
the mappings of a missing plugin follows S06 (a project opened without Surge XT and saved again loses nothing).
owner decision Q7.

---

## 8. Visual language (tokens) and metrics

### 8.1 What colour means

| Colour | Means |
|---|---|
| `track.base` (the track's hue) | "track macro": macro value arcs, badges of driven controls, mapped-range rings, the current-value mark |
| `accent` | selection and mapping mode: the selected macro's badge and ring, the „Zuordnen“ toggle, `knob.learn` |
| `accent.surface` | candidate ground in mapping mode |
| `state.error` | missing targets |

No new colour, type or size token. New component metrics (rule 2, each one constant in
`komponist_ui::widgets::metrics` with this proposal as source): `MACRO_BADGE = 12` (badge square), `MACRO_BADGE_INSET =
1` (from the cell's top right), `MACRO_ERROR_DOT = 6`, `MAP_ROW_HANDLE = [6, 12]`, `MAP_ROW_MARK = [2, 8]`,
`MAP_ROW_TRACK = 4`, `EQ_HANDLE_RING = 8`, `MAP_ROW_CURVE = [28, 18]` (the curve control); view metrics of the card and the inspector section in the devices and
inspector `metrics` modules (card = 4 grid columns; inspector cell 64 × 50).

### 8.2 Contract change requests

None. The badge digits use `type.mono-xxs` (9/12, 400); the mockups were drawn with it.

---

## 9. Undo

- Edits and labels: §3.6. A macro knob drag is one gesture (`EditAction::BeginGesture { label }` … `EndGesture`), the
  intermediate values use `Command::SetMacro` (fast path, no recompile per frame), the program follows at the gesture's
  end — the fader pattern. Esc cancels the gesture and the macro returns.
- Labels with the macro's name use the U+001F scheme: `undo_label::track_macro(name)` → `undo-macro-value␟Open`
  („Spur-Makro „Open“ ändern“); an unnamed macro uses `undo-macro-value-slot` („Spur-Makro 5 ändern“).
- Undo/redo of a macro value or a mapping recompiles the track; the program's base values and the re-assert rule
  (§6.3) bring every target to the restored state, built-in and bridged alike.
- **Plugin snapshots** (param_undo): a bridged device's state snapshot that arrives while a macro gesture is open, or
  within the existing `COALESCE` window after it, joins the macro's step; snapshots caused by engine-driven values
  (automation, MIDI, re-asserts) are stored with `Document::apply_untracked`, like the echo of an undo; GUI edits of a
  driven parameter make no step (the macro re-asserts). Most plugins do not mark their state dirty for host parameter
  events at all (CLAP host events, VST3 without `performEdit`); the rule covers the ones that do.

---

## 10. Interfaces to automation (#56) and MIDI learn (#15)

### 10.1 Automation (matches `../automation/SPEC.md` §3.4)

1. Identity: `AutomationTarget::Macro { index }`, index 0 ..= 7, stable (fixed slots).
2. Value: normalised 0–1; automation sets the **macro value**, the macro stage fans it out; automation never writes
   mapped parameters.
3. Name: lane label = „Spur-Makro 1 · Open“ (`macros-slot-named`), unnamed „Spur-Makro 5“ (`macros-slot`); the
   chooser's section is titled „Spur-Makros“ (`macros-title`) and lists „1 · Open“ … „8 · frei“ (`macros-entry`).
4. **Overlap**, from §4: a parameter that is driven by a macro is a mapping target, not an automation target, while the
   mapping exists. The chooser shows it greyed with „über Spur-Makro 1 „Open““ (`automation-mapped-by`, whose
   `$macro` is `macros-ref`) and offers the macro's lane instead; an existing lane of a parameter that gets mapped is
   kept, does not play, and its header says „⚠ gesteuert von Spur-Makro 1 „Open““ (`macros-lane-overridden`); removing
   the mapping makes it play again. Touch/Latch never write a driven parameter (its control does not move).
5. **Modes are per lane** (automation spec, owner 2026-11-16): the macro's lane has its own Off / Read / Touch / Latch.
   Off on the macro's lane returns the macro to its base value; the modes of the lanes of other targets do not affect
   the macro, and a driven parameter's own lane stays silent whatever its mode (item 4).
6. **Master:** the master row's lanes (automation v1) never drive a track macro, and a track macro never drives the
   master (§1).
7. A macro knob is a control like any other for the automation spec's touch, override and writing rules (§6 there):
   a manual move of an automated macro in Read is a temporary override; in Touch/Latch it writes the macro's lane.

### 10.2 MIDI learn (#15)

- A macro is a learn target with the same identity (`Macro { index }` on its track). Learning, takeover and the
  learn UI are #15's; the macro knob already has the `knob.learn` look for "listening".
- Engine entry: §6.2 item 3 (sample-accurate at the MIDI event's offset).
- Recommendation for #15: learn targets use `AutomationTarget` too, so a learned mapping and an automation lane address
  parameters, mixer controls and macros the same way.

---

## 11. File format (#73)

- `FORMAT_VERSION` + 1 when #73 lands (3 if before automation's #65, else 4; if both land in one release they may share
  one step). `migrate_vN_to_vN+1` is the identity on the JSON (no macros = defaults), with its test; a file with macros
  is rejected by older builds (the intended direction, as in v2).
- Track: `"macros"` is a dense array of 8 objects, omitted when all 8 are default (name "", value 0, no mappings); a
  default slot is written as `{}`:

```json
"macros": [
  { "name": "Open", "value": 0.4, "mappings": [
      { "target": { "kind": "param", "device": 12, "param": "cutoff" }, "label": "Volt · Cutoff",
        "min": 0.35, "max": 0.8, "bend": 0.4 },
      { "target": { "kind": "param", "device": 14, "param": "b5_freq" }, "label": "EQ · Frequenz 5",
        "min": 0.62, "max": 0.95 },
      { "target": { "kind": "param", "device": 12, "param": "resonance" }, "label": "Volt · Resonanz",
        "min": 0.1, "max": 0.3, "invert": true } ] },
  { "name": "Space", "value": 0.25, "mappings": [ … ] },
  {}, {}, {}, {}, {}, {}
]
```

- `invert` omitted when false, `bend` omitted when 0; values `f32` shortest round-trip. The `target` object is the automation format's.
- Loading validates: fewer than 8 slots are padded, more are an error (malformed); values clamped to 0..1, `bend` to −1..1; `min > max`
  swapped (logged); a `Macro` target or a duplicate target on the track dropped (first wins, logged); more than 16
  mappings per macro kept (the cap is an edit rule; the engine compiles the first 16, logged); a target on a device
  of another track kept and shown as missing.
- Round-trip tests: every target kind, inverted, bent, a missing plugin's mapping, a default track (writes no key), a v2
  file (migrates to no macros).

---

## 12. Accessibility (rule 12)

Read the language once before every `widget_info` closure (I18N pitfall).

| Element | Role | Name (de / en) | Value / state / description |
|---|---|---|---|
| Card / inspector section | `Group` | „Spur-Makros“ / "Track macros" | — |
| Macro knob | `Slider` | „Spur-Makro 1, Open“ / "Track macro 1, Open" (`macros-a11y`); unnamed: „Spur-Makro 5, frei“ | value „40 %“ / "40%"; `selected` when selected; description „4 Zuordnungen“ / "4 mappings" (`macros-a11y-mappings`), plus „2 Ziele fehlen“ when any |
| „Zuordnen“ | `Button`, `toggled` | „Zuordnen“ / "Map" | tooltip text as description |
| Driven control | its role (`Slider`, …), `read_only` | its name | description „Gesteuert von Spur-Makro 1 „Open““ |
| Candidate in mapping mode | its role | its name | description „Zuordenbar: Spur-Makro 1“ / "Mappable: track macro 1" (`macros-a11y-candidate`) |
| Mapping row | `Group` | „Zuordnung Cutoff (Volt), 320 Hz bis 4.8 kHz“ (`macros-a11y-mapping`) | description: missing text when missing |
| Range handles | `Slider` | „Minimum, Cutoff“ / „Maximum, Cutoff“ (`macros-a11y-min` / `-max`) | plain value text |
| Curve control | `Slider` | „Kurve, Cutoff“ / "Curve, Cutoff" (`macros-a11y-bend`) | value „linear“ / „+0.40“ (`macros-bend-value`); description: the tooltip text |
| Umkehren | `Button`, `toggled` | „Umkehren“ / "Invert" | — |
| Remove | `Button` | „Zuordnung entfernen“ / "Remove mapping" | — |

kittest finds a macro by `get_by_role_and_label(Role::Slider, "Spur-Makro 1, Open")`, the egui MCP server likewise.

---

## 13. UI texts (`de` source, `en`; new file `locales/*/macros.ftl`, undo labels in `undo.ftl`)

German: „…“ quotes, U+2212, „35 %“, „ · “; English: “…”, "35%", Title Case for titles, buttons and menu items,
sentence case for labels, hints, tooltips and accessible names (docs/I18N.md §6). Names of macros are user content
(I18N §5); the demo's are English like „Night Drive“.

| Key | de | en | Where |
|---|---|---|---|
| `macros-title` | Spur-Makros | Track Macros | card head, inspector section (upper case there by the label style), chooser section, collapsed strip |
| `macros-slot` | Spur-Makro { $n } | Track macro { $n } | lane label and undo of an unnamed macro, a11y |
| `macros-slot-named` | Spur-Makro { $n } · { $name } | Track macro { $n } · { $name } | lane label |
| `macros-ref` | Spur-Makro { $n } „{ $name }“ | track macro { $n } “{ $name }” | inside sentences (status, tooltips, `automation-mapped-by`) |
| `macros-entry` | { $n } · { $name } | { $n } · { $name } | submenu „Spur-Makro zuordnen“, chooser entries |
| `macros-unused` | frei | unused | name of an unnamed macro without mappings |
| `macros-map` | Zuordnen | Map | toggle (card head, inspector) |
| `macros-map-tip` | Zuordnen: Spur-Makro wählen, dann Parameter bewegen | Map: choose a track macro, then move parameters | tooltip of the toggle |
| `macros-map-chip` | Zuordnen · { $macro } | Map · { $macro } | chip in the chain head and plugin frame head (`$macro` = `macros-ref` with capital T in en: "Track macro 1 “Open”") |
| `macros-map-hint` | Parameter bewegen, um sie zuzuordnen | Move parameters to map them | card row 2 in mapping mode |
| `macros-map-hint-esc` | Esc beendet | Esc ends | card row 2, second line |
| `status-macros-mapping` | Zuordnen: Parameter bewegen ordnet sie { $macro } zu · Klick auf ein anderes Makro wechselt · Esc beendet | Map: move parameters to map them to { $macro } · Click another macro to switch · Esc ends | status bar (file `status`) |
| `status-macros-locked` | { $param } wird von { $macro } gesteuert | { $param } is controlled by { $macro } | status after a drag on a driven control |
| `status-macros-moved` | { $param }: jetzt { $macro } | { $param }: now { $macro } | status when a mapping moved to another macro |
| `status-macros-not-mappable` | { $param } lässt sich nicht zuordnen | { $param } can’t be mapped | status: not automatable |
| `status-macros-full` | Höchstens 16 Zuordnungen pro Spur-Makro | At most 16 mappings per track macro | status on the 17th |
| `status-macros-dropped` | { $n -> [one] 1 Zuordnung entfernt *[other] { $n } Zuordnungen entfernt } | { $n -> [one] 1 mapping removed *[other] { $n } mappings removed } | status when a device removal dropped mappings |
| `macros-show-in-devices` | in Geräten zeigen | show in devices | inspector section meta action |
| `macros-targets` | { $n -> [one] 1 Ziel *[other] { $n } Ziele } | { $n -> [one] 1 target *[other] { $n } targets } | card row 2 |
| `macros-mappings` | Zuordnungen · { $n } | Mappings · { $n } | inspector list header |
| `macros-name` | Name | Name | inspector field |
| `macros-value` | Wert | Value | inspector field |
| `macros-no-mappings` | Keine Zuordnungen. „Zuordnen“ einschalten und Parameter bewegen. | No mappings. Turn on “Map” and move parameters. | empty list |
| `macros-bend` | Kurve | Curve | curve control of a row (tooltip title) |
| `macros-bend-tip` | Kurve: ziehen biegt · Alt-Klick: linear | Curve: drag to bend · Alt+click: linear | tooltip of the curve control |
| `macros-bend-tip-mac` | Kurve: ziehen biegt · ⌥-Klick: linear | Curve: drag to bend · ⌥+click: linear | the same on macOS (I18N: every Alt text has a `-mac` message) |
| `macros-bend-linear` | linear | linear | value of an unbent mapping (tooltip, a11y value) |
| `macros-bend-value` | { $value } | { $value } | value of a bent mapping, `$value` formatted in Rust („+0.40“, „−0.40“ with U+2212) |
| `macros-a11y-bend` | Kurve, { $param } | Curve, { $param } | accessible name of the curve control |
| `macros-invert` | Umkehren | Invert | toggle in a row (tooltip and a11y) |
| `macros-remove` | Zuordnung entfernen | Remove mapping | × in a row (tooltip and a11y) |
| `macros-remove-menu` | Zuordnung entfernen | Remove Mapping | value menu of a driven control |
| `macros-remove-all` | Alle Zuordnungen entfernen | Remove All Mappings | value menu of a macro |
| `macros-rename` | Umbenennen | Rename | value menu of a macro |
| `macros-show` | Spur-Makro zeigen | Show Track Macro | value menu of a driven control |
| `macros-map-to` | Spur-Makro zuordnen | Map to Track Macro | value menu of a mappable control (submenu) |
| `macros-controlled-by` | Gesteuert von | Controlled by | inspector „Parameter“ row label |
| `macros-controlled-tip` | Gesteuert von { $macro } | Controlled by { $macro } | hover text of a driven control (range on line 2 via `devices-range`) |
| `macros-min` | Minimum | Minimum | handle tooltip |
| `macros-max` | Maximum | Maximum | handle tooltip |
| `macros-missing` | fehlt | missing | row status (crashed etc. reuse `inspector-device-*`) |
| `macros-plugin-missing-hint` | Die Zuordnung bleibt gespeichert und wirkt wieder, sobald das Plugin geladen ist. | The mapping stays saved and works again as soon as the plugin loads. | row text |
| `macros-param-missing-hint` | Das Plugin hat diesen Parameter nicht mehr. Die Zuordnung bleibt gespeichert. | The plugin no longer has this parameter. The mapping stays saved. | row text |
| `macros-targets-missing` | { $n -> [one] 1 Ziel fehlt *[other] { $n } Ziele fehlen } | { $n -> [one] 1 target missing *[other] { $n } targets missing } | badge tooltip after `macros-ref` and „ · “ |
| `macros-kept` | { $n -> [one] 1 Zuordnung bleibt gespeichert *[other] { $n } Zuordnungen bleiben gespeichert } | { $n -> [one] 1 mapping kept *[other] { $n } mappings kept } | footer of a missing device's card |
| `macros-expand-to-map` | Aufklappen, um Parameter zuzuordnen | Expand to map parameters | tooltip of a collapsed device in mapping mode |
| `macros-lane-overridden` | gesteuert von { $macro } | controlled by { $macro } | automation lane header of a driven parameter (after „⚠ “) |
| `macros-device-macros` | Geräte-Makros | Device Macros | heading where komponist lists a device's macro parameters (plugin frame, chooser), if a view wants one |
| `macros-a11y` | Spur-Makro { $n }, { $name } | Track macro { $n }, { $name } | accessible name of a macro knob |
| `macros-a11y-mappings` | { $n -> [0] Keine Zuordnungen [one] 1 Zuordnung *[other] { $n } Zuordnungen } | { $n -> [0] No mappings [one] 1 mapping *[other] { $n } mappings } | description of a macro knob |
| `macros-a11y-candidate` | Zuordenbar: { $macro } | Mappable: { $macro } | description in mapping mode |
| `macros-a11y-mapping` | Zuordnung { $param } ({ $device }), { $range } | Mapping { $param } ({ $device }), { $range } | a row (`$range` = `devices-range`) |
| `macros-a11y-min` | Minimum, { $param } | Minimum, { $param } | handle |
| `macros-a11y-max` | Maximum, { $param } | Maximum, { $param } | handle |
| `undo-macro-value` | Spur-Makro „{ $name }“ ändern | Change Track Macro “{ $name }” | undo (U+001F scheme) |
| `undo-macro-value-slot` | Spur-Makro { $n } ändern | Change Track Macro { $n } | undo, unnamed |
| `undo-macro-rename` | Spur-Makro umbenennen | Rename Track Macro | undo |
| `undo-macro-add-mapping` | Zuordnung hinzufügen | Add Mapping | undo |
| `undo-macro-remove-mapping` | Zuordnung entfernen | Remove Mapping | undo |
| `undo-macro-clear` | Alle Zuordnungen entfernen | Remove All Mappings | undo |
| `undo-macro-range` | Zuordnungsbereich ändern | Change Mapping Range | undo |
| `undo-macro-invert` | Zuordnung umkehren | Invert Mapping | undo |
| `undo-macro-bend` | Zuordnungskurve ändern | Change Mapping Curve | undo |
| `key-macros-map` | Zuordnen ein/aus | Toggle Map Mode | keymap list (Q10) |

Reused unchanged: `devices-range` („{ $min } bis { $max }“ / "{ $min } to { $max }"), `devices-collapse`,
`ui-value-to-default`, `ui-value-enter`, `inspector-device-crashed` / `-load-failed` / `-unresponsive`,
`devices-missing`. `macros-ref` starts lower case in English because it sits inside sentences; the chip passes the
capitalised form (`macros-ref-title`: "Track macro { $n } “{ $name }”", identical to `macros-ref` in German) — one
more key, listed here so #75 does not forget it.

---

## 14. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Users expect to tweak a driven knob | "the knob is stuck" | badge + ring on the control, status text that names the macro and selects it, value menu with „Spur-Makro zeigen“ / „Zuordnung entfernen“; owner decision Q1 |
| #74 needs the per-slot sample-accurate parameter path and `EngineTarget`/`PlainMap` that #66 builds | order dependency | #74 after #66, or #74 builds them as the automation spec describes (§7 there) and #66 reuses them; the two specs share the types on purpose |
| Plugin GUIs do not follow host changes (VST3 controller) | GUI shows stale values | #67's controller sync and echo guard; until then the card shows `f(macro)` from telemetry, the plugin GUI may lag |
| Plugins that report internally modulated values as edits | re-assert fights | re-assert only at gesture end / 250 ms quiet, at most every 250 ms per parameter |
| State snapshots after engine-driven values | undo noise | §9: untracked unless a macro gesture is open; test with a plugin that marks dirty on host events |
| Event budget with many ramping macros | dropped steps | 16 mappings per macro, shared overflow rule, test at the largest block (§6.5) |
| Device macros that write several fields (Hexa „Attack“ scales all carriers, „Algorithmus“ rewrites the routing) mapped while the card's own fields are edited | the card edit and the macro write the same patch field | documented: the driven parameter is locked, the overlapping card field is not (they are different parameters); last writer wins inside Hexa, as today between macro and card |
| Unmap writes the value into a built-in state | extra code path | one helper shared with the card's `SetParam` path; unit test per device kind |
| Format version shared with automation | merge conflict | one step per WP, or one shared step if they land together (§11) |
| Mixer targets (Q2, accepted) widen #74 | scope | they use #66's gain-stage ramps; if #74 lands before #66, the mixer targets follow in #76 |
| Bend per mapping: one `exp` per emitted value | CPU | precomputed `inv_den`, linear mappings skip it; counted in the `engine_perf` case |

---

## 15. Order and dependencies

#73 model/format (needs `AutomationTarget`: added by #73 or #65, whichever is first) → #74 engine (after #66's
per-slot parameter events, or builds them) and #75 UI (needs #73; can run on the fast path before #74 by sending
`SetParam` per mapping, if it must) → #76 automation and MIDI learn (needs #66/#68; the MIDI part needs #15's learn
mechanism — if #15 is not specced by then, #76 delivers the automation part and the learn-target identity, and the
MIDI tests move to #15) → #77 showcase and docs.

---

## 16. Proposed issue bodies (to apply after owner review)

### #73 Track macros: model, undo and file format

> Spec: track macros proposal §3, §9, §11 (internal notes, `proposals/track-macros/`).
>
> - `TrackMacros` on every track: exactly 8 `TrackMacro { name, value (0–1), mappings }`; `Mapping { target:
>   AutomationTarget (not Macro, never the master), label, min, max, invert, bend (−1…1) }`, normalised 0–1; the
>   mapping function with the automation spec's curve shape as one shared pure function. `AutomationTarget` is shared with #65
>   (add it in `komponist_model::target` if #65 has not).
> - Invariants (§3.4): one driver per target per track, ≤ 16 mappings per macro, min ≤ max, targets of this track only.
> - Pure edits in `edits::macros` with undo labels (§3.6); `remove_device`, `replace_instrument` and send removal drop
>   their mappings in the same step; the mapping function (§3.2) as a pure function with tests.
> - File: `"macros"` (dense array of 8, omitted when default), `FORMAT_VERSION` + 1 with identity migration and test;
>   load validation (§11); round-trip tests; property tests of random edit sequences keeping the invariants.
> - Catalog: the `undo-macro-*` messages (de, en).
>
> Acceptance: `cargo test -p komponist-model` covers every edit, repair and validation rule; a v2 project loads with
> default macros and saves without a `macros` key; a project with macros round-trips byte-identically.

### #74 Track macros: engine

> Spec: §6, §4 (re-assert), §7.
>
> - `MacroProgram` per track in the program (base values, compiled mappings with `EngineTarget` + `PlainMap`);
>   unresolvable mappings compile to nothing; bridged plugins recompile on `ParamsChanged`.
> - Bend evaluated RT-safe (one `exp` per emitted value, precomputed denominator, linear mappings skip it).
> - `Command::SetMacro` (fast path), `Command::MacroReassert`; the macro stage fans macro changes out as
>   `EventKind::Param` into the slots' sample-accurate event lists (shared with #66) and as ramp
>   points for gain, pan and sends.
> - Re-assert on program changes of a mapping, processor (re)insert/re-activation, `MacroReassert`, and a plugin's
>   gesture end on a driven parameter (250 ms rule).
> - `Telemetry::macro_values`.
> - RT: no allocation (`assert_no_alloc` around every test's `process`), budget test at 8 × 16 mappings and the largest
>   block, `engine_perf` case and PERFORMANCE.md row.
>
> Acceptance: tests with `TestGain`-style processors show the mapped plain values at the right offsets (normal,
> inverted, bent, stepped, bool; volume, pan and a send), a bypassed and a re-inserted processor get the value, a missing plugin's mapping is
> skipped and works after the plugin is back, a bridged test plugin receives the values with frame offsets (CLAP and
> VST3, Linux and macOS), export and live render produce identical events.

### #75 Track macros: UI and mapping mode

> Spec: §5, §8, §12, §13; mockups 01–05.
>
> - „Spur-Makros“ card pinned at the head of the device chain (4 columns, collapsible) and the inspector section with
>   the selected macro's detail and mapping list.
> - Mapping mode („Zuordnen“, Esc, `Strg Shift M` / `⇧⌘M`): candidates, learn style, mapping by moving a control (cards,
>   plugin frame generic view, plugin GUI via the bridge's gestures), no-jump default range, value-menu path
>   „Spur-Makro zuordnen“.
> - Driven controls: badge, range ring, locked („the macro wins“) with status text and value menu, inspector
>   „Gesteuert von“ row; hover highlighting of a macro's targets; missing targets (§5.6).
> - Range editing (handles, text entry, Alt-drag, invert, the curve control per row), rename, remove; unmap writes the value into built-in states.
> - AccessKit (§12), catalogs `macros.ftl` + status/undo texts (de, en), metrics constants (§8.1), kittest snapshots
>   German and English (card, mapping mode, list, driven control, missing target), checked live with the egui MCP server.
>
> Acceptance: the five mockup states exist as snapshots; a kittest maps two parameters in mapping mode, edits a range,
> inverts, bends (drag and Alt-click reset), undoes and redoes each step; a drag on a driven knob changes nothing and shows the status text; English
> snapshots show nothing cut off.

### #76 Track macros: automation and MIDI learn

> Spec: §10.
>
> - Macros as automation targets: lanes and clip automation (`AutomationTarget::Macro { index }`), chooser section
>   „Spur-Makros“, lane labels, the macro lane's own mode (modes are per lane; Off = base value), touch/override/write
>   rules for the macro knob; driven parameters greyed in the chooser
>   („über Spur-Makro …“) and an existing lane of a driven parameter muted with its header text.
> - MIDI learn target identity for macros and the engine entry (§6.2 item 3); the learn UI itself is #15's.
>
> Acceptance: tests for an automated macro (playback, loop wrap, locate chase, export identical to live), a Touch pass
> on a macro, a muted lane of a driven parameter that plays again after unmapping, and (with #15) a CC driving a macro
> sample-accurately.

### #77 Track macros: showcase and docs

> - Demo (`showcase_project`, rule 15): on „Pad“ (Volt „Wide Pad“, EQ, post-fader send to the Hall bus) a track macro
>   „Open“ driving Volt's Cutoff (≈ 30 % → 70 %, bend +0.4) and Resonanz, and the send to Hall (−18 → −8 dB) — with #56 an
>   automation lane that opens it over bars 9–16; on „Lead“ a macro „Bite“ on Hexa's device macros „Feedback“ and
>   „Index“ (shows a device macro as a target). Names are English content like the project's.
> - Keep the mix balanced (`showcase::render_levels`: master peak ≤ −0.3 dBFS, limiter ≤ ~2 dB, no silent track);
>   update the showcase tests (macros present, mappings resolve, values in range).
> - ARCHITECTURE: a "Track macros" section (model, mapping function, the macro-wins rule, engine stage, re-assert,
>   missing targets, format) and the §5.2 `Track` struct; I18N glossary rows (Spur-Makro / Track macro, Zuordnung /
>   Mapping, Geräte-Makro / Device macro).
>
> Acceptance: `komponist --demo` shows both macros with badges on their targets; the showcase tests and the ignored
> level test pass.

# Automation — spec and design proposal (#64, epic #56)

Status: **decided by the owner 2026-11-16** (`DECISIONS.md`); this text includes the decisions, including the two
deviations from the proposal: the mode is **per lane** (Q4) and the **master** is automatable in v1 (Q11). Nothing of this is built. Inputs: epic #56 and its
sub-issues #65–#71, `docs/ARCHITECTURE.md` (§5 model, §5.3 undo, §5.5 persistence, §6 engine, §8 plugins, §9 UI,
§14), `docs/I18N.md`, the code as of `6a12357` (`komponist-model::params`, `komponist-ipc::shm`, `komponist-bridge`,
`komponist-plugins::bridge`, `komponist-app::param_undo`, the arrangement and piano-roll views), and the current
app look (Linux snapshots in `crates/komponist-app/tests/snapshots`, design tokens). The design handoff is obsolete
(owner, 2026-11-16) and not cited.

Companion files in this folder: `UI-TEXTS.md` (every new text, `de` and `en`), `OPEN-QUESTIONS.md` (decisions for the
owner), `mockups/*.html` + `*.png`. Parallel spec: track macros (#72) in `../track-macros/`; the interface between
the two is §3.4.

---

## 1. Goals and non-goals

**Goals (v0.4)**

1. Arrangement **automation lanes** under every MIDI track, every bus and the **master** (a master row at the end of
   the arrangement, §3.5): breakpoints in ticks, three segment shapes, drawn and edited with the mouse and the
   keyboard, shown and hidden per track.
2. **Clip automation** inside MIDI clips (piano roll and step sequencer clips): relative to the clip, moved, copied and
   looped with it.
3. **Targets:** automatable parameters of built-in devices and of CLAP/VST3 plugins (stable ids), the track's volume,
   pan and send levels, the track's 8 track macros (#57), and the master's volume, pan and insert parameters.
4. **Recording** with the modes Off, Read, Touch and Latch **per lane**, from komponist's own controls, from plugin GUIs
   (parameter gestures over the bridge) and from MIDI-learned controllers (#15).
5. **Playback** sample-accurate and deterministic: live, looped, after a locate, and in the offline export, with the
   same result.
6. Undo for every edit (a drag is one step, a recording pass is one step), file format v3 with migration, AccessKit
   nodes for lanes and points, both languages.

**Non-goals (v1)** — each one keeps a seam, none is designed away:

- **MIDI CC, pitch bend and aftertouch lanes** in clips (R9): a later epic, see §11.
- Tempo automation (needs the `TempoMap`, §5.1); mute automation (a switch, not a value; later as a `Hold`-only
  target).
- **Modulation** (LFOs, relative clip envelopes on top of the lane): the knob's modulation arc stays prepared only.
- Time-range editing across all lanes and clips together ("move automation with clips" for arrangement lanes),
  automation on audio tracks (no audio tracks yet), a lane per MIDI channel.
- A preference for the Touch return time or a thinning tolerance (fixed constants in v1, §6.4, §6.5).

---

## 2. Terms

| Term (en) | Begriff (de) | Meaning |
|---|---|---|
| Automation | Automation | Values of a target over time. |
| Automation lane | Automationsspur | One target's breakpoints under a track in the arrangement. |
| Clip automation | Clip-Automation | One target's breakpoints inside a MIDI clip. Deliberately **not** "Hüllkurve"/"envelope": Volt and Hexa already call their ADSRs Hüllkurven. |
| Point | Punkt | A breakpoint: time, value, shape of the segment to the next point. |
| Target | Ziel (only in texts that need it) | What a lane drives: a device parameter, volume, pan, a send, a track macro. |
| Automation mode | Automationsmodus | Per lane: Off / Read / Touch / Latch (Aus / Lesen / Touch / Latch). |
| Master row | (Zeile „Master“) | The arrangement row of the master channel, last in the arrangement, holding the master's lanes (§3.5). |
| Pass | (Durchgang, not shown) | One write between the start and the stop of the transport (or a loop wrap, §6.3). |
| Base value | (Grundwert, not shown) | The value the document stores for the target itself (device state, `volume_db`, …). |

Full text list: `UI-TEXTS.md`.

---

## 3. Model (`komponist-model`, #65)

### 3.1 Target identity

```rust
/// What a lane or a clip automation drives. Always relative to the track that owns it.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AutomationTarget {
    Volume,                                   // Channel::volume_db, on the fader taper
    Pan,                                      // Channel::pan, −1 … +1
    Send { send: SendId },                    // TrackSend::level_db, on the fader taper
    Param { device: DeviceId, param: ParamId },  // a device of this track's chain
    Macro { index: u8 },                      // track macro 0 ..= 7 (#57)
}
```

- **Device parameters** use the stable ids that already exist (ARCHITECTURE R9, `params::ParamId`): readable keys
  for internal devices and Hexa (`"op1_attack"`, `"feedback"`), `"p<id>"` for bridged plugins (CLAP `clap_id`, VST3
  `ParamID`, `ParamId::plugin`). The device is the project-unique `DeviceId`, so a lane survives reordering the chain,
  bypass, preset loads, a crash and a missing plugin.
- **Only automatable parameters** become targets: `ParamInfo::automatable` (internal devices: every parameter that is
  continuous, stepped, enum or bool; bridged: `param_flags::AUTOMATABLE`, CLAP `CLAP_PARAM_IS_AUTOMATABLE`, VST3
  `kCanAutomate`; never `READ_ONLY`, `HIDDEN` or the plugin's bypass parameter, which stays komponist's bypass).
- **Mixer targets** carry no ids: `Volume` and `Pan` mean "of the owning track". Sends exist since wave 9, so
  `Send` is in v1 (owner's "sends once they exist"). On the master the same variants mean the master channel
  (`Volume`, `Pan`, `Param` of a master insert; §3.5).
- **Track macros** are addressed by index; their names, values and mappings are #57's model (§3.4).
- A lane remembers the **last known name** of its target (`label`, e.g. "Surge XT · Filter 1 Cutoff"), written when
  the lane is created or its target renamed, so a lane of a missing plugin still reads well. It is display only, never
  identity, and plugin-provided text (I18N §5).

### 3.2 Points, value domain and shapes

```rust
pub struct Point {
    pub tick: Tick,        // lane: absolute timeline; clip: relative to the clip's content start
    pub value: f32,        // normalised 0.0 ..= 1.0
    pub shape: Shape,      // shape of the segment from this point to the next one
}
pub enum Shape {
    Linear,
    Curve { tension: f32 }, // −1.0 ..= 1.0, 0 is linear (never stored as 0: that is `Linear`)
    Hold,                   // the value stays until the next point, then jumps
}
pub struct Points(Arc<Vec<Point>>);   // sorted by tick; at most two points share a tick (a jump)
```

**Value domain: normalised 0–1 — decision.** Reasons:

1. Every target already has a normalised form: `ParamInfo::to_normalized` / `to_plain` with the parameter's
   `Scale` (Lin, Log, Db, Pan), VST3 parameters are normalised by definition, macros are 0–1, the fader uses the DBT
   taper (`params::scale::db_to_p`), pan maps −1…+1 to 0…1. One domain means one lane widget, one evaluator and one
   thinning rule.
2. **Curves mean what the user sees.** A line drawn on a log-scaled cutoff sweeps evenly in pitch, and a linear volume
   fade follows the fader taper, as on the knob or fader the user just moved. In plain Hz or dB the same straight line
   would sound wrong and need per-target interpolation rules.
3. **A lane is complete without its device.** Drawing, editing, copying, thinning and the file need no `ParamInfo`, so
   lanes of a missing plugin, a plugin still loading, or a removed parameter stay intact and editable (§8).
4. Conversion happens once, where a value reaches a device: the compiler hands each target a small `PlainMap`
   (scale, min, max, steps; `Copy`), the engine converts per emitted event (§7.2). Stepped, enum and bool parameters
   snap in `to_plain`, as today.

Cost of the decision: if a plugin update changes a parameter's range, a lane keeps its *position*, not its plain value
— the same as every VST3 host, and the same as the plugin's own knob. Stored as `f32` (24-bit mantissa: finer than any
control; the file writes the shortest round-trip form).

**Shapes — decision: `Linear`, `Curve { tension }`, `Hold`.**

- `Curve` is one exponential bend per segment, `y(t) = (e^(k·t) − 1) / (e^k − 1)` with `k = 6 · tension` (monotone,
  passes both points, symmetric for ±tension, cheap: one `exp` per emitted value). Dragged with the segment's middle
  handle (§9.3). It covers fades, swells and "log-ish" sweeps; S-curves are two segments.
- **Shared with track macros:** the function is `komponist_model::automation::bend(t: f32, tension: f32) -> f32`
  (tension −1 … +1, 0 = identity, **positive = slow start, fast end**, clamped). #57's mapping bend (`Mapping::bend`,
  ±1, owner decision 2026-11-16) calls the same function on the macro value before the range, so a bend of 0.5 looks
  and sounds the same on a lane segment and on a mapping, and both UIs draw it with the same handle.
- `Hold` is a step: for switch, enum and stepped parameters every recorded or drawn segment is `Hold` automatically
  (the lane offers only `Hold` there), and on continuous targets it draws gated or rhythmic changes.
- No Bézier handles, no sine/LFO shapes in v1: the three shapes keep the file and evaluator trivial; a later shape is
  one more enum variant and a format version step.

**Before the first point** the lane holds the first point's value, **after the last point** the last one's. A lane
without points does not drive its target (the base value applies) and is removed when it is hidden and empty.

### 3.3 Where lanes and clip automation live

```rust
pub struct Track {                       // existing fields unchanged
    pub automation: TrackAutomation,     // #[serde(default)]
}
pub struct Project {                     // existing fields unchanged
    pub master_automation: Arc<TrackAutomation>,  // #[serde(default)]; the master row's lanes (§3.5)
}
pub struct TrackAutomation {
    pub shown: bool,                     // lanes visible under the track (or the master row)
    pub lanes: Vec<Arc<Lane>>,           // display order
}
pub struct Lane {
    pub id: LaneId,                      // from Project::next_id
    pub target: AutomationTarget,
    pub label: String,                   // last known target name (§3.1)
    pub mode: AutomationMode,            // Off | Read | Touch | Latch; default Read (§5, per lane: owner, Q4)
    pub points: Points,
    pub height: u16,                     // 24 ..= 160, default 40 (part of the project, like track height)
}
/// Where a lane lives; used by edits, the engine's capture events and the UI.
pub enum AutomationOwner { Track(TrackId), Master }
pub struct Clip {                        // existing fields unchanged
    pub automation: Vec<ClipAutomation>, // #[serde(default)]; MIDI and step clips
}
pub struct ClipAutomation { pub target: AutomationTarget, pub label: String, pub points: Points }
```

- One lane per target and track (an edit that would add a second one selects the existing one).
- **Every target that has clip automation on a track has a lane on that track** (an invariant the edits keep: adding
  clip automation, or moving or pasting a clip onto a track, adds the missing lane, empty and in Read). The lane holds
  the target's **mode** and shows the clip spans hatched (§4), so clip automation is never invisible in the
  arrangement. On such a lane „Automationsspur löschen“ only deletes its own points (the lane stays while clips use it).
- A lane without points and without clip automation is removed when its track's lanes are hidden, unless its mode is
  not Read.
- A bus has lanes (volume, pan, sends, its insert effects, its macros); it has no clips, so no clip automation.
- **Clip automation time** is relative to the start of the clip's *content*: `0 ..= loop_length` for a looping clip
  (`loop_length = Some`), `0 ..= length` otherwise. It moves, copies, duplicates, splits (cut at the split point, a point
  is inserted at the cut on both sides) and loops with the clip; changing the length of a non-looping clip shows more
  or less of it (points past the end are kept, as notes are). Clip automation is a property of the clip, not of the
  take: switching takes keeps it.
- **A clip automation's target refers to its track**: `Volume`, `Pan` and `Macro` follow the clip to another track;
  a `Param` or `Send` target that the new track does not have is kept and marked (§8), and works again when the clip
  comes back.

### 3.4 Interface to track macros (#57, #72)

Automation needs only three things from the track-macros model; everything else is #72's decision:

1. **Identity:** a track macro is addressed as `AutomationTarget::Macro { index }`, `index` 0 ..= 7, stable for the
   macro's life (macros are fixed slots, never inserted or reordered). If #72 picks another identity (e.g. ids), the
   variant changes, nothing else.
2. **Value:** a macro's value is normalised 0–1, so a macro lane is an ordinary lane. Automation sets the **macro
   value**; the macro's mappings fan it out to its targets (#57's engine path). Automation never writes the mapped
   parameters itself.
3. **Name:** the lane label is #72's `macros-slot-named` („Spur-Makro 1 · Open“), an unnamed macro
   `macros-slot` („Spur-Makro 5“); the chooser section is `macros-title` („Spur-Makros“) with entries `macros-entry`
   („1 · Open“ … „8 · frei“).

Overlap rule (agreed with #72's spec, its §4 and §10.1 — "the macro wins"): a parameter that a track macro drives
is a mapping target, **not an automation target, while the mapping exists**. Automation of the parameter counts as a
direct edit, and direct edits of a driven parameter do not apply:

- The chooser lists a driven parameter greyed with the hint „über Spur-Makro 1 „Open““ (`automation-mapped-by`,
  `$macro` = #72's `macros-ref`) and offers the macro's lane instead.
- An existing lane of a parameter that gets mapped is kept, does not play, and its header reads
  „⚠ gesteuert von Spur-Makro 1 „Open““ (#72's `macros-lane-overridden`); removing the mapping makes it play again.
- Touch/Latch never write a driven parameter (its control does not move); they write the macro's lane when the macro
  knob moves. Off returns the macro to its base value like every target.
- Both specs share `AutomationTarget`, `EngineTarget` and `PlainMap` (§7.1) on purpose; whichever of #65 and #73 lands
  first adds `AutomationTarget`, and they share the format step (§13).

### 3.5 The master (owner, Q11)

- The master channel (`Project::master`, volume, pan, its insert effects such as Presse or the Limiter) gets lanes like
  a track: `Project::master_automation`. Valid targets: `Volume`, `Pan` and `Param` of a device in
  `master.devices`; no `Send`, no `Macro` (the master has neither), no clip automation (no clips).
- The UI shows them under a **master row**, the last row of the arrangement (§9.6). Edits take an `AutomationOwner`
  (`Track(id)` or `Master`); removing a master insert deletes its lanes in the same step, as for tracks.
- Engine and recording treat the master as one more owner (§7.6); the mixer's master strip and the inspector's master
  view show the automation dot and the effective value like any track's controls.

---

## 4. Clip automation and arrangement lane on the same target — decision: **the clip overrides**

Where an unmuted clip that has clip automation for target T plays, **the clip automation drives T**; everywhere
else the track's lane for T drives it (or the base value, without a lane). At the clip's end the lane takes over again
(the value jumps to the lane's value; volume and pan are smoothed by the gain ramp, plugins by their own smoothing).

Why override, not relative (offset or multiply):

1. **Both are absolute curves in one domain** (§3.2). Override keeps every curve's meaning: what the clip editor
   shows is what plays during the clip, what the lane shows is what plays outside it. A relative rule makes neither
   view true on its own and needs clamping rules at 0 and 1 that users cannot see.
2. **It is the clip's purpose** (owner: envelopes "move and loop with the clip"): a filter movement written in a
   clip must sound the same wherever the clip is dropped, whatever the lane does there.
3. **The UI can show the result exactly**: the arrangement lane draws each overriding clip's span hatched with the
   clip's curve dashed on top ("durch Clip-Automation"), so the effective curve is always visible (mockup 01).
4. **Compile-time merge**: the compiler flattens lane and clip automation into one effective curve per target (§7.1),
   like it expands clip loops into notes today; the engine evaluates one curve per target and never mixes sources.

Relative clip automation (modulation on top of the lane) is a modulation feature for later; the model leaves room
(a `mode` field on `ClipAutomation` could be added in a later format version).

Rules in detail: a muted clip overrides nothing; where two clips with automation for T overlap on one track, the one
that starts later wins until its end; clip automation for T plays when the mode of T's lane is Read, Touch or Latch
(Off ignores it like the lane; §3.3 guarantees the lane exists); points before 0 or after the content length of the
clip are ignored at playback.

---

## 5. Automation modes (per lane)

| Mode | Lanes and clip automation play | Writing |
|---|---|---|
| **Off** (Aus) | no; every automated target gets its **base value** when the mode switches to Off and stays manual | never |
| **Read** (Lesen) | yes | never; a manual change is a temporary override (§6.6) |
| **Touch** | yes | while the transport plays and the lane's control is **held**; on release the value returns to the existing curve (§6.3) |
| **Latch** | yes | while the transport plays, from the first touch of a control **until the transport stops** or the loop wraps (§6.3) |

- **The mode belongs to the lane** (owner decision Q4, 2026-11-16): each lane has its own mode chip, so one lane
  can write in Touch while the others of the track read. **New lanes start in Read.** The mode is saved with the
  project; a change is one undo step („Automationsmodus“).
- The chip sits in the lane header (§9.1), in the clip editor's lane (it is the mode of the track's lane for that
  target, §3.3) and in the inspector's parameter section. A bulk action sets every lane of a track (or of the master)
  at once: „Modus für alle Automationsspuren ›“ in the track header's and the lane's context menu (one undo step,
  „Automationsmodi“). It sets the lanes; there is no stored track mode.
- **Writing needs a running transport, not the record button** (Q2): Touch and Latch are themselves the "arm", as in
  Pro Tools, Logic and Reaper. A writing lane's chip and the track header's automation button turn `state.record`
  while the transport plays, so a write mode is never silent.
- **Only lanes write.** With per-lane modes a target without a lane is in Read, so nothing writes it: to record a new
  parameter, show its lane (knob menu „Automation zeigen“, the chooser, or the inspector row) and set it to Touch or
  Latch. Moving a control of a lane in Read is a temporary override (§6.6).

---

## 6. Recording (#70)

### 6.1 Sources

All three sources reach the **engine**, which stamps every value with the tick it was applied at (the same clock as
playback, so a write lands where it was heard):

| Source | Path to the engine | Touch begin/end |
|---|---|---|
| komponist controls (device cards, plugin frame generic view, inspector, mixer faders, pan knobs, send levels, macro knobs) | `Command::SetParam`, `SetTrackGain`, `SetTrackPan`, `SetSendGain`, macro command (#57) — **for bridged plugins too** (today their card edits go over the control socket; with automation they go through the engine slot, §10.2) | new `Command::Touch { owner, target, begin }` from the gesture brackets the views already send (`BeginParamGesture` / `EndGesture`, fader drags, wheel bursts: a burst is one touch that ends 0.4 s after the last notch, as its undo step does) |
| Plugin GUIs | the plugin's output events (`ParamValue`, `ParamGesture`), read by the engine after each bridge block (§10.3) | CLAP `param_gesture_begin/end`, VST3 `beginEdit/endEdit` |
| MIDI-learned controllers (#15) | mapped in the engine (assumption for #15: the learned mapping is applied on the audio thread, addressed by `AutomationTarget` as #72 recommends too; if #15 maps on the UI thread instead, it sends the same commands as komponist's controls) | a controller has no touch sensor: the first value **begins** a touch, **500 ms** without a new value **ends** it (`MIDI_TOUCH_TIMEOUT`, Pro Tools' rule) |

The engine sends **capture events** to the app over the existing event ring: `AutomationTouch { owner, target, begin,
tick }` and `AutomationValue { owner, target, tick, value }` (`owner` = a track or the master) (normalised, at most one per target per block: the last
value of the block, stamped with its event offset). Preallocated, no allocation; when the ring is full the value is
dropped and counted (`Telemetry::dropped_events`) — the next block's value repairs the curve.

### 6.2 What is written

- The app collects a pass in a `WritePass` per lane in Touch or Latch: raw values, in ticks. It shows them live (the lane
  draws the written part in `state.record`, mockup 05) and applies them to the document once per frame inside **one
  gesture** (`Document::begin_gesture("undo-automation-record")`), so the pass is **one undo step** at the end.
- **Writing replaces** the lane's points in the written time range `[touch begin, touch end]` (plus the return ramp,
  §6.3); points outside it stay. Before the first written value a point with the curve's value at the touch begin is
  kept, so the curve does not lean into the write.
- When the target is driven by a clip automation at that moment (§4), the write goes into **that clip automation**
  (the effective source), at the clip-relative position. In a looping clip, that position is the content position, so
  every repetition changes (Q5).
- A pass that also records notes (transport in Record) ends in the take's undo step: notes and automation of one pass
  are one step, labelled as the take ("Take 3").
- Esc during a pass discards it (as it discards a running take), and cancels the gesture.

### 6.3 When writing starts and ends

- **Touch:** starts at the touch begin; ends at the touch end. **Return:** after the release, the value glides from the
  last written value back to the existing curve over **`TOUCH_RETURN` = 250 ms** (linear, in normalised values; Pro
  Tools' AutoMatch default). The pass writes that ramp as two points (release value at the release tick, the curve's
  value at release + 250 ms), and the engine plays the same ramp at once, so what is heard and what is stored agree.
- **Latch:** starts at the first touch; after the release it **keeps writing the held value** until the transport
  stops or the loop wraps. At the end it writes a point with the held value and then the curve's value 1 tick later
  (a jump back to the old curve, which is what Latch means). A new touch during the latch continues the same write.
- **Loop wrap ends a Touch or Latch write** (the touch itself may go on: a control still held at the wrap starts a new
  write at the loop start in Touch, and nothing in Latch until it is touched again). Reason: latching across the
  wrap would flatten the whole loop with the last value on the next pass. Every wrap during one play is still part of
  the same pass and the same undo step.
- **Stop** ends every write of every track; locate while playing ends writes like a stop and starts no new one until
  the next touch.
- **Read** never writes; Off never writes.

### 6.4 Thinning (reduction of recorded points)

Raw capture gives one value per target per block (~190/s at 256 frames). When a write segment ends (touch end, latch
end, wrap, stop), the app reduces it:

1. **Continuous targets:** Ramer–Douglas–Peucker in (tick, value) space, the tick axis scaled so that one beat
   counts like 1.0 of value, with a tolerance of **`THIN_EPSILON` = 0.004** (0.4 % of the range, under the resolution
   of a 200 px knob drag) plus a hard cap of one point per **`THIN_MIN_GAP` = 10 ticks** (~5 ms at 120 BPM). Kept
   points are `Linear`.
2. **Stepped, enum and bool targets:** only the change points are kept, as `Hold`.
3. The first and last raw values of the segment are always kept.

Expected density: a slow filter sweep over 8 bars → 10–30 points; a fast hand wiggle → a point per turning point.
While the segment runs, the live lane shows the raw values (cheap to draw: one polyline); thinning on segment end
keeps the drawn curve within 0.4 % of what was heard. Thinning is a pure function in `komponist-model`, unit-tested
and property-tested (no thinned value further than ε from the raw polyline).

Freehand drawing with the mouse (Ctrl+drag, §9.3) uses the same thinning.

### 6.5 Constants (v1, not user settings)

`TOUCH_RETURN` 250 ms, `MIDI_TOUCH_TIMEOUT` 500 ms, `THIN_EPSILON` 0.004, `THIN_MIN_GAP` 10 ticks, `RAMP_STEP` §7.2.
They live in `komponist-model::automation` (thinning) and `komponist-engine` (engine side), each with its source.

### 6.6 Manual edits of automated targets (no write)

A target is **automated** when its lane's mode is Read, Touch or Latch and its effective curve has points.

- **Read** (or Touch/Latch with the transport stopped): a manual change is a **temporary override**: it is heard at
  once and while the control is held the curve does not move it; after the release the value stays until the curve
  changes value, the transport starts, or the playhead is located. It is **not** stored in the device state and makes
  **no undo step** (it would have no lasting effect). The status bar says so (`automation-override-hint`).
- **Off:** the target is not automated; a change is an ordinary tracked edit of the base value, as today.
- Bridged plugins: an edit in the plugin's own GUI of an automated parameter still changes the plugin's state, so a
  state snapshot may follow (param_undo coalesces it into "<Plugin>: Zustand ändern"); harmless, documented.

The knob, fader or field of an automated target shows the **effective value at the playhead** (the UI evaluates the
same pure function as the compiler, §7.1) and the existing automation dot (`device.widget.knob.automation_dot`,
5 px, `track.base`); the dot turns `state.record` while the target is being written.

---

## 7. Playback (#66)

### 7.1 Compile

`compile::build` adds to each `TrackProgram` an `automation: Arc<[CompiledTarget]>`, rebuilt only when the track's
`Arc` changed (lanes and clips are inside the track):

```rust
pub struct CompiledTarget {
    pub target: EngineTarget,     // Slot{slot, wire_id} | Gain | Pan | Send(SendIdx) | Macro(u8)
    pub map: PlainMap,            // normalised → plain (scale, min, max, steps); identity for Gain/Pan/Send/Macro
    pub points: Arc<[CPoint]>,    // effective curve: lane + clip overrides, clip loops expanded, absolute ticks
}
```

- The **effective curve** merges the lane with every overriding clip span (§4): inside a span the clip's points
  (looped and shifted to timeline ticks, cut at the span edges with interpolated points), outside the lane's. A pure
  function `effective_points(track, target) -> Vec<Point>` in `komponist-model::automation` does it; the compiler,
  the arrangement's hatched view and the knob display all call it, so they cannot disagree.
- Targets the track cannot resolve (§8) compile to nothing. A bridged plugin's `PlainMap` needs its parameter list;
  until `Loaded` arrives the target is skipped, and the existing `ParamsChanged` event recompiles the track.
- Wire ids: internal devices and Hexa map `ParamId` → wire id through `komponist-builtin` (as `param_changes` does
  today), plugins through `ParamId::plugin_id()`. The engine never sees a `ParamId`.

### 7.2 Evaluation per block (RT)

Per track, in phase 1 (events, ARCHITECTURE §6.4), for each compiled target unless it is **held** (touched or
latched, §6.6) or its lane's mode is Off (the compiled target carries its lane's mode; a mode change is a document
edit and recompiles the track):

1. A **cursor** per target (index of the current segment, kept in the engine's per-track state, not in the program)
   advances monotonically; a locate, a loop wrap or a program swap re-seeks it with a binary search. O(1) per block
   amortised.
2. Values are emitted on a **fixed ramp grid**: one value every `RAMP_STEP` frames, where `RAMP_STEP` =
   `round(sample_rate / 750)` (64 frames at 48 kHz, ≈ 1.33 ms), **aligned to the absolute song sample position**,
   not to the block. Live playback with any buffer size and the offline export therefore produce the same events at
   the same samples (determinism, ARCHITECTURE §6.5). A value is emitted only when it differs from the last sent value
   by more than 1e-5 (flat segments cost nothing); `Hold` jumps are emitted at their exact sample.
3. Device parameters become `EventKind::Param { id, value }` (plain, via `PlainMap`) at the frame offset, merged into
   the track's sorted event list for the slot — the path that already reaches every processor sample-accurately and
   that `ExternalProcessor` already turns into wire `ParamValue` events.
4. **Volume, pan and sends** are not events: the gain stage (`klang_dsp::buffer::gain_ramp`, `pan::balance`, the send
   gain) is split at the same grid points and ramps linearly between the values, so a fade is click-free and
   sample-accurate. Volume is evaluated on the fader taper (normalised → dB → linear gain). These targets are
   evaluated **shifted by the track chain's latency** (the gain stage scales audio that left the chain that much
   later), so a fade lines up with the notes it fades; device parameters are not shifted (they act at the chain's
   input).
5. **Macros**: the macro value goes to #57's macro stage, which fans it out (§3.4).

**Built-in devices must smooth:** a parameter that clicks when it changes every 1.33 ms (cutoff, gain, mix, delay
time) gets a one-pole smoother in its `Dsp` if it has none; #66 audits Hexa, Volt, Presse, EQ, Delay, Hall, Limiter,
Sampler, and the MIDI effects take values as they come.

### 7.3 Transport events

- **Chase** (like note chasing, ARCHITECTURE §6.5): on play start, locate (also while stopped), loop wrap and on a
  program swap that changed a target's curve, every automated target gets its value at the new position at offset 0
  (or at the wrap's offset) — only if it differs from the last value sent.
- **Stopped transport:** the playhead position still drives automated targets (the user locates and hears/sees the
  value there); no ramps run.
- **Off:** switching a lane to Off sends its target's base value once (from the program, which carries the base
  values it was compiled with).
- **Touch end / override end:** the engine glides from the last manual value to the curve over `TOUCH_RETURN` when it
  was writing, and continues on the curve from the next grid point when it was a Read override.

### 7.4 Offline render and export

Export starts with `Play` (ARCHITECTURE §6.7), so the chase at the range start sets every automated target, and the
grid alignment makes the rendered automation identical to live playback. Writing never runs during an export
(`ProcessContext::offline`). The export's recompile (`export::recompile`) compiles automation with the export rate's
`RAMP_STEP`.

### 7.5 Manual edits during playback

Covered in §6.6 (Read override) and §6.3 (Touch/Latch). The engine keeps a per-owner (track or master) `held` bitset of targets
(preallocated to the compiled target count + 64 for targets without a lane, which can only override); `Command::Touch { owner, target, begin }` and the plugin's
gesture events set and clear it. While held, the curve emits nothing for the target, the manual value passes.

### 7.6 Master

- `compile::build` compiles `master_automation` into the program's master part (recompiled when `Project::master` or
  `master_automation` changed, `Arc::ptr_eq`), with the same `CompiledTarget` list.
- The engine evaluates it in step 5 of the block (ARCHITECTURE §6.4: master inserts → master fader → meter and tap)
  on the audio thread, on the same ramp grid: insert parameters as `EventKind::Param` into the master inserts' events,
  the master fader and balance as ramps (shifted by the master inserts' latency, as for tracks).
- **Limiter and Presse on the master:** their parameters are ordinary insert targets. Master volume automation is the
  master fader, which sits **after** the inserts, so a fade-out lowers the output without driving the limiter
  differently (what a song fade needs); a level change *into* the limiter is automated with the limiter's own input gain
  („Eingang“, `param-limiter-gain`). The metronome stays after the master fader and is not automated.
- Capture events carry `AutomationOwner::Master`; the master fader's touch comes from the mixer and inspector fader
  gestures (`SetMasterGain` fast path, `Command::Touch { owner: Master, … }`).
- Export: master automation renders like track automation; the optional normalisation to −1 dBFS runs on the whole
  rendered file afterwards, as today. The showcase's level check (`showcase::render_levels`) includes master
  automation.

---

## 8. Missing targets

| Situation | Lane / clip automation | Plays | Shown as |
|---|---|---|---|
| Plugin missing (`DeviceStatus::Missing`), crashed, failed to load | kept unchanged, editable (normalised) | no (nothing to drive); resumes when the plugin is back (Neu laden, rescan) | header: label in `state.error`, second line „⚠ Plugin fehlt: Surge XT“ (the arrangement's existing wording); curve in `text.4`; values as % |
| Plugin still loading | kept | from the moment the parameter list arrives (recompile) | normal |
| Parameter driven by a track macro (#57) | kept, editable | no, while the mapping exists (§3.4) | „⚠ gesteuert von Spur-Makro 1 „Open““ |
| Parameter no longer exists (plugin update) or no longer automatable | kept, editable | no | „⚠ Parameter fehlt“ / „⚠ nicht automatisierbar“ |
| Device removed from the track | **lanes deleted with it, in the same undo step**; clip automation for it kept in the clips (it is clip content) and marked | — / no | clip editor: „⚠ Gerät nicht auf dieser Spur“ |
| Device moved to another track | lanes move with it (same step); clip automation stays in the clips | yes / no (until the clip moves too) | — / as above |
| Instrument replaced (`replace_instrument`) | the old instrument's lanes deleted (same step) | — | — |
| Send removed | its lane deleted (same step) | — | — |
| Clip moved to a track without the target device | kept | no | „⚠ Gerät nicht auf dieser Spur“ |
| Track duplicated, device copied/pasted, track copied to another project | lanes and clip automation copied with remapped device and send ids | yes | — |

Keeping missing data is the rule of S06 ("a missing plugin keeps its state"), so a project opened on a machine without
Surge XT and saved again loses nothing. Deleting lanes with their device follows the routing precedent: deleting a
track repairs every reference in the same step; undo brings both back.

---

## 9. UI

Mockups in `mockups/` (HTML + PNG rendered at 2× by headless Chrome; real tokens, the repo's Plex fonts; the
unchanged panels are the app's own Linux snapshot `arrangement-s01-2x.png` / `devices-hexa-operators-1x.png`, the
new parts are drawn on top):

| File | Shows |
|---|---|
| `01-arrangement-lanes.png` | Lead with two lanes (Hexa · Feedback with a curve segment, a hold step, two selected points, the curve handle, the value tooltip while dragging, both „Lead A“ clips overriding with hatched spans; Lautstärke), Pad with a lane of a missing plugin; track-header automation toggles (Bass: automation hidden) |
| `02-parameter-chooser.png` | an empty new lane with the chooser open: search, Zuletzt bewegt, Mixer, Spur-Makros, Hexa, LSP Para EQ x8 with a parameter driven by a track macro |
| `03-header-states.png` | sheet: the track-header toggle states, every lane-header state (Read 40/24, Off, Touch idle, Touch and Latch writing, plugin missing, parameter missing, not automatable), one track whose three lanes have three different modes, the lane's mode menu and the bulk „Modus für alle Automationsspuren“ menu, points/segments/box selection, the point menu with Form ›, the value entry |
| `04-clip-automation.png` | piano roll of „Lead A“ with a clip-automation lane (chooser chip, mode chip, value, ×, selected point with tooltip, dimmed area after the content end), the toolbar toggle „Automation“; the clips in the arrangement carry the spline icon |
| `05-touch-recording.png` | playing at 14.3.000, Lead's Feedback lane in Touch writing (red raw values from the touch start to the playhead, „schreibt“, red header toggle) while its Lautstärke lane reads, Hexa card with the knob being turned (red automation dot, tooltip), Lautstärke knob with its green dot, inspector's parameter section with the Automation row, status-bar text |
| `06-master-row.png` | the master row at the end of the arrangement with two lanes (Lautstärke fade-out with a selected point and its tooltip, Limiter · Eingang in steps) under „Neue Spur“; the mixer dock with the master strip's automation dot |

All PNGs exist as `*-en.png` too (the new texts in English, the snapshot panels around them stay German).

### 9.1 Arrangement (#68) — mockups 01, 02, 03

- **Track header:** a new automation toggle (Lucide `spline`, 12 px in an 18 × 16 toggle) as its own group left of
  M S (gap `TOGGLE_GROUP_GAP`). States: no automation `text.3`; automation exists but hidden: icon in the track's
  `base` colour; shown: pressed look (`surface.5`, `text.1`); one of its lanes writing: `state.record`. Tooltip and
  AccessKit name „Automation zeigen“ / „Automation ausblenden“. The name column loses 22 px (truncation as today).
- **Lanes** sit directly under their track, indented like group members (`MEMBER_INDENT` 14 px) with the track's
  colour bar continuing on the left, so they read as part of the track. Default height 40, min 24, max 160, resized at
  the bottom edge like tracks (`HEIGHT_GRAB`).
- **Lane header** (224 px, `surface.2`; selected lane `canvas.lane-selected`):
  row 1: parameter chooser (compact dropdown, `type.ui`: "Hexa · Feedback", device part in `text.3`) and on the right
  the lane's **mode chip** (compact dropdown 20 px: Aus / Lesen / Touch / Latch; Touch/Latch tinted
  `state.record-surface` with `state.record-ink` text, Off `text.3`; lanes of one track may differ, mockup 03);
  row 2 (height ≥ 40): the effective value at the playhead (`type.value.s`, formatted by the parameter, e.g.
  "1.2 kHz", "−6.0 dB", "L 12", "42 %"), and while writing the badge „schreibt“ in `state.record`.
  Context menu: Automationsspur ausblenden · Alle Punkte löschen · Automationsspur löschen · Automationsmodus › ·
  Modus für alle Automationsspuren ›.
- **Lane body:** background `surface.1` like the arrangement; bar lines as the arrangement grid; a dashed 50 % guide
  in `line.0`; the curve 1.5 px in the track's `base` colour (`text.4` when Off or the target is missing) with a fill
  below it in the track's `body` colour at 50 % opacity; points 7 px circles in `base` with a 1 px `surface.0` ring,
  selected points `text.1` with the 1 px `accent` ring (`select.ring`); a curve handle (5 px diamond, hollow) in the
  middle of a segment on hover and for segments of selected points.
- **Clip overrides** in the lane: the span of every clip whose automation overrides this target is hatched
  (`canvas.outside-clip` stripes) and shows the clip's curve dashed, with the label „durch Clip-Automation“ in
  `type.label` at its top left. Clips with clip automation show the spline icon in their header.
- **Value tooltip** (`size.tooltip` 22 px, at once while dragging, after 600 ms on hover): "Feedback · 1.2 kHz ·
  10.1.000" — parameter, formatted value, position.
- **Parameter chooser** (mockup 02): a popover menu (width 280, wider than `size.menu-width` 220 because rows carry a
  right-aligned hint; rows `size.menu-row` 24) with a search field, „Zuletzt bewegt“ (the last 3 touched parameters
  of this track) on top, then MIXER (Lautstärke, Pan, each send „Send Hall“), „Spur-Makros“ („1 · Open“ … „8 · frei“,
  #72), then one section per device in chain order with its automatable parameters (built-in: catalog names in the UI
  language; plugins: their names; groups as `module` paths). Already automated parameters carry the 5 px dot;
  parameters driven by a track macro are greyed with „über Spur-Makro 1 „Open““ and choosing one selects the macro's
  lane (§3.4). Keyboard: arrows move, Enter takes, Esc closes, typing filters.
  Choosing a parameter in an empty lane retargets it; in a lane with points it asks nothing and adds a new lane
  instead (points are never re-interpreted for another target).
- **Adding a lane:** the chooser of the last lane offers „+ Automationsspur“; a device knob's right-click menu
  (`value_menu`) gets „Automation zeigen“, which shows the track's lanes and adds or selects that parameter's lane;
  the inspector's parameter section gets an "Automation" row with the lane's point count and mode chip (mockup 05).
- The ruler, playhead, loop band and zoom are the arrangement's; lanes scroll with the tracks.

### 9.2 Clip editor (#69) — mockup 04

- The piano roll gets clip automation lanes **between the notes and the velocity lane**, each 96 px by default
  (resizable 48–200, stored per clip in the UI state, not in the project), with the same lane body as the arrangement
  and the dimmed `canvas.outside-clip` beyond the clip's content end; a looping clip shows its repetitions dimmed.
- The keyboard column shows the spline icon; the lane's **chooser chip** sits in the lane's top-left corner
  ("Hexa · Feedback ▾"), its value, the mode chip (the mode of the track's lane for this target, §3.3) and the × to remove it at the top right. The chooser lists the clip's track's
  targets (same menu as §9.1).
- The toolbar gets „Automation“ (spline icon + text, a toggle that shows or hides the clip's automation lanes; when
  there is none, it opens the chooser). The step sequencer shows the same lanes below its grid (same widget).
- Editing is identical to §9.3; time snaps to the editor's grid.

### 9.3 Editing (both places)

| Input | Effect |
|---|---|
| Double-click on empty lane | adds a point (time snapped, value at the pointer) |
| Double-click on a point | deletes it |
| Click on a point / Shift+click | selects / adds to the selection |
| Drag on empty lane | box selection (across the lanes of one track in the arrangement) |
| Drag a point or selection | moves in time (snapped) and value; Shift: fine (× 0.1 values) and no snap; points never pass their neighbours |
| Alt+drag | copies the selection (⌥ on macOS) |
| Ctrl+drag on empty lane | draws freehand; thinned like a recording (§6.4) (⌘ on macOS) |
| Drag the curve handle up/down | bends the segment (`Curve`, tension −1 … +1); Alt+click on the handle: back to `Linear` |
| Right-click on a point or segment | Form › Linear / Kurve / Halten · Wert eingeben … · Punkte löschen |
| Enter on selected point | value text entry (formatted by the parameter, parsed back as today's knob text entry) |
| Del / Backspace | deletes the selection |
| Ctrl+C / Ctrl+V | copies points / pastes them at the playhead into the focused lane (relative times kept) |
| ← → / ↑ ↓ on a selection | moves by one grid step / 1 % (Shift: 0.1 %) |

Every edit is one undo step; a drag is one gesture. Point editing never changes the device state. Status-bar hint while
the pointer is over a lane: `automation-lane-hint` (`UI-TEXTS.md`).

### 9.4 Recording look (#70) — mockup 05

While a lane writes: its mode chip and the badge „schreibt“ in `state.record`, the track header's automation toggle
in `state.record`, the written part of the
curve in `state.record` with a `state.record-surface` fill from the touch start to the playhead, the device knob's
automation dot in `state.record`. Nothing blinks (the design has no animation).

### 9.5 Accessibility (AccessKit, rule 12)

- Lane: role `Group`, name `a11y-automation-lane` („Automation Feedback, Lead“), children: the header's controls
  (chooser `ComboBox` „Parameter der Automationsspur“, mode `ComboBox` „Automationsmodus“ with value, value text),
  and one node per **visible** point.
- Point: role **`Slider`** (it has a value that can be stepped), `selected` state, name `a11y-automation-point`
  („Punkt 10.1.000, 1.2 kHz“), the formatted value as its value text, bounds of the 7 px circle (min 10 px hit area).
  Actions: `Click` (select), `Increment`/`Decrement` (value ± 1 %); deletion through the keyboard (Del). Only visible points
  are exposed (ARCHITECTURE §9.2), so cost follows the screen.
- The track header's automation toggle: role `ToggleButton`, name „Automation zeigen“, toggled when shown.
- A missing target adds the warning text to the lane's description.
- The clip editor lanes use the same nodes with the clip editor as parent.
- The master row: role `Group`, name „Master“ (`a11y-automation-master`), its toggle and lanes as a track's
  („Automation Lautstärke, Master“).
- The issue's example "Automation Cutoff, Takt 3, 42 %" becomes lane „Automation Cutoff, Lead“ + point
  „Punkt 3.1.000, 42 %“ (positions as notes have them, `a11y-note`).

---

### 9.6 Master row (#68) — mockup 06

- **Where:** the master row is the **last row of the arrangement**, below „Neue Spur“ and the „Busse“ section, set off
  by a 1 px `line.2` rule; it scrolls with the tracks. It is always there (32 px, `size.track.min`, like a bus row), so
  the master's automation has a fixed place; its lanes are hidden until shown.
- **Header** (224 px, `surface.2`): a neutral colour bar (`text.3`; the master has no hue), „Master“ in `type.track`,
  the automation toggle and M (the master strip has only M), and the header meter. The row's body shows
  „Master · 2 Effekte“ in `type.small` `text.3`, as bus rows show their summary.
- **Lanes:** as a track's (§9.1), curves and points in neutral `text.2` with a `surface.3` fill; chooser sections
  MIXER (Lautstärke, Pan) and one per master insert. Targets of a removed insert disappear with it (§8).
- The mixer's master strip and the inspector's master view show the automation dot and the effective value; the
  master fader's gestures write a master lane in Touch/Latch.

## 10. Bridge and plugin side (#67)

### 10.1 To the plugin

- Values arrive as `EventKind::Param` in the slot's events and become wire `ParamValue` events with their frame
  offset (exists: `komponist-plugins/src/external.rs`). The bridge turns them into CLAP `CLAP_EVENT_PARAM_VALUE` events
  and VST3 `IParameterChanges` queues (one queue per parameter, points at their sample offsets).
- Only automatable parameters are compiled (§3.1), so the bridge never sends a value the plugin refused.
- **VST3 controller sync:** the host must tell the edit controller about automation. The bridge RT thread writes the
  last value per parameter of each block into a preallocated table (atomic `u64` per parameter, a dirty bitmap); the
  bridge main loop calls `IEditController::setParamNormalized` for dirty entries at most 60×/s. CLAP GUIs follow the
  events by themselves.
- **Echo guard:** a VST3 controller that answers `setParamNormalized` with `performEdit` outside a `beginEdit` pair is
  an echo, not an edit; the bridge drops such values for a parameter within 50 ms after it set it.

### 10.2 komponist's own controls for bridged plugins

Card and generic-view edits of a bridged device go through the engine (`Command::SetParam { slot, id, value }` →
`EventKind::Param` → wire event), the same path as Hexa, so the engine sees and stamps them (§6.1). The control-socket
`SetParam` stays for devices that are not in the engine (loading, offline). The bridge's output to the app
(`ParamValues` ~30 Hz, state snapshots) is unchanged.

### 10.3 From the plugin (recording)

- The plugin's output events (`ParamValue`, and `ParamGesture` begin/end, already produced by the CLAP and VST3 hosts
  in `komponist-hosting`) are written into the shared output ring per block, as today.
- **The ring's consumer moves from the app's `BridgeHandle::read_output_events` (UI thread) to
  `ExternalProcessor::process` (the engine's track job)**, which drains it right after its block (single consumer,
  lock-free, bounded) and knows the block's tick: it turns values and gestures into capture events (§6.1) and forwards
  them to the plugin manager through the engine's event ring (`EngineEvent::PluginParams { slot, … }`, a fixed-size
  batch), where today's `ParamValuesChanged` / `user_changed` / `snapshot_soon` logic continues unchanged.
- **Plugins without gestures** (values without begin/end) are treated like MIDI controllers: implicit touch,
  `MIDI_TOUCH_TIMEOUT`.
- Shm layout: no change (the ring and `WireEvent` exist); the semantics change is announced by bumping
  `PROTOCOL_VERSION` (bridge and app ship in one executable, WP-94, so they never mismatch in practice).
- Tests: the existing test plugins (CLAP and VST3 on Linux and macOS) get a parameter that a test drives by host
  automation and one whose GUI edits a test simulates with gestures (`hosting/*/tests/gui.rs` already checks gesture
  output).

---

## 11. CC lanes — decision: **later** (own epic, v0.5 or after)

Reasons: MIDI CC, pitch bend and aftertouch are **MIDI data in the clip**, not parameter values: they are recorded
from the MIDI input together with the notes (the input capture in phase 1, before the MIDI effects), they pass the
MIDI effect stage and go to the instrument as `EventKind::Midi`, and their domain is 7/14 bit with per-channel state.
That is a different capture path, a different event path and different editing (per-note-ish density, pitch bend
centre). The parts this epic builds are reusable: the lane widget, the point editing, the thinning and the file's
point encoding. A seam: `ClipAutomation` gets a second target family later (`ClipTarget::Midi { kind, cc }`), so
CC lanes live next to clip automation in the piano roll.

---

## 12. Undo

| Action | Undo label (de / en) | Notes |
|---|---|---|
| add point (double-click) | Automationspunkt setzen / Add Automation Point | |
| move points (drag, keys) | Automationspunkte verschieben / Move Automation Points | drag = one gesture |
| copy points (Alt+drag) | Automationspunkte kopieren / Copy Automation Points | |
| delete points | Automationspunkte löschen / Delete Automation Points | |
| freehand draw | Automation zeichnen / Draw Automation | one gesture |
| bend / shape | Kurve biegen / Bend Curve · Kurvenform ändern / Change Curve Shape | |
| enter value | Automationswert / Automation Value | |
| paste | Automation einfügen / Paste Automation | |
| add / delete lane, retarget | Automationsspur hinzufügen / … löschen / Automationsziel ändern | |
| clear lane | Automationsspur leeren / Clear Automation Lane | |
| lane height | Höhe der Automationsspur / Automation Lane Height | drag = one gesture |
| show / hide lanes | Automation ein- oder ausblenden / Show or Hide Automation | like „Gruppe falten“ |
| mode of a lane | Automationsmodus / Automation Mode | |
| mode of all lanes of a track | Automationsmodi / Automation Modes | bulk action |
| recording pass | Automation aufnehmen / Record Automation | or the take's label (§6.2) |

All edits are pure functions in `komponist-model::edits::automation` (`fn(&Project, args) -> EditResult`),
unit-tested and in the property tests (random edit sequences keep points sorted, values in 0..1, ≤ 2 points per tick,
lanes unique per target). Device, send and track deletion repair lanes in the same edit (§8).

---

## 13. File format (#65)

- `FORMAT_VERSION` 2 → **3**. `migrate_v2_to_v3` is the identity on the JSON (all new fields have defaults: mode
  Read, no lanes, no clip automation), with a test; a v3 file is rejected by older builds (the intended direction,
  as in v2). Track macros (#73) need a step too: whichever of #65 and #73 lands first takes 3, the other 4; if both
  ship in one release they may share one step.
- Track: `"automation": { "shown": true, "lanes": [ { "id": 41, "target": { "kind": "param", "device": 12, "param":
  "feedback" }, "label": "Hexa · Feedback", "mode": "touch", "height": 40, "points": [[0, 0.25], [3840, 0.8, 0.4],
  [7680, 0.8, "hold"], [11520, 0.1]] } ] }`. `mode` is per lane and omitted when `read`; the whole object is omitted
  when there are no lanes, so a project without automation writes no new keys.
- Project: `"master_automation": { … }`, the same object, omitted when empty.
- Clip: `"automation": [ { "target": { "kind": "volume" }, "label": "", "points": [[0, 0.7], [1920, 0.2]] } ]`.
- **Point encoding:** `[tick, value]` = Linear, `[tick, value, tension]` = Curve, `[tick, value, "hold"]` = Hold.
  Compact (a recorded lane with 2 000 points ≈ 40 KB), readable, diffable. Values as `f32` shortest round-trip.
- Round-trip tests (every target kind, every shape, clip automation in looping clips, missing devices), and a
  `fixtures` project with automation for the acceptance tests.
- Loading validates: points sorted (sorted on load, logged), values clamped to 0..1, lanes with a target the track
  cannot have (a `Param` of a device on another track) are kept and marked (§8), duplicate lanes per target merged
  (first wins, logged).

---

## 14. Engine and RT notes

- All new RT state is preallocated when the program or the track state is built off-thread: cursors and last-sent
  values per compiled target, the `held` bitset, the capture batch. `Engine::process` stays allocation-free; every
  automation test runs `process` inside `assert_no_alloc` (rule 4).
- The capture path (engine → app) uses the existing event ring with fixed-size events; overflow drops and counts.
- `Arc<[CompiledTarget]>` in the program is retired through the garbage queue like every program part.
- Event budget: per target and block at most `frames / RAMP_STEP + 2` events (34 for a 2048-frame block at 48 kHz);
  the track's event list holds `MAX_EVENTS_PER_BLOCK` (1024). A track that would exceed it (≈ 30 simultaneously
  ramping parameters at the largest block) drops the excess ramp steps, keeps jumps and the block's last value per
  target, and counts it; #66 adds a test.
- Determinism test: render the same project with block sizes 64, 256, 1000, 2048 and offline: identical event lists
  and bit-identical audio for in-process devices.

---

## 15. Performance budget

| What | Budget | Check |
|---|---|---|
| Engine, 16 tracks × 16 automated targets all ramping, 256 frames @ 48 kHz | ≤ 2 % of the block period (≈ 100 µs) on the audio thread, no allocation | new `--release --ignored` bench in `engine_perf` (#66), PERFORMANCE.md row |
| Compile of one track with 32 lanes × 2 000 points and 50 looping clips with automation | ≤ 2 ms on the UI thread | `compile` bench |
| Arrangement UI frame, 16 tracks with 4 shown lanes each, 2 000 points per lane, playing | ≤ 8 ms (ARCHITECTURE §9.2), median ≤ 2 ms | `arrangement_perf` extension (#68): lanes cull to the visible range, points aggregated per pixel column when denser than 1 per 3 px (min/max like the clip previews) |
| Recording, one target, 1 hour of touch | memory of raw values bounded per segment (thinned at segment end, ≤ 10 min of raw values kept before an interim thinning) | test |
| File size | ≤ 20 bytes per point | round-trip test asserts it |

---

## 16. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Plugins that echo host automation as edits (VST3 `performEdit` on `setParamNormalized`) | phantom writes, undo noise | echo guard (§10.1), writes only between gestures or implicit touches; test plugin with a deliberate echo |
| Zipper noise in built-in devices with stepped values every 1.33 ms | audible | smoothing audit in #66 (§7.2), listening test in the showcase |
| Moving the output-ring consumer to the engine (#67) | regressions in plugin GUI edit tracking | keep `PluginManager` logic, only change the transport of the events; existing param_undo tests must pass unchanged |
| Event list overflow on tracks with many ramping parameters | dropped steps | budget (§14), counted, test |
| Override rule surprises a user who expected clip automation to add to the lane | confusion | the hatched clip span in the lane shows the effective curve; relative mode is a later option (Q1) |
| Touch/Latch writing during plain playback without the record button | accidental writes | mode chip and header button in `state.record` while writing; Esc discards the pass; one undo step per pass (Q2) |
| Clip automation in looping clips: one touch rewrites every repetition | surprise | documented, visible live; Q5 offers "lane only" instead |
| A plugin update changes a parameter's range | lanes keep positions, not plain values | the same as every VST3 host; documented |
| UI frame cost with dense recorded lanes | jank | per-column aggregation, culling, perf test |
| macOS: plugin GUIs are not embedded yet (#52) | no plugin-GUI recording on macOS until then | komponist's controls and MIDI controllers work; plugin gestures follow #52 |

---

## 17. Order and dependencies

#65 model/format → #66 engine playback → #67 plugins over the bridge (parallel to #68 once #65 is in) → #68
arrangement UI → #69 clip editor → #70 recording (needs #66, #67, #68; MIDI controllers need #15, otherwise that part
moves to #15) → #71 showcase and docs. Track macros (#57) as targets: the `Macro` variant is in #65's model from the
start; its playback lands with #57's engine work (whichever comes second wires it).

---

## 18. Issue bodies (final, applied to #65–#71 on 2026-11-16)

The convention footer of each issue stays as it is (not repeated below). The master row (Q11) has no issue of its
own: its model is in #65, its engine part in #66, its UI in #68, its recording in #70.

### #65 Automation: model, undo and file format

> Model of automation per the automation spec §3 (internal notes, `proposals/automation/SPEC.md`; owner decisions in
> `DECISIONS.md` there).
>
> - `AutomationTarget` (volume, pan, send, device parameter by `DeviceId` + stable `ParamId`, track macro by index),
>   `AutomationOwner` (track or master), `Point { tick, value (normalised 0–1), shape: Linear | Curve{tension} | Hold }`,
>   `Points` (sorted, ≤ 2 per tick); `automation::bend(t, tension)`, shared with the track-macro mapping bend (#57).
> - `Track::automation { shown, lanes }`, `Lane { id, target, label, mode (Off/Read/Touch/Latch per lane, default
>   Read), points, height 24–160 }`, one lane per target; every target with clip automation on a track has a lane
>   there. `Clip::automation` for MIDI and step clips, relative to the content start, moved/copied/duplicated/split/
>   looped with the clip.
> - **Master:** `Project::master_automation` (volume, pan, master insert parameters; no sends, macros or clips).
> - Pure functions in `komponist-model::automation`: `value_at`, `effective_points` (the clip overrides the lane
>   where an unmuted clip plays, the later clip wins on overlap), thinning (RDP, ε 0.004, min gap 10 ticks; change
>   points for stepped targets).
> - Edits (`edits::automation`) with the undo labels of the spec §12, including the per-lane mode and the bulk „Modus
>   für alle Automationsspuren“; device/send/track/master-insert deletion deletes their lanes, device move, instrument
>   replace, track duplicate and clip move repair or keep automation per §8, all in the same step.
> - File format step (3, or 4 if track macros #73 land first; one shared step if both ship together):
>   `migrate_v2_to_v3` identity + test, compact point encoding, `mode` per lane omitted when `read`, empty objects
>   omitted; load validation.
> - Tests: unit + property tests for edits, evaluation and thinning; round-trip of every target, owner and shape; a
>   fixture project with automation including the master.
>
> Acceptance: a v2 project loads and saves byte-identical apart from the version; a project with lanes, master lanes
> and clip automation round-trips; undo/redo of every edit restores the exact project; `scripts/check.sh` passes.

### #66 Automation: engine playback

> Playback per the automation spec §7 (internal notes, `proposals/automation/SPEC.md`).
>
> - Compile the effective curve per target (lane + clip overrides, loops expanded) into `TrackProgram::automation`
>   and the master part (`CompiledTarget`: `EngineTarget`, `PlainMap`, the lane's mode; recompile only changed
>   tracks / master; targets of missing or loading plugins compile to nothing until `ParamsChanged`).
> - Evaluate per block with cursors on the ramp grid (`RAMP_STEP` = sample rate / 750, aligned to the absolute
>   sample position): device parameters as `EventKind::Param`; volume/pan/sends in the gain stage as linear ramps,
>   shifted by the chain latency; master inserts and master fader in step 5; macros to the macro stage (#57).
> - Chase on play, locate (also stopped), loop wrap and curve-changing program swaps; a lane switched to Off sends
>   its base value; held targets (`Command::Touch { owner, target, begin }`) are skipped; Touch return ramp (250 ms).
> - Capture events `AutomationTouch` / `AutomationValue` with owner and tick for the recording, RT-safe, overflow
>   counted.
> - Smoothing audit of the built-in devices' automatable parameters (spec §7.2).
> - Tests: `assert_no_alloc` around every automation test; determinism across block sizes 64/256/1000/2048 and
>   offline (with master automation); chase and loop tests; event budget overflow; a `--release --ignored` bench and
>   a PERFORMANCE.md row (16 tracks × 16 ramping targets ≤ 2 % of the period).

### #67 Automation: plugin parameters over the bridge

> Plugin side per the automation spec §10 (internal notes, `proposals/automation/SPEC.md`).
>
> - Automation values (`EventKind::Param` → wire `ParamValue` with offsets) → CLAP param events / VST3
>   `IParameterChanges` at their sample offsets; only automatable parameters.
> - VST3: sync automated values to the edit controller from the bridge main loop (≤ 60/s); echo guard (50 ms).
> - Card and generic-view edits of bridged devices go through the engine slot (`Command::SetParam`) when the device
>   runs in the engine; the control-socket path stays otherwise.
> - The output ring's consumer moves to `ExternalProcessor` (engine): values and gestures (`ParamGesture`) become
>   capture events with ticks (owner decision: the engine stamps plugin-GUI edits) and are forwarded to the plugin
>   manager, whose existing edit/undo/snapshot logic stays. Plugins without gestures: implicit touch with 500 ms
>   timeout. `PROTOCOL_VERSION` bump.
> - Tests with the CLAP and VST3 test plugins on Linux and macOS: host automation reaches the plugin at the right
>   offsets (also on a master insert); GUI gestures produce touch begin/end and values with ticks; an echoing test
>   plugin writes nothing; existing `param_undo` tests unchanged. `scripts/check-gui.sh` on Linux.

### #68 Automation: lanes in the arrangement

> Arrangement UI per the automation spec §9.1, §9.3, §9.5, §9.6 and mockups 01–03, 06 (internal notes,
> `proposals/automation/`).
>
> - Track header automation toggle (spline icon; states: none, hidden, shown, a lane writing); lanes under the track
>   (indented, colour bar, height 24–160, default 40); lane header with parameter chooser, **per-lane mode chip**
>   (Aus/Lesen/Touch/Latch), value; „Modus für alle Automationsspuren ›“ in the track and lane menus; lane body with
>   grid, curve, fill, points, curve handles, hatched clip overrides, value tooltip.
> - **Master row** as the last arrangement row (32 px, neutral bar, „Master“, automation toggle, M, meter, summary
>   text) with its lanes (volume, pan, master inserts); automation dot and effective value on the mixer's master strip.
> - Parameter chooser popover (search, Zuletzt bewegt, Mixer, Spur-Makros, devices in chain order, automated dot,
>   driven parameters greyed „über Spur-Makro …“); „Automation zeigen“ in the knob value menu; "Automation" row
>   (points, mode chip) in the inspector's parameter section.
> - Editing per spec §9.3 (double-click add/delete, select, box select, drag with snap, Shift fine, Alt copy,
>   Ctrl freehand, curve bend, shape menu, value entry, Del, copy/paste, arrow keys), every edit one undo step.
> - Missing targets per spec §8 (warning line, grey curve, values in %).
> - Knob/fader automation dot and the effective value at the playhead on cards, inspector and mixer.
> - AccessKit per spec §9.5; catalog texts from `UI-TEXTS.md` in `de`/`en`; snapshots German and English (lanes,
>   mixed lane modes, chooser, missing target, Off, master row), live check with egui-mcp; arrangement perf test
>   with dense lanes.

### #69 Automation: clip automation in the clip editor

> Clip editor UI per the automation spec §9.2 and mockup 04 (internal notes, `proposals/automation/`).
>
> - Clip automation lanes between notes and velocity in the piano roll and below the step grid of step clips (same
>   lane widget as #68), chooser chip with the clip's track's targets, value, mode chip (the mode of the track's lane
>   for that target) and remove at the lane's top; „Automation“ toggle in the toolbar; dimmed area beyond the content
>   end and dimmed loop repetitions.
> - Editing identical to the arrangement lanes, snapping to the editor grid; the arrangement clip shows the spline
>   icon; the track gets a lane for every clip-automated target, which shows the clip's override span (#68).
> - Targets not on the track („⚠ Gerät nicht auf dieser Spur“).
> - AccessKit, catalogs `de`/`en`, snapshots German and English.

### #70 Automation: recording (Read, Touch, Latch)

> Recording per the automation spec §5 and §6 and mockup 05 (internal notes, `proposals/automation/`).
>
> - Mode **per lane** (Off/Read/Touch/Latch, default Read) in the lane header, the clip editor's lane and the
>   inspector's parameter row; bulk „Modus für alle Automationsspuren“; one undo step per change. Only lanes write: a
>   target without a lane is not recorded.
> - Touch and Latch write while the transport plays (no record button needed); Touch returns over 250 ms; Latch holds
>   until stop; a loop wrap ends a write; writing into a target driven by clip automation writes into that clip
>   automation (in a looping clip: every repetition).
> - Sources: komponist controls (`Command::Touch` from the existing gesture brackets, including the master fader and
>   master insert cards), plugin GUI gestures (#67), MIDI-learned controllers (#15; implicit touch, 500 ms timeout) —
>   if #15 is not done, that source moves to #15.
> - Live display of the written part; thinning at each segment end (spec §6.4); one undo step per pass (or the take's
>   step when notes were recorded); Esc discards the pass.
> - Read overrides of automated targets are temporary and not undo steps (spec §6.6).
> - Tests: capture path RT safety (`assert_no_alloc`), touch/latch timing, wrap and stop behaviour, lanes in
>   different modes on one track, master lanes, thinning, undo per pass, echo-free plugin writing (with #67's test
>   plugins).

### #71 Automation: showcase and docs

> - Demo project (`showcase_project()`): a filter sweep lane on the pad's or lead's cutoff over the build-up, a
>   volume fade-in lane on one track, a clip automation (e.g. Hexa feedback swell in "Lead B") that loops with the
>   clip, a master volume fade-out at the end, the Lead track's lanes shown; mix balanced
>   (`showcase::render_levels`: master peak ≤ −0.3 dBFS, limiter ≤ ~2 dB, no silent track); showcase tests updated.
> - ARCHITECTURE: new §6.9 Automation (model, per-lane modes, override rule, compile, ramp grid, chase, master, capture
>   path, bridge consumer change), §5.5 format step, §14 row updated; PERFORMANCE.md rows from #66/#68; I18N glossary
>   entries (Automationsspur / Automation Lane, Clip-Automation / Clip Automation, Lesen / Read, Touch, Latch).

# Automation — decisions for the owner (#64)

**Decided 2026-11-16: see `DECISIONS.md`** (Q4 and Q11 deviate from the recommendations below).

Each question: options, **recommendation**, reason in one or two lines. Section numbers refer to `SPEC.md`.

**Q1 — Clip automation and arrangement lane on the same target (§4).**
(a) **Clip overrides the lane while the clip plays**; (b) relative: clip offsets or multiplies the lane; (c) clip
plays only where the track has no lane. **Recommendation: (a).** Both curves stay true on their own, the arrangement
lane shows the overriding clip spans hatched with the clip's curve, and the compiler merges once. Relative clip
automation is modulation and can come later.

**Q2 — What arms writing in Touch/Latch (§5).**
(a) **Running transport is enough** (Play or Record), the mode is the arm (Pro Tools, Logic, Reaper); (b) only while
the transport records (Ableton's automation arm + record); (c) only on tracks with R armed. **Recommendation: (a).**
That is what Touch and Latch mean everywhere; the header button, the lane chip and the status bar turn red while
writing, Esc discards the pass, and a pass is one undo step.

**Q3 — Stored value domain (§3.2).** (a) **Normalised 0–1**; (b) plain values (Hz, dB). **Recommendation: (a)**,
please confirm. Curves follow the knob's and fader's taper, lanes stay intact and editable without the plugin, VST3 is
normalised anyway; #72 uses the same domain for macro ranges.

**Q4 — Mode per track or per lane (§5).** (a) **Per track** (your #70 text), shown on every lane header of the track;
(b) per lane; (c) per track with a per-lane override. **Recommendation: (a)** for v1. Touch writes only what you touch,
so per-lane modes are rarely needed; (c) can be added without a format break (an optional field).

**Q5 — Writing where clip automation drives the target (§6.2).**
(a) **Write into the source that is playing** (the clip automation; in a looping clip every repetition changes);
(b) always write the arrangement lane (then the clip still overrides it and the write is inaudible there).
**Recommendation: (a).** What you hear while writing is what stays.

**Q6 — Manual change of an automated target outside a write (§6.6).**
(a) **Temporary override**: heard until the curve moves, playback starts or you locate; not stored, no undo step;
(b) stored as the base value (undo step, no audible lasting effect); (c) Ableton-style persistent override with a
"re-enable automation" button. **Recommendation: (a).** No meaningless undo steps, no hidden override state; the status
bar says that Touch or Latch writes it.

**Q7 — Latch at a loop wrap (§6.3).** (a) **A wrap ends the write**; (b) Latch continues across wraps until stop.
**Recommendation: (a).** Otherwise the held value flattens the whole loop on the next pass.

**Q8 — CC lanes (§11).** (a) **Later, own epic** (v0.5+), reusing the lane widget and thinning; (b) in this epic.
**Recommendation: (a).** CCs are MIDI data in the clip with their own capture path (MIDI input, before the MIDI
effects), event path and editing; parameters are not.

**Q9 — Lanes of a removed device (§8).** (a) **Deleted with the device in the same undo step** (undo restores both);
(b) kept as orphan lanes. **Recommendation: (a).** Same rule as routing and #72's mappings. A *missing* plugin keeps
everything (S06 rule).

**Q10 — Clip automation in step-sequencer clips (§3.3, §9.2).** (a) **Yes, v1**, same widget under the step grid;
(b) piano-roll clips only. **Recommendation: (a).** The model is on `Clip`, the widget is shared; a drum filter
movement inside a pattern is a typical use.

**Q11 — Sends and master (§3.1).** Send levels as targets in v1: **yes** (sends exist since wave 9, fast path
`SetSendGain` is there). Master channel and its inserts: **later** (no arrangement row for the master yet).
Recommendation as stated; say if the master should come in v1 (it needs a master row in the arrangement).

**Q12 — Timestamps of plugin-GUI edits (§10.3).** (a) **The engine reads the plugin's output events after each block
and stamps them with the block's tick** (the output ring's consumer moves from the UI thread to the engine); (b) the
UI stamps them with the playhead when it reads them (simpler, 10–30 ms late). **Recommendation: (a).** Writes land
where they were heard, the same clock as komponist's own controls and MIDI controllers.

**Q13 — Fixed constants vs settings (§6.5).** Touch return 250 ms, MIDI-controller touch timeout 500 ms, thinning
tolerance 0.4 %, ramp grid ≈ 1.33 ms. **Recommendation:** fixed in v1, settings only if real use asks for them.

**Q14 — A default shortcut for "Automation zeigen".** `A` is the computer keyboard's C4 when a track is armed (§13
item 2 of ARCHITECTURE). **Recommendation:** a keymap action without a default key in v1 (the header button, the knob
menu and the track menu reach it); assign one later if wanted.

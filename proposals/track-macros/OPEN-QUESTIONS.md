# Track macros — open questions for the owner (#72)

**Decided 2026-11-16 — see `DECISIONS.md`** (all as recommended, except Q3: every mapping gets a bend). Kept as asked.

Each with options and the recommendation (**bold**). References are to `SPEC.md`.

**Q1 — Direct edit of a driven parameter** (§4)
- A **The macro wins**: the driven control is locked, shows `f(macro)`, a drag shows a status text and selects the
  macro; plugin-GUI edits are re-asserted at gesture end; an automation lane on it does not play.
- B The edit detaches the mapping.
- C Offset: the edit moves a base value, the macro adds its range around it (modulation model).
- Recommendation: **A** — predictable, one source of truth, what Ableton does; C is modulation and belongs to #15.

**Q2 — Mixer targets** (§1, §6.3)
- A Device parameters only (the epic's wording).
- B **Also the track's volume, pan and send levels**, through the automation engine's gain-stage ramps.
- Recommendation: **B** — the same target type as automation, little extra engine code once #66 exists, and the
  showcase's "more reverb" is naturally the Pad's send to the Hall bus. If #66 is late, B follows in #76.

**Q3 — Curve per mapping** (§3.3)
- A **No curve in v1**: linear in the parameter's normalised scale (already log for Hz and time, fader taper for dB).
- B A bend ±1 per mapping (the automation spec's `Curve { tension }`), a third control per row.
- Recommendation: **A**; B can be added later with one optional field and a format step.

**Q4 — Where the 8 controls live** (§5.1)
- A **Card pinned at the head of the device chain + a „Spur-Makros“ section in the inspector** (with the mapping list).
- B Chain only. C Inspector only. D Also the track header (tall tracks) or a mixer strip mode.
- Recommendation: **A**; D later if wanted.

**Q5 — UI terms** (§2, §13)
- „Spur-Makro(s)“ / "Track macro(s)"; the device concept „Geräte-Makro“ / "Device macro"; never a bare „Makro“;
  an unnamed macro shows its number badge and „frei“ / "unused", never „Makro 5“; mapping = „Zuordnung“ / "Mapping",
  the mode „Zuordnen“ / "Map".
- Recommendation: **accept as proposed**. Alternative for the mode: „Zuweisen“ / "Assign".

**Q6 — Several macros on one parameter** (§3.5)
- A **One driver per parameter**: mapping it to another macro moves the mapping.
- B Allowed, last change wins (Ableton). C Allowed, contributions add up.
- Recommendation: **A** — the badge on the knob stays unambiguous.

**Q7 — Removing a device that has mappings** (§7)
- A **Delete its mappings in the same undo step** (as automation deletes its lanes; status text names the count).
- B Keep them as dangling „fehlt“ rows.
- Recommendation: **A**. A *missing* plugin (S06) always keeps its mappings.

**Q8 — Default range of a new mapping** (§5.3)
- A **No jump**: the range is chosen so the parameter keeps its current value at the macro's current value (a fresh
  macro at 0 % gives „current value … maximum“).
- B Full range (Ableton): the parameter jumps to `f(macro)` at once.
- Recommendation: **A** — mapping never changes the sound.

**Q9 — Value after unmapping** (§3.6)
- A **The parameter keeps the value the macro gave it** (written into the device state in the same undo step).
- B It returns to the value it had before it was mapped.
- Recommendation: **A** — B needs a stored value per mapping and surprises after long sessions.

**Q10 — Shortcut for mapping mode** (§5.3)
- A **`Strg Shift M` / `⇧⌘M`** (⌘M is the macOS "minimize" shortcut; plain M mutes tracks).
- B No shortcut in v1.
- Recommendation: **A**.

**Q11 — Mappings per macro** (§3.4, §6.5)
- A **16 per macro** (128 per track; fits the per-track event budget shared with automation).
- B 8. C Unlimited (needs an unbounded RT table, against rule 4).
- Recommendation: **A**.

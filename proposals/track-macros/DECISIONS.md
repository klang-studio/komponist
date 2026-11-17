# Track macros — owner decisions (2026-11-16)

The owner decided the 11 questions of `OPEN-QUESTIONS.md`. `SPEC.md` is updated to them.

| Q | Decision | Spec |
|---|---|---|
| Q1 | **The macro wins**: a driven control is locked and shows `f(macro)`; plugin-GUI edits are re-asserted at gesture end; a lane on a driven parameter does not play | §4 |
| Q2 | **Mixer targets too**: the track's volume, pan and send levels besides device parameters | §1, §6.3 |
| Q3 | **Deviation from the proposal: every mapping has a bend** (−1 … +1, the automation spec's `Curve { tension }` shape) in v1 | §3.2, §3.3, §5.4, §6.3, §11, §13 |
| Q4 | Card pinned at the head of the device chain + „Spur-Makros“ section in the inspector | §5.1 |
| Q5 | Terms as proposed: „Spur-Makro“ / "Track macro", „Geräte-Makro“ / "Device macro", „Zuordnung“ / "Mapping", „Zuordnen“ / "Map", unnamed = „frei“ / "unused", never a bare „Makro“ | §2, §13 |
| Q6 | One macro per parameter (mapping it elsewhere moves the mapping) | §3.5 |
| Q7 | Removing a device deletes its mappings in the same undo step (a missing plugin keeps them) | §3.6, §7 |
| Q8 | No jump: a new mapping's range keeps the parameter's current value | §5.3 |
| Q9 | The value stays after unmapping | §3.6 |
| Q10 | `Strg Shift M` / `⇧⌘M` toggles mapping mode | §5.3 |
| Q11 | At most 16 mappings per macro | §3.4, §6.5 |

Consequences of the automation spec's changes (owner, 2026-11-16):

- **Automation modes are per lane**: the macro's lane has its own mode; Off on that lane returns the macro to its base
  value; a driven parameter's own lane stays silent whatever its mode (SPEC §10.1).
- **The master channel is an automation target in v1** (master row in the arrangement). Track macros are not affected:
  the master has no macros, is never a mapping target, and master lanes never drive a track macro (SPEC §1, §10.1).

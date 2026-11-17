# Automation — owner decisions (#64)

Date: **2026-11-16**. The owner decided all 14 questions of `OPEN-QUESTIONS.md`. `SPEC.md`, `UI-TEXTS.md` and the
mockups include these decisions; the issue texts of #65–#71 were updated from `SPEC.md` §18 the same day.

| Q | Question | Decision |
|---|---|---|
| Q1 | Clip automation and arrangement lane on the same target | **As recommended:** the clip overrides the lane while it plays (§4). |
| Q2 | What arms writing in Touch/Latch | **As recommended:** a running transport is enough; no record button (§5). |
| Q3 | Stored value domain | **As recommended:** normalised 0–1 (§3.2). |
| Q4 | Mode per track or per lane | **Deviation: per lane.** Every lane has its own mode chip (Off/Read/Touch/Latch, new lanes Read); a bulk action „Modus für alle Automationsspuren“ sets all lanes of a track; no stored track mode (§3.3, §5, §9.1). Consequence: only lanes write — a target without a lane is not recorded; every clip-automated target gets a lane that holds its mode. |
| Q5 | Writing where clip automation drives the target | **As recommended:** write into the source that is playing (the clip automation; in a looping clip every repetition changes) (§6.2). |
| Q6 | Manual change of an automated target outside a write | **As recommended:** temporary override, not stored, no undo step (§6.6). |
| Q7 | Latch at a loop wrap | **As recommended:** the wrap ends the write (§6.3). |
| Q8 | CC lanes | **As recommended:** later, own epic (§11). |
| Q9 | Lanes of a removed device | **As recommended:** deleted with the device in the same undo step (§8). |
| Q10 | Clip automation in step clips | **As recommended:** yes, in v1 (§3.3, §9.2). |
| Q11 | Sends and master | **Deviation: sends and the master channel (volume, pan, its inserts) in v1.** The master gets a master row as the last row of the arrangement, with its own lanes (§3.5, §7.6, §9.6, mockup 06). |
| Q12 | Timestamps of plugin-GUI edits | **As recommended:** the engine reads the plugin's output events after each block and stamps them (§10.3). |
| Q13 | Fixed constants vs settings | **As recommended:** fixed in v1 (§6.5). |
| Q14 | Default shortcut for „Automation zeigen“ | **As recommended:** none by default (keymap action only). |

Related decision from the track-macro spec (#72, same day): each macro mapping gets a **bend** (±1). It uses the
automation's curve function `komponist_model::automation::bend(t, tension)` (SPEC §3.2), so both share one shape
function and one sign convention (positive = slow start).

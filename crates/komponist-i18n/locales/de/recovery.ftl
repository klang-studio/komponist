## The recovery prompt at start (#227, crash-recovery proposal (internal notes); `dialogs/recovery.rs`).
## Terms (owner decision Q8): „Sicherungskopie“, „Wiederherstellen“, „Später“, „Verwerfen“.

# Title of the prompt (mockup 01).
recovery-title = Ungespeicherte Änderungen wiederherstellen?

# Header counter when several copies wait (mockup 02); `$n` is this copy, `$total` all of them.
recovery-counter = { $n } von { $total }

# Text of variants a, c and d: a saved project; `$name` is the project name.
recovery-text-saved = komponist wurde nicht regulär beendet. Von „{ $name }“ gibt es eine Sicherungskopie mit Änderungen, die noch nicht in der Projektdatei stehen.

# Text of variant b: a project that was never saved.
recovery-text-untitled = komponist wurde nicht regulär beendet. Vom ungespeicherten Projekt „{ $name }“ gibt es eine Sicherungskopie.

# Text of variant e: the copy cannot be read; `$reason` is one of the `project-reason-*` texts.
recovery-text-unreadable = komponist wurde nicht regulär beendet. Die Sicherungskopie von „{ $name }“ lässt sich nicht lesen: { $reason }.

# Label of the fact: when the copy was written.
recovery-fact-copy = Sicherungskopie

# Label of the fact: the project file's state and path.
recovery-fact-file = Projektdatei

# The project file is there; `$time` is "heute 13:05", `$path` the shortened path.
recovery-file-saved = gespeichert { $time } · { $path }

# The project was never saved (also the path line of its row on the start screen).
recovery-file-never = nie gespeichert

# The project file is gone; `$path` is the shortened path.
recovery-file-gone = nicht gefunden · { $path }

# Notice of variant c (neutral dot).
recovery-notice-newer = Die Projektdatei wurde nach der Sicherungskopie gespeichert. Wiederherstellen öffnet den Stand der Kopie; die Datei ändert sich erst, wenn du speicherst.

# Notice of variant d (red dot).
recovery-notice-gone = Die Projektdatei gibt es nicht mehr. Nach dem Wiederherstellen fragt Speichern nach einem Ort.

# Button: move the copy away (kept 7 days without UI) and continue.
recovery-discard = Verwerfen

# Button (Esc, ×): ask again at the next start.
recovery-later = Später

# Primary button (Enter): open the copy.
recovery-recover = Wiederherstellen

## The recovery prompt at start (#227, crash-recovery proposal (internal notes); `dialogs/recovery.rs`).
## Terms (owner decision Q8): "recovery copy", "Recover", "Later", "Discard".

# Title of the prompt (mockup 01).
recovery-title = Recover Unsaved Changes?

# Header counter when several copies wait (mockup 02); `$n` is this copy, `$total` all of them.
recovery-counter = { $n } of { $total }

# Text of variants a, c and d: a saved project; `$name` is the project name.
recovery-text-saved = komponist did not quit normally. There is a recovery copy of “{ $name }” with changes that are not in the project file yet.

# Text of variant b: a project that was never saved.
recovery-text-untitled = komponist did not quit normally. There is a recovery copy of the unsaved project “{ $name }”.

# Text of variant e: the copy cannot be read; `$reason` is one of the `project-reason-*` texts.
recovery-text-unreadable = komponist did not quit normally. The recovery copy of “{ $name }” cannot be read: { $reason }.

# Label of the fact: when the copy was written.
recovery-fact-copy = Recovery copy

# Label of the fact: the project file's state and path.
recovery-fact-file = Project file

# The project file is there; `$time` is "today 13:05", `$path` the shortened path.
recovery-file-saved = saved { $time } · { $path }

# The project was never saved (also the path line of its row on the start screen).
recovery-file-never = never saved

# The project file is gone; `$path` is the shortened path.
recovery-file-gone = not found · { $path }

# Notice of variant c (neutral dot).
recovery-notice-newer = The project file was saved after the recovery copy. Recover opens the state of the copy; the file changes only when you save.

# Notice of variant d (red dot).
recovery-notice-gone = The project file no longer exists. After recovering, Save asks for a location.

# Button: move the copy away (kept 7 days without UI) and continue.
recovery-discard = Discard

# Button (Esc, ×): ask again at the next start.
recovery-later = Later

# Primary button (Enter): open the copy.
recovery-recover = Recover

## Recording (`Screens.dc.html` S02, `Prototyp.dc.html` `toggleRec`, `finishRec`).

# The piano roll's selection info while recording (`selT:'live'`).
record-live = live

# R without an armed or selected track (status message).
record-no-track = Keine Spur scharfgeschaltet · R im Spur-Header

# Name of a take. Made in the current language when the take is recorded, content afterwards.
record-take = Take { $number }

# Name of the clip being recorded (S02 `mc(…, '● Aufnahme · Take 1', …)`); `$take` is the take's name.
record-clip = ● Aufnahme · { $take }

# The arrangement's info while recording (S02 `info`); `$tracks` are the armed track names.
record-info = Aufnahme · { $tracks }

# Status after Stop (`finishRec`); `$take` is the take's name, `$notes` the number of notes.
record-recorded = { $take } aufgenommen · { $notes ->
        [one] 1 Note
       *[other] { $notes } Noten
    } · Q quantisiert

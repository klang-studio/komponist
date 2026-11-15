## Shared by the dialogs and the start screen (`dialogs/`): common buttons, places and the dates the dialogs compute.

# Name and tooltip of the × button of a dialog.
dialog-close = Schließen

# Cancel button of the dialogs.
dialog-cancel = Abbrechen

# The master channel as the place of a device.
dialog-master = Master

# A track as the place of a device or sample (S06 "Spur Arp"); `$name` is the track name.
dialog-track-place = Spur { $name }

# A place whose track is gone.
dialog-track-unknown = Spur

## Dates and relative times (S09 recent list, S07 last scan). Months as the screens write them.

# January, abbreviated.
dialog-month-1 = Jan.

# February, abbreviated.
dialog-month-2 = Feb.

# March, abbreviated.
dialog-month-3 = März

# April, abbreviated.
dialog-month-4 = Apr.

# May, abbreviated.
dialog-month-5 = Mai

# June, abbreviated.
dialog-month-6 = Juni

# July, abbreviated.
dialog-month-7 = Juli

# August, abbreviated.
dialog-month-8 = Aug.

# September, abbreviated.
dialog-month-9 = Sept.

# October, abbreviated.
dialog-month-10 = Okt.

# November, abbreviated.
dialog-month-11 = Nov.

# December, abbreviated.
dialog-month-12 = Dez.

# A date in the current year; `$month` is one of the `dialog-month-*` texts.
dialog-date = { $day }. { $month }

# A date in another year.
dialog-date-year = { $day }. { $month } { $year }

# Today, as the day of a time ("heute 09:12").
dialog-today = heute

# Yesterday, alone or as the day of a time.
dialog-yesterday = gestern

# A day and a time of day; `$day` is `dialog-today`, `dialog-yesterday` or a date, `$time` is "09:12".
dialog-day-time = { $day } { $time }

# Less than a minute ago (S09 recent).
dialog-just-now = gerade eben

# Minutes ago, today.
dialog-minutes-ago = { $n ->
        [one] vor 1 Minute
       *[other] vor { $n } Minuten
    }

# Hours ago, today.
dialog-hours-ago = { $n ->
        [one] vor 1 Stunde
       *[other] vor { $n } Stunden
    }

# Latency next to the buffer size (S07); `$ms` is "5.3".
dialog-latency = { $ms } ms Latenz

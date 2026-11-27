## S06 missing plugins and files (`dialogs/missing_plugins.rs`).

# S06 secondary button: rescan the plugin folders.
missing-check-folders = Plugin-Ordner prüfen

# S06 primary button with several plugins.
missing-continue-many = Ohne diese Plugins fortfahren

# S06 primary button with one plugin.
missing-continue-one = Ohne dieses Plugin fortfahren

# Button of a missing plugin file: look for the file.
missing-search = Suchen …

# Role in a place: the track's instrument.
missing-role-instrument = Instrument

# Role in a place: an audio effect; `$n` is its number in the chain.
missing-role-effect = Effekt { $n }

# Role in a place: a MIDI effect; `$n` is its number in the chain.
missing-role-midi-effect = MIDI-Effekt { $n }

# Place of a missing device; `$owner` is a track or the master, `$role` one of the roles above.
missing-place = { $owner } · { $role }

# Title with plugins only (S06).
missing-title-plugins = { $n ->
        [one] 1 Plugin fehlt
       *[other] { $n } Plugins fehlen
    }

# S06 text under the title; `$project` is the project name.
missing-text = { $n ->
        [one] „{ $project }“ ist geöffnet. Dieses Gerät ist nicht installiert oder liegt nicht in einem Plugin-Ordner. Seine Einstellungen bleiben im Projekt gespeichert und werden wieder geladen, sobald das Plugin gefunden wird.
       *[other] „{ $project }“ ist geöffnet. Diese Geräte sind nicht installiert oder liegen nicht in einem Plugin-Ordner. Ihre Einstellungen bleiben im Projekt gespeichert und werden wieder geladen, sobald das Plugin gefunden wird.
    }

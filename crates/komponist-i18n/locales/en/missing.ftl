## S06 missing plugins and files (`dialogs/missing_plugins.rs`).

# S06 secondary button: rescan the plugin folders.
missing-check-folders = Check Plugin Folders

# S06 primary button with several plugins.
missing-continue-many = Continue Without These Plugins

# S06 primary button with one plugin.
missing-continue-one = Continue Without This Plugin

# Button of a missing plugin file: look for the file.
missing-search = Search…

# Role in a place: the track's instrument.
missing-role-instrument = Instrument

# Role in a place: an audio effect; `$n` is its number in the chain.
missing-role-effect = Effect { $n }

# Role in a place: a MIDI effect; `$n` is its number in the chain.
missing-role-midi-effect = MIDI Effect { $n }

# Place of a missing device; `$owner` is a track or the master, `$role` one of the roles above.
missing-place = { $owner } · { $role }

# Title with plugins only (S06).
missing-title-plugins = { $n ->
        [one] 1 Plugin Missing
       *[other] { $n } Plugins Missing
    }

# S06 text under the title; `$project` is the project name.
missing-text = { $n ->
        [one] “{ $project }” is open. This device is not installed or is not in a plugin folder. Its settings stay saved in the project and load again as soon as the plugin is found.
       *[other] “{ $project }” is open. These devices are not installed or are not in a plugin folder. Their settings stay saved in the project and load again as soon as the plugins are found.
    }

## S06 missing plugins and samples (`dialogs/missing_plugins.rs`).

# S06 secondary button: rescan the plugin folders.
missing-check-folders = Plugin-Ordner prüfen

# S06 primary button with several plugins.
missing-continue-many = Ohne diese Plugins fortfahren

# S06 primary button with one plugin.
missing-continue-one = Ohne dieses Plugin fortfahren

# Primary button with several missing samples only.
missing-continue-samples = Ohne diese Samples fortfahren

# Primary button with one missing sample only.
missing-continue-sample = Ohne dieses Sample fortfahren

# Button of a missing sample: look for the file.
missing-search = Suchen …

# Format chip of a sample row.
missing-sample-chip = SAMPLE

# Format chip of a built-in device.
missing-format-builtin = INTERN

# Role in a place: the track's instrument.
missing-role-instrument = Instrument

# Role in a place: an audio effect; `$n` is its number in the chain.
missing-role-effect = Effekt { $n }

# Role in a place: a MIDI effect; `$n` is its number in the chain.
missing-role-midi-effect = MIDI-Effekt { $n }

# Place of a missing device; `$owner` is a track or the master, `$role` one of the roles above.
missing-place = { $owner } · { $role }

# Place of a missing sample; `$pad` is the pad number from 1.
missing-place-pad = { $owner } · Pad { $pad }

# Place of a missing sample in the general-purpose Sampler.
missing-place-sampler = { $owner } · Sampler

# Title with plugins only (S06).
missing-title-plugins = { $n ->
        [one] 1 Plugin fehlt
       *[other] { $n } Plugins fehlen
    }

# Title with samples only.
missing-title-samples = { $n ->
        [one] 1 Sample fehlt
       *[other] { $n } Samples fehlen
    }

# Part of the title with both: the plugins.
missing-count-plugins = { $n ->
        [one] 1 Plugin
       *[other] { $n } Plugins
    }

# Part of the title with both: the samples.
missing-count-samples = { $n ->
        [one] 1 Sample
       *[other] { $n } Samples
    }

# Title with plugins and samples; `$plugins` and `$samples` are the two counts.
missing-title-both = { $plugins } und { $samples } fehlen

# S06 text under the title; `$project` is the project name.
missing-text = { $n ->
        [one] „{ $project }“ ist geöffnet. Dieses Gerät ist nicht installiert oder liegt nicht in einem Plugin-Ordner. Seine Einstellungen bleiben im Projekt gespeichert und werden wieder geladen, sobald das Plugin gefunden wird.
       *[other] „{ $project }“ ist geöffnet. Diese Geräte sind nicht installiert oder liegen nicht in einem Plugin-Ordner. Ihre Einstellungen bleiben im Projekt gespeichert und werden wieder geladen, sobald das Plugin gefunden wird.
    }

# Text under the title with samples only; mentions the button `missing-search` as "Suchen".
missing-samples-text = { $n ->
        [one] „{ $project }“ ist geöffnet. Dieses Sample liegt nicht mehr an seinem Platz. Mit „Suchen“ verknüpfst du sie neu; die Pads bleiben im Projekt gespeichert.
       *[other] „{ $project }“ ist geöffnet. Diese Samples liegen nicht mehr an ihrem Platz. Mit „Suchen“ verknüpfst du sie neu; die Pads bleiben im Projekt gespeichert.
    }

## S06 missing plugins and samples (`dialogs/missing_plugins.rs`).

# S06 secondary button: rescan the plugin folders.
missing-check-folders = Check Plugin Folders

# S06 primary button with several plugins.
missing-continue-many = Continue Without These Plugins

# S06 primary button with one plugin.
missing-continue-one = Continue Without This Plugin

# Primary button with several missing samples only.
missing-continue-samples = Continue Without These Samples

# Primary button with one missing sample only.
missing-continue-sample = Continue Without This Sample

# Button of a missing sample: look for the file.
missing-search = Search…

# Format chip of a sample row.
missing-sample-chip = SAMPLE

# Format chip of a built-in device.
missing-format-builtin = BUILT-IN

# Role in a place: the track's instrument.
missing-role-instrument = Instrument

# Role in a place: an audio effect; `$n` is its number in the chain.
missing-role-effect = Effect { $n }

# Role in a place: a MIDI effect; `$n` is its number in the chain.
missing-role-midi-effect = MIDI Effect { $n }

# Place of a missing device; `$owner` is a track or the master, `$role` one of the roles above.
missing-place = { $owner } · { $role }

# Place of a missing sample; `$pad` is the pad number from 1.
missing-place-pad = { $owner } · Pad { $pad }

# Place of a missing sample in the general-purpose Sampler.
missing-place-sampler = { $owner } · Sampler

# Title with plugins only (S06).
missing-title-plugins = { $n ->
        [one] 1 Plugin Missing
       *[other] { $n } Plugins Missing
    }

# Title with samples only.
missing-title-samples = { $n ->
        [one] 1 Sample Missing
       *[other] { $n } Samples Missing
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
missing-title-both = { $plugins } and { $samples } Missing

# S06 text under the title; `$project` is the project name.
missing-text = { $n ->
        [one] “{ $project }” is open. This device is not installed or is not in a plugin folder. Its settings stay saved in the project and load again as soon as the plugin is found.
       *[other] “{ $project }” is open. These devices are not installed or are not in a plugin folder. Their settings stay saved in the project and load again as soon as the plugins are found.
    }

# Text under the title with samples only; mentions the button `missing-search` as "Search".
missing-samples-text = { $n ->
        [one] “{ $project }” is open. This sample is no longer in its place. Use “Search” to link it again; the pad stays saved in the project.
       *[other] “{ $project }” is open. These samples are no longer in their place. Use “Search” to link them again; the pads stay saved in the project.
    }

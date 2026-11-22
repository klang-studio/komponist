## Browser (`Screens.dc.html` `br()`, `ks-view.js` `browserView`; `views/browser`).

# Placeholder and accessible name of the search field (`br()`: `search:'Suchen'`).
browser-search = Suchen

# Accessible name of the segmented control.
browser-segments = Browser-Filter

# Segment 1 (`segs`).
browser-segment-all = Alle

# Segment 2.
browser-segment-instruments = Instr.

# Segment 3.
browser-segment-effects = Effekte

# Segment 4.
browser-segment-files = Dateien

# Accessible name of the row list.
browser-list = Browser-Liste

# Format and origin tag for a shipped CLAP in the browser and device pickers.
browser-bundled-clap = CLAP intern

# Section of the favourites.
browser-favorites = Favoriten

# Section of the instruments.
browser-instruments = Instrumente

# Section of the effects.
browser-effects = Effekte

# Section of the MIDI effects (WP-88): they sit in front of the instrument.
browser-midi-effects = MIDI-Effekte

# Section of hexa's factory presets.
browser-presets = Presets · Hexa

# Section of the project's files (segment Alle).
browser-project = Projekt

# Folder and section of the recorded takes.
browser-takes = Takes

# Folder and section of the exports.
browser-exports = Exporte

# No takes yet (`browserView`).
browser-no-takes = Noch keine Aufnahmen

# No exports yet (`browserView`).
browser-no-exports = Noch keine Exporte

# Footer without search (`br()` `foot`, S01).
browser-foot = Auf eine Spur ziehen lädt das Instrument. Auf leere Fläche ziehen legt eine neue Spur an.

# Footer while searching (S05 `foot`).
browser-foot-search = Enter fügt den markierten Effekt an die ausgewählte Spur an. Stern: Favorit.

# Effect category label.
browser-category-eq-filter = EQ und Filter

# Effect category label.
browser-category-dynamics = Dynamik

# Effect category label.
browser-category-reverb-delay = Hall und Delay

# Effect category label.
browser-category-modulation = Modulation

# The header of the search results: the number of hits for the query.
browser-hits =
    { $count ->
        [one] 1 Treffer für „{ $query }“
       *[other] { $count } Treffer für „{ $query }“
    }

# Header of the hexa presets that match the query.
browser-presets-with = Presets mit „{ $query }“

# Header of the other effects of a category while searching: "Kategorie Hall und Delay".
browser-category = Kategorie { $label }

# A preset in search results: "Glas · Hexa".
browser-preset-hit = { $name } · Hexa

# Accessible name of a row's star.
browser-star = Favorit: { $name }

# Linked audio import and browsing (epic #8).
browser-foot-files = Audio-Datei auf eine Audio-Spur ziehen legt einen Clip an. Auf leere Fläche ziehen legt eine neue Audio-Spur an.
browser-file-meta = { $duration } · { $channels }
browser-audio-files = Audio-Dateien
browser-audio-mono = mono
browser-audio-stereo = Stereo
browser-audio-channels = { $n } Kanäle
browser-parent-folder = Übergeordneter Ordner
browser-files-loading = Dateien werden gelesen …
browser-files-unreadable = Ordner lässt sich nicht lesen: { $why }
browser-no-audio-files = Keine Audio-Dateien
browser-file-unreadable = lässt sich nicht öffnen

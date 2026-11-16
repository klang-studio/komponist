## Browser (`Screens.dc.html` `br()`, `ks-view.js` `browserView`; `views/browser`).

# Placeholder and accessible name of the search field (`br()`: `search:'Suchen'`).
browser-search = Search

# Accessible name of the segmented control.
browser-segments = Browser filter

# Segment 1 (`segs`).
browser-segment-all = All

# Segment 2.
browser-segment-instruments = Instr.

# Segment 3.
browser-segment-effects = Effects

# Segment 4.
browser-segment-files = Files

# Accessible name of the row list.
browser-list = Browser list

# Section of the favourites.
browser-favorites = Favorites

# Section of the instruments.
browser-instruments = Instruments

# Section of the effects.
browser-effects = Effects

# Section of the MIDI effects (WP-88): they sit in front of the instrument.
browser-midi-effects = MIDI Effects

# Section of hexa's factory presets.
browser-presets = Presets · Hexa

# Section of the project's files (segment Alle).
browser-project = Project

# Folder and section of the recorded takes.
browser-takes = Takes

# Folder and section of the exports.
browser-exports = Exports

# No takes yet (`browserView`).
browser-no-takes = No recordings yet

# No exports yet (`browserView`).
browser-no-exports = No exports yet

# Footer without search (`br()` `foot`, S01).
browser-foot = Drag onto a track to load the instrument. Drag onto empty space to create a new track.

# Footer while searching (S05 `foot`).
browser-foot-search = Enter adds the marked effect to the selected track. Star: favorite.

# Effect category label.
browser-category-eq-filter = EQ and Filter

# Effect category label.
browser-category-dynamics = Dynamics

# Effect category label.
browser-category-reverb-delay = Reverb and Delay

# Effect category label.
browser-category-modulation = Modulation

# The header of the search results: the number of hits for the query.
browser-hits =
    { $count ->
        [one] 1 result for “{ $query }”
       *[other] { $count } results for “{ $query }”
    }

# Header of the hexa presets that match the query.
browser-presets-with = Presets matching “{ $query }”

# Header of the other effects of a category while searching: "Kategorie Hall und Delay".
browser-category = Category { $label }

# A preset in search results: "Glas · Hexa".
browser-preset-hit = { $name } · Hexa

# Accessible name of a row's star.
browser-star = Favorite: { $name }

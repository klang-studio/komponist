macros-title = Track Macros
macros-slot = Track macro { $n }
macros-slot-named = Track macro { $n } · { $name }
macros-ref = track macro { $n } “{ $name }”
macros-entry = { $n } · { $name }
macros-unused = unused
macros-map = Map
macros-map-tip = Map: choose a track macro, then move parameters
macros-map-chip = Map · { $source }
macros-map-hint = Move parameters to map them
macros-map-hint-esc = Esc ends
macros-show-in-devices = show in devices
macros-targets = { $n ->
        [one] 1 target
       *[other] { $n } targets
    }
macros-mappings = Mappings · { $n }
macros-name = Name
macros-value = Value
macros-no-mappings = No mappings. Turn on “Map” and move parameters.
macros-bend-tip = Curve: drag to bend · Alt+click: linear
macros-bend-tip-mac = Curve: drag to bend · ⌥+click: linear
macros-bend-linear = linear
macros-a11y-bend = Curve, { $param }
macros-invert = Invert
macros-remove = Remove mapping
macros-remove-menu = Remove Mapping
macros-remove-all = Remove All Mappings
macros-rename = Rename
macros-show = Show Track Macro
macros-map-to = Map to Track Macro
macros-controlled-tip = Controlled by { $source }
macros-plugin-missing-hint = The mapping stays saved and works again as soon as the plugin loads.
macros-mapping-kept = The mapping stays saved.
macros-targets-missing = { $n ->
        [one] 1 target missing
       *[other] { $n } targets missing
    }
macros-kept = { $n ->
        [one] 1 mapping kept
       *[other] { $n } mappings kept
    }
macros-expand-to-map = Expand to map parameters
macros-lane-overridden = via { $source }
macros-a11y = Track macro { $n }, { $name }
macros-a11y-mappings = { $n ->
        [0] No mappings
        [one] 1 mapping
       *[other] { $n } mappings
    }
macros-a11y-candidate = Mappable: { $source }
macros-a11y-mapping = Mapping { $param } ({ $device }), { $range }
macros-a11y-min = Minimum, { $param }
macros-a11y-max = Maximum, { $param }

//! Cross-device tests for localized catalog metadata, parameters, layouts and presets.

#[cfg(test)]
mod tests {
    use komponist_device_core::{category_text, value_texts};
    use komponist_hexa::{
        Macro, categories,
        device::{self, Field, OpField},
    };
    use komponist_i18n::Lang;
    use komponist_model::PluginFormat;
    use komponist_model::params::layout::{CellParam, DeviceLayout};
    use komponist_model::params::va_curve::{FILTER_TYPES, LFO_DIVS, LFO_SHAPES, WAVES, dst, src};
    use komponist_model::params::{ParamInfo, ParamKind, ValueTexts};

    use crate::{Internal, device_name};
    use {
        komponist_arpeggiator as arpeggiator, komponist_eq as eq, komponist_scale as tonleiter,
        komponist_volt as volt,
    };

    fn find<'a>(list: &'a [ParamInfo], id: &str) -> &'a ParamInfo {
        list.iter()
            .find(|p| p.id.as_str() == id)
            .unwrap_or_else(|| panic!("no parameter {id}"))
    }

    fn choices(info: &ParamInfo) -> Vec<&str> {
        match &info.kind {
            ParamKind::Enum(names) => names.iter().map(String::as_str).collect(),
            other => panic!("{} is {other:?}", info.id),
        }
    }

    fn names(list: &[ParamInfo]) -> Vec<&str> {
        list.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn language_changes_texts_never_ids_kinds_ranges_or_defaults() {
        for kind in Internal::ALL {
            let (de, en) = (kind.params_in(Lang::De), kind.params_in(Lang::En));
            assert_eq!(de.len(), en.len(), "{kind:?}");
            for (a, b) in de.iter().zip(en) {
                assert_eq!(a.id, b.id, "{kind:?}");
                assert_eq!(a.default, b.default, "{kind:?} {}", a.id);
                assert_eq!(a.unit, b.unit, "{kind:?} {}", a.id);
                assert_eq!(a.range(), b.range(), "{kind:?} {}", a.id);
                assert_eq!(a.steps(), b.steps(), "{kind:?} {}", a.id);
                assert!(
                    !a.name.is_empty() && !b.name.is_empty(),
                    "{kind:?} {}",
                    a.id
                );
            }
            let (de, en) = (kind.layout_in(Lang::De), kind.layout_in(Lang::En));
            assert_eq!(cells(de), cells(en), "{kind:?}");
        }
        for (a, b) in device::params().iter().zip(device::params_in(Lang::En)) {
            assert_eq!((a.wire, a.field, &a.info.id), (b.wire, b.field, &b.info.id));
            assert_eq!(a.info.range(), b.info.range());
        }
    }

    /// The cells of a layout without its texts: where they are and what they edit.
    fn cells(layout: &DeviceLayout) -> Vec<(u8, u8, u8, u8, String)> {
        layout
            .pages
            .iter()
            .flat_map(|p| &p.cells)
            .map(|c| {
                let param = match &c.param {
                    CellParam::Choice { key, options, .. } => {
                        format!(
                            "choice {key} {:?}",
                            options.iter().map(|o| o.0).collect::<Vec<_>>()
                        )
                    }
                    CellParam::Graph(g) => format!("graph {:?} {:?}", g.config, g.bind),
                    other => format!("{other:?}"),
                };
                (c.col, c.row, c.cols, c.rows, param)
            })
            .collect()
    }

    #[test]
    fn the_german_lists_are_the_constants_the_code_always_had() {
        let wave = |l| volt::params::params(l);
        let de = wave(Lang::De);
        assert_eq!(choices(find(&de, "osc_a_wave")), WAVES);
        assert_eq!(choices(find(&de, "lfo1_shape")), LFO_SHAPES);
        assert_eq!(choices(find(&de, "lfo2_div")), LFO_DIVS);
        assert_eq!(choices(find(&de, "mod1_src")), src::NAMES);
        assert_eq!(choices(find(&de, "mod4_dst")), dst::NAMES);
        assert_eq!(choices(find(&de, "filter_type")), FILTER_TYPES);
        assert_eq!(choices(find(&de, "mode")), volt::params::MODES);
        let tone = Internal::Tonleiter.params_in(Lang::De);
        let scales: Vec<&str> = tonleiter::SCALES.iter().map(|s| s.0).collect();
        assert_eq!(choices(find(tone, "scale")), scales);
        assert_eq!(choices(find(tone, "mode")), tonleiter::MODES);
        let arp = Internal::Arpeggiator.params_in(Lang::De);
        assert_eq!(choices(find(arp, "mode")), arpeggiator::MODES);
        let eq = Internal::Eq.params_in(Lang::De);
        assert_eq!(choices(find(eq, "b1_type")), eq::TYPES);
        // The same for the macros and the card.
        for m in Macro::ALL {
            assert_eq!(m.name_in(Lang::De), m.name());
        }
        assert_eq!(Macro::Algorithm.text_in(Lang::De, 0.0), "Frei");
        assert_eq!(Macro::Index.text_in(Lang::De, 0.35), "35 %");
    }

    #[test]
    fn device_names_in_english() {
        let name = |k: Internal| (k.name(), k.name_in(Lang::De), k.name_in(Lang::En));
        assert_eq!(name(Internal::Hall), ("Hall", "Hall", "Reverb"));
        assert_eq!(
            name(Internal::Tonleiter),
            ("Tonleiter", "Tonleiter", "Scale")
        );
        // Brands and loan words stay.
        for kind in [
            Internal::Volt,
            Internal::Presse,
            Internal::Sampler,
            Internal::Eq,
            Internal::Delay,
            Internal::Limiter,
            Internal::Arpeggiator,
        ] {
            assert_eq!(kind.name_in(Lang::De), kind.name());
            assert_eq!(kind.name_in(Lang::En), kind.name());
        }
        // The stored reference keeps the German name; the shown name is looked up.
        let hall = Internal::Hall.plugin_ref();
        assert_eq!(hall.name, "Hall");
        assert_eq!(device_name(Lang::En, &hall), Some("Reverb"));
        assert_eq!(device_name(Lang::De, &hall), Some("Hall"));
        let hexa = komponist_hexa::CATALOG.plugin_ref();
        assert_eq!(device_name(Lang::En, &hexa), Some("Hexa"));
        let mut clap = hall;
        clap.format = PluginFormat::Clap;
        assert_eq!(device_name(Lang::En, &clap), None);
    }

    #[test]
    fn parameter_labels_in_english() {
        let eq = Internal::Eq.params_in(Lang::En);
        assert_eq!(find(eq, "out").name, "Output");
        assert_eq!(find(eq, "b1_on").name, "Band 1 On");
        assert_eq!(find(eq, "b2_freq").name, "Freq 2");
        assert_eq!(find(eq, "analyse").name, "Analysis");
        assert_eq!(
            choices(find(eq, "b3_type")),
            ["LC", "LS", "Bell", "HS", "HC"]
        );
        let delay = Internal::Delay.params_in(Lang::En);
        assert_eq!(find(delay, "note").name, "Note Value");
        assert_eq!(find(delay, "width").name, "Width");
        let hall = Internal::Hall.params_in(Lang::En);
        assert_eq!(find(hall, "damping").name, "Damping");
        assert_eq!(find(hall, "wet_only").name, "Wet Only");
        assert_eq!(choices(find(hall, "room")), ["Room", "Hall", "Church"]);
        let limiter = Internal::Limiter.params_in(Lang::En);
        assert_eq!(names(limiter), ["Input", "Ceiling", "Release", "Lookahead"]);
        let presse = Internal::Presse.params_in(Lang::En);
        assert_eq!(find(presse, "knee").name, "Knee");
        assert_eq!(find(presse, "sc_listen").name, "SC Listen");
        let arp = Internal::Arpeggiator.params_in(Lang::En);
        assert_eq!(names(arp), ["Pattern", "Rate", "Gate", "Octaves", "Hold"]);
        assert_eq!(
            choices(find(arp, "mode")),
            ["Up", "Down", "Up/Down", "Random", "As Played"]
        );
        let tone = Internal::Tonleiter.params_in(Lang::En);
        assert_eq!(names(tone), ["Root", "Scale", "Mode"]);
        let scales = choices(find(tone, "scale"));
        assert_eq!(scales[0], "Major");
        assert_eq!(scales[2], "Harmonic Minor");
        assert_eq!(scales[13], "Chromatic");
        assert_eq!(
            choices(find(tone, "mode")),
            ["Nearest", "Up", "Down", "Filter"]
        );
        // Note names are the same in every language.
        assert_eq!(
            choices(find(tone, "root")),
            [
                "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"
            ]
        );
    }

    #[test]
    fn sampler_labels_and_texts_in_english() {
        let sampler = Internal::Sampler.params_in(Lang::En);
        assert_eq!(find(sampler, "kit_tune").name, "Kit Tune");
        assert_eq!(find(sampler, "follow").name, "Pad follows input");
        assert_eq!(find(sampler, "pad3_level").name, "Level 3");
        assert_eq!(find(sampler, "pad1_end").name, "End 1");
        assert_eq!(choices(find(sampler, "pad2_mode")), ["1×", "Gate"]);
        let end = find(sampler, "pad1_end");
        assert_eq!(end.format(komponist_sampler::MAX_MS), "End");
        assert_eq!(end.format(375.0), "375 ms");
        assert_eq!(
            find(sampler, "pad1_decay").format(komponist_sampler::MAX_DECAY_S),
            "Off"
        );
        let de = Internal::Sampler.params_in(Lang::De);
        assert_eq!(find(de, "follow").name, "Pad folgt Eingang");
        assert_eq!(
            find(de, "pad1_end").format(komponist_sampler::MAX_MS),
            "Ende"
        );
        assert_eq!(
            find(de, "pad1_decay").format(komponist_sampler::MAX_DECAY_S),
            "Aus"
        );
    }

    #[test]
    fn volt_labels_pages_and_texts_in_english() {
        let volt = Internal::Volt.params_in(Lang::En);
        assert_eq!(find(volt, "osc_a_wave").name, "Osc A");
        assert_eq!(find(volt, "noise_level").name, "Noise");
        assert_eq!(find(volt, "mod2_src").name, "Source 2");
        assert_eq!(find(volt, "lfo1_div").name, "Divider");
        assert_eq!(find(volt, "vel_filter").name, "Vel → Env");
        assert_eq!(
            choices(find(volt, "osc_b_wave")),
            ["Sine", "Triangle", "Saw", "Square"]
        );
        assert_eq!(
            choices(find(volt, "lfo2_div"))[..3],
            ["4 Bars", "2 Bars", "1 Bar"]
        );
        assert_eq!(choices(find(volt, "mod1_src"))[3], "Env 3");
        assert_eq!(choices(find(volt, "mod1_dst"))[2], "Osc B Pitch");
        // Units: octaves are "oct" in English, "Okt." in German; "off" is lower case in both.
        assert_eq!(find(volt, "octave").format(-1.0), "−1 oct");
        assert_eq!(find(volt, "glide").format(0.0), "off");
        assert_eq!(find(volt, "glide").format(0.5), "500 ms");
        let de = Internal::Volt.params_in(Lang::De);
        assert_eq!(find(de, "octave").format(-1.0), "−1 Okt.");
        assert_eq!(find(de, "glide").format(0.0), "aus");
        let layout = Internal::Volt.layout_in(Lang::En);
        let page_names: Vec<&str> = layout.pages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(page_names, ["Sound", "Envelopes", "Matrix"]);
        let choice_labels: Vec<&str> = layout.pages[1]
            .cells
            .iter()
            .filter_map(|c| match &c.param {
                CellParam::Choice { label, .. } => Some(*label),
                _ => None,
            })
            .collect();
        assert_eq!(choice_labels, ["Envelope", "LFO"]);
        let title = layout.pages[1]
            .cells
            .iter()
            .find_map(|c| match &c.param {
                CellParam::Graph(g) if !g.title.is_empty() => Some(g.title.as_str()),
                _ => None,
            })
            .unwrap();
        assert_eq!(title, "{env_view_label} Env.");
        let de_layout = Internal::Volt.layout_in(Lang::De);
        assert_eq!(de_layout.pages[1].name, "Hüllkurven");
    }

    #[test]
    fn hexa_card_in_english() {
        let card = device::params_in(Lang::En);
        let name = |wire: u32| {
            card.iter()
                .find(|p| p.wire == wire)
                .map(|p| p.info.name.as_str())
                .unwrap()
        };
        assert_eq!(name(Macro::Algorithm.id()), "Algorithm");
        assert_eq!(name(Field::Volume.wire()), "Volume");
        assert_eq!(name(Field::Op(1, OpField::Level).wire()), "Op 2");
        assert_eq!(name(Field::Op(0, OpField::AttackCurve).wire()), "Curve A 1");
        assert_eq!(name(Field::Op(2, OpField::Output).wire()), "Carrier 3");
        let de = device::params();
        let de_name = |wire: u32| {
            de.iter()
                .find(|p| p.wire == wire)
                .unwrap()
                .info
                .name
                .as_str()
        };
        assert_eq!(
            de_name(Field::Op(0, OpField::AttackCurve).wire()),
            "Kurve A 1"
        );
        assert_eq!(de_name(Field::Op(2, OpField::Output).wire()), "Träger 3");
        assert_eq!(de_name(Field::Volume.wire()), "Lautstärke");
        // Value texts follow the language.
        let delay = Field::Op(0, OpField::Delay);
        assert_eq!(delay.info_in(Lang::De).format(0.0), "Aus");
        assert_eq!(delay.info_in(Lang::En).format(0.0), "Off");
        let curve = Field::Op(0, OpField::DecayCurve);
        assert_eq!(curve.info_in(Lang::De).format(0.5), "+50 %");
        assert_eq!(curve.info_in(Lang::En).format(0.5), "+50%");
        assert_eq!(Macro::Algorithm.param_info_in(Lang::En).format(0.0), "Free");
        assert_eq!(Macro::Sustain.param_info_in(Lang::En).format(0.35), "35%");
        assert_eq!(Macro::Sustain.param_info_in(Lang::De).format(0.35), "35 %");
        let layout = device::layout_in(Lang::En);
        let pages: Vec<&str> = layout.pages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(pages, ["Operators", "Envelopes", "LFO", "Global"]);
        let title = |lang| {
            device::layout_in(lang).pages[1]
                .cells
                .iter()
                .find_map(|c| match &c.param {
                    CellParam::Graph(g) => Some(g.title.clone()),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(title(Lang::De), "Op {env_op} · Amplitude");
        assert_eq!(title(Lang::En), "Op {env_op} · Amplitude");
        assert_eq!(device::layout().pages[0].name, "Operatoren");
    }

    #[test]
    fn value_texts_follow_the_language() {
        // Switch states and the percent suffix come from the catalog; units do not change.
        let texts = value_texts(Lang::En);
        assert_eq!((texts.on, texts.off, texts.percent), ("On", "Off", "%"));
        assert_eq!(value_texts(Lang::De), ValueTexts::DE);
        let en = Internal::Delay.params_in(Lang::En);
        let de = Internal::Delay.params_in(Lang::De);
        assert_eq!(find(en, "sync").format(1.0), "On");
        assert_eq!(find(de, "sync").format(1.0), "Ein");
        assert_eq!(find(en, "mix").format(0.4), "40%");
        assert_eq!(find(de, "mix").format(0.4), "40 %");
        for lang in [Lang::De, Lang::En] {
            let delay = Internal::Delay.params_in(lang);
            assert_eq!(find(delay, "time").format(420.0), "420 ms");
            assert_eq!(find(delay, "low_cut").format(1200.0), "1.2 kHz");
            let eq = Internal::Eq.params_in(lang);
            assert_eq!(find(eq, "b1_gain").format(-4.0), "−4.0 dB");
        }
    }

    #[test]
    fn categories_are_translated_and_unknown_ones_pass_through() {
        assert_eq!(category_text(Lang::De, "Tasten"), "Tasten");
        assert_eq!(category_text(Lang::En, "Tasten"), "Keys");
        assert_eq!(category_text(Lang::En, "Blech & Holz"), "Brass & Woodwinds");
        assert_eq!(
            category_text(Lang::En, "Streicher & Pads"),
            "Strings & Pads"
        );
        assert_eq!(category_text(Lang::En, "Bass"), "Bass");
        assert_eq!(category_text(Lang::En, "Something"), "Something");
        // Every category of hexa's bank and of the internal devices has a text in every language.
        let internal = Internal::ALL
            .into_iter()
            .flat_map(|k| k.presets().iter().filter_map(|p| p.category));
        for category in categories().chain(internal) {
            for lang in Lang::ALL {
                let text = category_text(lang, category);
                assert!(
                    lang != Lang::De || text == category,
                    "German text of {category} differs: {text}"
                );
                assert!(!text.is_empty());
            }
        }
        // English texts never keep a German word of the source.
        for category in categories() {
            let en = category_text(Lang::En, category);
            assert!(
                ![
                    "Tasten",
                    "Gezupft",
                    "Blech",
                    "Streicher",
                    "Effekte",
                    "Verzerrt",
                    "Rhythmisch"
                ]
                .iter()
                .any(|w| en.contains(w)),
                "{category} → {en}"
            );
        }
    }
}

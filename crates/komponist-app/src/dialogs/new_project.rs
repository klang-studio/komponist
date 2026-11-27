//! New projects (S10) and the question before the open project is replaced.
//!
//! - Ctrl N / "Neues Projekt": an empty project "Unbenannt" at 120 BPM, 4/4 (S10; the arrangement
//!   shows the "Leeres Projekt" hint).
//! - "Aus Vorlage · Beat-Skizze" (S09): three tracks to start a beat: "Drums" with the packaged
//!   Sampler and a one-bar step pattern on its pads 1–8 ([`TEMPLATE_FIRST_NOTE`]), "Bass" and
//!   "Keys" with the packaged Hexa.
//! - A project with unsaved changes asks first: "Speichern" (Enter), "Nicht speichern",
//!   "Abbrechen" (Esc). The screens have no such dialog; its texts are komponist's own.

use egui::{Context, vec2};
use komponist_i18n::Lang;
use komponist_model::edits::{self, NewClip, NewClipContent, NewTrack, StepProp};
use komponist_model::{ClipId, Hue, PPQ, PluginRef, Project, Tick, TrackId};
use komponist_plugins::Catalog;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, typography};
use komponist_ui::widgets::button::Kind;
use komponist_ui::widgets::{self as w};

use super::metrics::dialog as m;
use super::{ActionButton, button_row, galley, header, modal};
use crate::step_rows;

/// The answer to the unsaved-changes prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Save first, then continue.
    Save,
    /// Continue without saving.
    Discard,
    /// Keep the open project.
    Cancel,
}

/// The empty project of Ctrl N (S10): "Unbenannt" ("Untitled"), 120 BPM, 4/4, no tracks. The name
/// is made in `t` and is content afterwards (`docs/I18N.md` §5).
pub fn empty_project(t: Lang) -> Project {
    Project::new(t.new_project_default_name())
}

/// The text of the prompt for project `name`.
pub fn unsaved_text(t: Lang, name: &str) -> String {
    t.new_project_unsaved_text(name)
}

/// Draws the unsaved-changes prompt for project `name`; returns the answer in the frame it is
/// given.
pub fn show_unsaved(ctx: &Context, name: &str) -> Option<Answer> {
    let t = komponist_ui::lang(ctx);
    let mut out = None;
    let response = modal(ctx, "unsaved", color32(color::SURFACE_2), |ui| {
        let width = m::WIDTH;
        ui.set_width(width);
        if header(ui, width, t.new_project_unsaved_title()) {
            out = Some(Answer::Cancel);
        }
        let body = egui::Frame::new()
            .fill(color32(color::SURFACE_1))
            .inner_margin(egui::Margin::same(m::PADDING as i8));
        body.show(ui, |ui| {
            ui.set_width(width - 2.0 * m::PADDING);
            let text = unsaved_text(t, name);
            let g = galley(
                ui,
                &text,
                w::font_at(&typography::TRACK, m::TEXT_WEIGHT),
                m::TEXT_LINE_HEIGHT,
                color32(color::TEXT_2),
                Some(width - 2.0 * m::PADDING),
            );
            super::paint_text(ui, g, &text);
        });
        let footer = egui::Frame::new().inner_margin(egui::Margin::symmetric(
            m::PADDING as i8,
            m::FOOTER_PADDING_Y as i8,
        ));
        footer.show(ui, |ui| {
            let r = button_row(
                ui,
                width - 2.0 * m::PADDING,
                vec![
                    ActionButton::dialog(t.new_project_discard(), Kind::Secondary, 2),
                    ActionButton::dialog(t.dialog_cancel(), Kind::Secondary, 2),
                    ActionButton::dialog(t.new_project_save(), Kind::Primary, 2),
                ],
            );
            for (response, answer) in r
                .iter()
                .zip([Answer::Discard, Answer::Cancel, Answer::Save])
            {
                if response.clicked() {
                    out = Some(answer);
                }
            }
        });
        ui.allocate_space(vec2(width, 0.0));
    });
    let enter = ctx.memory(|mem| mem.focused().is_none())
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if out.is_none() {
        if enter {
            out = Some(Answer::Save);
        } else if response.should_close() {
            out = Some(Answer::Cancel);
        }
    }
    out
}

/// Track hues of the template (S09's "Beat-Skizze 14" row: 45, 250, 300).
const TEMPLATE_HUES: [u16; 3] = [45, 250, 300];

/// Step rows of the template's beat and the steps that are on (of 16): kick on the quarters,
/// snare and clap on 2 and 4, closed hat on the eighths.
const TEMPLATE_BEAT: [(usize, &[usize]); 4] = [
    (0, &[0, 4, 8, 12]),
    (1, &[4, 12]),
    (2, &[12]),
    (3, &[0, 2, 4, 6, 8, 10, 12, 14]),
];

/// The CLAP IDs of the template's drum and synth instruments in the packaged catalog.
const TEMPLATE_DRUMS: &str = "org.klang-studio.komponist.sampler";
const TEMPLATE_SYNTH: &str = "org.klang-studio.komponist.hexa";

/// The note of the template beat's first row: the drum bundle's pad 1 (C1). The beat is authored
/// for pads 1–8, which follow on consecutive notes. The project is built before the device runs,
/// so its pad bank is not available yet.
pub const TEMPLATE_FIRST_NOTE: u8 = 36;

/// The instruments of the template for `catalog`: the drum sampler and Hexa from the packaged
/// bundles. `None` when the package lacks one of them.
pub fn template_instruments(catalog: &Catalog) -> Option<(PluginRef, PluginRef)> {
    let packaged = |id: &str| catalog.packaged(id).map(|entry| entry.plugin.clone());
    Some((packaged(TEMPLATE_DRUMS)?, packaged(TEMPLATE_SYNTH)?))
}

/// The template project "Beat-Skizze" (see the module docs): the drums play `drums` (the
/// Sampler), bass and keys `hexa`.
pub fn template(t: Lang, drums: PluginRef, hexa: PluginRef) -> Project {
    let mut p = Project::new(t.new_project_template_name());
    let specs = [("Drums", drums), ("Bass", hexa.clone()), ("Keys", hexa)];
    let mut tracks: Vec<TrackId> = Vec::new();
    for ((name, instrument), hue) in specs.into_iter().zip(TEMPLATE_HUES) {
        let spec = NewTrack {
            name: Some(name.to_owned()),
            hue: Some(Hue(hue)),
            instrument: Some(instrument),
            index: None,
            arm_exclusive: false,
        };
        if let Ok((next, id)) = edits::add_track(&p, spec) {
            p = next;
            tracks.push(id);
        }
    }
    let Some(&drums) = tracks.first() else {
        return p;
    };
    // The drum instrument's pads 1–8, with the default row names.
    let rows = step_rows::rows_for_pads(t, TEMPLATE_FIRST_NOTE.., &[]);
    let bar = Tick(PPQ * i64::from(p.meter.num));
    let clip = NewClip {
        start: Tick::ZERO,
        length: bar,
        name: "Beat".to_owned(),
        content: NewClipContent::Step { rows },
    };
    let Ok((next, beat)) = edits::add_clip(&p, drums, clip) else {
        return p;
    };
    p = next;
    set_beat(&mut p, beat);
    p
}

fn set_beat(p: &mut Project, clip: ClipId) {
    for (row, steps) in TEMPLATE_BEAT {
        for &step in steps {
            if let Ok(next) = edits::set_step(p, clip, row, step, StepProp::On(true)) {
                *p = next;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::{ClipContent, persist};

    #[test]
    fn the_empty_project_is_s10() {
        let p = empty_project(Lang::De);
        assert_eq!(p.name, "Unbenannt");
        assert_eq!(empty_project(Lang::En).name, "Untitled");
        assert_eq!(p.tempo.bpm, 120.0);
        assert!(p.tracks.is_empty());
    }

    #[test]
    fn the_template_needs_the_packaged_instruments() {
        assert_eq!(template_instruments(&Catalog::default()), None);
        assert_eq!(template_instruments(&Catalog::default()), None);
    }

    #[test]
    fn the_template_has_a_sampler_beat_and_two_hexa_tracks() {
        let host = crate::app::App::headless_plugin_host();
        let (drums, hexa) =
            template_instruments(crate::device_host::DeviceHost::catalog(&host)).unwrap();
        let p = template(Lang::De, drums.clone(), hexa.clone());
        assert_eq!(p.name, "Beat-Skizze");
        assert_eq!(template(Lang::En, drums, hexa).name, "Beat Sketch");
        let names: Vec<&str> = p.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Drums", "Bass", "Keys"]);
        let hues: Vec<u16> = p.tracks.iter().map(|t| t.hue.0).collect();
        assert_eq!(hues, TEMPLATE_HUES);
        let instruments: Vec<&str> = p
            .tracks
            .iter()
            .map(|t| t.channel.devices[0].plugin.id.as_str())
            .collect();
        assert_eq!(
            instruments,
            [TEMPLATE_DRUMS, TEMPLATE_SYNTH, TEMPLATE_SYNTH]
        );
        let ClipContent::Step(steps) = &p.tracks[0].clips[0].content else {
            panic!("the beat is a step clip");
        };
        let on = |row: usize| -> Vec<usize> {
            steps.rows[row]
                .steps
                .iter()
                .enumerate()
                .filter(|(_, s)| s.on)
                .map(|(i, _)| i)
                .collect()
        };
        assert_eq!(on(0), [0, 4, 8, 12]);
        assert_eq!(on(1), [4, 12]);
        // The sampler's pads 1–8 are the rows, named like the demo's.
        let rows: Vec<(&str, u8)> = steps
            .rows
            .iter()
            .map(|r| (r.name.as_str(), r.note))
            .collect();
        assert_eq!(
            rows[..4],
            [("Kick", 36), ("Snare", 37), ("Clap", 38), ("Hat zu", 39)]
        );
        assert_eq!(rows[7], ("Perc", 43));
        assert!(steps.rows[0].steps[0].on && steps.rows[3].steps[2].on);
        // It saves and loads.
        let json = persist::to_json(&p).unwrap();
        assert_eq!(persist::from_json(&json).unwrap(), p);
    }
}

//! New projects (S10) and the question before the open project is replaced.
//!
//! - Ctrl N / "Neues Projekt": an empty project "Unbenannt" at 120 BPM, 4/4 (S10; the arrangement
//!   shows the "Leeres Projekt" hint).
//! - "Aus Vorlage · Beat-Skizze" (S09): three tracks to start a beat: "Drums" with klopfer (pisynth
//!   if klopfer is not installed) and a one-bar step pattern whose rows the drum instrument decides
//!   ([`crate::step_rows::rows_for`]: klopfer's pads 1–8, else the General MIDI rows), "Bass" and
//!   "Keys" with pisynth.
//! - A project with unsaved changes asks first: "Speichern" (Enter), "Nicht speichern",
//!   "Abbrechen" (Esc). The screens have no such dialog; its texts are komponist's own.

use egui::{Context, vec2};
use komponist_model::edits::{self, NewClip, NewClipContent, NewTrack, StepProp};
use komponist_model::project::{BUILTIN_KLOPFER, DEFAULT_PROJECT_NAME};
use komponist_model::{ClipId, Hue, PPQ, PluginFormat, PluginRef, Project, Tick, TrackId};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, typography};
use komponist_ui::widgets::button::Kind;
use komponist_ui::widgets::{self as w};

use super::metrics::dialog as m;
use super::{ActionButton, button_row, galley, header, modal};
use crate::step_rows;

/// Texts of the prompt and the template.
pub mod names {
    /// Title of the unsaved-changes prompt.
    pub const UNSAVED_TITLE: &str = "Änderungen speichern?";
    /// "Speichern" (primary, Enter).
    pub const SAVE: &str = "Speichern";
    /// "Nicht speichern".
    pub const DISCARD: &str = "Nicht speichern";
    /// "Abbrechen" (Esc).
    pub const CANCEL: &str = "Abbrechen";
    /// The template's name (S09 "Aus Vorlage · Beat-Skizze").
    pub const TEMPLATE: &str = "Beat-Skizze";
}

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

/// The empty project of Ctrl N (S10): "Unbenannt", 120 BPM, 4/4, no tracks.
pub fn empty_project() -> Project {
    Project::new(DEFAULT_PROJECT_NAME)
}

/// The text of the prompt for project `name`.
pub fn unsaved_text(name: &str) -> String {
    format!("„{name}“ hat ungespeicherte Änderungen. Ohne Speichern gehen sie verloren.")
}

/// Draws the unsaved-changes prompt for project `name`; returns the answer in the frame it is
/// given.
pub fn show_unsaved(ctx: &Context, name: &str) -> Option<Answer> {
    let mut out = None;
    let response = modal(ctx, "unsaved", color32(color::SURFACE_2), |ui| {
        let width = m::WIDTH;
        ui.set_width(width);
        if header(ui, width, names::UNSAVED_TITLE) {
            out = Some(Answer::Cancel);
        }
        let body = egui::Frame::new()
            .fill(color32(color::SURFACE_1))
            .inner_margin(egui::Margin::same(m::PADDING as i8));
        body.show(ui, |ui| {
            ui.set_width(width - 2.0 * m::PADDING);
            let text = unsaved_text(name);
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
                    ActionButton::dialog(names::DISCARD, Kind::Secondary, 2),
                    ActionButton::dialog(names::CANCEL, Kind::Secondary, 2),
                    ActionButton::dialog(names::SAVE, Kind::Primary, 2),
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

/// The template project "Beat-Skizze" (see the module docs). `klopfer` is the catalog's klopfer
/// reference if it is installed.
pub fn template(klopfer: Option<PluginRef>, pisynth: PluginRef) -> Project {
    let mut p = Project::new(names::TEMPLATE);
    let drums_instrument = klopfer.unwrap_or_else(|| pisynth.clone());
    let specs = [
        ("Drums", drums_instrument),
        ("Bass", pisynth.clone()),
        ("Keys", pisynth),
    ];
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
    let rows = p
        .track(drums)
        .and_then(|t| step_rows::instrument(t))
        .map(|d| step_rows::rows_for(Some(&d.plugin), &[]))
        .unwrap_or_default();
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

/// The catalog's klopfer reference, if klopfer is installed.
pub fn klopfer_in(catalog: &komponist_plugins::Catalog) -> Option<PluginRef> {
    catalog
        .entries()
        .iter()
        .map(|e| &e.plugin)
        .find(|p| p.format == PluginFormat::Builtin && p.id == BUILTIN_KLOPFER)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::{ClipContent, persist};

    #[test]
    fn the_empty_project_is_s10() {
        let p = empty_project();
        assert_eq!(p.name, "Unbenannt");
        assert_eq!(p.tempo.bpm, 120.0);
        assert!(p.tracks.is_empty());
    }

    #[test]
    fn the_template_has_a_beat_and_two_pisynth_tracks() {
        let p = template(None, crate::app::pisynth_ref());
        assert_eq!(p.name, "Beat-Skizze");
        let names: Vec<&str> = p.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Drums", "Bass", "Keys"]);
        let hues: Vec<u16> = p.tracks.iter().map(|t| t.hue.0).collect();
        assert_eq!(hues, TEMPLATE_HUES);
        // Without klopfer every instrument is pisynth, so nothing is missing.
        assert!(
            p.tracks
                .iter()
                .all(|t| t.channel.devices[0].plugin.id == "pisynth")
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
        // pisynth plays the General MIDI rows.
        let notes: Vec<u8> = steps.rows.iter().map(|r| r.note).collect();
        assert_eq!(notes, [36, 38, 39, 42, 46, 45, 37, 47]);
        // It saves and loads.
        let json = persist::to_json(&p).unwrap();
        assert_eq!(persist::from_json(&json).unwrap(), p);
    }

    #[test]
    fn the_template_beat_plays_klopfers_pads() {
        let klopfer = PluginRef {
            format: PluginFormat::Builtin,
            id: BUILTIN_KLOPFER.into(),
            name: "klopfer".into(),
            vendor: "Klopfer".into(),
            version: "0.1.0".into(),
            module: Box::default(),
        };
        let p = template(Some(klopfer), crate::app::pisynth_ref());
        let ClipContent::Step(steps) = &p.tracks[0].clips[0].content else {
            panic!("the beat is a step clip");
        };
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
    }
}

//! With nothing selected: the project (`sec({title:'Projekt', name, chip:C.t2, fields:[Tempo,
//! Taktart, Spuren, Länge]})`, prototype `inspView`) and "Auswahl" with the hint (S10).

use egui::{Rangef, Ui};
use komponist_model::edits::ProjectProp;
use komponist_model::{Project, Tempo};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, typography};
use komponist_ui::fonts;
use komponist_ui::widgets::number_field::NumberField;

use super::model::names;
use super::{Meta, access_label, field_grid, field_key, frame, place, read_only_field};
use crate::actions::{Action, EditAction};
use crate::views::ViewCtx;
use crate::views::mixer::send_gesture;

/// The project section: name, Tempo (drag, text entry), Taktart, Spuren, Länge in bars.
pub(super) fn section(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    x: Rangef,
    y: &mut f32,
) {
    frame(ui, x, y, names::PROJECT, Meta::None, |ui, col| {
        super::name_row(ui, col, &project.name, color::TEXT_2, false, false);
        let cells = field_grid(col, 4);
        let (key, field) = cells[0];
        field_key(ui, key, names::TEMPO);
        let mut bpm = project.tempo.bpm;
        let r = place(ui, field, "tempo", |ui| {
            ui.add(
                NumberField::new(&mut bpm, names::TEMPO)
                    .unit(names::BPM)
                    .range(Tempo::MIN_BPM..=Tempo::MAX_BPM)
                    .step(1.0)
                    .decimals(2)
                    .default_value(Tempo::DEFAULT.bpm)
                    .width(field.width()),
            )
        });
        send_gesture(ctx, &r, names::UNDO_TEMPO, || {
            Action::Edit(EditAction::SetProjectProp(ProjectProp::Tempo(Tempo {
                bpm,
            })))
        });
        let bar = project.meter.ticks_per_bar();
        let bars = (project.song_end().0 + bar - 1) / bar;
        let values = [
            (
                names::METER_FIELD,
                format!("{}/{}", project.meter.num, project.meter.den),
                "",
            ),
            (names::TRACKS, project.tracks.len().to_string(), ""),
            (names::LENGTH, bars.to_string(), names::BARS),
        ];
        for ((key, field), (k, v, unit)) in cells.into_iter().skip(1).zip(values) {
            field_key(ui, key, k);
            read_only_field(ui, field, k, &v, unit);
        }
    });
}

/// "Auswahl": nothing is selected, so the inspector shows the project.
pub(super) fn selection_section(ui: &mut Ui, x: Rangef, y: &mut f32) {
    frame(ui, x, y, names::SELECTION, Meta::None, |ui, col| {
        let style = typography::UI;
        let mut job = fonts::layout_job(names::NOTHING_SELECTED, &style, color32(color::TEXT_2));
        job.wrap.max_width = col.width();
        let g = ui.painter().layout_job(job);
        let rect = col.take(g.size().y);
        ui.painter().galley(rect.min, g, color32(color::TEXT_2));
        access_label(ui, rect, "hint", names::NOTHING_SELECTED);
    });
}

//! With nothing selected: the project (`sec({title:'Projekt', name, chip:C.t2, fields:[…]})`)
//! and "Auswahl" with the hint (S10). The screens list Tempo, Taktart, Abtastrate, Spuren (S10)
//! or Tempo, Taktart, Spuren, Länge (S06–S08, prototype `inspView`); the inspector shows all
//! five.

use egui::{Rangef, Ui};
use komponist_i18n::Lang;
use komponist_model::edits::ProjectProp;
use komponist_model::{Project, Tempo};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, typography};
use komponist_ui::fonts;
use komponist_ui::widgets::number_field::NumberField;

use super::model;
use super::{Meta, access_label, field_grid, field_key, frame, place, read_only_field};
use crate::actions::{Action, EditAction};
use crate::views::ViewCtx;
use crate::views::mixer::send_gesture;

/// The project section: name, Tempo (drag, text entry), Taktart, Abtastrate, Spuren, Länge in
/// bars.
pub(super) fn section(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    x: Rangef,
    y: &mut f32,
) {
    let t = ctx.lang;
    frame(
        ui,
        x,
        y,
        t.inspector_section_project(),
        Meta::None,
        |ui, col| {
            super::name_row(ui, col, &project.name, color::TEXT_2, false, false);
            let cells = field_grid(col, 5);
            let (key, field) = cells[0];
            field_key(ui, key, t.inspector_project_tempo());
            let mut bpm = project.tempo.bpm;
            let r = place(ui, field, "tempo", |ui| {
                ui.add(
                    NumberField::new(&mut bpm, t.inspector_project_tempo())
                        .unit(model::units::BPM)
                        .range(Tempo::MIN_BPM..=Tempo::MAX_BPM)
                        .step(1.0)
                        .decimals(2)
                        .default_value(Tempo::DEFAULT.bpm)
                        .width(field.width()),
                )
            });
            send_gesture(ctx, &r, "undo-tempo", || {
                Action::Edit(EditAction::SetProjectProp(ProjectProp::Tempo(Tempo {
                    bpm,
                })))
            });
            let bar = project.meter.ticks_per_bar();
            let bars = (project.song_end().0 + bar - 1) / bar;
            let values = [
                (
                    t.inspector_project_meter(),
                    format!("{}/{}", project.meter.num, project.meter.den),
                    "",
                ),
                (
                    t.inspector_project_sample_rate(),
                    model::khz(ctx.engine.sample_rate()),
                    model::units::KHZ,
                ),
                (
                    t.inspector_project_tracks(),
                    project.tracks.len().to_string(),
                    "",
                ),
                (
                    t.inspector_clip_length(),
                    bars.to_string(),
                    t.inspector_project_bars(),
                ),
            ];
            for ((key, field), (k, v, unit)) in cells.into_iter().skip(1).zip(values) {
                field_key(ui, key, k);
                read_only_field(ui, field, k, &v, unit);
            }
        },
    );
}

/// "Auswahl": nothing is selected, so the inspector shows the project.
pub(super) fn selection_section(ui: &mut Ui, t: Lang, x: Rangef, y: &mut f32) {
    frame(
        ui,
        x,
        y,
        t.inspector_section_selection(),
        Meta::None,
        |ui, col| {
            let style = typography::UI;
            let mut job = fonts::layout_job(
                t.inspector_nothing_selected(),
                &style,
                color32(color::TEXT_2),
            );
            job.wrap.max_width = col.width();
            let g = ui.painter().layout_job(job);
            let rect = col.take(g.size().y);
            ui.painter().galley(rect.min, g, color32(color::TEXT_2));
            access_label(ui, rect, "hint", t.inspector_nothing_selected());
        },
    );
}

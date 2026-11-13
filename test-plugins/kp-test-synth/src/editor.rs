//! The editor: a label and one slider for the gain, nothing else.

use crate::SynthParams;
use nice_plug::context::gui::GuiContext;
use nice_plug::editor::dpi::LogicalSize;
use nice_plug_egui::widgets::ParamSlider;
use nice_plug_egui::{
    EguiEditor, EguiEditorState, EguiNiceSettings, NiceEguiApp, RepaintNotifier, create_egui_editor,
};
use std::sync::Arc;

/// Fixed editor size, in logical pixels.
const SIZE: LogicalSize<f32> = LogicalSize::new(240.0, 80.0);

pub fn default_state() -> Arc<EguiEditorState> {
    EguiEditorState::from_size(SIZE, 1.0)
}

pub fn create(
    state: Arc<EguiEditorState>,
    params: Arc<SynthParams>,
) -> Option<EguiEditor<SynthEditor>> {
    create_egui_editor(
        state,
        RepaintNotifier::new(),
        EguiNiceSettings::new().with_tile("kp-test-synth"),
        SynthEditor { params, gui: None },
    )
}

pub struct SynthEditor {
    params: Arc<SynthParams>,
    /// Set while the editor window is open.
    gui: Option<GuiContext>,
}

impl NiceEguiApp for SynthEditor {
    fn build(
        &mut self,
        _egui_ctx: egui::Context,
        nice_gui_ctx: GuiContext,
        _frame: &mut nice_plug_egui::Frame,
    ) -> Result<(), nice_plug_egui::baseview::HandlerError> {
        self.gui = Some(nice_gui_ctx);
        Ok(())
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut nice_plug_egui::Frame) {
        let Some(gui) = &self.gui else { return };
        let setter = gui.param_setter();
        // The panel paints the whole window, so screenshots tell the editor from an empty
        // (black) embedding window.
        egui::CentralPanel::default().show(ui, |ui| {
            ui.label("kp-test-synth");
            ui.add(ParamSlider::for_param(&self.params.gain, &setter));
        });
    }

    fn editor_closed(&mut self) {
        self.gui = None;
    }
}

//! The "Paste N lines?" question a multiline paste raises (Tabby's warnOnMultilinePaste): a preview
//! with Paste and Cancel. The host keeps the queue and decides where a confirmed paste goes.

use crate::widgets;
use stdusk_core::colors;

/// What the user answered this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Paste,
    Cancel,
    Asking,
}

/// Show the question for `text` (normalized, lines split by CR). `keys` lets Enter paste and Esc
/// cancel; pass false while another modal owns them.
pub fn show(ctx: &egui::Context, text: &str, keys: bool) -> Verdict {
    let mut verdict = Verdict::Asking;
    let lines = text.split('\r').count();
    egui::Window::new("Paste multiple lines?")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .frame(widgets::overlay_frame())
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(format!("Paste {lines} lines?")).strong().color(colors::fg()),
            );
            ui.add_space(4.0);
            let preview: String =
                text.split('\r').take(4).collect::<Vec<_>>().join("\n").chars().take(200).collect();
            ui.label(egui::RichText::new(preview).monospace().small().color(colors::dim()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if widgets::action_button(ui, "Paste", true).clicked() {
                    verdict = Verdict::Paste;
                }
                if widgets::action_button(ui, "Cancel", false).clicked() {
                    verdict = Verdict::Cancel;
                }
            });
        });
    if keys {
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            verdict = Verdict::Paste;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            verdict = Verdict::Cancel;
        }
    }
    verdict
}

//! stdusk-egui: egui rendering for a stdusk terminal pane.
//!
//! `ui::render_grid` paints a `stdusk_core::terminal::GridSnap`, `ui::collect_input` turns a
//! frame's key events into pty bytes, and `ui::draw_tab` paints a tab chip. `fonts` registers
//! the terminal font families, `widgets` holds the settings-style controls, `keys` and `mouse`
//! encode input for the pty. The theme is the process-wide one in `colors`; the host app sets
//! it once at startup.

pub mod fonts;
pub mod keys;
pub mod mouse;
pub mod ui;
pub mod widgets;

pub use stdusk_core::colors;
pub use stdusk_core::repaint::Repaint;

pub fn repaint_handle(ctx: &egui::Context) -> Repaint {
    let ctx = ctx.clone();
    Repaint::new(move |after| ctx.request_repaint_after(after))
}

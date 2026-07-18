//! Settings window (M11), toggled by the tab-bar gear. Controls edit `self.cfg` directly so
//! everything live-applies on the next frame (theme changes ride the per-frame reconciliation
//! in main.rs); nothing persists until Save writes the TOML back to the config file.

use eframe::egui;

use crate::ui;
use crate::{Stdusk, colors, config};

/// Window content width (controls wrap inside this).
const PANEL_W: f32 = 520.0;
/// Label column width, so controls line up across rows.
const LABEL_W: f32 = 130.0;

/// Themes offered directly in the combo; anything else is typed into the scheme field.
const BUILT_IN_THEMES: [&str; 4] = ["one-half-dark", "one-half-light", "dracula", "tokyo-night"];

/// A labeled control row: fixed-width label, then the control (keeps columns aligned).
fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(LABEL_W, 24.0), egui::Sense::hover());
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(14.0),
            colors::fg(),
        );
        add(ui);
    });
}

/// Section heading with breathing room above.
fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(14.0);
    ui.heading(text);
    ui.add_space(4.0);
}

/// A themed combo box selecting one of `options` into `value`.
fn combo(ui: &mut egui::Ui, id: &str, value: &mut String, options: &[&str]) {
    egui::ComboBox::from_id_salt(id).selected_text(value.clone()).width(160.0).show_ui(ui, |ui| {
        for opt in options {
            ui.selectable_value(value, (*opt).to_string(), *opt);
        }
    });
}

fn appearance_section(ui: &mut egui::Ui, cfg: &mut config::Config) {
    heading(ui, "Appearance");
    let a = &mut cfg.appearance;
    ui.checkbox(&mut a.follow_system, "Follow the system light/dark appearance");
    if a.follow_system {
        // The fixed `theme` is ignored in this mode - only show what actually applies.
        ui.label(
            egui::RichText::new("Theme follows macOS: pick one per appearance.")
                .small()
                .color(colors::dim()),
        );
        row(ui, "Light theme", |ui| {
            ui::text_field(ui, &mut a.theme_light, "theme when light", 160.0, colors::fg());
        });
        row(ui, "Dark theme", |ui| {
            ui::text_field(ui, &mut a.theme_dark, "theme when dark", 160.0, colors::fg());
        });
    } else {
        row(ui, "Theme", |ui| {
            combo(ui, "theme", &mut a.theme, &BUILT_IN_THEMES);
        });
        row(ui, "Custom scheme", |ui| {
            ui::text_field(ui, &mut a.theme, "community scheme name", 160.0, colors::fg());
        });
    }
    row(ui, "Opacity", |ui| {
        ui.add(egui::Slider::new(&mut a.opacity, 0.4..=1.0));
    });
    row(ui, "Font size", |ui| {
        ui.add(egui::Slider::new(&mut a.font_size, 9.0..=24.0));
    });
}

fn terminal_section(ui: &mut egui::Ui, cfg: &mut config::Config) {
    heading(ui, "Terminal");
    let t = &mut cfg.terminal;
    row(ui, "Cursor style", |ui| {
        combo(ui, "cursor", &mut t.cursor, &["block", "underline", "beam"]);
    });
    row(ui, "Scrollback lines", |ui| {
        ui.add(egui::DragValue::new(&mut t.scrollback_lines).range(100..=1_000_000));
    });
    ui.checkbox(&mut t.cursor_blink, "Blink the cursor");
    ui.checkbox(&mut t.detect_progress, "Detect progress in output (tab progress bar)");
    ui.checkbox(&mut t.detect_clis, "Badge tabs running a known AI CLI");
    ui.checkbox(&mut t.clickable_links, "Clickable links");
    ui.checkbox(&mut t.copy_on_select, "Copy on select");
    ui.checkbox(&mut t.paste_on_middle_click, "Paste on middle click");
    ui.checkbox(&mut t.warn_on_multiline_paste, "Warn on multiline paste");
    ui.checkbox(&mut t.trim_whitespace_on_paste, "Trim whitespace on paste");
    ui.checkbox(&mut t.bold_bright, "Draw bold text in bright colors");
    ui.checkbox(&mut t.ligatures, "Symbol ligatures (-> => != ...)");
    ui.checkbox(&mut t.alt_is_meta, "Option sends Meta (ESC-prefixed keys)");
    ui.checkbox(&mut t.notify_on_done, "Notify when a long command finishes while hidden");
    ui.checkbox(&mut t.shell_integration, "Shell integration (OSC 133 command marks)");
}

fn quake_section(ui: &mut egui::Ui, cfg: &mut config::Config) {
    heading(ui, "Quake");
    let q = &mut cfg.quake;
    row(ui, "Hotkey", |ui| {
        ui::text_field(ui, &mut q.hotkey, "e.g. Ctrl+Grave, F13", 160.0, colors::fg());
        ui.label(egui::RichText::new("restart to apply").color(colors::dim()));
    });
    row(ui, "Height", |ui| {
        ui.add(egui::Slider::new(&mut q.height_pct, 0.2..=0.9));
    });
    ui.checkbox(&mut q.hide_on_focus_loss, "Hide when the window loses focus");
    ui.checkbox(&mut q.hide_from_dock, "Hide from the Dock (accessory app)");
    ui.checkbox(&mut q.menu_bar_icon, "Menu-bar icon");
    ui.checkbox(&mut q.dock_when_visible, "Show the Dock icon while visible");
}

fn session_section(ui: &mut egui::Ui, cfg: &mut config::Config) {
    heading(ui, "Session");
    ui.checkbox(&mut cfg.session.restore, "Restore last session's tabs on launch");
}

impl Stdusk {
    /// The settings window, shown while `settings_open`. Sections mutate `self.cfg` in place
    /// (live-apply); Save persists to the config file; Close / Esc / the gear dismiss it.
    pub(crate) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut close = false;
        egui::Window::new("Settings")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .frame(ui::overlay_frame())
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(PANEL_W);
                ui.heading("Settings");
                egui::ScrollArea::vertical().max_height(ctx.content_rect().height() * 0.75).show(
                    ui,
                    |ui| {
                        appearance_section(ui, &mut self.cfg);
                        terminal_section(ui, &mut self.cfg);
                        quake_section(ui, &mut self.cfg);
                        session_section(ui, &mut self.cfg);
                    },
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui::action_button(ui, "Save", true).clicked()
                        && let Some(p) = config::ensure_and_path()
                        && std::fs::write(p, config::config_to_toml(&self.cfg)).is_ok()
                    {
                        let now = ctx.input(|i| i.time);
                        self.toast = Some(("Saved".into(), now + 1.4));
                    }
                    if ui::action_button(ui, "Open config file", false).clicked()
                        && let Some(p) = config::ensure_and_path()
                    {
                        let _ = std::process::Command::new("open").arg(p).spawn();
                    }
                    if ui::action_button(ui, "Close", false).clicked() {
                        close = true;
                    }
                });
            });
        // Esc closes - but not while a hard modal (rename/paste/palette) or the find bar owns it.
        let modal_owns_esc = self.renaming.is_some()
            || !self.pending_pastes.is_empty()
            || self.palette.is_some()
            || self.search.is_some();
        if !modal_owns_esc && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if close {
            self.settings_open = false;
        }
    }
}

//! User config from `~/.config/stdusk/config.toml`. Missing file or fields fall back to
//! defaults (chosen to match Tabby where applicable). Also parses the quake hotkey string.
use std::str::FromStr;

use global_hotkey::hotkey::{Code, Modifiers};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Config {
    pub(crate) appearance: Appearance,
    pub(crate) quake: Quake,
    pub(crate) terminal: Terminal,
    pub(crate) session: Session,
    pub(crate) sync: Sync,
    pub(crate) hotkeys: Hotkeys,
    pub(crate) profiles: Vec<Profile>,
}

/// Settings sync: a git repo (ideally a private GitHub repo) that config.toml + custom
/// schemes are pushed to / pulled from using the user's own git credentials.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Sync {
    pub(crate) repo: String, // e.g. "git@github.com:you/stdusk-settings.git"; empty = off
    pub(crate) auto: bool,   // pull on launch + push after every settings Save
}

/// App hotkey remapping (`[hotkeys]`): action -> chord string ("Cmd+Shift+K"). A struct with
/// per-field defaults (not a map) so a typoed action name is a parse-ignored unknown field,
/// never a silently dead bind; missing fields keep their default via `#[serde(default)]`.
/// Empty string = unbound. The quake summon hotkey stays in `[quake] hotkey` (a GLOBAL OS
/// hotkey with its own parser/registration, not an in-app bind).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Hotkeys {
    pub(crate) new_tab: String,
    pub(crate) close: String,
    pub(crate) reopen: String,
    pub(crate) toggle_last_tab: String,
    pub(crate) find: String,
    pub(crate) palette: String,
    pub(crate) settings: String,
    pub(crate) broadcast: String,
    pub(crate) split_right: String,
    pub(crate) split_down: String,
    pub(crate) select_all: String,
    pub(crate) clear: String,
    pub(crate) zoom_in: String,
    pub(crate) zoom_out: String,
    pub(crate) zoom_reset: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            new_tab: "Cmd+T".into(),
            close: "Cmd+W".into(),
            reopen: "Cmd+Shift+T".into(),
            toggle_last_tab: "Cmd+O".into(),
            find: "Cmd+F".into(),
            palette: "Cmd+Shift+P".into(),
            settings: "Cmd+,".into(),
            broadcast: "Cmd+Shift+I".into(),
            split_right: "Cmd+D".into(),
            split_down: "Cmd+Shift+D".into(),
            select_all: "Cmd+A".into(),
            clear: "Cmd+K".into(),
            zoom_in: "Cmd+=".into(),
            zoom_out: "Cmd+-".into(),
            zoom_reset: "Cmd+0".into(),
        }
    }
}

/// A named launch profile (Tabby-style): per-tab shell/args/cwd/env overrides.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct Profile {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) shell: Option<String>, // override $SHELL
    #[serde(default)]
    pub(crate) args: Vec<String>, // extra shell arguments
    #[serde(default)]
    pub(crate) cwd: Option<String>, // starting directory; leading ~ expands to $HOME
    // BTreeMap (not HashMap): deterministic iteration = stable TOML output, so the settings
    // dirty guard and Save diffs can't flap on map order.
    #[serde(default)]
    pub(crate) env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) color: Option<String>, // tab color, "#rrggbb"
}

/// Look up a profile by name, case-insensitively.
#[allow(dead_code)] // the pickers (tab menu / palette) select by index; kept for by-name callers
pub(crate) fn find_profile<'a>(profiles: &'a [Profile], name: &str) -> Option<&'a Profile> {
    profiles.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// Expand a leading `~` / `~/...` to `$HOME`. `~user` and paths without a leading `~` pass
/// through unchanged, as does everything when $HOME is unset.
pub(crate) fn expand_tilde(path: &str) -> String {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            std::env::var("HOME").map_or_else(|_| path.to_string(), |home| format!("{home}{rest}"))
        }
        _ => path.to_string(),
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Session {
    pub(crate) restore: bool, // reopen last session's tabs (cwd/title/color) on launch
    pub(crate) confirm_quit_running: bool, // confirm before quitting while child processes run
}

impl Default for Session {
    fn default() -> Self {
        Self { restore: true, confirm_quit_running: true }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Appearance {
    pub(crate) theme: String, // used when follow_system = false
    pub(crate) opacity: f32,
    pub(crate) font: String, // terminal font family (e.g. "JetBrainsMono Nerd Font"); "" = bundled default
    pub(crate) font_size: f32,
    pub(crate) line_padding: f32, // extra px added to each cell's height (0-8)
    pub(crate) follow_system: bool, // pick theme_light/theme_dark by the OS appearance
    pub(crate) theme_light: String,
    pub(crate) theme_dark: String,
    pub(crate) tab_width: String, // "fixed" (equal widths, the default) | "dynamic" (fit title)
}

#[allow(clippy::struct_excessive_bools)] // independent quake toggles, not a mode
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Quake {
    /// `"dropdown"` (default: drops from the top edge on the global hotkey - the quake behavior)
    /// or `"window"` (a conventional resizable macOS window). Window mode disables the global
    /// hotkey and ignores every focus/Dock option below (it's always a normal Dock app).
    pub(crate) mode: String,
    pub(crate) hotkey: String,
    pub(crate) height_pct: f32,
    pub(crate) hide_on_focus_loss: bool,
    /// Run as a macOS accessory app: no Dock icon, no app-switcher/menu-bar entry - it just drops
    /// from the top on the hotkey (the quake default). Set false to appear as a normal Dock app.
    pub(crate) hide_from_dock: bool,
    /// Show a menu-bar (status) icon with a Show/Hide + Quit menu. The accessory app's main entry
    /// point + presence indicator; set false to hide it.
    pub(crate) menu_bar_icon: bool,
    /// (With hide_from_dock) show the Dock icon + a real menu bar *while the window is visible*,
    /// flipping back to accessory when it's hidden. Off by default (pure accessory - no Dock, and
    /// the menu bar belongs to whatever other app is frontmost).
    pub(crate) dock_when_visible: bool,
    /// Extra opacity multiplier while the window is visible but unfocused (only applies with
    /// `hide_on_focus_loss = false`). 1.0 = off.
    pub(crate) unfocused_opacity: f32,
    /// (Dropdown mode) drop the window onto whatever macOS Space/desktop is active when summoned,
    /// instead of yanking you back to the Space it was created on. Default true - the expected
    /// quake behavior. Set false to pin it to its origin Space. Ignored in window mode.
    pub(crate) follow_active_space: bool,
}

// Independent user toggles, not a mode - a state machine would be more code, not less.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Terminal {
    pub(crate) detect_progress: bool,
    pub(crate) cursor: String,          // "block" | "underline" | "beam"
    pub(crate) shell_integration: bool, // inject OSC 133 hooks into the spawned shell
    pub(crate) bell: String,            // "visual" | "off"
    pub(crate) detect_clis: bool,       // badge tabs running a known AI CLI (claude/gemini/...)
    pub(crate) clickable_links: bool,   // open URLs / file paths on click
    pub(crate) link_modifier: String,   // "none" (hover) | "cmd" | "ctrl" | "alt" | "shift"
    pub(crate) notify_on_done: bool, // desktop notification when a long (>10s) command finishes while hidden
    pub(crate) scrollback_lines: usize, // history size (Tabby default 25000)
    pub(crate) cursor_blink: bool,   // blink the focused pane's cursor
    pub(crate) alt_is_meta: bool,    // Option+key sends ESC+key instead of composed chars
    pub(crate) word_separators: String, // chars that end a double-click word selection
    pub(crate) copy_on_select: bool, // copy to clipboard whenever a selection finishes
    pub(crate) paste_on_middle_click: bool, // middle-click pastes the clipboard
    pub(crate) warn_on_multiline_paste: bool, // confirm before pasting multiple lines
    pub(crate) trim_whitespace_on_paste: bool, // strip leading/trailing whitespace from pastes
    pub(crate) replace_newlines_on_paste: bool, // newlines -> spaces on paste
    pub(crate) bold_bright: bool,    // draw bold text in the bright ANSI colors
    pub(crate) ligatures: bool, // render common code sequences (-> => != >= <=) as single glyphs
    pub(crate) warn_on_close_running: bool, // confirm closing a tab with a running process
    pub(crate) on_exit: String, // pane when its shell exits: "close" | "keep" | "restart"
    pub(crate) dynamic_title: bool, // tab titles follow the shell's OSC 0/2 title
    pub(crate) minimum_contrast: f32, // WCAG ratio text is nudged to meet, 1 (off) ..= 21
    pub(crate) right_click: String, // "menu" | "paste" | "clipboard" (copy selection, else paste)
    pub(crate) focus_follows_mouse: bool, // hovering a pane focuses it (no click)
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "one-half-dark".into(),
            opacity: 0.85,
            font: String::new(),
            font_size: 13.0,
            line_padding: 0.0,
            follow_system: true,
            theme_light: "one-half-light".into(),
            theme_dark: "one-half-dark".into(),
            tab_width: "fixed".into(),
        }
    }
}
impl Default for Quake {
    fn default() -> Self {
        Self {
            mode: "dropdown".into(),
            hotkey: "Ctrl+Grave".into(),
            height_pct: 0.5,
            hide_on_focus_loss: true,
            hide_from_dock: true,
            menu_bar_icon: true,
            dock_when_visible: false,
            unfocused_opacity: 1.0,
            follow_active_space: true,
        }
    }
}
impl Default for Terminal {
    fn default() -> Self {
        Self {
            detect_progress: true,
            cursor: "block".into(),
            shell_integration: true,
            bell: "visual".into(),
            detect_clis: true,
            clickable_links: true,
            link_modifier: "none".into(),
            notify_on_done: true,
            scrollback_lines: 25000,
            cursor_blink: true,
            alt_is_meta: false,
            word_separators: " ()[]{}'\"".into(),
            copy_on_select: false,
            paste_on_middle_click: true,
            warn_on_multiline_paste: true,
            trim_whitespace_on_paste: true,
            replace_newlines_on_paste: false,
            bold_bright: true,
            ligatures: false,
            warn_on_close_running: true,
            on_exit: "close".into(),
            dynamic_title: true,
            // Tabby ships 4; serde fills only ABSENT fields, so a config that explicitly
            // set 1.0 (off) keeps its exact-theme cells. See config.example.toml.
            minimum_contrast: 4.0,
            right_click: "menu".into(),
            focus_follows_mouse: false,
        }
    }
}

impl Config {
    pub(crate) fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
                eprintln!("stdusk: config parse error ({e}); using defaults");
                Self::default()
            }),
            Err(_) => Self::default(), // no file - defaults
        }
    }
}

fn config_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(|home| std::path::Path::new(&home).join(".config/stdusk/config.toml"))
}

/// Serialize a config to TOML - the settings window's Save path. Pure; round-trips through
/// `Config::deserialize` (unit-tested below).
pub(crate) fn config_to_toml(cfg: &Config) -> String {
    toml::to_string(cfg).unwrap_or_default()
}

/// Whether two configs differ (settings unsaved-changes guard). Compared via their TOML
/// serialization so it can't drift from what Save would write.
pub(crate) fn config_dirty(a: &Config, b: &Config) -> bool {
    config_to_toml(a) != config_to_toml(b)
}

/// Return the config path, creating it (with the example content) if it doesn't exist.
/// Used by the settings gear so "open config" always opens something.
pub(crate) fn ensure_and_path() -> Option<std::path::PathBuf> {
    let p = config_path()?;
    if !p.exists() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, include_str!("../config.example.toml"));
    }
    Some(p)
}

/// Parse a hotkey string like "Ctrl+Grave", "F13", "Cmd+Grave", "Ctrl+Shift+T" into
/// (modifiers, key). Falls back to Ctrl+Grave on anything unparseable.
pub(crate) fn parse_hotkey(s: &str) -> (Option<Modifiers>, Code) {
    let parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return (Some(Modifiers::CONTROL), Code::Backquote);
    }
    let mut mods = Modifiers::empty();
    for m in &parts[..parts.len() - 1] {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "cmd" | "command" | "super" | "meta" | "win" => mods |= Modifiers::META,
            "alt" | "option" | "opt" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            _ => {}
        }
    }
    let code = parse_code(parts[parts.len() - 1]).unwrap_or(Code::Backquote);
    let mods = if mods.is_empty() { None } else { Some(mods) };
    (mods, code)
}

/// Map a friendly key name to a W3C key `Code`.
fn parse_code(k: &str) -> Option<Code> {
    let lower = k.to_ascii_lowercase();
    let w3c = match lower.as_str() {
        "`" | "grave" | "backquote" => "Backquote".to_string(),
        // The §/± key left of "1" / above Tab on Mac ISO keyboards. macOS reports it as
        // kVK_ISO_Section, which maps to W3C `IntlBackslash`. NOTE: global-hotkey 0.8's macOS
        // keycode table has no IntlBackslash entry, so registering it is a no-op until that's
        // addressed in main.rs (see the LEDGER 1.2.0 entry) - the parse is correct regardless.
        "§" | "±" | "section" | "paragraph" | "intlbackslash" => "IntlBackslash".to_string(),
        "space" => "Space".to_string(),
        "enter" | "return" => "Enter".to_string(),
        "tab" => "Tab".to_string(),
        s if s.len() == 1 && s.chars().next().unwrap().is_ascii_alphabetic() => {
            format!("Key{}", s.to_ascii_uppercase())
        }
        s if s.len() == 1 && s.chars().next().unwrap().is_ascii_digit() => {
            format!("Digit{s}")
        }
        s if s.starts_with('f') && s[1..].parse::<u8>().is_ok() => {
            format!("F{}", &s[1..]) // f13 -> F13
        }
        _ => k.to_string(), // assume already a W3C code name
    };
    Code::from_str(&w3c).ok()
}

// --- Quake-vs-window mode decisions (pure; table-tested below) ---------------------------------
// Dropdown mode must answer these exactly as the app did before the mode existed; window mode
// flips the quake-specific ones. Keep the branching here so the render loop stays dumb.

/// `[quake] mode = "window"` runs stdusk as a conventional resizable macOS window; anything else
/// (the default `"dropdown"`) keeps the quake drop-from-the-top behavior. Case-insensitive.
pub(crate) fn window_mode(mode: &str) -> bool {
    mode.eq_ignore_ascii_case("window")
}

/// Whether the app is running as a normal window (vs the quake dropdown).
pub(crate) fn is_window_mode(cfg: &Config) -> bool {
    window_mode(&cfg.quake.mode)
}

/// The global summon hotkey is registered only in dropdown mode; window mode has none.
pub(crate) fn should_register_hotkey(mode: &str) -> bool {
    !window_mode(mode)
}

/// Only dropdown mode forces the top-edge position + monitor-width quake sizing; a window keeps
/// its own geometry.
pub(crate) fn forces_quake_geometry(mode: &str) -> bool {
    !window_mode(mode)
}

/// Whether the window hides when another app takes focus. Never in window mode (it stays put like
/// any app); in dropdown it follows `hide_on_focus_loss`.
pub(crate) fn hides_on_blur(cfg: &Config) -> bool {
    !window_mode(&cfg.quake.mode) && cfg.quake.hide_on_focus_loss
}

/// Whether the quake window should join all Spaces so it drops onto the active desktop when
/// summoned (`NSWindowCollectionBehaviorCanJoinAllSpaces`). Only in dropdown mode - a window-mode
/// window is a normal app window and keeps the default (origin-Space) behavior.
pub(crate) fn wants_all_spaces(cfg: &Config) -> bool {
    !window_mode(&cfg.quake.mode) && cfg.quake.follow_active_space
}

/// Whether the macOS activation policy should be Regular (Dock icon + app-switcher entry).
/// Always Regular in window mode; in dropdown it follows the Dock toggles (accessory unless a
/// Dock icon is wanted, or `dock_when_visible` while shown).
pub(crate) fn activation_is_regular(cfg: &Config, visible: bool) -> bool {
    window_mode(&cfg.quake.mode)
        || !cfg.quake.hide_from_dock
        || (cfg.quake.dock_when_visible && visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_tabby_baseline() {
        let c = Config::default();
        assert_eq!(c.appearance.theme, "one-half-dark");
        assert_eq!(c.appearance.opacity, 0.85);
        assert_eq!(c.appearance.tab_width, "fixed");
        assert!(c.appearance.font.is_empty()); // "" = bundled default
        assert_eq!(c.appearance.line_padding, 0.0);
        assert_eq!(c.quake.hotkey, "Ctrl+Grave");
        assert_eq!(c.quake.mode, "dropdown"); // preserves the pre-mode quake behavior
        assert!(c.quake.hide_on_focus_loss);
        assert!(c.quake.follow_active_space); // quake drops on the active Space by default
        assert_eq!(c.quake.unfocused_opacity, 1.0); // off by default
        assert!(c.terminal.detect_progress);
        assert!(c.terminal.warn_on_close_running);
        assert_eq!(c.terminal.on_exit, "close");
        assert!(c.terminal.dynamic_title);
        assert_eq!(c.terminal.minimum_contrast, 4.0); // Tabby's default; 1.0 = off
        assert_eq!(c.terminal.right_click, "menu"); // Tabby default
        assert!(!c.terminal.focus_follows_mouse); // Tabby default
    }

    #[test]
    fn dirty_detects_any_field_change() {
        let a = Config::default();
        let mut b = Config::default();
        assert!(!config_dirty(&a, &b));
        b.appearance.opacity = 0.5;
        assert!(config_dirty(&a, &b));
        b = Config::default();
        b.quake.hotkey = "F13".into();
        assert!(config_dirty(&a, &b));
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let c: Config = toml::from_str("[appearance]\ntheme = \"dracula\"\n").unwrap();
        assert_eq!(c.appearance.theme, "dracula");
        assert_eq!(c.appearance.opacity, 0.85); // default preserved
        assert_eq!(c.quake.hotkey, "Ctrl+Grave"); // default section
    }

    #[test]
    fn explicit_minimum_contrast_survives_the_default_bump() {
        // 1.0.3 changed the default 1.0 -> 4.0. `#[serde(default)]` fills only ABSENT
        // fields, so a user who explicitly opted out at 1.0 keeps their exact theme.
        let c: Config = toml::from_str("[terminal]\nminimum_contrast = 1.0\n").unwrap();
        assert_eq!(c.terminal.minimum_contrast, 1.0);
        let c: Config = toml::from_str("[terminal]\n").unwrap();
        assert_eq!(c.terminal.minimum_contrast, 4.0);
    }

    #[test]
    fn quake_mode_default_and_round_trips() {
        // A fresh config and one that omits `mode` both parse as dropdown (identical to the
        // pre-mode behavior); an explicit window value survives the round-trip.
        assert_eq!(Config::default().quake.mode, "dropdown");
        let bare: Config = toml::from_str("[quake]\n").unwrap();
        assert_eq!(bare.quake.mode, "dropdown");
        let mut cfg = Config::default();
        cfg.quake.mode = "window".into();
        let back: Config = toml::from_str(&config_to_toml(&cfg)).unwrap();
        assert_eq!(back.quake.mode, "window");
        assert!(config_dirty(&Config::default(), &cfg)); // the mode flip is a real edit
    }

    #[test]
    fn mode_decisions_dropdown_matches_today_window_flips() {
        // Dropdown: every decision matches what the app did before the mode existed.
        let mut d = Config::default();
        assert!(!window_mode(&d.quake.mode) && !is_window_mode(&d));
        assert!(should_register_hotkey(&d.quake.mode)); // hotkey registered
        assert!(forces_quake_geometry(&d.quake.mode)); // top-edge + monitor sizing
        assert!(hides_on_blur(&d)); // hide_on_focus_loss default true
        assert!(!activation_is_regular(&d, true)); // accessory by default (hidden from Dock)
        d.quake.hide_from_dock = false;
        assert!(activation_is_regular(&d, true)); // a Dock app in dropdown when opted in
        d = Config::default();
        d.quake.hide_on_focus_loss = false;
        assert!(!hides_on_blur(&d));

        // Window: the quake-specific decisions flip and the Dock/focus toggles are ignored.
        let mut w = Config::default();
        w.quake.mode = "Window".into(); // case-insensitive
        assert!(window_mode(&w.quake.mode) && is_window_mode(&w));
        assert!(!should_register_hotkey(&w.quake.mode)); // no global hotkey
        assert!(!forces_quake_geometry(&w.quake.mode)); // keeps its own geometry
        assert!(!hides_on_blur(&w)); // never auto-hides even with the flag on
        assert!(activation_is_regular(&w, false)); // always a Dock app, visible or not
        w.quake.hide_from_dock = true; // ignored in window mode
        assert!(activation_is_regular(&w, false));
    }

    #[test]
    fn follow_active_space_default_and_round_trips() {
        // Default on (fixes the "summon yanks you to Desktop 1" bug); omitting it keeps the
        // default; an explicit false survives the round-trip.
        assert!(Config::default().quake.follow_active_space);
        let bare: Config = toml::from_str("[quake]\n").unwrap();
        assert!(bare.quake.follow_active_space);
        let mut cfg = Config::default();
        cfg.quake.follow_active_space = false;
        let back: Config = toml::from_str(&config_to_toml(&cfg)).unwrap();
        assert!(!back.quake.follow_active_space);
        assert!(config_dirty(&Config::default(), &cfg)); // flipping it is a real edit
    }

    #[test]
    fn wants_all_spaces_only_in_dropdown_with_the_flag_on() {
        // (mode, follow_active_space) -> wants_all_spaces
        for (mode, follow, want) in [
            ("dropdown", true, true),   // default quake behavior: join all Spaces
            ("dropdown", false, false), // opted out: pinned to its origin Space
            ("window", true, false),    // window mode is a normal app window - never
            ("window", false, false),
        ] {
            let mut c = Config::default();
            c.quake.mode = mode.into();
            c.quake.follow_active_space = follow;
            assert_eq!(wants_all_spaces(&c), want, "mode={mode} follow={follow}");
        }
    }

    #[test]
    fn hotkey_parsing() {
        assert_eq!(parse_hotkey("Ctrl+Grave"), (Some(Modifiers::CONTROL), Code::Backquote));
        assert_eq!(parse_hotkey("F13"), (None, Code::F13));
        assert_eq!(parse_hotkey("Cmd+Grave"), (Some(Modifiers::META), Code::Backquote));
        assert_eq!(parse_hotkey("F12"), (None, Code::F12));
        assert_eq!(
            parse_hotkey("Ctrl+Shift+T"),
            (Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyT)
        );
    }

    #[test]
    fn section_key_names_parse_to_intl_backslash() {
        // The §/± key (Mac ISO, left of "1") - accept the glyphs and friendly names. Bare
        // (no modifier) works too, like the F-keys.
        for name in ["§", "±", "Section", "paragraph", "IntlBackslash"] {
            assert_eq!(parse_hotkey(name), (None, Code::IntlBackslash), "name={name}");
        }
        // Still composes with modifiers.
        assert_eq!(parse_hotkey("Ctrl+Section"), (Some(Modifiers::CONTROL), Code::IntlBackslash));
    }

    #[test]
    fn garbage_hotkey_falls_back() {
        assert_eq!(parse_hotkey(""), (Some(Modifiers::CONTROL), Code::Backquote));
        assert_eq!(parse_hotkey("nonsense"), (None, Code::Backquote));
    }

    #[test]
    fn profiles_parse_with_all_fields_and_defaults() {
        let c: Config = toml::from_str(
            r##"
[[profiles]]
name = "work"
shell = "/bin/zsh"
args = ["-c", "echo hi"]
cwd = "~/Git"
env = { AWS_PROFILE = "work" }
color = "#61afef"

[[profiles]]
name = "ops"
"##,
        )
        .unwrap();
        assert_eq!(c.profiles.len(), 2);
        let w = &c.profiles[0];
        assert_eq!(w.name, "work");
        assert_eq!(w.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(w.args, ["-c", "echo hi"]);
        assert_eq!(w.cwd.as_deref(), Some("~/Git"));
        assert_eq!(w.env["AWS_PROFILE"], "work");
        assert_eq!(w.color.as_deref(), Some("#61afef"));
        // name-only entry: every optional field defaults
        let o = &c.profiles[1];
        assert_eq!(o.name, "ops");
        assert!(o.shell.is_none() && o.cwd.is_none() && o.color.is_none());
        assert!(o.args.is_empty() && o.env.is_empty());
    }

    #[test]
    fn empty_config_has_no_profiles() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.profiles.is_empty());
    }

    #[test]
    fn find_profile_matches_case_insensitively() {
        let profiles: Vec<Profile> =
            toml::from_str::<Config>("[[profiles]]\nname = \"Work\"\n").unwrap().profiles;
        assert_eq!(find_profile(&profiles, "work").map(|p| p.name.as_str()), Some("Work"));
        assert_eq!(find_profile(&profiles, "WORK").map(|p| p.name.as_str()), Some("Work"));
        assert!(find_profile(&profiles, "missing").is_none());
        assert!(find_profile(&[], "work").is_none());
    }

    #[test]
    fn config_to_toml_round_trips() {
        let mut cfg = Config::default();
        cfg.appearance.theme = "dracula".into();
        cfg.appearance.opacity = 0.7;
        cfg.appearance.font = "JetBrainsMono Nerd Font".into();
        cfg.appearance.line_padding = 2.0;
        cfg.terminal.ligatures = true;
        cfg.quake.height_pct = 0.6;
        cfg.profiles.push(Profile {
            name: "work".into(),
            shell: Some("/bin/zsh".into()),
            args: vec!["-c".into(), "echo hi".into()],
            cwd: Some("~/Git".into()),
            env: [("AWS_PROFILE".to_string(), "work".to_string())].into(),
            color: Some("#61afef".into()),
        });
        let back: Config = toml::from_str(&config_to_toml(&cfg)).unwrap();
        assert_eq!(back.appearance.theme, "dracula");
        assert_eq!(back.appearance.opacity, 0.7);
        assert_eq!(back.appearance.font, "JetBrainsMono Nerd Font");
        assert_eq!(back.appearance.line_padding, 2.0);
        assert!(back.terminal.ligatures);
        assert_eq!(back.quake.height_pct, 0.6);
        assert_eq!(back.profiles.len(), 1);
        let p = &back.profiles[0];
        assert_eq!(p.name, "work");
        assert_eq!(p.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(p.cwd.as_deref(), Some("~/Git"));
        assert_eq!(p.env["AWS_PROFILE"], "work");
        assert_eq!(p.color.as_deref(), Some("#61afef"));
    }

    #[test]
    fn config_to_toml_round_trips_defaults_and_bare_profile() {
        // Optional profile fields all None/empty must serialize (and come back) cleanly.
        let mut cfg = Config::default();
        cfg.profiles.push(Profile {
            name: "ops".into(),
            shell: None,
            args: Vec::new(),
            cwd: None,
            env: std::collections::BTreeMap::new(),
            color: None,
        });
        let back: Config = toml::from_str(&config_to_toml(&cfg)).unwrap();
        assert_eq!(back.appearance.theme, Config::default().appearance.theme);
        assert_eq!(back.terminal.scrollback_lines, 25000);
        assert!(back.session.restore);
        assert!(back.session.confirm_quit_running);
        assert_eq!(back.profiles[0].name, "ops");
        assert!(back.profiles[0].shell.is_none() && back.profiles[0].color.is_none());
    }

    #[test]
    fn hotkeys_default_to_the_shipped_binds() {
        let h = Hotkeys::default();
        assert_eq!(h.new_tab, "Cmd+T");
        assert_eq!(h.close, "Cmd+W");
        assert_eq!(h.reopen, "Cmd+Shift+T");
        assert_eq!(h.toggle_last_tab, "Cmd+O");
        assert_eq!(h.find, "Cmd+F");
        assert_eq!(h.palette, "Cmd+Shift+P");
        assert_eq!(h.settings, "Cmd+,");
        assert_eq!(h.broadcast, "Cmd+Shift+I");
        assert_eq!(h.split_right, "Cmd+D");
        assert_eq!(h.split_down, "Cmd+Shift+D");
        assert_eq!(h.select_all, "Cmd+A");
        assert_eq!(h.clear, "Cmd+K");
        assert_eq!(h.zoom_in, "Cmd+=");
        assert_eq!(h.zoom_out, "Cmd+-");
        assert_eq!(h.zoom_reset, "Cmd+0");
    }

    #[test]
    fn partial_hotkeys_table_keeps_other_defaults() {
        let c: Config = toml::from_str("[hotkeys]\nnew_tab = \"Cmd+N\"\nclear = \"\"\n").unwrap();
        assert_eq!(c.hotkeys.new_tab, "Cmd+N"); // remapped
        assert_eq!(c.hotkeys.clear, ""); // explicitly unbound
        assert_eq!(c.hotkeys.close, "Cmd+W"); // untouched field keeps its default
        assert_eq!(c.hotkeys.palette, "Cmd+Shift+P");
    }

    #[test]
    fn hotkeys_round_trip_through_toml() {
        let mut cfg = Config::default();
        cfg.hotkeys.find = "Cmd+Shift+F".into();
        cfg.hotkeys.zoom_reset = String::new();
        let back: Config = toml::from_str(&config_to_toml(&cfg)).unwrap();
        assert_eq!(back.hotkeys.find, "Cmd+Shift+F");
        assert_eq!(back.hotkeys.zoom_reset, "");
        assert_eq!(back.hotkeys.new_tab, "Cmd+T");
        assert!(!back.sync.auto); // default off
    }

    #[test]
    fn tilde_expands_only_at_home_prefix() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/Git"), format!("{home}/Git"));
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("/abs/path"), "/abs/path");
        assert_eq!(expand_tilde("~user/x"), "~user/x"); // ~user unsupported, passes through
    }
}

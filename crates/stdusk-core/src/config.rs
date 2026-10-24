//! The `[terminal]` table of stdusk's `config.toml`: how a terminal pane behaves. The stdusk app reads it
//! from the file; an embedder builds it in code and gets the same behavior from the `from_config`
//! constructors in `stdusk-egui` and `SpawnOpts`.

use serde::{Deserialize, Serialize};
// Independent user toggles, not a mode - a state machine would be more code, not less.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TerminalConfig {
    pub detect_progress: bool,
    pub cursor: String,                  // "block" | "underline" | "beam"
    pub shell_integration: bool,         // inject OSC 133 hooks into the spawned shell
    pub autosuggestions: bool, // fish-style inline history suggestions (zsh; needs shell_integration)
    pub bell: String,          // "visual" | "off"
    pub detect_clis: bool,     // badge tabs running a known AI CLI (claude/gemini/...)
    pub clickable_links: bool, // open URLs / file paths on click
    pub link_modifier: String, // "none" (hover) | "cmd" | "ctrl" | "alt" | "shift"
    pub app_link_modifier: String, // "cmd" | "alt" | "ctrl": links in apps that take the mouse
    pub notify_on_done: bool, // desktop notification when a long (>10s) command finishes while hidden
    pub scrollback_lines: usize, // history size (Tabby default 25000)
    pub cursor_blink: bool,   // blink the focused pane's cursor
    pub alt_is_meta: bool,    // Option+key sends ESC+key instead of composed chars
    pub word_separators: String, // chars that end a double-click word selection
    pub copy_on_select: bool, // copy to clipboard whenever a selection finishes
    pub paste_on_middle_click: bool, // middle-click pastes the clipboard
    pub warn_on_multiline_paste: bool, // confirm before pasting multiple lines
    pub trim_whitespace_on_paste: bool, // strip leading/trailing whitespace from pastes
    pub replace_newlines_on_paste: bool, // newlines -> spaces on paste
    pub bold_bright: bool,    // draw bold text in the bright ANSI colors
    pub ligatures: bool,      // render common code sequences (-> => != >= <=) as single glyphs
    pub warn_on_close_running: bool, // confirm closing a tab with a running process
    pub on_exit: String,      // pane when its shell exits: "close" | "keep" | "restart"
    pub dynamic_title: bool,  // tab titles follow the shell's OSC 0/2 title
    pub minimum_contrast: f32, // WCAG ratio text is nudged to meet, 1 (off) ..= 21
    pub right_click: String,  // "menu" | "paste" | "clipboard" (copy selection, else paste)
    pub focus_follows_mouse: bool, // hovering a pane focuses it (no click)
    pub new_tab_position: String, // Cmd+T / menus / palette: "after_current" | "end"
    pub plus_button_position: String, // the tab bar's + button: "end" | "after_current"
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            detect_progress: true,
            cursor: "block".into(),
            shell_integration: true,
            autosuggestions: false,
            bell: "visual".into(),
            detect_clis: true,
            clickable_links: true,
            link_modifier: "none".into(),
            app_link_modifier: "cmd".into(),
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
            new_tab_position: "after_current".into(),
            plus_button_position: "end".into(),
        }
    }
}

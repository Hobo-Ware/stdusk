//! stdusk - a quake terminal with a real GUI tab bar.
//! M0 chrome · M1 shell · M1.5 progress · M2 colors · M3 quake · M4 config · M5 tabs · M6 io · M6.5 selection.
//! The `eframe::App` loop here stays thin; tabs live in `tabs.rs`, the pane workspace in
//! `workspace.rs`, find/paste overlays in `finder.rs`, drawing widgets + pure helpers in `ui.rs`.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use eframe::egui;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

mod colors;
mod config;
mod finder;
mod instance;
mod links;
mod osc;
mod palette;
mod pane;
mod procwatch;
mod progress;
mod search;
mod session;
mod settings;
mod shell;
mod sync;
mod tabs;
mod terminal;
mod themes;
mod tray;
mod ui;
mod workspace;
use config::Config;
use finder::Search;
use tabs::{Tab, TabAction, spawn_opts, spawn_tab};
use terminal::PtyTerm;
use ui::{apply_theme, auto_title, draw_toast, tint, toast_alpha};

const COLS: usize = 80;
const ROWS: usize = 24;

/// Leading inset (px) reserved in the tab strip in window mode so the first tab clears the macOS
/// traffic-light buttons, which float over the unified titlebar (`FullSizeContentView`). Dropdown
/// mode is borderless and gets no inset; non-macOS window mode keeps standard decorations.
#[cfg(target_os = "macos")]
pub(crate) const WINDOW_TRAFFIC_INSET: f32 = 78.0;
#[cfg(not(target_os = "macos"))]
pub(crate) const WINDOW_TRAFFIC_INSET: f32 = 0.0;

/// egui font-family name for the terminal's real bold face (registered by `build_fonts` only
/// when the user's font family resolves a bold sibling - the bundled default has none).
pub(crate) const BOLD_FONT_FAMILY: &str = "term-bold";

/// A user font resolved to raw file bytes + the face index inside the file (.ttc collections
/// like Menlo need the index; plain .ttf/.otf use 0).
struct ResolvedFont {
    bytes: Vec<u8>,
    index: u32,
}

/// Distance of a face from "plain regular", judged by its NAME - lowest wins. Slant keywords
/// dominate (an italic must never beat any upright face), then weight, then width.
/// Names, not `Font::properties()`: font-kit's core-text loader reports broken properties
/// (every Menlo face comes back `Italic, w400`) while `full_name()` is accurate.
fn face_name_score(name: &str) -> u32 {
    let n = name.to_ascii_lowercase();
    [
        ("italic", 100),
        ("oblique", 100),
        ("bold", 10),
        ("black", 8),
        ("heavy", 8),
        ("thin", 8),
        ("light", 8),
        ("medium", 4),
        ("condensed", 2),
    ]
    .iter()
    .filter(|(kw, _)| n.contains(kw))
    .map(|(_, pts)| pts)
    .sum()
}

/// Score a face as the family's BOLD sibling, judged by its NAME (same rationale as
/// `face_name_score`: core-text `properties()` lie). `None` disqualifies: the name must say
/// "bold" and must not be a slant. Among qualifiers, the plain Bold beats Semi/Extra/width
/// variants - lowest wins.
fn bold_face_name_score(name: &str) -> Option<u32> {
    let n = name.to_ascii_lowercase();
    if !n.contains("bold") || n.contains("italic") || n.contains("oblique") {
        return None;
    }
    Some(
        [
            ("semibold", 8),
            ("semi bold", 8),
            ("demibold", 8),
            ("demi bold", 8),
            ("extrabold", 4),
            ("extra bold", 4),
            ("ultrabold", 4),
            ("ultra bold", 4),
            ("condensed", 2),
            ("narrow", 2),
        ]
        .iter()
        .filter(|(kw, _)| n.contains(kw))
        .map(|(_, pts)| pts)
        .sum(),
    )
}

/// Pick the family face with the lowest `score(full_name())` and read its bytes + face index.
fn resolve_face(family: &str, score: impl Fn(&str) -> Option<u32>) -> Option<ResolvedFont> {
    use font_kit::source::SystemSource;
    let name = family.trim();
    if name.is_empty() {
        return None;
    }
    let fam = SystemSource::new().select_family_by_name(name).ok()?;
    let best = fam
        .fonts()
        .iter()
        .filter_map(|h| h.load().ok().and_then(|f| score(&f.full_name()).map(|s| (h, s))))
        .min_by_key(|&(_, s)| s)
        .map(|(h, _)| h)?;
    match best.clone() {
        font_kit::handle::Handle::Path { path, font_index } => {
            Some(ResolvedFont { bytes: std::fs::read(path).ok()?, index: font_index })
        }
        font_kit::handle::Handle::Memory { bytes, font_index } => {
            Some(ResolvedFont { bytes: (*bytes).clone(), index: font_index })
        }
    }
}

/// Resolve a font FAMILY name (e.g. "Menlo", "JetBrainsMono Nerd Font") to its font file via
/// the system font source (core-text on macOS). None for an empty or unknown family.
/// NOTE: font-kit's `select_best_match` is NOT face-accurate on macOS (it returned Menlo
/// *Italic* for "Menlo"), so select the family and pick the closest-to-regular face ourselves.
fn resolve_font(family: &str) -> Option<ResolvedFont> {
    resolve_face(family, |n| Some(face_name_score(n)))
}

/// Resolve the family's real BOLD face (upright, closest to plain Bold), or `None` when the
/// family doesn't ship one - bold cells then keep the regular face.
fn resolve_bold_font(family: &str) -> Option<ResolvedFont> {
    resolve_face(family, bold_face_name_score)
}

/// Installed font family names, sorted; cached (the font list doesn't change mid-run).
fn installed_families() -> &'static [String] {
    static FAMILIES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FAMILIES.get_or_init(|| {
        let mut names = font_kit::source::SystemSource::new().all_families().unwrap_or_default();
        names.sort();
        names.dedup();
        names
    })
}

/// The full font set: egui defaults + Phosphor icons + the user's terminal font (when resolved)
/// at the TOP of the Monospace family + emoji/symbol fallbacks appended to both families.
/// Shared by startup and the settings live-apply so the two paths can't drift.
/// A resolved `bold` face registers a second family (`BOLD_FONT_FAMILY`) that `render_grid`
/// switches to for BOLD cells; without one, bold cells keep the regular face (the bright-ANSI
/// color treatment `terminal.bold_bright` is independent and stands either way).
fn build_fonts(custom: Option<ResolvedFont>, bold: Option<ResolvedFont>) -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    // Phosphor icon font (tab-bar controls + close x) as a fallback in the proportional
    // family, so icon codepoints render in buttons/labels.
    fonts.font_data.insert(
        "phosphor".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/Phosphor.ttf")).into(),
    );
    if let Some(keys) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        keys.insert(1, "phosphor".to_owned());
    }
    // The user's terminal font goes FIRST in Monospace only (chrome text stays the bundled
    // proportional); every fallback below stays behind it so emoji/symbols keep rendering.
    if let Some(f) = custom {
        let mut data = egui::FontData::from_owned(f.bytes);
        data.index = f.index;
        fonts.font_data.insert("user-font".to_owned(), data.into());
        if let Some(keys) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            keys.insert(0, "user-font".to_owned());
        }
    }
    // Full monochrome Noto Emoji (vendored) - egui's bundled emoji font is a subset that
    // misses most SMP emoji (😀 💰 ...), so append this to both families to fill the gap.
    // Monochrome (glyf outlines) so egui can rasterize it; color emoji still won't render.
    fonts.font_data.insert(
        "noto-emoji".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/NotoEmoji-Regular.ttf")).into(),
    );
    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(keys) = fonts.families.get_mut(&fam) {
            keys.push("noto-emoji".to_owned());
        }
    }
    // Broad monochrome fallbacks (macOS) for arrows / box-drawing / powerline / misc symbols
    // the bundled fonts miss - appended as lowest priority so the primary fonts win. Loaded
    // best-effort; absent files (other OSes) are simply skipped.
    for (name, path) in [
        ("sys-unicode", "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
        ("sys-symbols", "/System/Library/Fonts/Apple Symbols.ttf"),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(name.to_owned(), egui::FontData::from_owned(bytes).into());
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(keys) = fonts.families.get_mut(&fam) {
                    keys.push(name.to_owned());
                }
            }
        }
    }
    // The bold family: the bold face first, then the full Monospace stack behind it (regular
    // face + fallbacks), so a glyph the bold file misses degrades to regular, never tofu.
    // Registered only when a bold face resolved - a FontId naming an absent family panics.
    if let Some(f) = bold {
        let mut data = egui::FontData::from_owned(f.bytes);
        data.index = f.index;
        fonts.font_data.insert("user-font-bold".to_owned(), data.into());
        let mut keys = vec!["user-font-bold".to_owned()];
        keys.extend(
            fonts.families.get(&egui::FontFamily::Monospace).into_iter().flatten().cloned(),
        );
        fonts.families.insert(egui::FontFamily::Name(BOLD_FONT_FAMILY.into()), keys);
    }
    fonts
}

#[allow(clippy::struct_excessive_bools)] // independent app-state flags, not a mode
struct Stdusk {
    tabs: Vec<Tab>,
    active: usize,
    prev_active: usize, // previously active tab index, for toggle-last-tab (Cmd+O)
    cfg: Config,
    hotkey_mgr: GlobalHotKeyManager, // kept alive so the registration persists
    registered_hotkey: String, // the hotkey string currently registered (live re-registration)
    hotkey_registered: bool,   // whether a global hotkey is registered (false in window mode)
    applied_font: String,      // the appearance.font last applied/attempted (live font re-apply)
    bold_font_ready: bool,     // a real bold face is registered (BOLD_FONT_FAMILY exists this run)
    toggle: Arc<AtomicBool>,   // set by the hotkey thread, consumed in ui()
    visible: bool,
    dock_shown: bool, // last-applied Dock-icon state (dynamic dock_when_visible mode)
    was_focused: bool, // gained focus since last show (so blur can hide)
    sized: bool,      // applied quake sizing once the monitor size was known
    renaming: Option<(usize, String, bool)>, // (tab index, edit buffer, request-focus-once)
    search: Option<Search>, // scrollback-search overlay (Cmd+F), None when closed
    palette: Option<palette::PaletteState>, // command palette (Cmd+Shift+P), None when closed
    settings_open: bool, // settings view is SHOWING (replaces the workspace; edits cfg live)
    settings_tab: bool, // a settings session exists: the right-pinned Settings tab + staged edits survive tab switches
    settings: settings::SettingsState, // selected section + scheme search/hover state
    closed: Vec<String>, // cwds of recently closed tabs, for reopen (Cmd+Shift+T)
    pending_pastes: std::collections::VecDeque<(u64, String)>, // multiline pastes awaiting confirm (tab id, text)
    pending_close: Option<(u64, String)>, // close-tab confirm: (tab id, prompt message)
    right_press: Option<(Vec<pane::Side>, f64)>, // right-button press on a pane: (path, egui time)
    window_top: Option<bool>, // last-applied always-on-top state (re-applied when it changes)
    space_all: Option<bool>, // last-applied "join all Spaces" collectionBehavior (re-applied on change)
    titlebar_unified: Option<bool>, // last-applied unified-titlebar state (window mode; re-applied on change)
    cmdv_paste: Arc<AtomicUsize>, // Cmd+V image-paste requests from the NSEvent monitor, drained in ui()
    fx_opacity: f32, // this frame's effective window opacity (unfocused dim applied); derived
    color_preview: Option<(u64, Option<egui::Color32>)>, // Color-menu swatch hover preview (tab id, color)
    toast: Option<(String, f64)>, // transient status message + expiry (egui time)
    flash: f64,                   // bell visual-flash expiry (egui time); 0 = none
    zoom: f32,                    // font-size multiplier (Cmd +/-/0)
    theme_name: String,           // currently-applied theme (to detect OS light/dark changes)
    sys: sysinfo::System,         // process table for CLI-awareness scans
    next_cli_scan: f64,           // egui time of the next throttled procwatch scan
    next_session_save: f64,       // egui time of the next throttled session persist
    last_session: session::SavedSession, // last persisted session (skip identical writes)
    tray: Option<tray::Tray>,     // menu-bar status item (kept alive; Some when enabled)
    sync_slot: sync::SyncSlot,    // settings-sync worker result, polled each frame
    sync_busy: bool,              // a sync push/pull is in flight (buttons disabled)
    launch_pull_cfg: Option<String>, // config TOML when the launch autosync pull spawned (staleness gate)
    new_tab_req: Arc<AtomicUsize>,   // new-tab requests from other launches (single-instance)
    screenshot: Option<String>,      // --screenshot PATH: demo tabs, capture, exit
}

impl Stdusk {
    fn new(
        cc: &eframe::CreationContext<'_>,
        mut cfg: Config,
        screenshot: Option<String>,
        settings_shot: bool,
        instance_listener: Option<instance::Listener>,
    ) -> Self {
        // Fonts: the shared builder (Phosphor icons + `appearance.font` at the top of Monospace
        // + emoji/symbol fallbacks). An unresolvable family keeps the bundled default + toasts.
        let custom = resolve_font(&cfg.appearance.font);
        let font_missing = !cfg.appearance.font.trim().is_empty() && custom.is_none();
        let bold = custom.is_some().then(|| resolve_bold_font(&cfg.appearance.font)).flatten();
        let bold_font_ready = bold.is_some();
        cc.egui_ctx.set_fonts(build_fonts(custom, bold));

        apply_theme(&cc.egui_ctx);

        // Deterministic captures: skip egui's ~0.1s widget animations (scrollbar fade-in,
        // toggle knobs) so the pass-2 screenshot shows the settled UI, not a mid-fade frame.
        if screenshot.is_some() {
            cc.egui_ctx.all_styles_mut(|s| s.animation_time = 0.0);
        }

        // Global quake hotkey from config (default Ctrl+`). Carbon API on macOS - no
        // Accessibility grant needed. Window mode has no summon hotkey - skip registration.
        let mgr = GlobalHotKeyManager::new().expect("hotkey manager");
        let hotkey_registered = config::should_register_hotkey(&cfg.quake.mode);
        if hotkey_registered {
            let (mods, code) = config::parse_hotkey(&cfg.quake.hotkey);
            let _ = mgr.register(HotKey::new(mods, code));
        }

        // A thread wakes the UI (even while hidden) when the hotkey fires.
        let toggle = Arc::new(AtomicBool::new(false));
        let toggle_thread = toggle.clone();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let rx = GlobalHotKeyEvent::receiver();
            while let Ok(ev) = rx.recv() {
                if ev.state == HotKeyState::Pressed {
                    toggle_thread.store(true, Ordering::SeqCst);
                    ctx.request_repaint();
                }
            }
        });

        // Cmd+V image paste: egui-winit swallows Cmd+V by reading clipboard TEXT only, so an
        // image-only clipboard produces no egui event (see LEDGER). A macOS NSEvent LOCAL monitor
        // sees the KeyDown alongside egui-winit; when it's Cmd+V over an image-only clipboard it
        // bumps this counter (drained in ui() -> inject ^V to the focused pane) and swallows the
        // key. Skipped under --screenshot.
        let cmdv_paste = Arc::new(AtomicUsize::new(0));
        #[cfg(target_os = "macos")]
        if screenshot.is_none() {
            install_cmd_v_image_monitor(cc.egui_ctx.clone(), cmdv_paste.clone());
        }

        // Single-instance: as the primary, accept connections from later launches and, per
        // connection, surface the window + open a new tab. Skipped when we didn't take the lock
        // (screenshot harness) or off unix.
        let new_tab_req = instance::pending_counter();
        #[cfg(unix)]
        if let Some(listener) = instance_listener {
            instance::spawn_listener(listener, new_tab_req.clone(), cc.egui_ctx.clone());
        }
        #[cfg(not(unix))]
        let _ = instance_listener;

        // Session restore: reopen last session's tabs (cwd/title/color); else one fresh tab.
        let mut tabs = Vec::new();
        let mut active = 0;
        if cfg.session.restore && screenshot.is_none() {
            let saved = session::load();
            // Claude auto-resume targets, gathered while building tabs: (tab index, leaf path) plus
            // the parallel resume inputs (cwd + captured id). Split tabs contribute one entry per
            // claude leaf (from the pane tree); legacy single-pane sessions contribute the tab's
            // top-level claude state at the root leaf.
            let mut resume_targets: Vec<(usize, Vec<pane::Side>)> = Vec::new();
            let mut resume_inputs: Vec<session::ResumeTab> = Vec::new();
            for (i, st) in saved.tabs.iter().enumerate() {
                // Rebuild the split layout when present; else a single pane in the saved cwd.
                let mut tab = match &st.pane {
                    Some(sp) => tabs::spawn_saved_tab(&cfg, &cc.egui_ctx, sp),
                    None => spawn_tab(&cfg, &cc.egui_ctx, st.cwd.clone()),
                };
                // Same rule as the rename dialog: a persisted empty/whitespace rename is no
                // rename at all - auto-titling stays live.
                if let Some(title) = st.title.as_deref().and_then(ui::commit_rename) {
                    tab.title = title;
                    tab.renamed = true;
                }
                tab.color = st.color.as_deref().and_then(session::hex_to_color);
                tab.pinned = st.pinned;
                // Per-leaf claude: align each restored pane (by leaf path) with its saved claude
                // state. `flat_leaves()` and `leaf_paths()` share the A-before-B order.
                match &st.pane {
                    Some(sp) => {
                        for ((cwd, claude), path) in
                            sp.flat_leaves().into_iter().zip(tab.root().leaf_paths())
                        {
                            if let Some(c) = claude {
                                resume_targets.push((i, path));
                                resume_inputs.push(session::ResumeTab {
                                    cwd: cwd.clone().unwrap_or_default(),
                                    resume_id: c.resume_id.clone(),
                                });
                            }
                        }
                    }
                    None => {
                        if let Some(c) = &st.claude {
                            resume_targets.push((i, Vec::new()));
                            resume_inputs.push(session::ResumeTab {
                                cwd: st.cwd.clone().unwrap_or_default(),
                                resume_id: c.resume_id.clone(),
                            });
                        }
                    }
                }
                tabs.push(tab);
            }
            active = saved.active.min(tabs.len().saturating_sub(1));
            // Auto-resume Claude Code panes: inject a resume command into each claude pane's pty.
            // The shell buffers stdin, so the command lands at its first prompt (no shell-
            // integration dependency). Never runs under --screenshot (guarded by the block).
            if cfg.session.resume_claude {
                let cmds =
                    session::resume_commands(&resume_inputs, &session::claude_projects_dir());
                for ((i, path), cmd) in resume_targets.into_iter().zip(cmds) {
                    if let Some(term) = tabs[i].root_mut().leaf_at_mut(&path) {
                        // `\r` = Enter (a bare CR submits; matches ui::key_to_bytes for Enter).
                        term.send(format!("{cmd}\r").as_bytes());
                    }
                }
            }
        }
        if tabs.is_empty() {
            tabs.push(spawn_tab(&cfg, &cc.egui_ctx, None));
            active = 0;
        }
        let mut sized = false;

        // Visual-test harness: populate representative tabs and skip monitor sizing.
        if screenshot.is_some() {
            for _ in 0..3 {
                tabs.push(spawn_tab(&cfg, &cc.egui_ctx, None));
            }
            let titles =
                ["auth-session", "smart-lists-really-long-name", "cocaine", "deconversion-monitor"];
            for (t, name) in tabs.iter_mut().zip(titles) {
                t.title = name.into();
                t.renamed = true;
            }
            tabs[0].color = Some(colors::tab_colors()[0]); // red
            tabs[3].color = Some(colors::tab_colors()[4]); // green
            tabs[0].cli = Some(procwatch::Cli::Claude); // demo the CLI-awareness badge
            tabs[2].cli = Some(procwatch::Cli::Gemini);
            tabs[0].pinned = true; // demo the pinned-tab marker
            active = 1;
            sized = true;
            // STDUSK_SHOT_BROADCAST: split the active demo tab and force broadcast mode, so
            // the per-pane accent border (and the dropped unfocused fade) is capturable.
            if std::env::var("STDUSK_SHOT_BROADCAST").is_ok() {
                let tab = &mut tabs[active];
                let new = PtyTerm::spawn(COLS, ROWS, cc.egui_ctx.clone(), &spawn_opts(&cfg, None));
                let root = tab.root.take().expect("root");
                let (root, focus) = root.split(&tab.focused, pane::SplitDir::Row, new, false);
                tab.root = Some(root);
                if let Some(f) = focus {
                    tab.focused = f;
                }
                tab.broadcast = true;
            }
            // STDUSK_SHOT_SETTLE_MS: eframe captures at cumulative pass 2, which beats the
            // pty readers - sleep here (before the first pass) so demo-shell output lands in
            // the grid. The bold-face pixel proof drives real SGR output through $SHELL.
            if let Some(ms) =
                std::env::var("STDUSK_SHOT_SETTLE_MS").ok().and_then(|v| v.parse::<u64>().ok())
            {
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
        }

        // Menu-bar status item is the accessory app's presence + control; skip it in the
        // screenshot harness and when disabled.
        let tray = (cfg.quake.menu_bar_icon && screenshot.is_none()).then(tray::build).flatten();
        let theme_name = cfg.appearance.theme.clone();

        // --screenshot-settings: open the settings view on the scheme browser (the money shot)
        // or, via STDUSK_SHOT_SECTION, any other section (visual checks of the whole view).
        let mut settings = settings::SettingsState::new();
        if settings_shot {
            let section = match std::env::var("STDUSK_SHOT_SECTION").as_deref() {
                Ok("appearance") => settings::Section::Appearance,
                Ok("terminal") => settings::Section::Terminal,
                Ok("profiles") => settings::Section::Profiles,
                Ok("hotkeys") => settings::Section::Hotkeys,
                Ok("quake") => settings::Section::Quake,
                Ok("session") => settings::Section::Session,
                Ok("about") => settings::Section::About,
                _ => settings::Section::ColorScheme,
            };
            settings.open_section(section);
            // STDUSK_SHOT_DROPDOWN=<id_salt>: render with that searchable dropdown open (its
            // popup can't be pointer-driven headless). The light/dark slot dropdowns only
            // exist with follow-system on - flip it and pin both slots so the capture stays
            // deterministic (the per-frame OS-theme reconcile is skipped in the harness).
            if let Ok(salt) = std::env::var("STDUSK_SHOT_DROPDOWN") {
                if salt == "theme_light" || salt == "theme_dark" {
                    cfg.appearance.follow_system = true;
                    cfg.appearance.theme_light = "one-half-light".into();
                    cfg.appearance.theme_dark = "one-half-dark".into();
                }
                settings.force_dropdown(salt);
            }
            // The Profiles shot needs content: representative demo profiles (only when the
            // user config has none) with the first one expanded into the inline editor.
            if section == settings::Section::Profiles {
                if cfg.profiles.is_empty() {
                    cfg.profiles = vec![
                        config::Profile {
                            name: "work".into(),
                            shell: Some("/bin/zsh".into()),
                            args: vec!["-l".into()],
                            cwd: Some("~/Git".into()),
                            env: [("AWS_PROFILE".to_string(), "work".to_string())].into(),
                            color: Some("#61afef".into()),
                        },
                        config::Profile {
                            name: "ops".into(),
                            shell: None,
                            args: Vec::new(),
                            cwd: None,
                            env: std::collections::BTreeMap::new(),
                            color: None,
                        },
                    ];
                }
                settings.select_profile(0);
            }
        }

        let registered_hotkey = cfg.quake.hotkey.clone();
        let applied_font = cfg.appearance.font.clone();
        let toast = font_missing.then(|| (format!("Font not found: {}", cfg.appearance.font), 3.0));
        let fx_opacity = cfg.appearance.opacity;
        // Autosync: pull once on launch (in the background - startup never blocks on git).
        // The per-frame sync_done handler applies the result like a manual Pull; a failure
        // toasts once. Pushes happen on settings Save (the only disk write). The config
        // snapshot gates a SLOW pull against changes made while it ran (sync::pull_is_stale).
        let sync_slot = sync::SyncSlot::default();
        let repo = cfg.sync.repo.trim().to_owned();
        let sync_busy =
            screenshot.is_none() && sync::should_autosync(cfg.sync.auto, !repo.is_empty(), false);
        let launch_pull_cfg = sync_busy.then(|| config::config_to_toml(&cfg));
        if sync_busy {
            sync::spawn(sync::Op::Pull, repo, &sync_slot, cc.egui_ctx.clone());
        }
        // Initial Dock presence must mirror the launch activation policy (see main()). Visible at
        // launch, so `dock_when_visible` counts; window mode is always Regular.
        let dock_shown = config::activation_is_regular(&cfg, true);
        Self {
            tabs,
            active,
            prev_active: 0,
            cfg,
            hotkey_mgr: mgr,
            registered_hotkey,
            hotkey_registered,
            applied_font,
            bold_font_ready,
            toggle,
            visible: true,
            dock_shown,
            was_focused: false, // arm hide-on-blur only after the first focus gain
            sized,
            renaming: None,
            search: None,
            palette: None,
            settings_open: settings_shot,
            settings_tab: settings_shot,
            settings,
            closed: Vec::new(),
            pending_pastes: std::collections::VecDeque::new(),
            pending_close: None,
            right_press: None,
            window_top: None,
            space_all: None,
            titlebar_unified: None,
            cmdv_paste,
            fx_opacity,
            color_preview: None,
            toast,
            flash: 0.0,
            zoom: 1.0,
            theme_name,
            sys: sysinfo::System::new(),
            next_cli_scan: 0.0,
            next_session_save: 0.0,
            last_session: session::SavedSession::default(),
            tray,
            sync_slot,
            sync_busy,
            launch_pull_cfg,
            new_tab_req,
            screenshot,
        }
    }

    /// Keep the Dock icon (+ menu bar) in sync with the config and, in the dynamic
    /// `dock_when_visible` mode, with the window's visibility. Reconciling desired-vs-applied
    /// each frame makes the Dock toggles in settings live (no restart). Only touches the
    /// activation policy when it actually changes.
    fn sync_dock(&mut self) {
        let want = config::activation_is_regular(&self.cfg, self.visible);
        if want != self.dock_shown {
            set_dock_icon(want);
            self.dock_shown = want;
        }
    }

    /// Reconcile the global quake hotkey with the config (settings live-apply: field commit, Save,
    /// Revert, Discard, sync pull, and mode switches). Window mode wants NO hotkey, so it
    /// unregisters any live one; dropdown mode (re-)registers when the string changed or nothing
    /// is registered yet. Returns true when a hotkey became newly active (for the commit toast).
    fn reregister_hotkey(&mut self) -> bool {
        if !config::should_register_hotkey(&self.cfg.quake.mode) {
            if self.hotkey_registered {
                let (mods, code) = config::parse_hotkey(&self.registered_hotkey);
                let _ = self.hotkey_mgr.unregister(HotKey::new(mods, code));
                self.hotkey_registered = false;
            }
            return false;
        }
        if self.hotkey_registered && self.cfg.quake.hotkey == self.registered_hotkey {
            return false;
        }
        if self.hotkey_registered {
            let (mods, code) = config::parse_hotkey(&self.registered_hotkey);
            let _ = self.hotkey_mgr.unregister(HotKey::new(mods, code));
        }
        let (mods, code) = config::parse_hotkey(&self.cfg.quake.hotkey);
        let _ = self.hotkey_mgr.register(HotKey::new(mods, code));
        self.registered_hotkey = self.cfg.quake.hotkey.clone();
        self.hotkey_registered = true;
        true
    }

    /// Live-apply a dropdown<->window mode switch from settings: (un)register the global hotkey,
    /// flip the window chrome + level, and either restore the top-edge quake geometry (dropdown)
    /// or hand the window back to the user (window). Decoration/activation changes are best-effort
    /// at runtime on macOS - the settings row hints "chrome applies on restart" for what winit
    /// won't flip live. The Dock/activation policy reconciles next frame via `sync_dock`.
    fn apply_quake_mode(&mut self, ctx: &egui::Context) {
        self.reregister_hotkey();
        if self.screenshot.is_some() {
            return;
        }
        let window_mode = config::is_window_mode(&self.cfg);
        ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(window_mode));
        ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(window_mode));
        self.visible = true;
        self.window_top = None; // force the per-frame WindowLevel reconcile to re-send
        self.titlebar_unified = None; // re-apply the unified-titlebar state for the new mode
        if !window_mode {
            // Back to the quake window: pin it to the top edge at the configured height.
            self.sized = true; // geometry is applied now; skip the first-run sizing path
            apply_visibility(ctx, true, self.cfg.quake.height_pct);
            self.was_focused = false;
        }
    }

    /// Rebuild + apply the egui font set when `appearance.font` changed (settings live-apply:
    /// field commit, dropdown pick, Save, Revert, Discard, sync pull). An unresolvable family
    /// keeps the current fonts and toasts "Font not found". Returns true when fonts changed.
    fn reapply_font(&mut self, ctx: &egui::Context) -> bool {
        if self.cfg.appearance.font == self.applied_font {
            return false;
        }
        self.applied_font.clone_from(&self.cfg.appearance.font);
        let name = self.cfg.appearance.font.trim();
        let custom = resolve_font(name);
        if !name.is_empty() && custom.is_none() {
            let now = ctx.input(|i| i.time);
            self.toast = Some((format!("Font not found: {name}"), now + 3.0));
            return false;
        }
        let bold = custom.is_some().then(|| resolve_bold_font(name)).flatten();
        self.bold_font_ready = bold.is_some();
        ctx.set_fonts(build_fonts(custom, bold));
        true
    }
}

/// Show/hide the Dock icon (+ menu bar) at runtime by flipping the macOS activation policy.
/// Used only in the dynamic `dock_when_visible` mode. No-op off macOS.
#[cfg(target_os = "macos")]
fn set_dock_icon(visible: bool) {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        let policy = if visible {
            NSApplicationActivationPolicy::Regular
        } else {
            NSApplicationActivationPolicy::Accessory
        };
        app.setActivationPolicy(policy);
    }
}
#[cfg(not(target_os = "macos"))]
fn set_dock_icon(_visible: bool) {}

/// Set the quake window's Space/full-screen collection behavior. `all_spaces` = true makes it
/// join every Space (`CanJoinAllSpaces`) and drop over full-screen apps (`FullScreenAuxiliary`)
/// so summoning it lands on whatever desktop is active; false restores the default (pinned to
/// its origin Space). Applied to every app window (the app has one viewport). No-op off macOS.
#[cfg(target_os = "macos")]
fn set_space_behavior(all_spaces: bool) {
    use objc2_app_kit::{NSApplication, NSWindowCollectionBehavior};
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let behavior = if all_spaces {
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
        } else {
            NSWindowCollectionBehavior::Default
        };
        let app = NSApplication::sharedApplication(mtm);
        let windows = app.windows();
        for i in 0..windows.count() {
            windows.objectAtIndex(i).setCollectionBehavior(behavior);
        }
    }
}
#[cfg(not(target_os = "macos"))]
fn set_space_behavior(_all_spaces: bool) {}

/// Unified titlebar (window mode): make the OS title bar transparent + extend the content view
/// under it (`FullSizeContentView`) so the tab strip fills the top row and the traffic-light
/// buttons float over it. `enabled=false` restores the standard stacked title bar. No-op off
/// macOS.
///
/// NOTE: deliberately does NOT set `movableByWindowBackground`. winit 0.30 doesn't override
/// `mouseDownCanMoveWindow`, so enabling it could let AppKit start a window-drag on mouse-down
/// before egui sees the event - hijacking terminal text selection / tab clicks in window mode.
/// Losing drag-by-tab-bar is the safer trade.
#[cfg(target_os = "macos")]
fn set_unified_titlebar(enabled: bool) {
    use objc2_app_kit::{NSApplication, NSWindowStyleMask, NSWindowTitleVisibility};
    if let Some(mtm) = objc2::MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        let windows = app.windows();
        for i in 0..windows.count() {
            let w = windows.objectAtIndex(i);
            w.setTitlebarAppearsTransparent(enabled);
            w.setTitleVisibility(if enabled {
                NSWindowTitleVisibility::Hidden
            } else {
                NSWindowTitleVisibility::Visible
            });
            let mut mask = w.styleMask();
            mask.set(NSWindowStyleMask::FullSizeContentView, enabled);
            w.setStyleMask(mask);
        }
    }
}
#[cfg(not(target_os = "macos"))]
fn set_unified_titlebar(_enabled: bool) {}

/// Whether the whole app is the active (frontmost) macOS app. This stays TRUE when a *system*
/// panel (the emoji/character viewer, Ctrl+Cmd+Space) takes the key window - unlike winit's
/// per-window `focused`, which drops. Used to gate hide-on-blur so the emoji picker doesn't
/// dismiss the quake window; it only drops to false when another real app is activated. Off
/// macOS there's no such panel, so we report false and let winit focus drive hiding as before.
#[cfg(target_os = "macos")]
fn app_is_active() -> bool {
    objc2::MainThreadMarker::new()
        .is_some_and(|mtm| objc2_app_kit::NSApplication::sharedApplication(mtm).isActive())
}
#[cfg(not(target_os = "macos"))]
fn app_is_active() -> bool {
    false
}

/// Whether a Cmd+V keystroke over an image-only clipboard should be swallowed (and an image paste
/// injected). Pure decision so the seam is unit-testable; the impure clipboard probe lives in
/// `clipboard_image_only`.
fn decide_cmd_v_image_paste(command_down: bool, is_v: bool, image_only: bool) -> bool {
    command_down && is_v && image_only
}

/// True when the system clipboard holds an image and NO usable text. Text always wins (mirrors
/// the mouse-paste decision in `workspace.rs`), so a Cmd+V with text on the clipboard is left to
/// egui's normal text-paste path.
#[cfg(target_os = "macos")]
fn clipboard_image_only() -> bool {
    let Ok(mut cb) = arboard::Clipboard::new() else {
        return false;
    };
    let has_text = cb.get_text().ok().is_some_and(|t| !t.is_empty());
    !has_text && cb.get_image().is_ok()
}

/// Install a macOS NSEvent LOCAL key-down monitor for the Cmd+V image-paste hook. It runs on the
/// main thread inside `[NSApplication sendEvent:]`, BEFORE egui-winit sees the key: for Cmd+V over
/// an image-only clipboard it bumps `paste_req` + wakes the UI and returns nil (swallowing the
/// event so egui doesn't also handle it); every other key is returned unchanged.
#[cfg(target_os = "macos")]
fn install_cmd_v_image_monitor(ctx: egui::Context, paste_req: Arc<AtomicUsize>) {
    use std::ptr::NonNull;

    use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};

    let block = block2::RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands us a live NSEvent for the duration of this callback.
        #[allow(unsafe_code)]
        let ev = unsafe { event.as_ref() };
        let command_down = ev.modifierFlags().contains(NSEventModifierFlags::Command);
        let is_v = ev.charactersIgnoringModifiers().is_some_and(|s| s.to_string() == "v");
        // Only probe the clipboard for the Cmd+V combo (cheap on every other key).
        let image_only = command_down && is_v && clipboard_image_only();
        if decide_cmd_v_image_paste(command_down, is_v, image_only) {
            paste_req.fetch_add(1, Ordering::SeqCst);
            ctx.request_repaint();
            std::ptr::null_mut() // swallow: egui-winit must not also process this Cmd+V
        } else {
            event.as_ptr() // pass through unchanged (normal text Cmd+V still works)
        }
    });
    // SAFETY: the handler returns a valid NSEvent pointer or null, per the monitor contract.
    #[allow(unsafe_code)]
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block)
    };
    // The monitor + block must live for the app's lifetime; leak both (removed only at exit).
    std::mem::forget(monitor);
    std::mem::forget(block);
}

/// Post a desktop notification (macOS `osascript`); `body` is the visible line. Shared by
/// notify-when-done and notify-on-activity so the osascript plumbing can't drift.
fn notify(body: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!("display notification {body:?} with title \"stdusk\"");
        let _ = std::process::Command::new("osascript").args(["-e", &script]).spawn();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = body;
}

/// Notify that a long command finished (exit-code aware body).
fn notify_done(title: &str, code: i32) {
    let status = if code == 0 { "finished".to_owned() } else { format!("failed (exit {code})") };
    notify(&format!("{title}: command {status}"));
}

/// Show (drop to the top edge, focused) or hide the quake window by parking it fully above the
/// top edge (ZERO pixels remain, no sliver). The parked window stays a live viewport so `ui()`
/// keeps ticking - the hotkey thread's `request_repaint` + the 120ms tick then deliver the summon.
pub(crate) fn apply_visibility(ctx: &egui::Context, visible: bool, height_pct: f32) {
    let mon = ctx.input(|i| i.viewport().monitor_size);
    if visible {
        if let Some(m) = mon {
            let h = (m.y * height_pct).round();
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(m.x, h)));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, 0.0)));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    } else {
        // Park the window FULLY above the top edge: zero pixels remain (no sliver), but it stays
        // a live on-screen viewport so eframe keeps calling `ui()` - which re-arms the 120ms tick
        // that lets the summon hotkey reshow it. A native `orderOut:` / `set_visible(false)` would
        // hide it truly but PARK THE RUN LOOP (ui() stops ticking), so the hotkey could never
        // bring it back - the 1.3.0->1.3.1 quake regression.
        let h = mon.map_or(1200.0, |m| (m.y * height_pct).round());
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(0.0, -(h + 8.0))));
    }
}

impl eframe::App for Stdusk {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Transparent framebuffer; the panel fills below carry the tint at `opacity`.
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let height_pct = self.cfg.quake.height_pct;
        // Toggle-last-tab bookkeeping: any switch this frame (click, keybind, palette, close)
        // makes the tab active at frame start the "previous" one (diffed at the frame's end).
        let active_at_frame_start = self.active;

        // Effective window opacity: dim while visible-but-unfocused when hide-on-focus-loss is
        // off (the "keep it around" mode); eased so focus changes fade instead of popping.
        // The screenshot harness window is never focused - keep it at the configured base.
        let opacity = if self.screenshot.is_none() && !config::is_window_mode(&self.cfg) {
            let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
            let target = ui::effective_opacity(
                self.cfg.appearance.opacity,
                self.cfg.quake.unfocused_opacity,
                self.visible,
                focused,
                self.cfg.quake.hide_on_focus_loss,
            );
            ctx.animate_value_with_time(egui::Id::new("fx_opacity"), target, 0.15)
        } else {
            // Window mode ignores the unfocused-dim option (it's a normal app window).
            self.cfg.appearance.opacity
        };
        self.fx_opacity = opacity;

        // Single-instance: another launch asked us to surface + open a new tab. Drain the
        // counter regardless of mode (works even while a dropdown window is hidden).
        let new_tabs = self.new_tab_req.swap(0, Ordering::SeqCst);
        if new_tabs > 0 && self.screenshot.is_none() {
            if config::is_window_mode(&self.cfg) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            } else {
                self.visible = true;
                apply_visibility(&ctx, true, height_pct);
                self.was_focused = false;
            }
            for _ in 0..new_tabs {
                self.apply_tab_action(Some(TabAction::New), &ctx);
            }
        }

        // Cmd+V image paste: the NSEvent monitor swallowed a Cmd+V over an image-only clipboard.
        // Inject the raw ^V (0x16) into the focused pane's pty; Claude Code (iTerm-style) reads
        // the system clipboard itself on ^V and ingests the image (same path as Ctrl+V).
        if self.cmdv_paste.swap(0, Ordering::SeqCst) > 0
            && self.screenshot.is_none()
            && let Some(tab) = self.tabs.get_mut(self.active)
        {
            tab.focused_term_mut().send(&[0x16]);
        }

        // Quake window management is skipped in the screenshot harness.
        if self.screenshot.is_none() {
            // Menu-bar icon toggle is live in BOTH modes: build/drop the tray as it flips.
            if self.cfg.quake.menu_bar_icon && self.tray.is_none() {
                self.tray = tray::build();
            } else if !self.cfg.quake.menu_bar_icon && self.tray.is_some() {
                self.tray = None;
            }

            // Follow the active Space (dropdown quake behavior): join all Spaces so summoning
            // drops the window onto the current desktop instead of yanking back to its origin
            // Space. `wants_all_spaces` is false in window mode, so this also resets to the
            // default behavior on a live dropdown->window switch. Re-applied only on change.
            let want_spaces = config::wants_all_spaces(&self.cfg);
            if self.space_all != Some(want_spaces) {
                set_space_behavior(want_spaces);
                self.space_all = Some(want_spaces);
            }

            if config::forces_quake_geometry(&self.cfg.quake.mode) {
                // Dropdown mode is borderless: ensure the unified-titlebar tweaks are off (a live
                // window->dropdown switch would otherwise leave movable-by-background set).
                if self.titlebar_unified != Some(false) {
                    set_unified_titlebar(false);
                    self.titlebar_unified = Some(false);
                }
                // --- Dropdown mode: the quake drop-down window (behavior unchanged) ---
                // Always-on-top while hide-on-focus-loss is off (the window is meant to stay put
                // over other apps); Normal otherwise. Re-applied whenever the setting changes.
                let want_top = !self.cfg.quake.hide_on_focus_loss;
                if self.window_top != Some(want_top) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if want_top {
                        egui::WindowLevel::AlwaysOnTop
                    } else {
                        egui::WindowLevel::Normal
                    }));
                    self.window_top = Some(want_top);
                }
                // First run: apply full quake sizing once the monitor size is known.
                if !self.sized {
                    if ctx.input(|i| i.viewport().monitor_size).is_some() {
                        apply_visibility(&ctx, true, height_pct);
                        self.sized = true;
                    } else {
                        ctx.request_repaint();
                    }
                }

                // Quake toggle (from the global-hotkey thread).
                if self.toggle.swap(false, Ordering::SeqCst) {
                    self.visible = !self.visible;
                    apply_visibility(&ctx, self.visible, height_pct);
                    if self.visible {
                        self.was_focused = false;
                    }
                }

                // Menu-bar item: Show/Hide toggles the window, Quit exits.
                if let Some(tray) = &self.tray {
                    let (show, quit) = tray::poll(tray);
                    if quit {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if show {
                        self.visible = !self.visible;
                        apply_visibility(&ctx, self.visible, height_pct);
                        if self.visible {
                            self.was_focused = false;
                        }
                    }
                }
                // Hide on focus loss (after we've gained focus since showing), if enabled.
                let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
                if self.visible {
                    if focused {
                        self.was_focused = true;
                    } else if self.was_focused
                        && config::hides_on_blur(&self.cfg)
                        && !app_is_active()
                    {
                        // Only hide on a REAL app deactivation. A system panel (emoji/character
                        // viewer, Ctrl+Cmd+Space) steals winit's window focus but keeps the app
                        // active, so gating on `app_is_active()` stops it from dismissing quake.
                        self.visible = false;
                        apply_visibility(&ctx, false, height_pct);
                    }
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(120));
                }
            } else {
                // --- Window mode: a conventional macOS window ---
                // Unified titlebar: transparent title bar + FullSizeContentView so the tab strip
                // fills the top row and the traffic-light buttons float over it (the tab strip
                // reserves WINDOW_TRAFFIC_INSET on its leading edge to clear them).
                if self.titlebar_unified != Some(true) {
                    set_unified_titlebar(true);
                    self.titlebar_unified = Some(true);
                }
                // No always-on-top, no forced quake geometry, and it never auto-hides. Mark the
                // level Normal once; the red close button / last-window-closed quits via winit's
                // default CloseRequested handling.
                if self.window_top != Some(false) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                        egui::WindowLevel::Normal,
                    ));
                    self.window_top = Some(false);
                }
                self.visible = true;
                self.was_focused = true;
                // No summon hotkey is registered in window mode; drop any stray toggle flag.
                self.toggle.store(false, Ordering::SeqCst);
                // The menu-bar item still works: Show brings us to front, Quit exits.
                if let Some(tray) = &self.tray {
                    let (show, quit) = tray::poll(tray);
                    if quit {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if show {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                }
            }
            self.sync_dock();
        }

        // Settings-sync worker finished: toast the outcome; a successful pull replaced the
        // config file, so reload + re-apply it (same path as the footer Revert). The LAUNCH
        // autosync pull is gated for staleness: a slow git round-trip can land after the
        // user already saved or is mid-edit in settings - their version wins over the pull
        // (which is only a convenience), and the local file is restored (the worker's hard
        // reset already replaced it on disk). Manual Pull has no baseline: never stale.
        let sync_done = self.sync_slot.lock().unwrap().take();
        if let Some((op, res)) = sync_done {
            self.sync_busy = false;
            let launch_baseline = self.launch_pull_cfg.take();
            let now = ctx.input(|i| i.time);
            match (op, res) {
                (sync::Op::Push, Ok(())) => {
                    self.toast = Some(("Settings pushed".into(), now + 1.8));
                }
                (sync::Op::Pull, Ok(()))
                    if sync::pull_is_stale(
                        launch_baseline.as_deref(),
                        &config::config_to_toml(&self.cfg),
                    ) =>
                {
                    if let Some(p) = config::ensure_and_path() {
                        let _ = std::fs::write(p, config::config_to_toml(&self.cfg));
                    }
                    self.toast = Some(("Sync pull skipped (local changes)".into(), now + 2.6));
                }
                (sync::Op::Pull, Ok(())) => {
                    self.cfg = Config::load();
                    self.reapply_appearance(&ctx);
                    self.reregister_hotkey();
                    self.reapply_font(&ctx);
                    if self.settings_tab {
                        self.rebaseline_settings(); // hidden settings sessions rebaseline too
                    }
                    self.toast = Some(("Settings pulled".into(), now + 1.8));
                }
                (_, Err(e)) => {
                    let (mut msg, _) = ui::ellipsize(&format!("Sync failed: {e}"), 90);
                    msg = msg.replace('\n', " ");
                    self.toast = Some((msg, now + 3.0));
                }
            }
        }

        // Screenshot harness: keep repainting so eframe's built-in capture (triggered by
        // EFRAME_SCREENSHOT_TO at pass 2) fires, then it saves the PNG and exits.
        if self.screenshot.is_some() {
            ctx.request_repaint();
        }

        // Follow the OS light/dark appearance (or the manual theme when follow_system is off).
        // Re-inits colors + egui visuals only when the resolved theme actually changes.
        if self.screenshot.is_none() {
            let want = if self.cfg.appearance.follow_system {
                match ctx.input(|i| i.raw.system_theme) {
                    Some(egui::Theme::Light) => &self.cfg.appearance.theme_light,
                    _ => &self.cfg.appearance.theme_dark,
                }
            } else {
                &self.cfg.appearance.theme
            };
            if *want != self.theme_name {
                colors::set(colors::by_name(want));
                apply_theme(&ctx);
                self.theme_name = want.clone();
                ctx.request_repaint();
            }
        }

        // Auto-title unrenamed tabs: the shell's OSC 0/2 title (when dynamic_title) beats the
        // cwd basename; a user rename always wins.
        for tab in &mut self.tabs {
            if !tab.renamed {
                let term = tab.focused_term();
                if let Some(t) = auto_title(
                    self.cfg.terminal.dynamic_title,
                    term.title_osc().as_deref(),
                    term.cwd().as_deref(),
                ) {
                    tab.title = t;
                }
            }
        }

        // Session persist: snapshot open tabs (cwd/title/color) every few seconds; skip identical
        // writes so the file only changes when the session does.
        if self.cfg.session.restore && self.screenshot.is_none() {
            let now = ctx.input(|i| i.time);
            if now >= self.next_session_save {
                self.next_session_save = now + 3.0;
                // Remember window geometry in window mode (restored next launch); dropdown mode
                // uses the fixed top-edge quake geometry, so it never persists a rect.
                let window = config::is_window_mode(&self.cfg)
                    .then(|| {
                        ctx.input(|i| {
                            let vp = i.viewport();
                            vp.inner_rect.map(|r| {
                                let pos = vp.outer_rect.map_or(r.min, |o| o.min);
                                session::WindowGeom {
                                    x: pos.x,
                                    y: pos.y,
                                    w: r.width(),
                                    h: r.height(),
                                }
                            })
                        })
                    })
                    .flatten();
                // One process-table snapshot (reuses the ~1 Hz-refreshed `sys`) so each leaf can be
                // tagged claude with its own captured resume id - the split-restore per-pane source.
                let procs = procwatch::snapshot(&self.sys);
                let snap = session::SavedSession {
                    tabs: self
                        .tabs
                        .iter()
                        .map(|t| session::SavedTab {
                            title: t.renamed.then(|| t.title.clone()),
                            color: t.color.map(session::color_to_hex),
                            cwd: t.focused_term().cwd(),
                            pinned: t.pinned,
                            // Mark claude tabs and stash the session id parsed from the claude
                            // process's argv (~1 Hz scan) so restore resumes the exact session.
                            // Kept for backward-compat / single-pane restore; the pane tree below
                            // carries the authoritative per-leaf claude state.
                            claude: (t.cli == Some(procwatch::Cli::Claude)).then(|| {
                                session::ClaudeState { resume_id: t.claude_resume.clone() }
                            }),
                            // Persist the whole split layout so re-open restores every pane, each
                            // leaf marked claude (with its own resume id) when a claude runs there.
                            pane: Some(session::SavedPane::from_tree(
                                t.root(),
                                &|term: &PtyTerm| {
                                    let pid = term.shell_pid();
                                    let is_claude = pid.is_some_and(|p| {
                                        procwatch::detect(&procs, p) == Some(procwatch::Cli::Claude)
                                    });
                                    session::SavedPane::Leaf {
                                        cwd: term.cwd(),
                                        claude: is_claude.then(|| session::ClaudeState {
                                            resume_id: pid.and_then(|p| {
                                                procwatch::claude_resume_id(&procs, p)
                                            }),
                                        }),
                                    }
                                },
                            )),
                        })
                        .collect(),
                    active: self.active,
                    window,
                };
                if snap != self.last_session {
                    session::save(&snap);
                    self.last_session = snap;
                }
            }
        }

        // Notify-when-done: a long command finished. Consume the flag always (so it doesn't fire
        // late), but only post a notification when stdusk is hidden - no nagging while you watch.
        // Notify-on-activity (per-tab menu toggle): new output while the tab is unviewed (not
        // active, or the window hidden) fires ONE notification, re-armed when the tab is viewed.
        let visible = self.visible;
        let active = self.active;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if let Some(code) = tab.focused_term().take_done_notify()
                && self.cfg.terminal.notify_on_done
                && !visible
            {
                notify_done(&tab.title, code);
            }
            // Consume every pane's activity flag (|=, not any(): a short-circuit would leave
            // stale flags that mis-fire the moment the toggle is enabled later).
            let mut output = false;
            for t in tab.root().leaves() {
                output |= t.take_activity();
            }
            let viewed = i == active && visible;
            let (fire, notified) = ui::activity_notification(
                tab.notify_activity,
                viewed,
                tab.activity_notified,
                output,
            );
            if fire {
                notify(&format!("{}: new output", tab.title));
            }
            tab.activity_notified = notified;
        }

        // Shell-exit handling: apply `terminal.on_exit` (close pane / keep with overlay /
        // respawn) to any pane whose shell exited - a dead pty must never leave a frozen tab.
        self.handle_shell_exits(&ctx);

        // CLI awareness: ~1 Hz, refresh the process table once and badge each tab with any known
        // AI CLI running in it (scanned across all of the tab's panes), caching the running
        // child's name alongside (the tab menu's "Running:" row - never a synchronous scan on
        // menu open). Skipped in the screenshot harness (it sets demo badges directly).
        if self.cfg.terminal.detect_clis && self.screenshot.is_none() {
            let now = ctx.input(|i| i.time);
            if now >= self.next_cli_scan {
                self.next_cli_scan = now + 1.0;
                self.sys.refresh_processes_specifics(
                    sysinfo::ProcessesToUpdate::All,
                    true,
                    sysinfo::ProcessRefreshKind::nothing()
                        .with_cmd(sysinfo::UpdateKind::OnlyIfNotSet),
                );
                // ONE table snapshot serves every tab (detect/busy_child are pure walks on it).
                let procs = procwatch::snapshot(&self.sys);
                for tab in &mut self.tabs {
                    let pids: Vec<u32> =
                        tab.root().leaves().iter().filter_map(|t| t.shell_pid()).collect();
                    tab.cli = pids.iter().find_map(|&pid| procwatch::detect(&procs, pid));
                    // For claude tabs, capture the session id from its argv (auto-resume source).
                    tab.claude_resume = (tab.cli == Some(procwatch::Cli::Claude))
                        .then(|| {
                            pids.iter().find_map(|&pid| procwatch::claude_resume_id(&procs, pid))
                        })
                        .flatten();
                    tab.proc = pids.iter().find_map(|&pid| procwatch::busy_child(&procs, pid));
                }
                // Keep the cadence ticking even when the window is otherwise idle.
                ctx.request_repaint_after(std::time::Duration::from_millis(1100));
            }
        }

        // App keybinds. The `[hotkeys]`-remappable actions (defaults in parentheses) are
        // matched against the config below; pane/tab-index/scroll binds stay fixed.
        let mut kb_new = false; // (Cmd+T)
        let mut kb_close = false; // (Cmd+W) close focused pane, tab on its last pane
        let mut kb_find = false; // (Cmd+F)
        let mut kb_split: Option<pane::SplitDir> = None; // (Cmd+D right / Cmd+Shift+D down)
        let mut kb_switch: Option<usize> = None;
        let mut kb_pane_dir: Option<pane::Dir> = None; // Cmd+Alt+arrow: focus the neighbor pane
        let mut kb_maximize = false; // Cmd+Alt+Enter: toggle zooming the focused pane
        let mut kb_select_all = false; // (Cmd+A)
        let mut kb_clear = false; // (Cmd+K)
        let mut kb_zoom: Option<i8> = None; // (Cmd+= 1, Cmd+- -1, Cmd+0 reset)
        let mut kb_scroll_pages: Option<i32> = None; // Shift+PageUp/Down: -1 up, +1 down
        let mut kb_scroll_lines: Option<i32> = None; // Ctrl+Shift+Up/Down: one line (Tabby bind)
        let mut kb_tab_cycle: Option<i32> = None; // Ctrl+Tab next (+1) / Ctrl+Shift+Tab prev (-1)
        let mut kb_toggle_last = false; // (Cmd+O) jump to the previously active tab
        let mut kb_reopen = false; // (Cmd+Shift+T) reopen last closed tab
        let mut kb_resize: Option<(pane::SplitDir, f32)> = None; // Cmd+Ctrl+arrow: resize focused pane
        let mut kb_move_tab: Option<i32> = None; // Cmd+Shift+←/→: move the active tab
        let mut kb_scroll_edge: Option<bool> = None; // Shift+Home/End: scroll to top (true) / bottom
        let mut kb_palette = false; // (Cmd+Shift+P) toggle the command palette
        let mut kb_settings = false; // (Cmd+,) toggle the settings view
        let mut kb_broadcast = false; // (Cmd+Shift+I) broadcast input to all panes (pane-focus-all)
        // A hard modal (rename / paste confirm / close confirm / palette) owns the keyboard
        // entirely: tab switching or Cmd+W while a confirm shows would retarget/kill the tab
        // under it. The settings view suppresses them too - tab/pane mutations under a hidden
        // workspace would be invisible - EXCEPT tab switching (settings behaves like a tab;
        // see `settings_only` below).
        let text_modal = self.renaming.is_some()
            || !self.pending_pastes.is_empty()
            || self.pending_close.is_some();
        let hard_modal = text_modal || self.palette.is_some() || self.settings_open;
        ctx.input(|i| {
            // Remappable app hotkeys (`[hotkeys]`, defaults = the shipped binds): every key
            // event is matched against the configured chords (EXACT modifiers - see
            // ui::hotkey_matches; first match wins, so a user binding two actions to one
            // chord fires only the earlier action, never both). The palette / settings
            // toggles stay live over their own overlays (each is its own dismissal) and are
            // suppressed only under the text modals; every other action obeys hard_modal.
            let hk = &self.cfg.hotkeys;
            for ev in &i.events {
                let egui::Event::Key { key, pressed: true, modifiers, .. } = ev else {
                    continue;
                };
                let (key, mods) = (*key, *modifiers);
                if !text_modal && ui::hotkey_matches(&hk.palette, key, mods) {
                    kb_palette = true;
                    continue;
                }
                if !text_modal && ui::hotkey_matches(&hk.settings, key, mods) {
                    kb_settings = true;
                    continue;
                }
                if hard_modal {
                    continue;
                }
                if ui::hotkey_matches(&hk.new_tab, key, mods) {
                    kb_new = true;
                } else if ui::hotkey_matches(&hk.close, key, mods) {
                    kb_close = true;
                } else if ui::hotkey_matches(&hk.reopen, key, mods) {
                    kb_reopen = true;
                } else if ui::hotkey_matches(&hk.toggle_last_tab, key, mods) {
                    kb_toggle_last = true;
                } else if ui::hotkey_matches(&hk.find, key, mods) {
                    kb_find = true;
                } else if ui::hotkey_matches(&hk.split_right, key, mods) {
                    kb_split = Some(pane::SplitDir::Row);
                } else if ui::hotkey_matches(&hk.split_down, key, mods) {
                    kb_split = Some(pane::SplitDir::Column);
                } else if ui::hotkey_matches(&hk.broadcast, key, mods) {
                    kb_broadcast = true;
                } else if ui::hotkey_matches(&hk.select_all, key, mods) {
                    kb_select_all = true;
                } else if ui::hotkey_matches(&hk.clear, key, mods) {
                    kb_clear = true;
                } else if ui::hotkey_matches(&hk.zoom_in, key, mods) {
                    kb_zoom = Some(1);
                } else if ui::hotkey_matches(&hk.zoom_out, key, mods) {
                    kb_zoom = Some(-1);
                } else if ui::hotkey_matches(&hk.zoom_reset, key, mods) {
                    kb_zoom = Some(0);
                }
            }
            // Tab SWITCHING stays live while only the settings view is up: settings behaves
            // like a tab, so Cmd+1..9 / Ctrl+Tab must reach the terminal tabs (the switch
            // hides the view; the settings session + staged edits stay). Every other bind
            // below still obeys hard_modal - it would mutate a hidden workspace.
            let settings_only = self.settings_open && !text_modal && self.palette.is_none();
            if !hard_modal || settings_only {
                if i.modifiers.ctrl && i.key_pressed(egui::Key::Tab) {
                    kb_tab_cycle = Some(if i.modifiers.shift { -1 } else { 1 });
                }
                if i.modifiers.command {
                    use egui::Key::{Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9};
                    for (n, k) in [Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9]
                        .into_iter()
                        .enumerate()
                    {
                        if i.key_pressed(k) {
                            kb_switch = Some(n);
                        }
                    }
                }
            }
            if hard_modal {
                return;
            }
            // Fixed (non-remappable) binds from here down.
            // Ctrl+Shift+Up/Down: line-step scroll (Tabby's scroll-up/scroll-down binding).
            // The ctrl branch of key_to_bytes maps arrows to None, so nothing leaks to the pty.
            if i.modifiers.ctrl && i.modifiers.shift && !i.modifiers.command {
                if i.key_pressed(egui::Key::ArrowUp) {
                    kb_scroll_lines = Some(-1);
                }
                if i.key_pressed(egui::Key::ArrowDown) {
                    kb_scroll_lines = Some(1);
                }
            }
            if i.modifiers.shift {
                if i.key_pressed(egui::Key::PageUp) {
                    kb_scroll_pages = Some(-1);
                }
                if i.key_pressed(egui::Key::PageDown) {
                    kb_scroll_pages = Some(1);
                }
                if i.key_pressed(egui::Key::Home) {
                    kb_scroll_edge = Some(true);
                }
                if i.key_pressed(egui::Key::End) {
                    kb_scroll_edge = Some(false);
                }
                if i.modifiers.command {
                    if i.key_pressed(egui::Key::ArrowLeft) {
                        kb_move_tab = Some(-1);
                    }
                    if i.key_pressed(egui::Key::ArrowRight) {
                        kb_move_tab = Some(1);
                    }
                }
            }
            if i.modifiers.command {
                use egui::Key::{ArrowDown, ArrowLeft, ArrowRight, ArrowUp, Enter};
                // Cmd+Alt: pane navigation / maximize (kept separate from the terminal's own
                // Cmd/Alt+arrow line/word motion, which key_to_bytes reserves against Cmd+Alt).
                if i.modifiers.alt {
                    if i.key_pressed(ArrowLeft) {
                        kb_pane_dir = Some(pane::Dir::Left);
                    }
                    if i.key_pressed(ArrowRight) {
                        kb_pane_dir = Some(pane::Dir::Right);
                    }
                    if i.key_pressed(ArrowUp) {
                        kb_pane_dir = Some(pane::Dir::Up);
                    }
                    if i.key_pressed(ArrowDown) {
                        kb_pane_dir = Some(pane::Dir::Down);
                    }
                    if i.key_pressed(Enter) {
                        kb_maximize = true;
                    }
                }
                // Cmd+Ctrl: resize the focused pane (Right/Down grow, Left/Up shrink).
                if i.modifiers.ctrl {
                    const STEP: f32 = 0.05;
                    if i.key_pressed(ArrowRight) {
                        kb_resize = Some((pane::SplitDir::Row, STEP));
                    }
                    if i.key_pressed(ArrowLeft) {
                        kb_resize = Some((pane::SplitDir::Row, -STEP));
                    }
                    if i.key_pressed(ArrowDown) {
                        kb_resize = Some((pane::SplitDir::Column, STEP));
                    }
                    if i.key_pressed(ArrowUp) {
                        kb_resize = Some((pane::SplitDir::Column, -STEP));
                    }
                }
            }
        });

        // Rounded window background - the OS window is transparent, so painting a rounded
        // rect leaves the corner triangles clear and the window reads as rounded. Panels
        // below are transparent so this shows through.
        ui.painter().rect_filled(ui.max_rect(), 10.0, tint(colors::bg(), opacity));

        // Tab bar. Collect clicks + menu actions; apply after the panel to avoid borrow clashes.
        let (clicked, mut action) = self.tab_bar(ui);

        // Apply tab-bar clicks + keybinds + menu action (all structural mutations here).
        if let Some(i) = clicked {
            self.active = i;
        }
        if let Some(n) = kb_switch
            && n < self.tabs.len()
        {
            self.active = n;
        }
        if let Some(d) = kb_tab_cycle {
            let len = self.tabs.len() as i32;
            self.active = (self.active as i32 + d).rem_euclid(len) as usize;
        }
        if kb_toggle_last {
            self.active = ui::toggle_last_target(self.prev_active, self.tabs.len());
        }
        // A switch to a terminal tab hides the settings VIEW like any tab switch would; the
        // settings TAB (and the staged edits behind it) stays until explicitly closed.
        if self.settings_open
            && (clicked.is_some()
                || kb_tab_cycle.is_some()
                || kb_switch.is_some_and(|n| n < self.tabs.len()))
        {
            self.settings_open = false;
        }
        if let Some(d) = kb_move_tab {
            self.move_tab(self.active, d);
        }
        if kb_reopen {
            self.reopen_tab(&ctx);
        }
        if kb_new {
            action = Some(TabAction::New);
        }
        if kb_maximize {
            let tab = &mut self.tabs[self.active];
            tab.maximized = !tab.maximized;
        }
        if let Some((dir, delta)) = kb_resize {
            let tab = &mut self.tabs[self.active];
            let path = tab.focused.clone();
            tab.root_mut().resize_focused(&path, dir, delta);
        }
        if let Some(dir) = kb_split {
            let cwd = self.tabs[self.active].focused_term().cwd();
            let new = PtyTerm::spawn(COLS, ROWS, ctx.clone(), &spawn_opts(&self.cfg, cwd));
            let tab = &mut self.tabs[self.active];
            let root = tab.root.take().expect("root");
            let (root, focus) = root.split(&tab.focused, dir, new, false);
            tab.root = Some(root);
            if let Some(f) = focus {
                tab.focused = f;
            }
        }
        if kb_close {
            // Cmd+W closes the focused pane; the tab only closes on its last pane.
            let tab = &mut self.tabs[self.active];
            if tab.root().leaf_count() > 1 {
                let root = tab.root.take().expect("root");
                let (root, focus) = root.close(&tab.focused);
                tab.root = root;
                if let Some(f) = focus {
                    tab.focused = f;
                }
            } else {
                action = Some(TabAction::Close(self.active));
            }
        }
        if kb_find && !self.settings_open {
            match self.search.take() {
                Some(_) => self.tabs[self.active].focused_term().clear_selection(),
                None => self.search = Some(Search::new()),
            }
        }
        if kb_palette && self.palette.take().is_none() {
            self.palette = Some(palette::PaletteState::new());
        }
        if kb_settings {
            self.toggle_settings();
        }
        let now = ctx.input(|i| i.time);
        // Broadcast input toggle (Cmd+Shift+I / palette): the current tab only.
        if kb_broadcast {
            self.toggle_broadcast(now);
        }
        // Font zoom (harmless anytime). Reset (0), in (1), out (-1); clamped.
        if let Some(z) = kb_zoom {
            self.zoom = match z {
                0 => 1.0,
                1 => (self.zoom * 1.1).min(3.0),
                _ => (self.zoom / 1.1).max(0.5),
            };
            self.toast = Some((format!("Zoom {:.0}%", self.zoom * 100.0), now + 1.4));
        }

        // A text surface (focused find bar, rename dialog, command palette, settings view, or a
        // confirm modal) owns the keyboard: don't forward keys to the pty. MUST be sampled
        // BEFORE the modals run this frame - else the key that closes a modal (Enter to commit
        // a rename) would leak to the shell once the modal clears its own state. The find bar
        // captures only while its field has focus, so an open bar doesn't swallow shell input.
        let input_captured = ui::pty_input_captured(
            self.search.as_ref().is_some_and(|s| s.field_focused),
            self.renaming.is_some(),
            self.palette.is_some(),
            self.settings_open,
            !self.pending_pastes.is_empty(),
            self.pending_close.is_some(),
        );

        // Terminal input keybinds - suppressed while anything else owns the keyboard.
        if !input_captured {
            if kb_select_all {
                self.tabs[self.active].focused_term().select_all();
                self.toast = Some(("Selected all".into(), now + 1.4));
            }
            if kb_clear {
                // Ctrl-L redraws the prompt; the wipe drops the whole scrollback with it
                // (Tabby's `clear` does both). Wipe first - see PtyTerm::clear_all. A refused
                // wipe (alt screen) sends nothing either.
                let t = self.tabs[self.active].focused_term_mut();
                if t.clear_all() {
                    t.send(b"\x0c");
                }
            }
            if let Some(dir) = kb_scroll_pages {
                let t = self.tabs[self.active].focused_term();
                let page = t.rows().saturating_sub(1) as i32;
                t.scroll(-dir * page); // PageUp (-1) scrolls up into history
            }
            if let Some(dir) = kb_scroll_lines {
                self.tabs[self.active].focused_term().scroll(-dir); // up (-1) scrolls into history
            }
            if let Some(to_top) = kb_scroll_edge {
                let t = self.tabs[self.active].focused_term();
                if to_top {
                    let (_, history) = t.scroll_state();
                    t.scroll_to_offset(history);
                } else {
                    t.scroll_to_bottom();
                }
            }
        }
        self.apply_tab_action(action, &ctx);

        self.rename_window(&ctx);
        self.paste_confirm_window(&ctx);
        self.close_confirm_window(&ctx);
        self.palette_window(&ctx);

        // OSC 52: a shell "copy" request (from the focused pane) -> the system clipboard.
        if let Some(text) =
            self.tabs.get(self.active).and_then(|t| t.focused_term().take_clipboard())
        {
            ctx.copy_text(text);
        }

        // While settings are open, the settings view takes over the central area (the tab bar
        // stays); the terminal workspace and find bar come back when it closes.
        let out = if self.settings_open {
            self.settings_view(ui, &ctx);
            workspace::CentralOut { copied: false, bell_rang: false }
        } else {
            self.find_panel(ui);
            self.central_panel(ui, &ctx, input_captured, kb_pane_dir, now)
        };

        // Bell: a brief translucent flash over the whole window, fading out.
        if out.bell_rang {
            self.flash = now + 0.18;
        }
        if self.flash > now {
            let a = toast_alpha(self.flash - now, 0.18);
            let f = colors::fg();
            ui.painter().rect_filled(
                ui.max_rect(),
                10.0,
                egui::Color32::from_rgba_unmultiplied(f.r(), f.g(), f.b(), (55.0 * a) as u8),
            );
            ctx.request_repaint();
        }

        // Transient "Copied" toast at the bottom-center, fading out.
        if out.copied {
            self.toast = Some(("Copied".into(), now + 1.4));
        }
        if let Some((msg, until)) = self.toast.clone() {
            if now >= until {
                self.toast = None;
            } else {
                draw_toast(ui, &msg, toast_alpha(until - now, 0.35));
                ctx.request_repaint();
            }
        }

        // The active tab changed this frame: remember where we came from (Tabby keeps the same
        // index-based `lastTabIndex`; a stale index after closes clamps to tab 1 at use).
        if self.active != active_at_frame_start {
            self.prev_active = active_at_frame_start;
        }
        // Broadcast mode is bound to the CURRENT tab: switching away (any path - click, keybind,
        // palette, close) exits it, so keys can never fan out in a tab you're not looking at.
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if i != self.active {
                tab.broadcast = false;
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // `--version` / `-V`: print and exit before touching the display (used by the brew test and
    // handy for scripts). No window is created.
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("stdusk {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let mut cfg = Config::load();

    // `--screenshot PATH`: populate demo tabs, render, save the PNG, and exit. Uses eframe's
    // built-in glow-backend capture via EFRAME_SCREENSHOT_TO. `--screenshot-settings PATH`
    // does the same but opens the settings view on the Color scheme section, with the theme
    // pinned (deterministic regardless of the user's config).
    let settings_shot = args
        .iter()
        .position(|a| a == "--screenshot-settings")
        .and_then(|i| args.get(i + 1).cloned());
    let screenshot = settings_shot.clone().or_else(|| {
        args.iter().position(|a| a == "--screenshot").and_then(|i| args.get(i + 1).cloned())
    });
    if settings_shot.is_some() {
        cfg.appearance.follow_system = false;
        cfg.appearance.theme = "one-half-dark".into();
    }

    // Single-instance guard: only one stdusk runs. A second launch connects to the primary over
    // a Unix socket, tells it to surface + open a new tab, and exits(0) without a window of its
    // own. Skipped under the screenshot harness (it may run alongside a real instance).
    #[cfg(unix)]
    let sock_path = (screenshot.is_none()).then(instance::socket_path).flatten();
    #[cfg(unix)]
    let instance_listener = match sock_path.as_deref() {
        Some(path) => match instance::acquire(path) {
            instance::Acquired::Secondary => return Ok(()),
            instance::Acquired::Primary(l) => Some(l),
        },
        None => None,
    };
    #[cfg(not(unix))]
    let instance_listener: Option<instance::Listener> = None;

    colors::init(colors::by_name(&cfg.appearance.theme));
    if let Some(path) = &screenshot {
        // SAFE: single-threaded, set before any threads spawn (edition-2024 set_var is unsafe).
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("EFRAME_SCREENSHOT_TO", path);
        }
    }
    let size = if settings_shot.is_some() {
        [1400.0, 760.0] // tall enough for the sidebar + scheme browser
    } else if screenshot.is_some() {
        [1400.0, 420.0]
    } else {
        [1200.0, 500.0]
    };

    // Window mode is a conventional decorated, resizable macOS window; dropdown mode is the
    // borderless top-edge quake window pinned to [0,0].
    let window_mode = config::is_window_mode(&cfg);
    let mut viewport = egui::ViewportBuilder::default()
        .with_decorations(window_mode)
        .with_transparent(true)
        .with_inner_size(size);
    if window_mode {
        viewport = viewport.with_resizable(true);
        // Restore the remembered geometry (window mode only); else the OS places the window.
        if screenshot.is_none()
            && let Some(g) = session::load().window
        {
            viewport = viewport.with_inner_size([g.w, g.h]).with_position([g.x, g.y]);
        }
    } else {
        viewport = viewport.with_position([0.0, 0.0]);
    }
    // App/window icon (the dusk-sun prompt). macOS uses the .app bundle icon for the Dock, so
    // this mainly affects other platforms + the window itself; harmless where ignored.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/stdusk-icon.png"))
    {
        viewport = viewport.with_icon(Arc::new(icon));
    }

    let mut options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow, // __screenshot capture requires the glow backend
        viewport,
        ..Default::default()
    };
    // Dock/menu-bar presence on macOS:
    //   hide_from_dock && !dock_when_visible (default): launch as a pure accessory app - no Dock
    //     icon and (per Apple) no menu bar of its own; it just drops from the top on the hotkey.
    //   hide_from_dock && dock_when_visible: launch regular, then flip to accessory whenever the
    //     window is hidden (see `set_dock_icon`) - Dock icon + real menu bar only while visible.
    //   !hide_from_dock: a normal Dock app.
    //   window mode: always Regular (a normal Dock app), ignoring the Dock toggles above.
    if !window_mode && cfg.quake.hide_from_dock && !cfg.quake.dock_when_visible {
        options.event_loop_builder = Some(Box::new(|builder| {
            #[cfg(target_os = "macos")]
            {
                use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
                builder.with_activation_policy(ActivationPolicy::Accessory);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = builder;
        }));
    }
    let settings_shot = settings_shot.is_some();
    let result = eframe::run_native(
        "stdusk",
        options,
        Box::new(move |cc| {
            Ok(Box::new(Stdusk::new(cc, cfg, screenshot, settings_shot, instance_listener)))
        }),
    );
    // Clean up our single-instance socket on graceful exit (a crash leaves it stale, which the
    // next launch detects and takes over).
    #[cfg(unix)]
    if let Some(p) = sock_path {
        let _ = std::fs::remove_file(p);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // Font resolution hits the real system source - gate on macOS where Menlo always exists.
    #[test]
    #[cfg(target_os = "macos")]
    fn resolve_font_finds_menlo_regular_face() {
        let f = resolve_font("Menlo").expect("Menlo ships with macOS");
        assert!(!f.bytes.is_empty());
        // The face must be the upright Regular, not Italic/Bold (select_best_match regression:
        // core-text matching handed back Menlo-Italic). Checked by name - core-text loads
        // report broken `properties()` (see face_name_score).
        let font = font_kit::font::Font::from_bytes(std::sync::Arc::new(f.bytes), f.index)
            .expect("resolved bytes load");
        assert_eq!(font.full_name(), "Menlo Regular");
    }

    #[test]
    fn face_name_score_prefers_upright_regular() {
        assert!(face_name_score("Menlo Regular") < face_name_score("Menlo Bold"));
        assert!(face_name_score("Menlo Bold") < face_name_score("Menlo Italic"));
        assert!(face_name_score("Menlo Italic") < face_name_score("Menlo Bold Italic"));
        assert_eq!(face_name_score("JetBrainsMono Nerd Font"), 0);
    }

    #[test]
    fn bold_face_score_requires_upright_bold() {
        // (face name) -> qualifies? The plain Bold must win; slants never qualify.
        assert_eq!(bold_face_name_score("Menlo Bold"), Some(0));
        assert_eq!(bold_face_name_score("Menlo Regular"), None); // not bold
        assert_eq!(bold_face_name_score("Menlo Italic"), None);
        assert_eq!(bold_face_name_score("Menlo Bold Italic"), None); // slant disqualifies
        assert_eq!(bold_face_name_score("Menlo Bold Oblique"), None);
        assert_eq!(bold_face_name_score("Fira Code Black"), None); // heavy != bold
        // Bold variants qualify but rank behind the plain Bold.
        let plain = bold_face_name_score("JetBrainsMono NF Bold").unwrap();
        for variant in
            ["JetBrainsMono NF SemiBold", "JetBrainsMono NF ExtraBold", "Iosevka Bold Condensed"]
        {
            let s = bold_face_name_score(variant).unwrap_or_else(|| panic!("{variant}"));
            assert!(s > plain, "{variant} must rank behind the plain Bold");
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn resolve_bold_font_finds_menlo_bold_face() {
        let f = resolve_bold_font("Menlo").expect("Menlo ships a Bold face");
        let font = font_kit::font::Font::from_bytes(std::sync::Arc::new(f.bytes), f.index)
            .expect("resolved bytes load");
        assert_eq!(font.full_name(), "Menlo Bold");
        assert!(resolve_bold_font("NoSuchFontXyz").is_none());
        assert!(resolve_bold_font("").is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn resolve_font_rejects_unknown_and_empty() {
        assert!(resolve_font("NoSuchFontXyz").is_none());
        assert!(resolve_font("").is_none());
        assert!(resolve_font("   ").is_none());
    }

    #[test]
    fn build_fonts_without_user_font_keeps_fallbacks() {
        let fonts = build_fonts(None, None);
        assert!(!fonts.font_data.contains_key("user-font"));
        let mono = &fonts.families[&egui::FontFamily::Monospace];
        assert!(mono.contains(&"noto-emoji".to_owned()));
        let prop = &fonts.families[&egui::FontFamily::Proportional];
        assert_eq!(prop[1], "phosphor");
        // No bold face resolved -> the bold family must NOT exist (a FontId naming it panics).
        assert!(!fonts.families.contains_key(&egui::FontFamily::Name(BOLD_FONT_FAMILY.into())));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn build_fonts_puts_user_font_first_in_monospace_only() {
        let fonts = build_fonts(resolve_font("Menlo"), None);
        let mono = &fonts.families[&egui::FontFamily::Monospace];
        assert_eq!(mono[0], "user-font"); // top priority for the terminal grid
        assert!(mono.contains(&"noto-emoji".to_owned())); // fallbacks survive behind it
        let prop = &fonts.families[&egui::FontFamily::Proportional];
        assert!(!prop.contains(&"user-font".to_owned())); // chrome text untouched
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn build_fonts_registers_bold_family_with_fallbacks_behind() {
        let fonts = build_fonts(resolve_font("Menlo"), resolve_bold_font("Menlo"));
        let bold = &fonts.families[&egui::FontFamily::Name(BOLD_FONT_FAMILY.into())];
        assert_eq!(bold[0], "user-font-bold"); // the bold face leads
        assert!(bold.contains(&"user-font".to_owned())); // regular behind it
        assert!(bold.contains(&"noto-emoji".to_owned())); // fallbacks survive
        // The regular Monospace stack is untouched by the bold registration.
        assert_eq!(fonts.families[&egui::FontFamily::Monospace][0], "user-font");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn installed_families_lists_menlo_sorted() {
        let all = installed_families();
        assert!(all.iter().any(|n| n == "Menlo"));
        assert!(all.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn cmd_v_image_paste_only_swallows_command_v_over_an_image() {
        // The intended case: Cmd held, "v", image-only clipboard -> swallow + inject.
        assert!(decide_cmd_v_image_paste(true, true, true));
        // No image on the clipboard: let egui's normal (text) Cmd+V through.
        assert!(!decide_cmd_v_image_paste(true, true, false));
        // Not the V key, or Command not held: never our concern.
        assert!(!decide_cmd_v_image_paste(true, false, true));
        assert!(!decide_cmd_v_image_paste(false, true, true));
        assert!(!decide_cmd_v_image_paste(false, false, false));
    }
}

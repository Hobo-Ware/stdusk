# stdusk - Tabby parity gap list

A comprehensive audit of Tabby's (Electron, on `master`) user-facing feature surface vs stdusk
(the Rust rewrite), scanned from Tabby source: `tabby-core`, `tabby-terminal`, `tabby-local`,
`tabby-settings`, `tabby-linkifier`, `tabby-community-color-schemes` (config defaults, `hotkeys.ts`,
context menus, settings tabs). This is the living to-do; PLAN.md is the architecture, LEDGER.md the
build state.

Legend: ✅ done · 🟡 partial · ⬜ todo (want) · ⛔ drop (out of scope, per PLAN §1/§9)

## Already shipped (M0-M10)
Quake window (global hotkey, hide-on-blur, monitor sizing, height %); GUI tab bar; new/close/
switch(1-9)/reorder/rename(menu+dbl-click)/per-tab color; **progress on tabs** (%-regex + OSC 9;4);
colored renderer + cursor styles (block/underline/beam); 3 themes + opacity + font-size + config.toml;
resize; scrollback; paste + bracketed paste; OSC 52 clipboard; mouse selection + Cmd+C copy +
word/line select; macOS natural-editing keys (home/end/word/line); scrollback search (regex/case/
whole-word); split panes (4 dirs, drag-resize, click-focus, right-click menu, mini-layout glyph);
shell integration OSC 133 (fail signal); visual bell; cwd tracking + new-tab-in-cwd + cwd auto-title;
copy-current-path; ambient AI-CLI awareness badges.

## Also shipped (0.1.x-0.2.x wave)
Tabby-exact paste/copy/input suite (trim/replace-newlines/multiline-guard/intelligent Ctrl-C/
alt-is-meta/word-separators/copy-on-select/middle-click paste); font zoom + select-all + Cmd+K;
session restore; 191 community XRDB schemes + follow-OS light/dark; keyboard pane nav/resize/
maximize; tab power ops (cycle/move/duplicate/reopen/restart/close-others); notify-when-done;
command palette (20 commands + profile launchers); named profiles; drag-reorder tabs; symbol
ligatures; Tabby-grade settings GUI (Cmd+,); settings sync via git; menu-bar icon + dock modes;
`quake.unfocused_opacity` (beyond Tabby).

---

## Tabs
| Feature | State | Notes |
|---|---|---|
| New / close / switch (1-9) / reorder / rename / color | ✅ | reorder+rename via menu/dbl-click |
| `next-tab` / `previous-tab` cycle hotkey | ✅ | Ctrl+Tab / Ctrl+Shift+Tab (wraps) |
| `move-tab-left/right` hotkey | ✅ | Cmd+Shift+←/→ |
| Tab jump 10-20 (`tab-10`..`tab-20`) | ⛔ | skipped on purpose: Tabby ships `tab-10`..`tab-20` UNBOUND on every platform (configDefaults yaml), and Cmd+0 is zoom-reset here - nothing to mirror |
| `duplicate-tab` (clone incl. cwd) | ✅ | context-menu "Duplicate" |
| `reopen-tab` (reopen last closed) | ✅ | Cmd+Shift+T; closed-cwd stack (cap 20) |
| `toggle-last-tab` (alt-tab between two) | ✅ | Cmd+O + palette (Tabby ships the hotkey unbound); index-based `prev_active` like Tabby's `lastTabIndex` |
| `pin-tab` (pin, guard close) | ✅ | context-menu Pin/Unpin; Tabby-exact group placement + no reorder across the boundary; close asks confirm (Tabby hard-refuses); pin glyph; session-persisted |
| `restart-tab` (respawn shell) | ✅ | context-menu Restart (same cwd, keeps title/color) |
| Close other / to-the-right / to-the-left | ✅ | context-menu items (feed the reopen stack) |
| `explode-tab` (panes -> tabs) / `combine-tabs` (tabs -> split) | ⬜ | power-user, low priority |
| Notify-when-done | ✅ | `terminal.notify_on_done`; osascript notification when a >10s command finishes while hidden. Notify-on-activity ✅ (per-tab menu toggle, one shot per unviewed stretch, re-armed on view) |
| Current-process display in menu | ✅ | disabled "Running: <name>" first row, fed by the ~1 Hz procwatch cache (needs `detect_clis` on) |
| Drag-reorder tabs | ✅ | midpoint-crossing swaps, mixed widths; between-windows N/A (single window) |
| Warn when closing a tab with a running process | ✅ | `terminal.warn_on_close_running` (default on) + confirm modal |
| `toggle-fullscreen` | ⬜ | |
| Save-as-profile / save-layout-as-profile | ⛔ | depends on profiles (deferred) |

## Panes / splits
| Feature | State | Notes |
|---|---|---|
| Split right/bottom/left/top + drag-resize + click-focus | ✅ | |
| Keyboard pane nav (directional) | ✅ | Cmd+Alt+arrows (`pane::neighbor`); prev/next + 1-9 still ⬜ |
| Keyboard pane resize (`pane-increase/decrease-*`, step) | ✅ | Cmd+Ctrl+arrows (Right/Down grow, Left/Up shrink) |
| `pane-maximize` / zoom | ✅ | Cmd+Alt+Enter toggles `tab.maximized` |
| Broadcast input (`pane-focus-all`) | ✅ | Cmd+Shift+I (Tabby's default bind) + palette "Broadcast Input"; keystrokes/paste to every pane, accent border per pane, exits on toggle/tab switch. `focus-all-tabs` (multi-TAB broadcast) skipped on purpose - a quake terminal rarely wants cross-tab fan-out; revisit on demand |
| `rearrange-panes` (labelled move mode) | ⬜ | |
| Aggregated tab progress/title across panes | ✅ | progress = max-fraction across panes (error wins); cmd Fail on any pane shows the red mark; CLI badge aggregates via pids. Title stays the FOCUSED pane's on purpose (the cwd/OSC you're working in) |
| Drag tab into a split (drop zones) | ⬜ | |

## Rendering
| Feature | State | Notes |
|---|---|---|
| Truecolor/256/16 + cursor styles | ✅ | |
| Cursor blink | ✅ | `cursor_blink` (default on); focused pane only, xterm cadence |
| Font weight / bold weight | ✅ | real bold face (1.0.1): `build_fonts` registers a second `term-bold` family when the user's font resolves an upright Bold sibling (name-scored, core-text lies); BOLD cells switch family in `render_grid`, metrics stay regular-derived. Bundled default has no bold sibling - regular fallback there; `bold_bright` independent |
| Font family + fallback font + line padding | ✅ | `appearance.font` via font-kit (Nerd Fonts work) + `line_padding`; bundled fallbacks kept behind |
| Ligatures | 🟡 | `ligatures` (default off): symbol substitution (-> => != >= <= ...); true OpenType shaping still ⬜ (egui limit) |
| Sixel / inline images | ⛔→FUTURE | alacritty grid has no image model |
| Bold-in-bright-colors | ✅ | `bold_bright` (default on) |
| Minimum contrast ratio (auto-contrast) | ✅ | `minimum_contrast` (default 1=off; WCAG nudge, tested) |
| Palette generate / harmonious | ⬜ | niche |
| Light color scheme + follow-OS light/dark | ✅ | `appearance.follow_system` + `theme_light`/`theme_dark`; adaptive chrome; `one-half-light` added |
| Background: image / vibrancy / blur | ⬜ | opacity only |
| Configurable scrollback lines (25k default) | ✅ | `scrollback_lines` (default 25000) |
| Wide-char / Unicode 11 widths | ✅ | WIDE_CHAR/spacer flags honored; CJK/emoji span two cells (real-pty tested) |
| Terminal query reporting (OSC 4/10/11/12 colors, DA1/DA2, DSR, DECRQM, CSI 18t) + COLORFGBG | ✅ | 1.0.2: `Event::ColorRequest`/`PtyWrite` answered with the LIVE theme (app-set OSC 4 overrides win); `COLORFGBG` set at spawn. How gemini/copilot detect light vs dark. Kitty keyboard stays deliberately silent (we don't encode CSI-u) |
| Hidden cursor (DECTCEM `?25l`) honored | ✅ | 1.0.2: snapshot drops the cursor while an app hides it (was always painted) |

## Input / copy-paste
| Feature | State | Notes |
|---|---|---|
| Copy / paste / bracketed paste / OSC 52 | ✅ | |
| Intelligent Ctrl-C (copy if selection else SIGINT) | ✅ | Tabby-parity: copy+clear when selected, else SIGINT |
| Natural editing keys (home/end/word/line) | ✅ | |
| `select-all` (Cmd-A) | ✅ | selects whole buffer; Cmd-C copies |
| `clear` (Cmd-K) | ✅ | wipes viewport + scrollback (`clear_all`) then Ctrl-L; palette "Clear Scrollback" drops history only |
| Font zoom (`zoom-in/out/reset`) | ✅ | Cmd +/-/0 (runtime `zoom` multiplier) |
| Copy-on-select | ✅ | `copy_on_select`; on selection finish, skips whitespace-only |
| Middle-click paste | ✅ | `paste_on_middle_click` (default on, arboard clipboard read); image-aware (see below) |
| Image paste (screenshot -> Claude Code) | ✅* | Ctrl+V forwards `^V` (0x16) so an app that reads the clipboard on ^V (Claude Code) ingests the image - works today. Right-click "paste" / middle-click also send `^V` when the clipboard holds an image and no text. *Cmd+V for an image-only clipboard is NOT interceptable: egui-winit 0.35 (`lib.rs` ~1007-1015) folds Cmd+V into `Event::Paste`, reads clipboard TEXT only, emits nothing when empty, and swallows the key - no egui event fires. Fixing Cmd+V needs an egui-winit patch or a raw-winit key hook |
| Copy-as-HTML (rich clipboard) | ⬜ | niche |
| Right-click mode (menu vs paste vs clipboard) | ✅ | `terminal.right_click` (default menu); Tabby-exact 250ms tap-vs-hold rule, clipboard = copy-selection-else-paste |
| Multiline-paste warning / paste protection | ✅ | `warn_on_multiline_paste`; modal w/ preview, suppressed on alt-screen (Tabby-exact) |
| Paste transforms (trim ws, newlines->spaces) | ✅ | `trim_whitespace_on_paste` (default on) + `replace_newlines_on_paste`; Tabby-exact rules |
| `altIsMeta` + configurable word separators | ✅ | `alt_is_meta`, `word_separators` |
| Focus follows mouse | ✅ | `terminal.focus_follows_mouse` (default off, Tabby default); mousemove over a pane focuses it |
| Alt+scroll -> arrow keys | ✅ | SS3 up/down per wheel line; Tabby's gate is Alt alone (no alt-screen check) |

## Scrolling
| Feature | State | Notes |
|---|---|---|
| Wheel scroll + scrollbar | ✅ | |
| Scroll hotkeys | ✅ | Shift+PageUp/Down (page) + Shift+Home/End (top/bottom) + Ctrl+Shift+Up/Down (line, Tabby's default bind) |

## Links
| Feature | State | Notes |
|---|---|---|
| Clickable links (URL + IP + file paths) | ✅ | `clickable_links` + `link_modifier` (default "none" = hover, Tabby-style); opens via `open`, cwd-relative + `~`; bare IPv4 literals open as http:// |

## Command palette / discoverability
| Feature | State | Notes |
|---|---|---|
| Command palette (`command-selector`) | ✅ | Cmd+Shift+P; fuzzy-scored, 20 commands + profile launchers |
| Profile selector | ⛔ | depends on profiles |

## Profiles & shell
| Feature | State | Notes |
|---|---|---|
| cwd tracking + new-tab-in-cwd | ✅ | |
| Named profiles (shell/args/cwd/env/color) | ✅ | `[[profiles]]`; tab menu + '+' right-click + palette + Settings > Profiles editor (0.5.0); icon ⬜ |
| Per-profile env editor / command-line editor | ✅ | Settings > Profiles: add/duplicate/delete/launch, inline editor (name/shell/args w/ quoted parsing/cwd/env rows/color swatches); Save round-trips through config.toml |
| `switch-profile` in active pane | ⬜ | deliberately deferred (V1 §P2): stdusk profiles launch new tabs; in-place respawn-with-profile revisited on demand |
| Shell auto-detection list (zsh/bash/fish/pwsh/...) | 🟡 | uses `$SHELL`; no picker. (Tabby fork ships no concrete ShellProvider either) |
| Run-as-administrator / UAC | ⛔ | security smell (PLAN §9) |

## Themes / color schemes
| Feature | State | Notes |
|---|---|---|
| Built-in themes | ✅ | 4 built-ins + 191 embedded community XRDB schemes |
| Import color schemes (XRDB) | ✅ | 191 community schemes embedded + user files in ~/.config/stdusk/schemes/; iTerm/base16 ⬜ |
| Custom color schemes in config | 🟡 | theme by name only |

## Quake / window / docking
| Feature | State | Notes |
|---|---|---|
| Top-edge drop, global hotkey, hide-on-blur, height % | ✅ | |
| Quake shows on the active Space/desktop (Tabby's "on active space") | ✅ | `quake.follow_active_space` (default on, dropdown mode); NSWindow `collectionBehavior` = CanJoinAllSpaces \| FullScreenAuxiliary so summoning drops on the current desktop instead of yanking back to Desktop 1 |
| Window mode (run as a normal resizable macOS window) | ✅ | `quake.mode = "window"` (default `"dropdown"`): decorated + resizable, always Regular activation, no global hotkey, never auto-hides, close = quit; geometry remembered in `session.toml`. Beyond Tabby |
| Single instance (second launch opens a tab, no new window) | ✅ | Unix-socket lock (`instance.rs`); secondary sends `new-tab` + exits(0). Dock-icon reopen not wired (no clean winit hook) |
| Accessory app (no Dock icon / tray, quake default) | ✅ | `quake.hide_from_dock`; ActivationPolicy::Accessory (dropdown mode) |
| Dock icon + menu bar only while visible (opt-in) | ✅ | `quake.dock_when_visible`; runtime activation-policy flip |
| Menu-bar (tray) icon + Show/Hide/Quit menu | ✅ | `quake.menu_bar_icon`; tray-icon crate (Tabby `hideTray` parity) |
| Light/dark/tinted-adaptive app icon (macOS 26) | ⬜ | needs Icon Composer `.icon`; static .icns now |
| Dock edge (top/bottom/left/right) | ⬜ | top only |
| Display/screen selection (`dockScreen`) | ⬜ | current monitor only |
| Always-on-top, dock fill/space tuning | ⬜ | |
| Drop animation | ⬜ | deferred polish |
| Tab-bar location (top/bottom/left/right) | ⬜ | top only |
| Flex/fixed tab width | ⬜ | |
| Native/thin/full window frame | 🟡 | borderless in dropdown mode; native decorated frame in `quake.mode = "window"` |
| Show tabs in fullscreen, hide index/close/options-button | ⬜ | cosmetic toggles |

## Settings GUI (M11)
| Feature | State | Notes |
|---|---|---|
| egui settings panel | ✅ | gear or Cmd+, opens the full view: Appearance/Color scheme/Terminal/Profiles/Hotkeys/Quake/Session/About sidebar, scheme browser w/ search + live preview card, unsaved-changes guard, live-apply + Save-to-toml. Quake hotkey editable (live re-register) |
| Hotkey remapping (`hotkeys.ts` rebinding) | ✅ | `[hotkeys]` table (15 app actions, per-field defaults, empty = unbound) + Settings > Hotkeys editable rows w/ parse validation; exact-modifier matcher (`ui::hotkey_matches`, tested). Live key-capture widget ⬜; pane-nav/tab-index/scroll binds stay fixed |
| Settings sync via git (config + custom schemes) | ✅ | `[sync] repo` push/pull with the user's own git creds (`sync.rs`); replaces the dropped SaaS sync. `[sync] auto` (0.5.0): pull on launch + push on save (leaner than Tabby's 60s loop) |
| Raw config editor / show-defaults | 🟡 | "Open config file/folder" link rows in settings About section |

## Session / lifecycle
| Feature | State | Notes |
|---|---|---|
| Session restore (`recoverTabs`, reopen tabs+cwd on launch) | ✅ | `[session] restore`; cwd/title/color, saved every 3s; also window geometry in `quake.mode = "window"` |
| Behavior on session end (keep/close/restart) | ✅ | `terminal.on_exit` (0.3.0): close (default) / keep with overlay / restart + crash-loop guard |
| Dynamic title from shell (OSC 0/2) + disable toggle | ✅ | `dynamic_title` (0.3.0); 1.0.2 moved parsing to the Term's Title events, adding the xterm title STACK (`CSI 22/23 t`) - a popped title restores instead of sticking |
| Save/load terminal output & state (debug) | ⛔ | niche debug tooling |

## Distribution / packaging
| Feature | State | Notes |
|---|---|---|
| Homebrew cask -> /Applications + `stdusk` CLI, Spotlight-findable | ✅ | `brew install hobo-ware/tap/stdusk` |
| Universal (arm64+x86_64) `.app` built + released on tag | ✅ | native-release.yml |
| Developer ID signing + notarization | 🟡 | CI scaffold ready (optional sign/notarize/staple step; cask drops the quarantine postflight when signed) - blocked only on the Apple Developer account secrets (packaging/README.md) |

## Dropped (out of scope - PLAN §1/§9)
⛔ SSH client + profiles/SFTP · ⛔ Serial · ⛔ Telnet · ⛔ Plugin system + marketplace ·
⛔ Web/SaaS config sync · ⛔ Vault (encrypted secrets) · ⛔ zmodem · ⛔ auto-sudo-password / UAC ·
⛔ analytics · ⛔ auto-update (brew handles updates) · ⛔ custom CSS · ⛔ welcome tab · ⛔ tray icon ·
⛔ login scripts / expect-send · ⛔ config-sync vault parts.

---

## Next milestones
M11-M17 from the original list all shipped (0.1.x-0.2.x). The remaining gaps, ranked and
batched into releases (0.3.0 → 1.0.0), live in **[V1.md](./V1.md)** - that file supersedes
this section as the release roadmap.

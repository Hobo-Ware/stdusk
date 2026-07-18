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
| Tab jump 10-20 (`tab-10`..`tab-20`) | ⬜ | have 1-9 |
| `duplicate-tab` (clone incl. cwd) | ✅ | context-menu "Duplicate" |
| `reopen-tab` (reopen last closed) | ✅ | Cmd+Shift+T; closed-cwd stack (cap 20) |
| `toggle-last-tab` (alt-tab between two) | ⬜ | |
| `pin-tab` (pin, guard close) | ⬜ | |
| `restart-tab` (respawn shell) | ✅ | context-menu Restart (same cwd, keeps title/color) |
| Close other / to-the-right / to-the-left | ✅ | context-menu items (feed the reopen stack) |
| `explode-tab` (panes -> tabs) / `combine-tabs` (tabs -> split) | ⬜ | power-user, low priority |
| Notify-when-done | ✅ | `terminal.notify_on_done`; osascript notification when a >10s command finishes while hidden. notify-on-activity still ⬜ |
| Current-process display in menu | ⬜ | have procwatch tree already |
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
| Broadcast input (`pane-focus-all`, `focus-all-tabs`) | ⬜ | multifocus |
| `rearrange-panes` (labelled move mode) | ⬜ | |
| Aggregated tab progress/title across panes | 🟡 | shows focused pane only |
| Drag tab into a split (drop zones) | ⬜ | |

## Rendering
| Feature | State | Notes |
|---|---|---|
| Truecolor/256/16 + cursor styles | ✅ | |
| Cursor blink | ✅ | `cursor_blink` (default on); focused pane only, xterm cadence |
| Font weight / bold weight | ⬜ | single weight |
| Font family + fallback font + line padding | ⬜ | no font config at all (egui bundled mono + hardcoded fallbacks); Nerd Font PUA glyphs (starship/p10k) render as tofu. Tabby: `font: Menlo`, configurable |
| Ligatures | 🟡 | `ligatures` (default off): symbol substitution (-> => != >= <= ...); true OpenType shaping still ⬜ (egui limit) |
| Sixel / inline images | ⛔→FUTURE | alacritty grid has no image model |
| Bold-in-bright-colors | ✅ | `bold_bright` (default on) |
| Minimum contrast ratio (auto-contrast) | ⬜ | |
| Palette generate / harmonious | ⬜ | niche |
| Light color scheme + follow-OS light/dark | ✅ | `appearance.follow_system` + `theme_light`/`theme_dark`; adaptive chrome; `one-half-light` added |
| Background: image / vibrancy / blur | ⬜ | opacity only |
| Configurable scrollback lines (25k default) | ✅ | `scrollback_lines` (default 25000) |
| Wide-char / Unicode 11 widths | 🟡 | verified broken-ish: no WIDE_CHAR/spacer handling in `grid_snapshot`; CJK/emoji squeeze into one cell and overlap |

## Input / copy-paste
| Feature | State | Notes |
|---|---|---|
| Copy / paste / bracketed paste / OSC 52 | ✅ | |
| Intelligent Ctrl-C (copy if selection else SIGINT) | ✅ | Tabby-parity: copy+clear when selected, else SIGINT |
| Natural editing keys (home/end/word/line) | ✅ | |
| `select-all` (Cmd-A) | ✅ | selects whole buffer; Cmd-C copies |
| `clear` (Cmd-K) | ✅ | sends Ctrl-L (shell clear); scrollback-wipe still ⬜ |
| Font zoom (`zoom-in/out/reset`) | ✅ | Cmd +/-/0 (runtime `zoom` multiplier) |
| Copy-on-select | ✅ | `copy_on_select`; on selection finish, skips whitespace-only |
| Middle-click paste | ✅ | `paste_on_middle_click` (default on, arboard clipboard read) |
| Copy-as-HTML (rich clipboard) | ⬜ | niche |
| Right-click mode (menu vs paste vs clipboard) | 🟡 | menu only |
| Multiline-paste warning / paste protection | ✅ | `warn_on_multiline_paste`; modal w/ preview, suppressed on alt-screen (Tabby-exact) |
| Paste transforms (trim ws, newlines->spaces) | ✅ | `trim_whitespace_on_paste` (default on) + `replace_newlines_on_paste`; Tabby-exact rules |
| `altIsMeta` + configurable word separators | ✅ | `alt_is_meta`, `word_separators`; focus-follows-mouse still ⬜ |
| Alt+scroll -> arrow keys | ⬜ | |

## Scrolling
| Feature | State | Notes |
|---|---|---|
| Wheel scroll + scrollbar | ✅ | |
| Scroll hotkeys | ✅ | Shift+PageUp/Down (page) + Shift+Home/End (top/bottom); line steps ⬜ |

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
| Named profiles (shell/args/cwd/env/color) | ✅ | `[[profiles]]`; tab menu + '+' right-click + palette; icon ⬜ |
| Per-profile env editor / command-line editor | ⬜ | with profiles |
| `switch-profile` in active pane | ⬜ | with profiles |
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
| Accessory app (no Dock icon / tray, quake default) | ✅ | `quake.hide_from_dock`; ActivationPolicy::Accessory |
| Dock icon + menu bar only while visible (opt-in) | ✅ | `quake.dock_when_visible`; runtime activation-policy flip |
| Menu-bar (tray) icon + Show/Hide/Quit menu | ✅ | `quake.menu_bar_icon`; tray-icon crate (Tabby `hideTray` parity) |
| Light/dark/tinted-adaptive app icon (macOS 26) | ⬜ | needs Icon Composer `.icon`; static .icns now |
| Dock edge (top/bottom/left/right) | ⬜ | top only |
| Display/screen selection (`dockScreen`) | ⬜ | current monitor only |
| Always-on-top, dock fill/space tuning | ⬜ | |
| Drop animation | ⬜ | deferred polish |
| Tab-bar location (top/bottom/left/right) | ⬜ | top only |
| Flex/fixed tab width | ⬜ | |
| Native/thin/full window frame | 🟡 | borderless only |
| Show tabs in fullscreen, hide index/close/options-button | ⬜ | cosmetic toggles |

## Settings GUI (M11)
| Feature | State | Notes |
|---|---|---|
| egui settings panel | ✅ | gear or Cmd+, opens the full view: Appearance/Terminal/Quake/Session/About sidebar, scheme browser w/ search + live preview card, unsaved-changes guard, live-apply + Save-to-toml. Quake hotkey editable (live re-register); general keybinding editor still ⬜ |
| Settings sync via git (config + custom schemes) | ✅ | `[sync] repo` push/pull with the user's own git creds (`sync.rs`); replaces the dropped SaaS sync |
| Raw config editor / show-defaults | 🟡 | "Open config file/folder" link rows in settings About section |

## Session / lifecycle
| Feature | State | Notes |
|---|---|---|
| Session restore (`recoverTabs`, reopen tabs+cwd on launch) | ✅ | `[session] restore`; cwd/title/color, saved every 3s |
| Behavior on session end (keep/close/restart) | ⬜ | **bug-grade**: shell exit leaves a dead frozen tab (reader thread breaks silently, nothing observes it). Tabby default auto-closes on clean exit |
| Dynamic title from shell (OSC 0/2) + disable toggle | 🟡 | cwd title only; OSC 0/2 confirmed NOT parsed (`osc.rs` drops it; EventProxy handles Bell only) |
| Save/load terminal output & state (debug) | ⛔ | niche debug tooling |

## Distribution / packaging
| Feature | State | Notes |
|---|---|---|
| Homebrew cask -> /Applications + `stdusk` CLI, Spotlight-findable | ✅ | `brew install hobo-ware/tap/stdusk` |
| Universal (arm64+x86_64) `.app` built + released on tag | ✅ | native-release.yml |
| Developer ID signing + notarization | ⬜ | needs Apple Developer acct; cask `postflight` strips quarantine as a stopgap |

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

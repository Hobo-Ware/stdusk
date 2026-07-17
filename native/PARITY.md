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

---

## Tabs
| Feature | State | Notes |
|---|---|---|
| New / close / switch (1-9) / reorder / rename / color | ✅ | reorder+rename via menu/dbl-click |
| `next-tab` / `previous-tab` cycle hotkey | ✅ | Ctrl+Tab / Ctrl+Shift+Tab (wraps) |
| `move-tab-left/right` hotkey | 🟡 | menu only, no hotkey |
| Tab jump 10-20 (`tab-10`..`tab-20`) | ⬜ | have 1-9 |
| `duplicate-tab` (clone incl. cwd) | ✅ | context-menu "Duplicate" |
| `reopen-tab` (reopen last closed) | ✅ | Cmd+Shift+T; closed-cwd stack (cap 20) |
| `toggle-last-tab` (alt-tab between two) | ⬜ | |
| `pin-tab` (pin, guard close) | ⬜ | |
| `restart-tab` (respawn shell) | ⬜ | |
| Close other / to-the-right / to-the-left | ⬜ | context-menu items |
| `explode-tab` (panes -> tabs) / `combine-tabs` (tabs -> split) | ⬜ | power-user, low priority |
| Notify-when-done / notify-on-activity | ⬜ | have OSC 133 + bell infra to build on |
| Current-process display in menu | ⬜ | have procwatch tree already |
| Drag-reorder tabs (+ between windows) | ⬜ | single-window; drag-reorder wanted |
| `toggle-fullscreen` | ⬜ | |
| Save-as-profile / save-layout-as-profile | ⛔ | depends on profiles (deferred) |

## Panes / splits
| Feature | State | Notes |
|---|---|---|
| Split right/bottom/left/top + drag-resize + click-focus | ✅ | |
| Keyboard pane nav (directional) | ✅ | Cmd+Alt+arrows (`pane::neighbor`); prev/next + 1-9 still ⬜ |
| Keyboard pane resize (`pane-increase/decrease-*`, step) | ⬜ | drag-only now |
| `pane-maximize` / zoom | ✅ | Cmd+Alt+Enter toggles `tab.maximized` |
| Broadcast input (`pane-focus-all`, `focus-all-tabs`) | ⬜ | multifocus |
| `rearrange-panes` (labelled move mode) | ⬜ | |
| Aggregated tab progress/title across panes | 🟡 | shows focused pane only |
| Drag tab into a split (drop zones) | ⬜ | |

## Rendering
| Feature | State | Notes |
|---|---|---|
| Truecolor/256/16 + cursor styles | ✅ | |
| Cursor blink | ⬜ | styles only, no blink |
| Font weight / bold weight | ⬜ | single weight |
| Configurable fallback font + line padding | 🟡 | fallbacks hardcoded; no linePadding |
| Ligatures | ⬜ | needs shaping (rustybuzz) |
| Sixel / inline images | ⛔→FUTURE | alacritty grid has no image model |
| Bold-in-bright-colors | ⬜ | cheap |
| Minimum contrast ratio (auto-contrast) | ⬜ | |
| Palette generate / harmonious | ⬜ | niche |
| Light color scheme + follow-OS light/dark | ✅ | `appearance.follow_system` + `theme_light`/`theme_dark`; adaptive chrome; `one-half-light` added |
| Background: image / vibrancy / blur | ⬜ | opacity only |
| Configurable scrollback lines (25k default) | 🟡 | fixed ~10k, not configurable |
| Wide-char / Unicode 11 widths | 🟡 | verify CJK/emoji cell widths |

## Input / copy-paste
| Feature | State | Notes |
|---|---|---|
| Copy / paste / bracketed paste / OSC 52 | ✅ | |
| Intelligent Ctrl-C (copy if selection else SIGINT) | 🟡 | Ctrl-C = SIGINT; Cmd-C copies. verify |
| Natural editing keys (home/end/word/line) | ✅ | |
| `select-all` (Cmd-A) | ✅ | selects whole buffer; Cmd-C copies |
| `clear` (Cmd-K) | ✅ | sends Ctrl-L (shell clear); scrollback-wipe still ⬜ |
| Font zoom (`zoom-in/out/reset`) | ✅ | Cmd +/-/0 (runtime `zoom` multiplier) |
| Copy-on-select | ⬜ | config option |
| Middle-click paste | ⬜ | |
| Copy-as-HTML (rich clipboard) | ⬜ | niche |
| Right-click mode (menu vs paste vs clipboard) | 🟡 | menu only |
| Multiline-paste warning / paste protection | ⬜ | |
| Paste transforms (trim ws, newlines->spaces) | ⬜ | |
| `altIsMeta`, configurable word separator, focus-follows-mouse | ⬜ | config options |
| Alt+scroll -> arrow keys | ⬜ | |

## Scrolling
| Feature | State | Notes |
|---|---|---|
| Wheel scroll + scrollbar | ✅ | |
| Scroll hotkeys | 🟡 | Shift+PageUp/Down (page); to-top/bottom + line steps still ⬜ |

## Links
| Feature | State | Notes |
|---|---|---|
| Clickable links (URL + file paths) | ✅ | `clickable_links` + `link_modifier` (default "none" = hover, Tabby-style); opens via `open`, cwd-relative + `~`. IP-only (no scheme) still ⬜ |

## Command palette / discoverability
| Feature | State | Notes |
|---|---|---|
| Command palette (`command-selector`) | ⬜ | fuzzy actions; nice-to-have |
| Profile selector | ⛔ | depends on profiles |

## Profiles & shell
| Feature | State | Notes |
|---|---|---|
| cwd tracking + new-tab-in-cwd | ✅ | |
| Named profiles (shell/args/cwd/env/color/icon launchers) | ⬜ | Tabby-core concept; useful post-v1 |
| Per-profile env editor / command-line editor | ⬜ | with profiles |
| `switch-profile` in active pane | ⬜ | with profiles |
| Shell auto-detection list (zsh/bash/fish/pwsh/...) | 🟡 | uses `$SHELL`; no picker. (Tabby fork ships no concrete ShellProvider either) |
| Run-as-administrator / UAC | ⛔ | security smell (PLAN §9) |

## Themes / color schemes
| Feature | State | Notes |
|---|---|---|
| Built-in themes | 🟡 | 4 (one-half dark/light, dracula, tokyo-night); Tabby community pack has **191** XRDB schemes |
| Import color schemes (XRDB/iTerm/base16) | ⬜ | parse into a theme; 191 XRDB files available to port |
| Custom color schemes in config | 🟡 | theme by name only |

## Quake / window / docking
| Feature | State | Notes |
|---|---|---|
| Top-edge drop, global hotkey, hide-on-blur, height % | ✅ | |
| Accessory app (no Dock icon / tray, quake default) | ✅ | `quake.hide_from_dock`; ActivationPolicy::Accessory |
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
| egui settings panel | ⬜ | Tabby: Application/Window/Hotkeys/Profiles/Config-file tabs. We ship config.toml + gear-opens-file |
| Raw config editor / show-defaults | 🟡 | gear opens config.toml in $EDITOR |

## Session / lifecycle
| Feature | State | Notes |
|---|---|---|
| Session restore (`recoverTabs`, reopen tabs+cwd on launch) | ⬜ | persist tab/cwd state |
| Behavior on session end (keep/close/restart) | ⬜ | |
| Dynamic title from shell (OSC 0/2) + disable toggle | 🟡 | cwd title; verify OSC 0/2 title |
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

## Suggested next milestones (priority order)
1. **M11 - clickable links** (M2.5 debt): URL/IP/file, cwd-relative + `~`, modifier-click, `open`. High daily-driver value, self-contained.
2. **M12 - keyboard pane nav + resize + maximize**: the biggest splits gap; power users live here.
3. **M13 - input/paste polish**: select-all, clear, font-zoom, copy-on-select, middle-click paste,
   scroll hotkeys, right-click modes, multiline-paste guard. Cheap, high-frequency wins.
4. **M14 - color-scheme import + light/dark follow-OS**: port the 191 XRDB schemes; huge perceived surface.
5. **M15 - tab power features**: next/prev cycle, move hotkeys, duplicate, reopen, toggle-last, pin,
   restart, close-others; drag-reorder; notify-when-done (OSC 133 already parsed).
6. **M16 - session restore** + dynamic OSC 0/2 title + behavior-on-exit.
7. **M17 - settings GUI** (egui) + more quake/docking + tab-bar location + cursor blink + font weight.
8. **Later/FUTURE**: named profiles, command palette, ligatures, sixel/images, broadcast input.

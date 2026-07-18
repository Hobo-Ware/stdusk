# stdusk - manual test guide

Step-by-step verification for everything shipped through 0.2.4.
Automated coverage: `cargo test` (unit + parser suites), `cargo clippy -- -D warnings`,
`--screenshot` render harness, and end-to-end theme/config checks (see LEDGER). Everything
below is the *human* pass - interactions the harness can't drive.

Setup: `brew install hobo-ware/tap/stdusk` (or `cd native && cargo run`). Summon with
`Ctrl+\``. Config lives at `~/.config/stdusk/config.toml` (gear icon opens it); after
config edits restart stdusk. Open a NEW tab after install so fresh shell hooks load.

## 1. Paste pipeline
| Step | Expect |
|---|---|
| Copy a single line with spaces around it (`  ls -la  `), paste (Cmd+V) | Pasted trimmed both ends (default `trim_whitespace_on_paste`) |
| Copy 3 lines of text, paste in the shell | Modal "Paste 3 lines?" with a preview; **Paste** sends all, **Cancel**/Esc sends nothing; Enter = Paste |
| While the modal is open, type letters | Nothing reaches the shell (keys don't leak) |
| Open `vim`, paste the same 3 lines | NO modal (suppressed on alt screen), text pastes into vim |
| Set `replace_newlines_on_paste = true`, paste 3 lines | Pasted as one line, newlines collapsed to single spaces, no modal |
| Copy text ending in a newline, paste | Trailing newline stripped -> command does NOT auto-execute |

## 2. Copy / selection
| Step | Expect |
|---|---|
| Select text with the mouse, press Ctrl+C | "Copied" toast, selection clears, NO ^C sent (prompt untouched) |
| No selection, Ctrl+C during `sleep 100` | SIGINT - the sleep dies |
| Select text, Cmd+C | Copies, selection stays |
| Set `copy_on_select = true`, drag-select some text | Clipboard updates the moment you release (no keypress) |
| Double-click a word inside `foo(bar)` | Selects `bar` (word separators `()[]{}'"` end words); change `word_separators` and re-verify |
| Middle-click anywhere in a pane | Clipboard pastes at the prompt (default `paste_on_middle_click`) |
| Cmd+A then Cmd+C | Whole scrollback in the clipboard |

## 3. Rendering
| Step | Expect |
|---|---|
| Watch the prompt cursor | Blinks (~1s cycle); set `cursor_blink = false` -> steady |
| Split panes: only the FOCUSED pane's cursor blinks | Unfocused pane cursor steady + dimmed |
| `printf '\e[1;31mBOLD RED\e[0m\n'` | Bold text renders in the BRIGHT red (vs `\e[31m` plain red); `bold_bright = false` -> same red |
| `seq 30000` then scroll up | History reaches ~25k lines (`scrollback_lines`) |
| macOS Appearance toggle light/dark | Whole app recolors live (one-half-light/dark) |
| `theme = "nord"` + `follow_system = false`, restart | Nord colors; try any of the 191 pack names (`dracula`, `solarized-dark`, ...) |
| Drop an XRDB file at `~/.config/stdusk/schemes/mytheme`, set `theme = "mytheme"`, restart | Your scheme loads (user dir beats the pack) |

## 4. Input
| Step | Expect |
|---|---|
| Set `alt_is_meta = true`, restart; in `zsh` press Option+B / Option+F | Cursor jumps by words (ESC-b/f), no `∫`/`ƒ` chars; default (false) keeps macOS composed chars |
| Shift+Home / Shift+End with history | Jump to scrollback top / bottom |
| Shift+PageUp / PageDown | Page up/down |
| Cmd+= / Cmd+- / Cmd+0 | Font zoom in/out/reset (grid reflows) |
| Cmd+K | Prompt clears (Ctrl-L) |

## 5. Tabs
| Step | Expect |
|---|---|
| Ctrl+Tab / Ctrl+Shift+Tab | Cycle tabs forward/back, wrapping |
| Cmd+Shift+← / → | Active tab moves left/right in the bar |
| Right-click tab -> Duplicate | New tab in the same cwd |
| Right-click -> Restart | Fresh shell, same cwd, title/color kept |
| Right-click -> Close other tabs / to the right / to the left | Exactly those close; Cmd+Shift+T reopens them (most recent first, cwd preserved) |
| `sleep 12`, hide stdusk before it ends | macOS notification "command finished" (only >10s + hidden) |

## 6. Panes
| Step | Expect |
|---|---|
| Cmd+D / Cmd+Shift+D | Split right / down (new pane focused, same cwd) |
| Cmd+Alt+arrows | Focus moves directionally between panes |
| Cmd+Ctrl+arrows | Focused pane grows/shrinks (divider moves) |
| Cmd+Alt+Enter | Focused pane maximizes / restores |
| Cmd+W | Closes focused pane; last pane closes the tab |

## 7. Links
| Step | Expect |
|---|---|
| `echo https://example.com`, hover | Underline + hand cursor; click opens browser (no modifier by default) |
| `echo 192.168.1.1` click | Opens `http://192.168.1.1` |
| `echo /etc/hosts` click | Opens the file; `echo ~/Downloads` opens Finder |
| Set `link_modifier = "cmd"` | Links only react while Cmd held |

## 8. Session restore
| Step | Expect |
|---|---|
| Open 3 tabs in different dirs, rename one, color one; Quit (menu-bar icon -> Quit); relaunch | All 3 tabs return with cwds, the rename, the color, and the same active tab |
| `[session] restore = false` | Launches with a single fresh tab |

## 9. Quake / window
| Step | Expect |
|---|---|
| Ctrl+\` toggle | Drops from top edge / hides; no Dock icon by default |
| Menu-bar icon | Show/Hide + Quit work; icon tints with the menu-bar appearance |
| `dock_when_visible = true` | Dock icon + "stdusk" menu bar appear while visible, vanish when hidden |
| Click another app (default `hide_on_focus_loss`) | stdusk hides |

## 10. Regression spot-checks
| Step | Expect |
|---|---|
| `printf '\e[8;10;40t'` (app resizes the grid), then move the mouse over the pane and select text | No crash (grid-dims fix); selection lands where clicked |
| Ctrl+C in Claude CLI / any REPL with no selection | Interrupt works as before |
| `brew upgrade`-style output with `%` | Tab progress bar still tracks |
| Failed command (`false`) in a background tab | Subtle red left-edge mark on that tab |
| Run `claude` in a tab | "claude" badge appears on the tab within ~1s |

## 11. 0.2.1 additions
| Step | Expect |
|---|---|
| Cmd+Shift+P, type "spl" | Palette opens; Split Right/Down rank top; Enter splits; Esc closes |
| Drag a tab sideways past its neighbor | Tabs swap while dragging; click/rename/menu still work |
| `[terminal] ligatures = true`, restart; `echo '-> => != >= <='` | Single glyphs → ⇒ ≠ ≥ ≤; copy still yields the real chars |
| Add a `[[profiles]]` block (see config.example), restart; right-click "+" | Profile listed; opens tab with its cwd/env/color/name; also in palette as "New Tab: <name>" |
| Click the gear | Central area swaps to the full settings view (tab bar stays); gear lights up while open; gear or Esc closes; terminal comes back untouched |
| Settings sidebar | Six sections (Appearance / Color scheme / Terminal / Quake / Session / About) with icons; selected row highlighted |
| Color scheme: type "nord" in search | List filters live (case-insensitive); clicking a row recolors the whole app instantly and the row gets an accent border + check |
| Color scheme: hover rows | The terminal preview card above follows the hovered scheme; reverts to the active one on hover-out |
| With `follow_system = true`, pick a scheme | Sets `theme_light`/`theme_dark` for the CURRENT macOS appearance (dim hint line explains); the app doesn't snap back next frame |
| Change opacity / font size / toggles | Live-apply; footer Save persists to config.toml ("Saved" toast); Revert reloads the file and re-applies the theme ("Reverted" toast) |
| About section | Version shown; Open config file / Open config folder rows work; "the machine speaks back" tagline |
| `cargo run -- --screenshot-settings /tmp/s.png` | Renders the settings view (Color scheme section, one-half-dark pinned) and exits 0 |

## 12. 0.2.3 / 0.2.4 additions
| Step | Expect |
|---|---|
| `hide_on_focus_loss = false`, summon, click another app | stdusk stays visible ON TOP of the other app's windows (always-on-top) |
| Also set `[quake] unfocused_opacity = 0.6`, focus another app | stdusk dims while unfocused; refocusing restores full opacity; with hide-on-blur back on the setting is moot (the window hides) |
| Settings -> Appearance theme pickers (manual theme, or the light/dark pair under follow-system) | Each is a searchable dropdown; typing filters live; picking applies instantly |
| Change any setting, then Close (gear / Esc / footer Close) | "Unsaved changes" modal: Save persists + closes, Discard reverts + closes, Keep editing stays in settings |
| `sleep 100`, then Cmd+W (or the tab close-x / menu Close) | Close-busy-tab confirm naming the running command; Cancel keeps it; `warn_on_close_running = false` closes instantly |
| Run `claude` in a tab | Compact brand-colored initial chip ("C") BEFORE the title; full name on hover; never overlaps the close-x |
| Right-click tab -> Color, hover the swatches | The tab underline previews the hovered color live; hover-out reverts; click applies |
| Settings -> Session -> Sync: set a (private!) repo, hit Push | Buttons disabled while the field is empty or a sync runs; Push saves first, then commits + pushes config.toml + custom schemes; session.toml + shell hooks are NOT in the repo |
| On another machine (or after local edits), hit Pull | Repo settings overwrite local; theme + hotkey re-apply live, no restart |
| `STDUSK_SHOT_SECTION=quake cargo run -- --screenshot-settings /tmp/s.png` | Renders that settings section instead of the scheme browser and exits 0 |

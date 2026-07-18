# stdusk - manual test guide

Step-by-step verification for everything shipped through the supreme pass (0.2.0).
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
| Click the gear | Settings window: change theme/opacity live; Save persists to config.toml; Open config file still works |

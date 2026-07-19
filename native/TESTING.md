# stdusk - manual test guide

Step-by-step verification for everything shipped through 0.5.0.
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
| Cmd+K | Screen AND scrollback wiped (scrollbar disappears), fresh prompt redraws |

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

## 13. 0.3.0 - shell exit + dynamic titles
| Step | Expect |
|---|---|
| `exit` in a single-pane tab (default `on_exit = "close"`) | The tab closes, no frozen leftover; other tabs untouched |
| `exit` in the LAST remaining tab | A fresh tab spawns in its place (app stays alive, never a zombie) |
| Split a tab, `exit` in one pane | Only that pane closes; the sibling expands and takes focus |
| `on_exit = "keep"`: `exit` | Pane dims with "[process exited: 0]"; Enter (while focused) or a click respawns the shell in the same cwd |
| `on_exit = "restart"`: `exit` | Shell respawns in place instantly |
| `on_exit = "restart"` with a crash-looping shell (e.g. a profile whose shell dies at once) | Two quick deaths (<2s from spawn) -> falls back to the keep overlay instead of looping |
| `printf '\033]0;hello\007'` | Tab title becomes "hello" (a user rename still wins) |
| `printf '\033]0;\007'` after that | Empty title resets -> title falls back to the cwd basename |
| `cd /somewhere` while a title is set | Title stays the OSC one (OSC beats cwd) |
| Settings > Terminal > "Dynamic tab title" off | OSC titles ignored live (no respawn); cwd basename titles return |

## 14. 0.3.1 - custom font + line padding
| Step | Expect |
|---|---|
| Settings > Appearance > Text: type "Menlo" in Font, click elsewhere | Grid re-renders in Menlo Regular (upright, never Italic) the moment the field loses focus; no restart |
| "Installed fonts" dropdown | Searchable list of every installed family; typing filters; picking applies instantly; "Default (bundled)" (unfiltered top row) resets to the bundled font |
| With a Nerd Font installed (e.g. "JetBrainsMono Nerd Font") + a powerline/starship prompt | PUA glyphs (\|arrows) render - no tofu; emoji + box-drawing still render (fallbacks kept behind the custom font) |
| Type a bogus name ("NoSuchFontXyz"), blur the field | "Font not found: NoSuchFontXyz" toast; current font kept; no crash (same on launch with a bad config value) |
| Line padding slider 0 -> 6 px | Lines space out live (cell height grows); pty rows shrink to fit; Save persists `line_padding` |
| Save / Revert / Discard after font edits | Each re-applies the font (Revert/Discard restore the previous one) |
| `[appearance] font = "Menlo"` in config, restart | Launches in Menlo; the settings field shows it |

## 15. 0.3.2 - wide chars, min contrast, all-match search, brand badges
| Step | Expect |
|---|---|
| `echo 你好世界` | Each CJK glyph spans TWO cells - no squeezing/overlap; text after it starts at the right column |
| `echo "🙂 emoji"` | The emoji occupies two cells (monochrome outline - color emoji is a documented v1 limit) |
| Select across `你好` with the mouse | Selection rectangles cover both cells of each glyph; Cmd+C copies `你好` intact |
| Block cursor over a wide glyph (arrow-key onto it in `zsh`) | The block covers both cells; the glyph stays legible inside it |
| Settings > Appearance > Text > Minimum contrast: slide to ~4.5 | Dim/low-contrast text brightens live toward readability; back to 1.0 restores the theme's exact colors |
| `minimum_contrast = 4.5` on a LIGHT theme | Too-light text darkens instead (the nudge goes toward black on light backgrounds) |
| Cmd+F, search a word appearing 5+ times on screen | EVERY visible occurrence gets a dim accent wash; the current one is clearly brighter; Enter cycles the bright one through them |
| Scroll while the find bar is open | Washes track matches into/out of view (only visible ones are drawn) |
| Run `claude` in a tab | The Anthropic mark (clay) appears before the title - a real brand icon, not a letter chip; crisp on retina |
| Run `gemini` / `gh copilot` / `ollama run …` / `cursor-agent` | Google Gemini spark (blue) / GitHub Copilot (grey) / Ollama / Cursor brand marks respectively |
| Run `codex` or `aider` | Letter chip (no Simple Icons slug exists for these two) |
| Toggle macOS light/dark with a badge showing | Brand icon stays legible on both tab-strip shades |

## 16. 0.4.0 - input & scroll parity
| Step | Expect |
|---|---|
| `seq 100`, then Alt+wheel over the pane | Viewport does NOT scroll; arrow keys hit the shell (history cycles at the prompt); in `less` the content moves line-by-line |
| Ctrl+Shift+Up / Ctrl+Shift+Down with history | Viewport scrolls exactly one line per press; nothing leaks to the shell |
| Right-click a pane (default `right_click = "menu"`) | Context menu opens, same as before |
| Settings > Terminal > Mouse > Right click: **Paste** - quick right-click | Clipboard pastes at the prompt (no menu) |
| Same, but HOLD the right button >=250ms before releasing | Context menu opens instead of pasting |
| Right click: **Copy or paste** - select text, quick right-click | Selection copied ("Copied" toast) and cleared; with NO selection it pastes |
| Settings > Terminal > Mouse > Focus follows mouse ON; split, then just move the pointer between panes | Focus (bright pane) follows the pointer without clicking; drag-selecting across the divider does NOT switch focus mid-drag |
| `seq 30000`, Cmd+K | Screen and the whole scrollback wiped (scrollbar gone), fresh prompt; scrolling up shows nothing |
| Cmd+Shift+P -> "Clear Scrollback" | History wiped, but the visible screen stays |
| Open tabs A B C; activate B, then C, then press Cmd+O repeatedly | Bounces between C and B; close the previous tab and Cmd+O falls back to tab 1 |
| Right-click tab -> Pin | Tab jumps to the front of the bar with a small pin glyph; a second pinned tab lands AFTER it |
| Drag a pinned tab right / an unpinned tab left across the boundary | Refuses to cross (Cmd+Shift+arrows too) |
| Close a pinned tab (x / Cmd+W) with nothing running | "This tab is pinned." confirm appears; Enter closes, Esc keeps |
| Pin a tab, quit, relaunch (`session.restore` on) | The tab comes back pinned, still first |
| Right-click tab -> Unpin | Pin glyph gone; the tab lands at the start of the unpinned group |

## 17. 0.4.1 - panes & tabs polish
| Step | Expect |
|---|---|
| Split a tab (Cmd+D), press Cmd+Shift+I | "Broadcast input on" toast; EVERY pane gets an accent border and the unfocused fade drops |
| Type `echo hi` + Enter | The command runs in BOTH panes |
| Cmd+V a single line | Pastes into both panes; a multiline paste confirms once, then lands in both |
| Cmd+Shift+I again | "Broadcast input off" toast; borders gone, unfocused pane dims again; typing hits only the focused pane |
| Broadcast on, switch to another tab and back | Mode is OFF (switching tabs exits it) |
| Cmd+Shift+P -> "Broadcast Input" | Same toggle as the hotkey |
| Split a tab, run `brew upgrade`-style output (with %) in the UNFOCUSED pane | The tab's top progress bar tracks it even though the pane isn't focused |
| Two panes with progress (e.g. two downloads) | The bar shows the FURTHEST one; a failing OSC 9;4 error state turns it red regardless |
| Run `false` in a background pane, focus the other pane | The tab still shows the red left-edge fail mark |
| Run `sleep 100`, right-click the tab | Disabled dim first row "Running: sleep"; with `claude` open it says "Running: claude"; idle shell = no row |
| Right-click tab -> Notify on activity (check appears), switch to another tab, run output in the first (e.g. `sleep 2 && echo hi` before switching) | ONE macOS notification "<tab>: new output"; more output stays quiet |
| View the tab, switch away again, produce output | Notification fires again (viewing re-armed it) |
| Toggle "Notify on activity" off | Check gone; no notifications |
| `STDUSK_SHOT_BROADCAST=1 cargo run -- --screenshot /tmp/s.png` | Renders a split active tab with the broadcast border on both panes and exits 0 |

## 18. 0.5.0 - profiles editor, hotkey remapping, autosync
| Step | Expect |
|---|---|
| Settings > Profiles > Add profile | "profile N" appears in the list, expanded into the inline editor; footer Save persists a `[[profiles]]` block |
| Edit Name / Shell / Working directory | List row updates live (shell summary line); empty Shell shows "default shell ($SHELL)" |
| Arguments: type `-c "echo hi there"` | Save writes `args = ["-c", "echo hi there"]` (quotes group; check the config file) |
| Environment: Add variable, type `AWS_PROFILE` = `work`; add a second row, leave its NAME blank | Save persists only the named variable (blank keys are dropped) |
| Tab color: click a swatch / "No color" | Row's leading dot recolors; a launched tab gets the underline |
| Row's ▶ (Launch) | New tab spawns with the profile (name/color/cwd/env) - visible in the tab bar above settings; "Launched <name>" toast |
| Row's duplicate icon | "<name> copy" inserted after, selected for editing |
| Row's trash icon | Profile gone; Save persists the removal; Revert restores it |
| Edit args, then footer Revert (or Discard on close) | Editor buffers reload from the restored config (no stale text) |
| Settings > Hotkeys: set New tab to `Cmd+N`, Save | Cmd+N opens a tab; Cmd+T stops (exact chords only); tab-menu/+ tooltips show the new chord |
| Type `garbage` in a field, click elsewhere | Text turns red while typing; "Invalid hotkey: garbage" toast on blur; the action simply never fires |
| Clear a field ("" = unbound) | No toast; the action has no hotkey; its menu hint disappears |
| Set Clear terminal to `Ctrl+K` | Ctrl+K wipes the scrollback AND sends ^K to the shell (documented collision - app bind + terminal byte both fire) |
| `[hotkeys] new_tab = "Cmd+N"` in config, restart | Bind applies from the file; missing fields keep defaults |
| Hotkeys > Reset to defaults | All 15 rows back to the shipped chords |
| Cmd+Shift+P / Cmd+, while a rename or paste-confirm modal is open | Still suppressed (text modals own the keyboard), rebound or not |
| Settings > Session > Auto sync (repo set), Save | Toggle enabled only with a repo; every Save pushes ("Settings pushed" toast); rapid saves don't stack pushes |
| `[sync] auto = true`, relaunch | One background pull on launch ("Settings pulled" toast; theme/hotkey re-apply); a failing repo toasts once and startup still completes |
| `STDUSK_SHOT_SECTION=profiles cargo run -- --screenshot-settings /tmp/s.png` | Renders the Profiles section (demo profiles + open editor) and exits 0; `=hotkeys` renders the Hotkeys section |

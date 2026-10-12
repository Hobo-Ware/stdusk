//! stdusk-core: the terminal engine behind stdusk, free of any UI toolkit.
//!
//! `terminal::PtyTerm` runs a shell on a pty and keeps an `alacritty_terminal` grid in sync;
//! the parsers (`osc`, `progress`, `modes`, `screen`, `search`, `links`), `shell` launch setup,
//! the `pane` split tree and the Claude Code / Codex session tracking (`agents`, `procwatch`)
//! build on it. Background threads ask for a redraw through `repaint::Repaint`.
//! `colors` keeps the process-wide theme every renderer reads.

pub mod agent_codex;
pub mod agent_codex_scan;
pub mod agent_pane;
pub mod agent_restore;
pub mod agent_track;
pub mod agents;
pub mod colors;
pub mod links;
pub mod modes;
pub mod mouse;
pub mod osc;
pub mod pane;
pub mod paths;
pub mod procwatch;
pub mod progress;
pub mod repaint;
pub mod screen;
pub mod search;
pub mod session;
pub mod shell;
pub mod terminal;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod themes;

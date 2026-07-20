//! Single-instance enforcement (both quake and window mode). Only one stdusk process runs; a
//! second launch connects to the primary over a Unix domain socket, asks it to surface + open a
//! new tab, and exits(0) without ever creating a window. A stale socket (previous crash) is
//! unlinked and taken over. The wire command parsing + the stale-lock decision are pure and
//! unit-tested; the socket round-trip is exercised below too.
#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The bound listener the primary holds. A cross-platform alias so `Stdusk::new`'s signature
/// compiles everywhere; only the unix build actually accepts connections.
#[cfg(unix)]
pub(crate) type Listener = UnixListener;
#[cfg(not(unix))]
pub(crate) type Listener = ();

/// The one command the socket carries today: focus the running instance and open a new tab.
pub(crate) const CMD_NEW_TAB: &str = "new-tab";

/// A parsed socket command. Kept as an enum so the wire protocol can grow (activate-only, run a
/// program, ...) without the listener thread having to string-match inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    NewTab,
}

/// Parse one line off the socket. Unknown/blank lines are ignored (forward-compatible).
pub(crate) fn parse_command(line: &str) -> Option<Command> {
    match line.trim() {
        CMD_NEW_TAB => Some(Command::NewTab),
        _ => None,
    }
}

/// The startup action given whether the socket file exists and whether a live primary answered a
/// connect. Pure so the branching is testable without touching the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Startup {
    /// No socket (or a dead one): bind it and become the primary.
    Primary,
    /// A live primary answered: hand it the command and exit.
    Secondary,
    /// The socket file exists but nobody answered (crash): unlink it, then become primary.
    TakeOverStale,
}

pub(crate) fn decide_startup(socket_exists: bool, connect_ok: bool) -> Startup {
    match (socket_exists, connect_ok) {
        (_, true) => Startup::Secondary,
        (false, false) => Startup::Primary,
        (true, false) => Startup::TakeOverStale,
    }
}

/// Fixed per-user socket path: `$XDG_RUNTIME_DIR/stdusk.sock` when set, else
/// `~/.config/stdusk/instance.sock` (per-user by $HOME - avoids a world-writable /tmp path).
pub(crate) fn socket_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(dir).join("stdusk.sock"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/stdusk/instance.sock"))
}

/// The outcome of trying to claim the single-instance lock.
#[cfg(unix)]
pub(crate) enum Acquired {
    /// We're the only instance; hold the listener and spawn the accept thread.
    Primary(UnixListener),
    /// Another instance is live and has been signaled; the caller must exit(0) immediately.
    Secondary,
}

/// Send the one-line new-tab command down a connected stream (best effort).
#[cfg(unix)]
fn signal_new_tab(mut stream: UnixStream) {
    let _ = writeln!(stream, "{CMD_NEW_TAB}");
    let _ = stream.flush();
}

/// Bind the socket and become primary, creating the parent dir first. Handles a bind race with
/// another launching instance: retry as a client, else give up and become primary anyway (better
/// a second window than a dead launch).
#[cfg(unix)]
fn bind_primary(path: &std::path::Path) -> Acquired {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match UnixListener::bind(path) {
        Ok(listener) => Acquired::Primary(listener),
        Err(_) => match UnixStream::connect(path) {
            Ok(stream) => {
                signal_new_tab(stream);
                Acquired::Secondary
            }
            Err(_) => match UnixListener::bind(path) {
                Ok(listener) => Acquired::Primary(listener),
                Err(_) => Acquired::Secondary,
            },
        },
    }
}

/// Try to claim the single-instance lock at `path`. Connect first: success = a live primary, so
/// send the command and report `Secondary`. On connect failure the socket is stale or absent -
/// unlink a stale one and bind, becoming `Primary` (the `decide_startup` decision drives this).
#[cfg(unix)]
pub(crate) fn acquire(path: &std::path::Path) -> Acquired {
    let exists = path.exists();
    let stream = UnixStream::connect(path).ok();
    match decide_startup(exists, stream.is_some()) {
        Startup::Secondary => {
            if let Some(stream) = stream {
                signal_new_tab(stream);
            }
            Acquired::Secondary
        }
        Startup::Primary => bind_primary(path),
        Startup::TakeOverStale => {
            let _ = std::fs::remove_file(path);
            bind_primary(path)
        }
    }
}

/// Spawn the primary's accept loop: each incoming connection's command bumps `pending` and wakes
/// the UI via `request_repaint`. It only touches an atomic + the egui context - never the pty
/// lock - so it can't deadlock the render loop.
#[cfg(unix)]
pub(crate) fn spawn_listener(
    listener: UnixListener,
    pending: Arc<AtomicUsize>,
    ctx: eframe::egui::Context,
) {
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { continue };
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() && parse_command(&line) == Some(Command::NewTab)
            {
                pending.fetch_add(1, Ordering::SeqCst);
                ctx.request_repaint();
            }
        }
    });
}

/// The number of new-tab requests signaled from other launches since last checked (the primary's
/// UI drains this each frame). A separate handle is what the listener thread bumps.
pub(crate) fn pending_counter() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_new_tab_and_ignores_noise() {
        assert_eq!(parse_command("new-tab"), Some(Command::NewTab));
        assert_eq!(parse_command("new-tab\n"), Some(Command::NewTab));
        assert_eq!(parse_command("  new-tab  "), Some(Command::NewTab));
        assert_eq!(parse_command(""), None);
        assert_eq!(parse_command("quit"), None);
        assert_eq!(parse_command("new-window"), None);
    }

    #[test]
    fn startup_decision_table() {
        // A live primary always wins (become secondary), regardless of the file existing.
        assert_eq!(decide_startup(true, true), Startup::Secondary);
        assert_eq!(decide_startup(false, true), Startup::Secondary);
        // No file, nobody answering: we're first.
        assert_eq!(decide_startup(false, false), Startup::Primary);
        // File left behind by a crash, nobody answering: take it over.
        assert_eq!(decide_startup(true, false), Startup::TakeOverStale);
    }

    #[cfg(unix)]
    #[test]
    fn socket_round_trip_primary_receives_secondary_command() {
        use std::time::{Duration, Instant};

        let dir = std::env::temp_dir().join(format!("stdusk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("instance.sock");
        let _ = std::fs::remove_file(&path);

        // First acquire binds (primary); a second acquire connects + sends new-tab (secondary).
        let Acquired::Primary(listener) = acquire(&path) else {
            panic!("first acquire must be primary");
        };
        let pending = pending_counter();
        let ctx = eframe::egui::Context::default();
        spawn_listener(listener, pending.clone(), ctx);

        let Acquired::Secondary = acquire(&path) else {
            panic!("second acquire must be secondary");
        };

        // The listener thread bumps the counter shortly after the connection lands.
        let deadline = Instant::now() + Duration::from_secs(2);
        while pending.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(pending.load(Ordering::SeqCst), 1, "primary must receive the new-tab command");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}

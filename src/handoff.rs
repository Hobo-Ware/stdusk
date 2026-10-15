//! The app half of the session handoff: what is specific to stdusk's tabs, window and instance
//! socket. The transport, the per-pane metadata and the failure rules live in
//! `stdusk_core::handoff` (read its module docs for the wire format and the "old window stays up"
//! contract); this file launches the successor, snapshots the tabs, and keeps the single-instance
//! socket coordination.
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;
use stdusk_core::handoff::{
    LAUNCH_TIMEOUT, Layout, PaneMeta, accept_within, capture_pane, listen, send_session,
};

use crate::session::SavedSession;

/// Set once a successor CONFIRMED the handoff. Process-global because it outlives the app struct:
/// `main` reads it on the way out to decide whether the single-instance socket is still ours to
/// unlink - after a handoff the successor has rebound that path and owns it.
static GAVE_SOCKET: AtomicBool = AtomicBool::new(false);

/// Did a successor take over our single-instance socket? (See [`GAVE_SOCKET`].)
pub(crate) fn gave_instance_socket() -> bool {
    GAVE_SOCKET.load(Ordering::SeqCst)
}

/// Panes handed to us by a predecessor, with stdusk's saved session as the layout they slot into.
pub(crate) type Incoming = stdusk_core::handoff::Incoming<SavedSession>;

impl Layout for SavedSession {
    /// One pane per leaf across every tab (a tab saved before split-restore has no tree and means a
    /// single pane).
    fn pane_count(&self) -> usize {
        self.tabs
            .iter()
            .map(|t| t.pane.as_ref().map_or(1, crate::session::SavedPane::leaf_count))
            .sum()
    }
}

/// Is a handoff even possible? Only from inside an `.app` bundle: `open` on the bundle is the
/// verified way to get a window, so a dev build (`cargo run`) has no successor to hand anything to
/// and must keep the honest "this terminates your shells" restart.
pub(crate) fn available() -> bool {
    std::env::current_exe().ok().as_deref().and_then(crate::update::bundle_path).is_some()
        && sock_path().is_some()
}

/// Where the handoff socket lives: the runtime dir when set, else the config dir (per-user via
/// `$HOME`, never a world-writable /tmp path - same rule as the single-instance socket). The pid
/// keeps two overlapping restarts from colliding and makes a leftover file obviously stale.
pub(crate) fn sock_path() -> Option<PathBuf> {
    socket_in_runtime_dir(&sock_name("stdusk-handoff", std::process::id()))
}

/// The socket a move to Tidewatch listens on: [`sock_path`]'s rule, a name of its own.
pub(crate) fn tidewatch_sock_path() -> Option<PathBuf> {
    socket_in_runtime_dir(&sock_name("stdusk-to-tidewatch", std::process::id()))
}

fn sock_name(prefix: &str, pid: u32) -> String {
    format!("{prefix}-{pid}.sock")
}

fn socket_in_runtime_dir(name: &str) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(dir).join(name));
    }
    stdusk_core::paths::config_dir().map(|dir| dir.join(name))
}

/// Launch the successor. `open` (not the inner binary) because launchd is what actually yields a
/// window, `-n` because we are still running and would otherwise just activate ourselves, and
/// `--args` to hand it the socket. A `--state-dir` dev instance forwards its isolation so the
/// successor keeps reading the same throwaway config instead of the real install's.
fn spawn_successor(app: &Path, sock: &Path) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new("open");
    cmd.arg("-n").arg("-a").arg(app).arg("--args").arg("--adopt").arg(sock);
    if let Some(dir) = state_dir_arg() {
        cmd.arg("--state-dir").arg(dir);
    }
    let status = cmd.status()?;
    if status.success() {
        return Ok(());
    }
    Err(std::io::Error::other(format!("open exited with {status}")))
}

/// This process's `--state-dir DIR`, if it was launched isolated.
fn state_dir_arg() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter().position(|a| a == "--state-dir").and_then(|i| args.get(i + 1).cloned())
}

impl crate::Stdusk {
    /// Hand this window's LIVE panes to a freshly-launched stdusk.
    ///
    /// `Ok(())` means the successor confirmed every pane is live AND our panes are disarmed: the
    /// caller must then close the window WITHOUT killing anything, and must do nothing else - our
    /// reader threads steal pty bytes from the successor until the process exits.
    ///
    /// `Err(reason)` means NOTHING moved. The shells are still ours, still armed; the caller keeps
    /// the window up. A failed handoff has to look like the restart never happened.
    pub(crate) fn hand_off(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| format!("no exe path ({e})"))?;
        let app = crate::update::bundle_path(&exe).ok_or("not running from an .app bundle")?;
        let path = sock_path().ok_or("no runtime dir for the handoff socket")?;
        let listener =
            listen(&path).map_err(|e| format!("cannot bind the handoff socket ({e})"))?;
        // Snapshot BEFORE the successor launches: same shape the session file gets, so the new
        // window rebuilds the identical layout and just adopts instead of spawning. The agent
        // records are judged first: a status no scan has seen would otherwise be lost with us.
        self.settle_agents();
        let session = self.session_snapshot(ctx);
        spawn_successor(&app, &path).map_err(|e| format!("cannot launch the successor ({e})"))?;
        let sock = accept_within(&listener, LAUNCH_TIMEOUT).map_err(|e| e.to_string());
        let result = sock.and_then(|sock| {
            // `panes` borrows the pane trees (each entry holds a BorrowedFd), so it must be gone
            // before the disarm below can take `&mut self`.
            let panes = self.handoff_panes()?;
            send_session(&sock, &session, &panes).map_err(|e| e.to_string())
        });
        let _ = std::fs::remove_file(&path);
        result?;
        // Confirmed: the successor owns these shells now. Disarm kill()/Drop for every pane, and
        // remember that our single-instance socket went with them.
        for tab in &mut self.tabs {
            for term in tab.root_mut().leaves_mut() {
                term.mark_handed_off();
            }
        }
        GAVE_SOCKET.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Every pane's master fd + metadata, in leaf order across tabs (`Pane::leaves` is A-before-B,
    /// the same order `SavedPane::from_tree` writes the header's trees in). A pane whose fd cannot
    /// be borrowed aborts the whole handoff: half a session is worse than no handoff at all.
    fn handoff_panes(&self) -> Result<Vec<(PaneMeta, BorrowedFd<'_>)>, String> {
        let mut panes = Vec::new();
        for tab in &self.tabs {
            for term in tab.root().leaves() {
                panes.push(capture_pane(term).ok_or("a pane's pty fd is not borrowable")?);
            }
        }
        Ok(panes)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use super::*;
    use crate::session::{SavedPane, SavedTab};

    /// A two-tab session: tab 0 is a Row split (two leaves), tab 1 a single pane. Three leaves in
    /// A-before-B order across tabs - the order the fds must pair with.
    fn three_leaf_session() -> SavedSession {
        SavedSession {
            tabs: vec![
                SavedTab {
                    title: Some("build".into()),
                    color: Some("#e06c75".into()),
                    cwd: Some("/left".into()),
                    pinned: true,
                    pane: Some(SavedPane::Split {
                        dir: crate::session::SavedSplitDir::Row,
                        ratio: 0.5,
                        a: Box::new(SavedPane::Leaf { cwd: Some("/left".into()), agent: None }),
                        b: Box::new(SavedPane::Leaf { cwd: Some("/right".into()), agent: None }),
                    }),
                    repo: None,
                },
                SavedTab {
                    cwd: Some("/solo".into()),
                    pane: Some(SavedPane::Leaf { cwd: Some("/solo".into()), agent: None }),
                    ..Default::default()
                },
            ],
            active: 1,
            window: None,
        }
    }

    #[test]
    fn the_two_handoff_sockets_never_share_a_name() {
        assert_eq!(sock_name("stdusk-handoff", 42), "stdusk-handoff-42.sock");
        assert_eq!(sock_name("stdusk-to-tidewatch", 42), "stdusk-to-tidewatch-42.sock");
    }

    #[test]
    fn expected_panes_counts_leaves_across_tabs() {
        assert_eq!(three_leaf_session().pane_count(), 3);
        assert_eq!(SavedSession::default().pane_count(), 0);
        // A tab saved before split-restore has no tree and means exactly one pane.
        let old = SavedSession {
            tabs: vec![SavedTab { cwd: Some("/tmp".into()), ..Default::default() }],
            ..Default::default()
        };
        assert_eq!(old.pane_count(), 1);
    }

    #[test]
    fn a_saved_session_arrives_whole_with_its_fds_in_leaf_order() {
        // The shared transport carrying stdusk's own layout: the nested tabs and split trees must
        // survive the header encoding, and fd N must still pair with leaf N.
        let (a, b) = UnixStream::pair().unwrap();
        let session = three_leaf_session();
        let pipes: Vec<_> = (0..3).map(|_| std::io::pipe().unwrap()).collect();
        for (n, (_rx, tx)) in pipes.iter().enumerate() {
            let mut tx = tx;
            write!(tx, "pane-{n}").unwrap();
        }
        let sent = session.clone();
        let sender = std::thread::spawn(move || {
            let panes: Vec<(PaneMeta, BorrowedFd<'_>)> = pipes
                .iter()
                .enumerate()
                .map(|(n, (rx, _))| (PaneMeta { cols: 80 + n, ..Default::default() }, rx.as_fd()))
                .collect();
            send_session(&a, &sent, &panes)
        });
        let (got_session, got_panes) =
            stdusk_core::handoff::recv_session::<SavedSession>(&b).unwrap();
        assert_eq!(got_session, session);
        for (n, (m, fd)) in got_panes.into_iter().enumerate() {
            assert_eq!(m.cols, 80 + n);
            let mut buf = [0u8; 6];
            std::fs::File::from(fd).read_exact(&mut buf).unwrap();
            assert_eq!(&buf, format!("pane-{n}").as_bytes(), "pane {n} got the wrong fd");
        }
        drop(b);
        assert!(sender.join().unwrap().is_err(), "no ACK, so the send must not report success");
    }

    // --- LIVE checks (ignored: they run the real binary and open a real window) ----------------
    //
    // No headless harness can cover the receiving half - it only exists in a second PROCESS with a
    // window. Both tests below act as the predecessor (real pty, real socket, real protocol) and
    // let the real binary be the successor; the ACK is the assertion, since only a successor that
    // adopted every pane and built its tabs sends it. Run them after a build:
    //
    //     cargo build && cargo test -- --ignored --nocapture real_
    //
    // Both launch the successor with `--state-dir`, so they can never touch the user's config,
    // session, or single-instance socket.

    /// The cwd the live checks hand over: a real directory, so the successor's own session persist
    /// echoes it back only if the adopted pane really knows where it is.
    const LIVE_CWD: &str = "/usr/share/dict";

    /// The predecessor half of a live check: a real shell on a real pty, the socket bound, the
    /// successor started by `launch`, then the full exchange. `Ok(())` means it acknowledged.
    fn live_handoff(sock_file: &Path, launch: impl FnOnce(&Path)) -> std::io::Result<()> {
        let listener = listen(sock_file)?;
        let opts = stdusk_core::terminal::SpawnOpts {
            detect_progress: false,
            shell_integration: false,
            autosuggestions: false,
            scrollback_lines: 200,
            word_separators: " ".into(),
            bold_bright: false,
            agent_tracking: None,
            cwd: None,
            profile: Some(stdusk_core::shell::ShellProfile {
                shell: Some("/bin/sh".into()),
                args: vec!["-c".into(), "while :; do printf 'tick '; sleep 1; done".into()],
                cwd: None,
                env: std::collections::BTreeMap::new(),
            }),
        };
        // Dropping this at the end of the test kills the shell again - we never mark it handed off,
        // because the successor is about to be killed too.
        let term = stdusk_core::terminal::PtyTerm::spawn(
            80,
            24,
            stdusk_core::repaint::Repaint::none(),
            &opts,
        );
        launch(sock_file);
        let sock = accept_within(&listener, Duration::from_secs(30))?;
        let session = SavedSession {
            tabs: vec![SavedTab {
                title: Some("live".into()),
                pane: Some(SavedPane::Leaf { cwd: Some(LIVE_CWD.into()), agent: None }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let (mut meta, fd) = capture_pane(&term).expect("master fd");
        meta.cwd = Some(LIVE_CWD.into());
        let panes = [(meta, fd)];
        send_session(&sock, &session, &panes)
    }

    /// Poll the successor's OWN session file (`--state-dir` puts it under DIR) for the first tab's
    /// cwd. It is written from the live `PtyTerm::cwd()`, so it is the only externally observable
    /// proof that an adopted pane knows its directory - which is what the tab's name is derived from.
    fn adopted_session_cwd(dir: &Path) -> Option<String> {
        let file = dir.join(".config/stdusk/session.toml");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(body) = std::fs::read_to_string(&file)
                && let Ok(s) = toml::from_str::<SavedSession>(&body)
                && let Some(cwd) = s.tabs.first().and_then(|t| t.cwd.clone())
            {
                return Some(cwd);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// The receiving half against the real binary: `--adopt` parsing, `Incoming::receive`, the
    /// adoption inside `Stdusk::new`, and the ACK that releases the shells. Also the user-visible
    /// half of it: an adopted pane must know its cwd immediately (the shell re-emits OSC 7 only at
    /// its next prompt, and the tab's auto-title is that cwd's basename).
    #[test]
    #[ignore = "launches the real binary and opens a window; run manually after cargo build"]
    fn real_successor_adopts_a_live_shell_and_acknowledges() {
        let dir = std::env::temp_dir().join(format!("stdusk-live-adopt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/stdusk");
        assert!(exe.exists(), "run `cargo build` first: {exe:?}");
        let mut child = None;
        let handed = live_handoff(&dir.join("handoff.sock"), |sock| {
            child = std::process::Command::new(&exe)
                .arg("--adopt")
                .arg(sock)
                .arg("--state-dir")
                .arg(&dir)
                .spawn()
                .ok();
        });
        let cwd = handed.is_ok().then(|| adopted_session_cwd(&dir)).flatten();
        if let Some(mut c) = child {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&dir);
        handed.expect("the successor must acknowledge a live adoption");
        assert_eq!(cwd.as_deref(), Some(LIVE_CWD), "the adopted pane must know its cwd at once");
    }

    /// The LAUNCH line, end to end: a real `.app` bundle started the way `spawn_successor` starts it
    /// (`open -n -a <bundle> --args ...`) while we are still running. Two load-bearing assumptions
    /// live here and both have burned this repo: `open` on the BUNDLE is what yields a window (not
    /// exec'ing the inner binary), and `--args` really does reach the app's argv.
    #[test]
    #[ignore = "launches a real .app through launchd; run manually after cargo build"]
    fn real_bundle_launch_reaches_the_successors_argv() {
        let dir = std::env::temp_dir().join(format!("stdusk-live-bundle-{}", std::process::id()));
        let macos = dir.join("stdusk.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let exe = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/stdusk");
        assert!(exe.exists(), "run `cargo build` first: {exe:?}");
        std::fs::copy(&exe, macos.join("stdusk")).unwrap();
        std::fs::write(
            dir.join("stdusk.app/Contents/Info.plist"),
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>",
                "<key>CFBundleName</key><string>stdusk</string>",
                "<key>CFBundleExecutable</key><string>stdusk</string>",
                "<key>CFBundleIdentifier</key><string>dev.stdusk.livetest</string>",
                "<key>CFBundleShortVersionString</key><string>0.0.0</string>",
                "</dict></plist>",
            ),
        )
        .unwrap();
        let bundle = dir.join("stdusk.app");

        // Mirrors `spawn_successor`, plus the `--state-dir` isolation it would have forwarded from
        // a dev instance's own argv (a test harness cannot carry that flag).
        let handed = live_handoff(&dir.join("handoff.sock"), |sock| {
            let ok = std::process::Command::new("open")
                .arg("-n")
                .arg("-a")
                .arg(&bundle)
                .arg("--args")
                .arg("--adopt")
                .arg(sock)
                .arg("--state-dir")
                .arg(&dir)
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "open must launch the bundle");
        });
        // launchd owns the process, so it is killed by the argv `open` handed it.
        let _ = std::process::Command::new("pkill")
            .arg("-f")
            .arg(bundle.to_string_lossy().to_string())
            .status();
        let _ = std::fs::remove_dir_all(&dir);
        handed.expect("a bundle launched through open must adopt and acknowledge");
    }
}

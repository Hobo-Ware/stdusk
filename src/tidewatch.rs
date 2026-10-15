//! "Move tabs to Tidewatch": hand every live pane of this window to the Tidewatch app, which keeps
//! the shells running. The transport is `stdusk_core::handoff` with an `Export` header (see its
//! docs); this file finds Tidewatch, launches it, and turns the tabs into panes. The window closes
//! only after Tidewatch confirmed every pane, exactly like the restart path in `handoff.rs`.
use std::ffi::OsStr;
use std::os::fd::BorrowedFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use eframe::egui;
use stdusk_core::handoff::export::{Export, ExportPane};
use stdusk_core::handoff::{
    LAUNCH_TIMEOUT, PaneMeta, accept_within, capture_pane, listen, send_session,
};

use crate::ui;

const BUNDLE_ID: &str = "dev.hoboware.tidewatch";
const BUNDLE_NAME: &str = "Tidewatch.app";
const BINARY: &str = "tidewatch";

/// The Tidewatch to launch: its `.app` bundle on macOS, the `tidewatch` binary elsewhere. Spotlight
/// is asked once per process, only when the usual install places miss, so opening the palette
/// never stalls on it.
pub(crate) fn find() -> Option<PathBuf> {
    static SPOTLIGHT: OnceLock<Option<PathBuf>> = OnceLock::new();
    find_from(
        cfg!(target_os = "macos"),
        std::env::var_os("HOME").as_deref().map(Path::new),
        std::env::var_os("PATH").as_deref(),
        Path::exists,
        || SPOTLIGHT.get_or_init(spotlight).clone(),
    )
}

pub(crate) fn installed() -> bool {
    find().is_some()
}

/// [`find`] over injected places, so the search order is testable without a real install.
/// macOS: `/Applications`, then `~/Applications`, then `mdfind`. Elsewhere: `PATH`.
fn find_from(
    macos: bool,
    home: Option<&Path>,
    path_var: Option<&OsStr>,
    exists: impl Fn(&Path) -> bool,
    mdfind: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    if !macos {
        return std::env::split_paths(path_var?).map(|dir| dir.join(BINARY)).find(|p| exists(p));
    }
    let system = PathBuf::from("/Applications").join(BUNDLE_NAME);
    let user = home.map(|h| h.join("Applications").join(BUNDLE_NAME));
    [Some(system), user].into_iter().flatten().find(|p| exists(p)).or_else(mdfind)
}

fn spotlight() -> Option<PathBuf> {
    let query = format!("kMDItemCFBundleIdentifier == '{BUNDLE_ID}'");
    let out = Command::new("mdfind").arg(query).output().ok()?;
    out.status.success().then(|| first_bundle(&String::from_utf8_lossy(&out.stdout))).flatten()
}

/// The first absolute `.app` path in `mdfind` output.
fn first_bundle(stdout: &str) -> Option<PathBuf> {
    stdout
        .lines()
        .map(str::trim)
        .map(Path::new)
        .find(|p| p.is_absolute() && p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
}

/// The command that starts Tidewatch with the handoff socket: `open -n -a` on macOS (launchd is
/// what yields a window, and `-n` because a running Tidewatch would otherwise just be activated),
/// the binary itself elsewhere.
fn launch_command(macos: bool, target: &Path, sock: &Path) -> Command {
    let args: [&OsStr; 2] = [OsStr::new("--from-stdusk"), sock.as_os_str()];
    if macos {
        let mut cmd = Command::new("open");
        cmd.arg("-n").arg("-a").arg(target).arg("--args").args(args);
        cmd
    } else {
        let mut cmd = Command::new(target);
        cmd.args(args);
        cmd
    }
}

fn launch(target: &Path, sock: &Path) -> std::io::Result<()> {
    let mut cmd = launch_command(cfg!(target_os = "macos"), target, sock);
    if cfg!(target_os = "macos") {
        let status = cmd.status()?;
        return if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!("open exited with {status}")))
        };
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    Ok(())
}

/// One pane's entry: a renamed tab's title wins over the pane's own OSC title, and an empty title
/// is no title.
fn export_pane(renamed_title: Option<&str>, meta: &PaneMeta, focused: bool) -> ExportPane {
    let title = renamed_title
        .map(str::to_owned)
        .or_else(|| meta.title_osc.clone())
        .filter(|t| !t.is_empty());
    ExportPane { title, cwd: meta.cwd.clone(), focused }
}

type Captured<'a> = (Export, Vec<(PaneMeta, BorrowedFd<'a>)>);

impl crate::Stdusk {
    /// Entry point for the palette command. On success this window closes WITHOUT killing anything
    /// and the caller must do nothing else; on failure nothing moved and a toast says why.
    pub(crate) fn move_to_tidewatch(&mut self, ctx: &egui::Context) {
        self.quit_confirmed = true;
        match self.hand_to_tidewatch() {
            Ok(()) => {
                self.forget_moved_session(ctx);
                self.session_frozen = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(why) => {
                eprintln!("stdusk: move to Tidewatch aborted ({why})");
                let now = ctx.input(|i| i.time);
                let (msg, _) = ui::ellipsize(&format!("Move to Tidewatch cancelled: {why}"), 90);
                self.toast = Some((msg.replace('\n', " "), now + 5.0));
                self.quit_confirmed = false;
                ctx.request_repaint();
            }
        }
    }

    /// `Ok(())` means Tidewatch confirmed every pane and ours are disarmed. `Err` means nothing
    /// moved: the shells are still ours and still armed.
    fn hand_to_tidewatch(&mut self) -> Result<(), String> {
        let target = find().ok_or("Tidewatch is not installed")?;
        let path =
            crate::handoff::tidewatch_sock_path().ok_or("no runtime dir for the handoff socket")?;
        let listener =
            listen(&path).map_err(|e| format!("cannot bind the handoff socket ({e})"))?;
        let result = self.send_to_tidewatch(&target, &path, &listener);
        let _ = std::fs::remove_file(&path);
        result?;
        for tab in &mut self.tabs {
            for term in tab.root_mut().leaves_mut() {
                term.mark_handed_off();
            }
        }
        Ok(())
    }

    fn send_to_tidewatch(
        &self,
        target: &Path,
        path: &Path,
        listener: &std::os::unix::net::UnixListener,
    ) -> Result<(), String> {
        let (export, panes) = self.live_panes()?;
        launch(target, path).map_err(|e| format!("cannot launch Tidewatch ({e})"))?;
        let sock = accept_within(listener, LAUNCH_TIMEOUT).map_err(|e| e.to_string())?;
        send_session(&sock, &export, &panes).map_err(|e| e.to_string())
    }

    /// Every live pane's master fd and metadata with its `Export` entry, in leaf order across
    /// tabs. A pane whose shell already exited has nothing to hand over and is skipped; one whose
    /// fd cannot be borrowed aborts the move, because half a window is worse than none.
    fn live_panes(&self) -> Result<Captured<'_>, String> {
        let mut export = Export::default();
        let mut panes = Vec::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            let focus = tab.focused_term();
            let title = tab.renamed.then_some(tab.title.as_str());
            for term in tab.root().leaves() {
                if term.exited().is_some() {
                    continue;
                }
                let (meta, fd) = capture_pane(term).ok_or("a pane's pty fd is not borrowable")?;
                let focused = i == self.active && std::ptr::eq(term, focus);
                export.panes.push(export_pane(title, &meta, focused));
                panes.push((meta, fd));
            }
        }
        if panes.is_empty() {
            return Err("no live panes to move".into());
        }
        Ok((export, panes))
    }

    /// The tabs now live in Tidewatch, so the next launch of this app must not reopen them as
    /// fresh shells. Only the window geometry is kept.
    fn forget_moved_session(&self, ctx: &egui::Context) {
        if self.cfg.session.restore && self.screenshot.is_none() {
            let kept = crate::session::SavedSession {
                tabs: Vec::new(),
                active: 0,
                ..self.session_snapshot(ctx)
            };
            crate::session::save_and_wait(&kept);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn args(cmd: &Command) -> Vec<OsString> {
        cmd.get_args().map(OsStr::to_os_string).collect()
    }

    fn only<'a>(present: &'a [&'a str]) -> impl Fn(&Path) -> bool + 'a {
        move |p| present.iter().any(|x| Path::new(x) == p)
    }

    #[test]
    fn macos_prefers_the_system_bundle_then_the_user_one_then_spotlight() {
        let home = Some(Path::new("/Users/me"));
        let system = "/Applications/Tidewatch.app";
        let user = "/Users/me/Applications/Tidewatch.app";
        let spot = || Some(PathBuf::from("/Volumes/x/Tidewatch.app"));
        let find =
            |present: &[&str]| find_from(true, home, Some(OsStr::new("/bin")), only(present), spot);
        assert_eq!(find(&[system, user]), Some(PathBuf::from(system)));
        assert_eq!(find(&[user]), Some(PathBuf::from(user)));
        assert_eq!(find(&[]), Some(PathBuf::from("/Volumes/x/Tidewatch.app")));
    }

    #[test]
    fn macos_asks_spotlight_only_when_the_usual_places_miss() {
        let found = find_from(true, None, None, only(&["/Applications/Tidewatch.app"]), || {
            panic!("mdfind must not run when /Applications has it")
        });
        assert_eq!(found, Some(PathBuf::from("/Applications/Tidewatch.app")));
        assert_eq!(find_from(true, None, None, |_| false, || None), None);
    }

    #[test]
    fn linux_searches_path_in_order_and_never_asks_spotlight() {
        let path = OsString::from("/usr/local/bin:/opt/tw:/usr/bin");
        let find = |present: &[&str]| {
            find_from(false, None, Some(&path), only(present), || panic!("no mdfind off macOS"))
        };
        let both = ["/opt/tw/tidewatch", "/usr/bin/tidewatch"];
        assert_eq!(find(&both), Some(PathBuf::from("/opt/tw/tidewatch")));
        assert_eq!(find(&[]), None);
        assert_eq!(find_from(false, None, None, |_| true, || None), None);
    }

    #[test]
    fn mdfind_output_yields_the_first_absolute_bundle() {
        let out = "\n  /Applications/Tidewatch.app\n/Volumes/x/Tidewatch.app\n";
        assert_eq!(first_bundle(out), Some(PathBuf::from("/Applications/Tidewatch.app")));
        assert_eq!(first_bundle("relative/Tidewatch.app\n/usr/bin/tidewatch\n"), None);
        assert_eq!(first_bundle(""), None);
    }

    #[test]
    fn the_launch_argv_hands_tidewatch_the_socket() {
        let sock = Path::new("/run/user/1/stdusk-to-tidewatch-7.sock");
        let mac = launch_command(true, Path::new("/Applications/Tidewatch.app"), sock);
        assert_eq!(mac.get_program(), "open");
        assert_eq!(
            args(&mac),
            [
                "-n",
                "-a",
                "/Applications/Tidewatch.app",
                "--args",
                "--from-stdusk",
                "/run/user/1/stdusk-to-tidewatch-7.sock"
            ]
        );
        let linux = launch_command(false, Path::new("/usr/bin/tidewatch"), sock);
        assert_eq!(linux.get_program(), "/usr/bin/tidewatch");
        assert_eq!(args(&linux), ["--from-stdusk", "/run/user/1/stdusk-to-tidewatch-7.sock"]);
    }

    #[test]
    fn a_renamed_tab_title_beats_the_osc_title_and_empty_is_none() {
        let osc = PaneMeta {
            title_osc: Some("vim".into()),
            cwd: Some("/p".into()),
            ..Default::default()
        };
        let want = |title: Option<&str>, focused| ExportPane {
            title: title.map(Into::into),
            cwd: Some("/p".into()),
            focused,
        };
        assert_eq!(export_pane(Some("build"), &osc, true), want(Some("build"), true));
        assert_eq!(export_pane(None, &osc, false), want(Some("vim"), false));
        assert_eq!(
            export_pane(
                Some(""),
                &PaneMeta { cwd: Some("/p".into()), ..Default::default() },
                false
            ),
            want(None, false)
        );
    }
}

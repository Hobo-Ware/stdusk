//! Background release check and Homebrew install. Owns the schedule (first check shortly after
//! launch, hourly, and on window focus) and the status the settings page and gear dot read.
//! Installing only swaps the bundle on disk; `update::pending_for_running_exe` then reports it and
//! the user restarts when ready.
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::update::{self, Release};

const FIRST_CHECK_AFTER: Duration = Duration::from_secs(5);
const CHECK_EVERY: Duration = Duration::from_hours(1);
const RETRY_AFTER: Duration = Duration::from_mins(15);
const FOCUS_CHECK_GAP: Duration = Duration::from_mins(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Installing(Release),
    Installed(Release),
    Failed(String),
}

enum Message {
    Checked { result: anyhow::Result<Release>, asked: bool },
    Installed(Release, anyhow::Result<()>),
}

pub(crate) struct Updater {
    pub(crate) status: Status,
    next_check: Instant,
    last_check: Option<Instant>,
    focused: bool,
    tx: Sender<Message>,
    rx: Receiver<Message>,
    ctx: egui::Context,
}

impl Updater {
    pub(crate) fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            status: Status::Idle,
            next_check: Instant::now() + FIRST_CHECK_AFTER,
            last_check: None,
            focused: true,
            tx,
            rx,
            ctx,
        }
    }

    pub(crate) fn check_now(&mut self) {
        self.check(true);
    }

    fn check(&mut self, asked: bool) {
        if matches!(self.status, Status::Checking | Status::Installing(_) | Status::Installed(_)) {
            return;
        }
        if asked {
            self.status = Status::Checking;
        }
        self.next_check = Instant::now() + CHECK_EVERY;
        self.last_check = Some(Instant::now());
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Message::Checked { result: update::latest_release(), asked });
            ctx.request_repaint();
        });
    }

    pub(crate) fn install(&mut self, release: Release) {
        self.status = Status::Installing(release.clone());
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Message::Installed(release, update::brew_upgrade()));
            ctx.request_repaint();
        });
    }

    /// Drive the schedule and apply finished work. Returns true when an install just finished, so
    /// the caller can re-read the bundle on disk right away instead of at its next poll.
    pub(crate) fn tick(&mut self, enabled: bool, auto_install: bool) -> bool {
        let mut installed = false;
        while let Ok(message) = self.rx.try_recv() {
            self.status = match message {
                Message::Checked { result: Ok(release), .. }
                    if update::is_newer(&release.version, update::RUNNING) =>
                {
                    Status::Available(release)
                }
                Message::Checked { result: Ok(_), .. } => Status::UpToDate,
                Message::Checked { result: Err(e), asked: true } => {
                    Status::Failed(format!("{e:#}"))
                }
                Message::Checked { result: Err(_), asked: false } => {
                    self.next_check = Instant::now() + RETRY_AFTER;
                    match &self.status {
                        Status::Checking => Status::Idle,
                        other => other.clone(),
                    }
                }
                Message::Installed(release, Ok(())) => {
                    installed = true;
                    Status::Installed(release)
                }
                Message::Installed(_, Err(e)) => Status::Failed(format!("{e:#}")),
            };
            if let Status::Available(release) = &self.status
                && auto_install
                && update::installed_with_brew()
            {
                let release = release.clone();
                self.install(release);
            }
        }
        let focused = self.ctx.input(|i| i.focused);
        let regained_focus = focused && !self.focused;
        self.focused = focused;
        let checked_lately = self.last_check.is_some_and(|at| at.elapsed() < FOCUS_CHECK_GAP);
        if enabled && (Instant::now() >= self.next_check || (regained_focus && !checked_lately)) {
            self.check(false);
        }
        if enabled {
            self.ctx
                .request_repaint_after(self.next_check.saturating_duration_since(Instant::now()));
        }
        installed
    }

    /// A newer release exists that the user hasn't installed yet.
    pub(crate) fn offered(&self) -> Option<&Release> {
        match &self.status {
            Status::Available(release) | Status::Installing(release) => Some(release),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn updater() -> Updater {
        let mut updater = Updater::new(egui::Context::default());
        updater.status = Status::UpToDate;
        updater
    }

    fn release(version: &str) -> Release {
        Release { version: version.into(), url: String::new() }
    }

    #[test]
    fn a_failed_background_check_stays_quiet_and_retries_soon() {
        let mut updater = updater();
        updater
            .tx
            .send(Message::Checked { result: Err(anyhow::anyhow!("403")), asked: false })
            .unwrap();
        updater.tick(false, false);
        assert_eq!(updater.status, Status::UpToDate);
        let wait = updater.next_check.saturating_duration_since(Instant::now());
        assert!(wait <= RETRY_AFTER && wait > Duration::from_mins(14));
    }

    #[test]
    fn a_failed_check_you_asked_for_says_why() {
        let mut updater = updater();
        updater
            .tx
            .send(Message::Checked { result: Err(anyhow::anyhow!("offline")), asked: true })
            .unwrap();
        updater.tick(false, false);
        assert_eq!(updater.status, Status::Failed("offline".into()));
    }

    #[test]
    fn a_newer_release_is_offered_and_the_current_one_is_not() {
        let mut updater = updater();
        updater.tx.send(Message::Checked { result: Ok(release("999.0.0")), asked: false }).unwrap();
        updater.tick(false, false);
        assert_eq!(updater.offered(), Some(&release("999.0.0")));

        updater
            .tx
            .send(Message::Checked { result: Ok(release(update::RUNNING)), asked: true })
            .unwrap();
        updater.tick(false, false);
        assert_eq!(updater.status, Status::UpToDate);
        assert_eq!(updater.offered(), None);
    }

    #[test]
    fn a_finished_install_is_reported_once() {
        let mut updater = updater();
        updater.tx.send(Message::Installed(release("999.0.0"), Ok(()))).unwrap();
        assert!(updater.tick(false, false));
        assert_eq!(updater.status, Status::Installed(release("999.0.0")));
        assert!(!updater.tick(false, false));
    }

    #[test]
    fn a_failed_install_says_why() {
        let mut updater = updater();
        updater
            .tx
            .send(Message::Installed(release("999.0.0"), Err(anyhow::anyhow!("no brew"))))
            .unwrap();
        assert!(!updater.tick(false, false));
        assert_eq!(updater.status, Status::Failed("no brew".into()));
    }

    #[test]
    fn checks_do_not_overlap_an_install() {
        let mut updater = updater();
        updater.status = Status::Installing(release("999.0.0"));
        updater.check_now();
        assert_eq!(updater.status, Status::Installing(release("999.0.0")));
    }
}

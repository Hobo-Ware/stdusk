//! Shared PTY options and isolated shell homes for tests. Fixtures remove their files on drop.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::shell::ShellProfile;
use crate::terminal::SpawnOpts;

pub fn spawn_opts(shell: &str, args: &[&str]) -> SpawnOpts {
    SpawnOpts {
        detect_progress: false,
        shell_integration: false,
        autosuggestions: false,
        scrollback_lines: 500,
        word_separators: " ".into(),
        bold_bright: false,
        agent_tracking: None,
        cwd: None,
        profile: Some(ShellProfile {
            shell: Some(shell.into()),
            args: args.iter().map(|arg| (*arg).into()).collect(),
            cwd: None,
            env: BTreeMap::new(),
        }),
    }
}

pub struct ShellFixture {
    pub base: PathBuf,
    pub home: PathBuf,
    pub bridge: PathBuf,
}

impl ShellFixture {
    pub fn new(tag: &str) -> Self {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let base = loop {
            let call = CALLS.fetch_add(1, Ordering::Relaxed);
            let base =
                std::env::temp_dir().join(format!("stdusk-{tag}-{}-{call}", std::process::id()));
            match std::fs::create_dir(&base) {
                Ok(()) => break base,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => panic!("cannot create shell fixture: {err}"),
            }
        };
        let home = base.join("home");
        std::fs::create_dir(&home).unwrap();
        Self { bridge: base.join("bridge"), base, home }
    }

    pub fn opts(&self, shell: &str, args: &[&str]) -> SpawnOpts {
        let mut opts = spawn_opts(shell, args);
        opts.scrollback_lines = 100;
        opts.cwd = Some(self.home.to_string_lossy().into_owned());
        opts.profile.as_mut().unwrap().env = BTreeMap::from([
            ("HOME".into(), self.home.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]);
        opts
    }

    pub fn zsh_opts(&self) -> SpawnOpts {
        crate::shell::write_files(&self.bridge, false).unwrap();
        let mut opts = self.opts("/bin/zsh", &[]);
        opts.profile.as_mut().unwrap().env.extend([
            ("ZDOTDIR".into(), self.bridge.to_string_lossy().into_owned()),
            ("STDUSK_REAL_ZDOTDIR".into(), self.home.to_string_lossy().into_owned()),
        ]);
        opts
    }

    pub fn bash_opts(&self) -> SpawnOpts {
        crate::shell::write_files(&self.bridge, false).unwrap();
        // Match the non-login --rcfile launch used by shell::configure.
        let script =
            format!("exec /bin/bash --rcfile '{}' -i", self.bridge.join("bashrc").display());
        let mut opts = self.opts("/bin/sh", &["-c", &script]);
        opts.profile
            .as_mut()
            .unwrap()
            .env
            .insert("BASH_SILENCE_DEPRECATION_WARNING".into(), "1".into());
        opts
    }
}

impl Drop for ShellFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

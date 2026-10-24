//! stdusk's side of settings sync: which folder and files travel.

use stdusk_core::sync::{Files, Op, Spec};
use stdusk_egui::sync::SyncSlot;

const MACHINE_LOCAL: &[&str] = &["session.toml", "shell/"];

fn spec() -> Option<Spec> {
    Some(Spec::new(stdusk_core::paths::config_dir()?, Files::Except(MACHINE_LOCAL), "stdusk"))
}

pub(crate) fn spawn(op: Op, repo: String, slot: &SyncSlot, ctx: egui::Context) {
    stdusk_egui::sync::spawn(spec(), op, repo, slot, ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_local_files_stay_out_of_the_repo() {
        assert_eq!(Files::Except(MACHINE_LOCAL).gitignore(), "session.toml\nshell/\n");
    }
}

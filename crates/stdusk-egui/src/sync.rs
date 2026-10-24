//! Runs a settings sync on a worker thread and hands the result to the frame loop.

use std::sync::{Arc, Mutex};

use stdusk_core::sync::{Op, Outcome, Spec, run};

pub type SyncSlot = Arc<Mutex<Option<(Op, Result<Outcome, String>)>>>;

pub fn spawn(spec: Option<Spec>, op: Op, repo: String, slot: &SyncSlot, ctx: egui::Context) {
    let slot = slot.clone();
    std::thread::spawn(move || {
        let res = spec.ok_or_else(|| "no HOME".to_owned()).and_then(|spec| run(&spec, op, &repo));
        *slot.lock().unwrap() = Some((op, res));
        ctx.request_repaint();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_folder_reports_through_the_slot_and_wakes_the_ui() {
        let slot = SyncSlot::default();
        spawn(None, Op::Push, "repo".into(), &slot, egui::Context::default());
        let got = loop {
            if let Some(got) = slot.lock().unwrap().take() {
                break got;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(got, (Op::Push, Err("no HOME".to_owned())));
    }
}

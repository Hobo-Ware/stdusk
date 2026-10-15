//! The header payload of a move between two apps that embed these crates: stdusk handing its tabs
//! to Tidewatch. It is the [`Layout`] of that exchange, so [`send_session`](super::send_session)
//! and [`recv_session`](super::recv_session) carry it unchanged, with one pane message (and one
//! pty master fd) per entry of [`Export::panes`], in the same order.
//!
//! Unlike an app's own session layout it describes no tabs and no splits: the receiving app decides
//! how to arrange what it adopts. That makes this a cross-app contract. A field may be added only
//! as an `Option` or a `#[serde(default)]` value so an older peer still decodes the header.
use serde::{Deserialize, Serialize};

use super::Layout;

/// Every live pane one app hands to another, in the order the panes travel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Export {
    /// One entry per leaf pane, in leaf order across the sender's tabs (left before right, top
    /// before bottom, tab by tab). Entry N describes the Nth pane message and its fd.
    pub panes: Vec<ExportPane>,
}

/// What the receiving app can show for one handed-over pane before its own shell integration has
/// had a chance to report anything.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPane {
    /// The tab's title when the user renamed it, else the pane's OSC 0/2 window title. `None`
    /// when neither exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The pane's working directory, `None` when the sender could not tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Whether this pane held focus in the sender. At most one entry of an [`Export`] is set.
    #[serde(default)]
    pub focused: bool,
}

impl Layout for Export {
    fn pane_count(&self) -> usize {
        self.panes.len()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, BorrowedFd};
    use std::os::unix::net::UnixStream;

    use super::super::{ACK, PaneMeta, capture_pane, recv_session, send_session, send_text};
    use super::*;
    use crate::repaint::Repaint;
    use crate::terminal::PtyTerm;
    use crate::test_support::spawn_opts;

    fn three_panes() -> Export {
        Export {
            panes: vec![
                ExportPane {
                    title: Some("build".into()),
                    cwd: Some("/left".into()),
                    focused: false,
                },
                ExportPane { title: None, cwd: Some("/right".into()), focused: true },
                ExportPane { title: Some("vim - notes".into()), cwd: None, focused: false },
            ],
        }
    }

    #[test]
    fn the_pane_count_is_the_number_of_entries() {
        assert_eq!(three_panes().pane_count(), 3);
        assert_eq!(Export::default().pane_count(), 0);
    }

    #[test]
    fn an_export_arrives_whole_with_its_fds_in_leaf_order() {
        let (a, b) = UnixStream::pair().unwrap();
        let export = three_panes();
        let pipes: Vec<_> = (0..3).map(|_| std::io::pipe().unwrap()).collect();
        for (n, (_rx, tx)) in pipes.iter().enumerate() {
            let mut tx = tx;
            write!(tx, "pane-{n}").unwrap();
        }
        let sent = export.clone();
        let sender = std::thread::spawn(move || {
            let panes: Vec<(PaneMeta, BorrowedFd<'_>)> = pipes
                .iter()
                .enumerate()
                .map(|(n, (rx, _))| (PaneMeta { cols: 80 + n, ..Default::default() }, rx.as_fd()))
                .collect();
            send_session(&a, &sent, &panes)
        });
        let (got, panes) = recv_session::<Export>(&b).unwrap();
        assert_eq!(got, export);
        assert_eq!(got.panes.iter().position(|p| p.focused), Some(1), "focus must survive");
        for (n, (meta, fd)) in panes.into_iter().enumerate() {
            assert_eq!(meta.cols, 80 + n);
            let mut buf = [0u8; 6];
            std::fs::File::from(fd).read_exact(&mut buf).unwrap();
            assert_eq!(&buf, format!("pane-{n}").as_bytes(), "pane {n} got the wrong fd");
        }
        send_text(&b, ACK).unwrap();
        sender.join().unwrap().expect("an acknowledged export reports success");
    }

    #[test]
    fn live_ptys_travel_in_the_order_the_export_lists_them() {
        let (a, b) = UnixStream::pair().unwrap();
        let export = three_panes();
        let mut terms: Vec<PtyTerm> = (0..3)
            .map(|n| {
                PtyTerm::spawn(
                    80 + n,
                    24,
                    Repaint::none(),
                    &spawn_opts("/bin/sh", &["-c", "sleep 30"]),
                )
            })
            .collect();
        let captured: Vec<_> =
            terms.iter().map(|t| capture_pane(t).expect("a spawned pty has a master fd")).collect();
        let pgids: Vec<_> = captured.iter().map(|(m, _)| m.pgid).collect();
        let (got, got_panes) = std::thread::scope(|s| {
            let sender = s.spawn(|| send_session(&a, &export, &captured));
            let received = recv_session::<Export>(&b).unwrap();
            send_text(&b, ACK).unwrap();
            sender.join().unwrap().expect("an acknowledged export reports success");
            received
        });
        assert_eq!(got, export);
        let geometry: Vec<_> = got_panes.iter().map(|(m, _)| (m.cols, m.pgid)).collect();
        let want: Vec<_> = (0..3).map(|n| (80 + n, pgids[n])).collect();
        assert_eq!(geometry, want, "pane N must pair with entry N");
        drop(got_panes);
        for t in &mut terms {
            t.kill();
        }
    }

    #[test]
    fn an_export_that_disagrees_with_its_panes_is_refused() {
        let (a, _b) = UnixStream::pair().unwrap();
        let (rx, _tx) = std::io::pipe().unwrap();
        let panes = [(PaneMeta::default(), rx.as_fd())];
        assert!(send_session(&a, &three_panes(), &panes).is_err());
    }

    #[test]
    fn absent_optional_fields_stay_off_the_wire_and_decode_back_to_none() {
        let sparse = Export { panes: vec![ExportPane::default()] };
        let text = toml::to_string(&sparse).unwrap();
        assert!(!text.contains("title") && !text.contains("cwd"), "wire: {text}");
        assert_eq!(toml::from_str::<Export>(&text).unwrap(), sparse);
        let old: Export = toml::from_str("[[panes]]\n").unwrap();
        assert_eq!(old, sparse, "a peer that sends only what it knows still decodes");
    }
}

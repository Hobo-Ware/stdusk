//! Session handoff: move LIVE pty master fds from a running stdusk to a freshly-launched one over
//! a unix socket, so updating the app restarts the UI without killing the shells. The shell keeps
//! running because its master fd is never closed - it just changes owner; the child reparents to
//! launchd, which is fine (we track it by process group, not parentage).
//!
//! Wire format, one message per pane: a `u32` big-endian length, that many bytes of JSON metadata,
//! and exactly ONE fd in the `SCM_RIGHTS` control message. One fd per message keeps the ancillary
//! buffer a fixed compile-time size (`cmsg_space!(ScmRights(1))`) - a batched send would need
//! runtime cmsg sizing for no real gain, since a window has a handful of panes.
//!
//! The sender and receiver must run CONCURRENTLY (they're separate processes in practice): a
//! payload larger than the socket buffer (~8K on macOS) blocks in `write_all` until the peer
//! drains it. Pane metadata is far smaller than that, but the property is real - don't "simplify"
//! this into a single-threaded send-then-receive.
//!
//! Everything here goes through `rustix`, whose `sendmsg`/`recvmsg`/`ScmRights` wrappers are SAFE -
//! the crate forbids `unsafe`, so a raw `libc::sendmsg` was never an option.
use std::io::{IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};

/// Cap on a single pane's metadata, so a corrupt or hostile length can't make us allocate wildly.
const MAX_META: usize = 64 * 1024;

/// Send one pane: its `meta` JSON plus the live pty master `fd`.
pub(crate) fn send_pane(sock: &UnixStream, meta: &str, fd: BorrowedFd<'_>) -> std::io::Result<()> {
    let bytes = meta.as_bytes();
    if bytes.len() > MAX_META {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "pane metadata too large",
        ));
    }
    let len = u32::try_from(bytes.len()).expect("checked against MAX_META above");
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    // The fd rides along with the FIRST byte of the message, so the length prefix and the fd must
    // go in the same sendmsg - splitting them would attach the fd to the wrong read.
    let fds = [fd];
    control.push(SendAncillaryMessage::ScmRights(&fds));
    let header = len.to_be_bytes();
    let iov = [IoSlice::new(&header)];
    let sent = sendmsg(sock, &iov, &mut control, SendFlags::empty())?;
    if sent != header.len() {
        return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "short handoff header"));
    }
    // The payload itself carries no ancillary data, so a plain write (which handles partial
    // writes) is correct here.
    (&mut { sock }).write_all(bytes)
}

/// Receive one pane sent by [`send_pane`]: its metadata and the adopted master fd.
pub(crate) fn recv_pane(sock: &UnixStream) -> std::io::Result<(String, OwnedFd)> {
    let mut header = [0u8; 4];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let mut iov = [IoSliceMut::new(&mut header)];
    let got = recvmsg(sock, &mut iov, &mut control, RecvFlags::empty())?;
    if got.bytes != header.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "truncated handoff header",
        ));
    }
    let fd = control
        .drain()
        .find_map(|msg| match msg {
            RecvAncillaryMessage::ScmRights(mut fds) => fds.next(),
            _ => None,
        })
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "handoff message carried no fd")
        })?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_META {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "pane metadata too large",
        ));
    }
    let mut buf = vec![0u8; len];
    (&mut { sock }).read_exact(&mut buf)?;
    let meta = String::from_utf8(buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok((meta, fd))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::AsFd;

    use super::*;

    #[test]
    fn passed_fd_is_the_same_open_file() {
        // The mechanism, proven on a pipe (no pty needed): send the READ end across, then write to
        // the write end and read it back through the RECEIVED fd. If SCM_RIGHTS didn't really
        // transfer the description, this read would see nothing.
        let (a, b) = UnixStream::pair().unwrap();
        let (rx, mut tx) = std::io::pipe().unwrap();

        send_pane(&a, r#"{"cwd":"/tmp","pgid":42}"#, rx.as_fd()).unwrap();
        let (meta, adopted) = recv_pane(&b).unwrap();
        assert_eq!(meta, r#"{"cwd":"/tmp","pgid":42}"#);

        tx.write_all(b"through-the-fd").unwrap();
        drop(tx);
        let mut out = String::new();
        std::fs::File::from(adopted).read_to_string(&mut out).unwrap();
        assert_eq!(out, "through-the-fd");
    }

    #[test]
    fn metadata_survives_a_payload_larger_than_the_socket_buffer() {
        // The fd rides with the 4-byte header and the payload follows on the stream, so a payload
        // bigger than one read must still arrive whole. Sent from a THREAD because a payload that
        // exceeds the socket buffer (~8K on macOS) blocks in `write_all` until someone drains it -
        // which mirrors the real shape, where the receiver is a separate process reading live.
        let (a, b) = UnixStream::pair().unwrap();
        let (rx, _tx) = std::io::pipe().unwrap();
        let big = format!(r#"{{"pad":"{}"}}"#, "x".repeat(32 * 1024));

        let sent = big.clone();
        let sender = std::thread::spawn(move || send_pane(&a, &sent, rx.as_fd()).unwrap());
        let (meta, _fd) = recv_pane(&b).unwrap();
        sender.join().unwrap();
        assert_eq!(meta, big);
    }

    #[test]
    fn oversized_metadata_is_rejected_not_allocated() {
        let (a, _b) = UnixStream::pair().unwrap();
        let (rx, _tx) = std::io::pipe().unwrap();
        let too_big = "x".repeat(MAX_META + 1);
        assert!(send_pane(&a, &too_big, rx.as_fd()).is_err());
    }
}

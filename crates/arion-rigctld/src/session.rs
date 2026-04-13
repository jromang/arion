//! Per-connection TCP session.
//!
//! Reads newline-delimited commands, parses them, forwards each as a
//! [`RigRequest`] to the UI thread over an `mpsc` channel, and waits
//! for the reply (per-request `sync_channel(1)`) before writing it
//! back to the client. The session ends on `q` / `\quit`, EOF, I/O
//! error, or when the global stop flag flips.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crate::protocol::{format_reply, parse_line};
use crate::reply::RigReply;
use crate::RigRequest;

pub(crate) fn run_session(
    stream: TcpStream,
    cmd_tx: mpsc::Sender<RigRequest>,
    stop: Arc<AtomicBool>,
) {
    let peer = stream.peer_addr().ok();
    tracing::debug!(?peer, "rigctld session opened");

    // Short read timeout so we can poll the stop flag between reads.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));

    let write_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "rigctld: failed to clone stream");
            return;
        }
    };
    let mut writer = write_stream;
    let mut reader = BufReader::new(stream);

    let mut line = String::new();
    while !stop.load(Ordering::Relaxed) {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => {
                tracing::debug!(error = %e, "rigctld: read error, closing session");
                break;
            }
        }

        let first_tok = line.trim().trim_start_matches('+').split_whitespace().next().unwrap_or("");
        let is_quit = matches!(first_tok, "q" | "Q" | "\\quit");
        let (cmd, extended) = parse_line(&line);

        let (tx, rx) = mpsc::sync_channel::<RigReply>(1);
        let req = RigRequest { cmd, reply: tx };
        if cmd_tx.send(req).is_err() {
            tracing::warn!("rigctld: UI side dropped the command channel");
            break;
        }

        let reply = match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(r) => r,
            Err(_) => RigReply::Error(-1),
        };

        let wire = format_reply(&reply, extended);
        if writer.write_all(wire.as_bytes()).is_err() {
            break;
        }
        // For Value / KeyValues replies we also need a RPRT terminator
        // on plain/extended lines — the rigctld protocol expects every
        // successful response to end with `RPRT 0`. We only skipped it
        // in Value/KeyValues for brevity; append here.
        let needs_rprt = matches!(
            reply,
            RigReply::Value(_) | RigReply::KeyValues(_) | RigReply::Raw(_)
        );
        if needs_rprt && writer.write_all(b"RPRT 0\n").is_err() {
            break;
        }

        if is_quit {
            break;
        }
    }
    tracing::debug!(?peer, "rigctld session closed");
}

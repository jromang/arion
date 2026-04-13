//! Hamlib `rigctld`-compatible TCP server for Arion.
//!
//! This crate is an adapter around [`arion_app::App`] that lets
//! Hamlib-aware clients (WSJT-X, fldigi, GPredict, CQRLOG, …) drive
//! the radio over a TCP socket. It speaks a subset of the rigctl
//! protocol — enough for QSY + mode + volume control.
//!
//! Threading model:
//!
//! ```text
//!   TCP client ─┐
//!   TCP client ─┼─ session thread ──┐
//!   TCP client ─┘                   │    mpsc::Sender<RigRequest>
//!                                   ├──────────────────────────────▶  UI thread
//!                 acceptor thread   │         drain(&mut app, &rx)
//!                                   │    mpsc::SyncSender<RigReply>
//!                                   └──────────────────────────────◀  (per-request)
//! ```
//!
//! The UI thread owns the `App` and drains the request channel once
//! per frame via [`drain`]. Each session thread blocks on its own
//! per-request `sync_channel(1)` reply channel, so the UI never has
//! to hold onto request state across frames.

#![forbid(unsafe_code)]

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use arion_app::App;

pub mod commands;
pub mod error;
pub mod protocol;
pub mod reply;
mod session;

pub use commands::RigCommand;
pub use error::RigctldError;
pub use reply::RigReply;

/// One request crossing the TCP-thread → UI-thread boundary. The
/// session holds the `reply` sender; the UI replies by sending a
/// single [`RigReply`] and then drops the sender.
pub struct RigRequest {
    pub cmd: Box<dyn RigCommand>,
    pub reply: mpsc::SyncSender<RigReply>,
}

impl std::fmt::Debug for RigRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RigRequest").field("cmd", &self.cmd).finish()
    }
}

/// Handle returned by [`RigctldHandle::start`]. Dropping it (or calling
/// [`RigctldHandle::stop`]) flips the stop flag, unblocks the acceptor,
/// and joins all outstanding threads.
pub struct RigctldHandle {
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
    acceptor: Option<JoinHandle<()>>,
    sessions: Arc<std::sync::Mutex<Vec<JoinHandle<()>>>>,
}

impl RigctldHandle {
    /// Start the server. Returns once the acceptor is listening, so
    /// callers can immediately connect or advertise the port.
    ///
    /// The caller must keep draining `cmd_rx` from the UI thread via
    /// [`drain`] — otherwise session threads will block on reply and
    /// the server will appear hung.
    pub fn start(
        addr: SocketAddr,
        cmd_tx: mpsc::Sender<RigRequest>,
    ) -> Result<Self, RigctldError> {
        let listener = TcpListener::bind(addr)?;
        let local = listener.local_addr()?;
        listener.set_nonblocking(false)?;

        let stop = Arc::new(AtomicBool::new(false));
        let sessions: Arc<std::sync::Mutex<Vec<JoinHandle<()>>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        let stop_a = stop.clone();
        let sessions_a = sessions.clone();
        let acceptor = thread::Builder::new()
            .name("rigctld-acceptor".into())
            .spawn(move || {
                let _ = listener.set_nonblocking(true);
                while !stop_a.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let stop_s = stop_a.clone();
                            let tx = cmd_tx.clone();
                            let handle = thread::Builder::new()
                                .name("rigctld-session".into())
                                .spawn(move || session::run_session(stream, tx, stop_s));
                            match handle {
                                Ok(h) => {
                                    if let Ok(mut g) = sessions_a.lock() {
                                        g.retain(|j| !j.is_finished());
                                        g.push(h);
                                    }
                                }
                                Err(e) => tracing::warn!(error = %e, "rigctld: session spawn"),
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(100));
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "rigctld: accept error");
                            break;
                        }
                    }
                }
                tracing::debug!("rigctld acceptor exiting");
            })
            .map_err(|e| RigctldError::Io(std::io::Error::other(format!("spawn: {e}"))))?;

        tracing::info!(addr = %local, "rigctld server started");

        Ok(RigctldHandle {
            stop,
            addr: local,
            acceptor: Some(acceptor),
            sessions,
        })
    }

    /// Address the server is actually bound to (resolved if the caller
    /// passed port 0 to let the OS choose).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop the server and join all threads. Equivalent to `drop(self)`.
    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.acceptor.take() {
            let _ = h.join();
        }
        if let Ok(mut g) = self.sessions.lock() {
            for h in g.drain(..) {
                let _ = h.join();
            }
        }
    }
}

impl Drop for RigctldHandle {
    fn drop(&mut self) {
        if !self.stop.load(Ordering::Relaxed) {
            self.stop_inner();
        }
    }
}

/// Drain pending rigctld commands against `app`. Called from the UI
/// thread's per-frame tick. Processes at most `max_per_frame` requests
/// so a burst of traffic can't starve the render loop.
pub fn drain(app: &mut App, rx: &mpsc::Receiver<RigRequest>) {
    drain_with_limit(app, rx, 64);
}

pub fn drain_with_limit(
    app: &mut App,
    rx: &mpsc::Receiver<RigRequest>,
    max_per_frame: usize,
) {
    for _ in 0..max_per_frame {
        match rx.try_recv() {
            Ok(req) => {
                let reply = req.cmd.execute(app);
                let _ = req.reply.send(reply);
            }
            Err(_) => break,
        }
    }
}

// --- Integration tests ---------------------------------------------------

#[cfg(test)]
mod session_tests {
    use super::*;
    use arion_app::{App, AppOptions};
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;

    fn start_test_server() -> (RigctldHandle, mpsc::Receiver<RigRequest>, SocketAddr) {
        let (tx, rx) = mpsc::channel::<RigRequest>();
        let handle = RigctldHandle::start("127.0.0.1:0".parse().unwrap(), tx).unwrap();
        let addr = handle.addr();
        (handle, rx, addr)
    }

    /// Pump the drain in a background thread against a fresh `App`.
    /// Returns a stop flag the test can flip to kill the pump cleanly.
    fn start_pump(rx: mpsc::Receiver<RigRequest>) -> Arc<AtomicBool> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();
        thread::spawn(move || {
            let mut app = App::new(AppOptions::default());
            while !stop_clone.load(Ordering::Relaxed) {
                drain(&mut app, &rx);
                thread::sleep(Duration::from_millis(10));
            }
        });
        stop
    }

    #[test]
    fn session_handles_freq_round_trip() {
        let (handle, rx, addr) = start_test_server();
        let pump_stop = start_pump(rx);

        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"F 14074000\n").unwrap();

        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "RPRT 0");

        s.write_all(b"f\n").unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "14074000");
        line.clear();
        r.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "RPRT 0");

        s.write_all(b"q\n").unwrap();
        drop(s);

        pump_stop.store(true, Ordering::Relaxed);
        handle.stop();
    }

    #[test]
    fn session_unknown_command_errors() {
        let (handle, rx, addr) = start_test_server();
        let pump_stop = start_pump(rx);

        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"nosuchverb\n").unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "RPRT -11");

        s.write_all(b"q\n").unwrap();
        drop(s);

        pump_stop.store(true, Ordering::Relaxed);
        handle.stop();
    }

    #[test]
    fn session_dump_state_emits_body() {
        let (handle, rx, addr) = start_test_server();
        let pump_stop = start_pump(rx);

        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(b"\\dump_state\n").unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut first = String::new();
        r.read_line(&mut first).unwrap();
        assert_eq!(first.trim(), "0");

        s.write_all(b"q\n").unwrap();
        drop(s);

        pump_stop.store(true, Ordering::Relaxed);
        handle.stop();
    }
}

//! In-process fake HermesLite 2 radio.
//!
//! Binds a UDP socket on `127.0.0.1:0` (ephemeral port), spawns a worker
//! thread, and speaks just enough of HPSDR Protocol 1 to be useful:
//!
//! - Responds to discovery requests (`0xEF 0xFE 0x02 …`) with a reply that
//!   identifies the radio as a HermesLite 2 at that loopback address.
//! - Acknowledges the Start command (`0xEF 0xFE 0x04 0x01`) by beginning
//!   to emit synthetic 1032-byte Metis data frames back to whichever peer
//!   sent the Start. The first stream is shaped at ~1 ms per packet which
//!   is slow enough not to peg a core in CI but fast enough for any test
//!   to collect thousands of samples in a fraction of a second.
//! - Acknowledges the Stop command (`0xEF 0xFE 0x04 0x00`) by pausing the
//!   stream.
//!
//! Synthetic IQ pattern: sample `n` of packet `seq` is
//! `I = seq as f32 / 10_000`, `Q = n as f32 / 10_000`, `mic = 0`. Tests
//! use this to assert that ordering is preserved end-to-end, not to
//! exercise a realistic radio signal.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use hpsdr_protocol::{
    control::CommandFrame,
    metis::SAMPLES_SECTION_LEN,
    Endpoint, IqSample, MetisPacket, UsbFrame,
};

const MOCK_MAC: [u8; 6] = [0x00, 0x1C, 0xC0, 0xA8, 0x42, 0x01];

/// Running mock radio. Dropping it signals the worker thread to exit and
/// joins it, so tests don't need to cleanup explicitly.
pub struct MockHl2 {
    addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MockHl2 {
    /// Bind on an ephemeral loopback port and start the worker.
    pub fn spawn() -> io::Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))?;
        socket.set_nonblocking(true)?;
        let addr = socket.local_addr()?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_inner = Arc::clone(&shutdown);

        let thread = thread::Builder::new()
            .name("mock-hl2".into())
            .spawn(move || worker(socket, shutdown_inner))?;

        Ok(MockHl2 {
            addr,
            shutdown,
            thread: Some(thread),
        })
    }

    /// Loopback address (with ephemeral port) to pass to
    /// [`crate::discover`] or [`crate::Session::start`].
    pub fn address(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for MockHl2 {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn worker(socket: UdpSocket, shutdown: Arc<AtomicBool>) {
    let mut buf = [0u8; 2048];
    let mut streaming_to: Option<SocketAddr> = None;
    let mut seq: u32 = 0;

    while !shutdown.load(Ordering::Acquire) {
        match socket.recv_from(&mut buf) {
            Ok((n, from)) => handle_command(&socket, &buf[..n], from, &mut streaming_to),
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }

        if let Some(client) = streaming_to {
            let packet = build_data_packet(seq);
            // Ignore send errors: the peer may have gone away without
            // sending Stop, which is fine for a test fake.
            let _ = socket.send_to(&packet, client);
            seq = seq.wrapping_add(1);
            // ~1 ms between packets ≈ 1000 packets/sec ≈ 126 k samples/sec,
            // which is more than enough headroom above 48 k.
            thread::sleep(Duration::from_millis(1));
        } else {
            // Idle — don't spin.
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn handle_command(
    socket: &UdpSocket,
    data: &[u8],
    from: SocketAddr,
    streaming_to: &mut Option<SocketAddr>,
) {
    if data.len() < 3 || data[0] != 0xEF || data[1] != 0xFE {
        return;
    }
    match data[2] {
        // Discovery request.
        0x02 => {
            let reply = build_discovery_reply();
            let _ = socket.send_to(&reply, from);
        }
        // Start / stop command packet.
        0x04 if data.len() >= 4 => {
            match data[3] {
                0x01 => *streaming_to = Some(from),
                0x00 => *streaming_to = None,
                _ => {}
            }
        }
        // Data packet from the client (control frames). We ignore
        // payloads in the mock — tests care about the RX path only.
        0x01 => {}
        _ => {}
    }
}

fn build_discovery_reply() -> [u8; 60] {
    let mut buf = [0u8; 60];
    buf[0] = 0xEF;
    buf[1] = 0xFE;
    buf[2] = 0x02; // idle
    buf[3..9].copy_from_slice(&MOCK_MAC);
    buf[9]  = 0x49; // firmware code version (arbitrary)
    buf[10] = 6;    // HermesLite
    buf[14] = 0;
    buf[15] = 0;
    buf[16] = 0;
    buf[17] = 0;
    buf[18] = 0;    // penny
    buf[19] = 0;    // metis
    buf[20] = 1;    // num RXs
    buf
}

fn build_data_packet(seq: u32) -> [u8; 1032] {
    let mut frame0 = empty_frame();
    let mut frame1 = empty_frame();

    frame0.fill_samples((0..63).map(move |n| IqSample {
        i: (seq as f32) / 10_000.0,
        q: (n as f32) / 10_000.0,
        mic: 0,
    }));
    // Frame 1 uses a slightly different pattern so tests can distinguish
    // the two halves of a Metis packet if they want to.
    frame1.fill_samples((63..126).map(move |n| IqSample {
        i: (seq as f32) / 10_000.0,
        q: (n as f32) / 10_000.0,
        mic: 0,
    }));

    // A real HL2 sends RX data on endpoint 6 (RadioRxAndStatus), NOT on
    // endpoint 2. The mock mirrors that so integration tests exercise the
    // same code path the real hardware does.
    let packet = MetisPacket {
        endpoint: Endpoint::RadioRxAndStatus,
        sequence: seq,
        frame0,
        frame1,
    };
    packet.encode()
}

fn empty_frame() -> UsbFrame {
    UsbFrame {
        command: CommandFrame::default(),
        samples: [0u8; SAMPLES_SECTION_LEN],
    }
}

//! Running RX session against a single Protocol 1 radio.
//!
//! Design:
//! - One UDP socket, one dedicated RX thread that `recv`s Metis packets,
//!   decodes them, and pushes the 63 × 2 = 126 [`IqSample`]s into an
//!   `rtrb` SPSC ring the caller drains from its DSP thread.
//! - One control thread that owns frequency / mode / sample-rate state
//!   and sends a command frame whenever the caller asks for a change. We
//!   do *not* spam the radio with a continuous TX stream in phase A — HL2
//!   happily emits data with no host-side TX traffic once `Start` is
//!   acknowledged; all we need is an out-of-band mechanism to push C&C
//!   updates.
//! - An `AtomicBool` plus a blocking `recv_timeout` on the socket give us
//!   a clean shutdown path.
//! - A shared `Mutex<SessionStatus>` tracks packet counts + last-rx time
//!   so the UI / watchdog can tell whether the link is still alive.
//!
//! Phase A scope: one RX, no TX, 48 kHz fixed. Extending this to multi-RX
//! and TX lands in phase B.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver as MpscRx, Sender as MpscTx};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hpsdr_protocol::{
    control::{register, CommandFrame, StartCommand, StopCommand},
    Endpoint, IqSample, MetisPacket, UsbFrame, METIS_PACKET_LEN, MAX_RX,
};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::NetError;

/// Configuration for [`Session::start`].
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Radio UDP endpoint, typically `<radio ip>:1024`.
    pub radio_addr: SocketAddr,
    /// Number of simultaneous receivers to enable. HL2 supports up to
    /// 2; larger radios (Saturn / ANAN-G2) support more. Must be in
    /// `1..=MAX_RX`.
    pub num_rx: u8,
    /// Initial tuned frequency of each RX in Hz. Only the first
    /// `num_rx` entries are meaningful on the wire.
    pub rx_frequencies: [u32; MAX_RX],
    /// Sample rate index as encoded in register 0 `C1 & 0x03`
    /// (0 = 48 kHz, 1 = 96 kHz, 2 = 192 kHz, 3 = 384 kHz).
    pub sample_rate_index: u8,
    /// Size of each per-RX IQ ring buffer in samples. Must fit at
    /// least one burst of incoming data at the chosen sample rate.
    /// Defaults to 16384 (~340 ms of headroom at 48 kHz, per RX).
    pub ring_capacity: usize,
    /// How long [`Session::start`] waits for the first data packet after
    /// issuing the Start command before giving up.
    pub start_timeout: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        let mut rx_frequencies = [0u32; MAX_RX];
        rx_frequencies[0] = 7_074_000;
        SessionConfig {
            radio_addr:        "127.0.0.1:1024".parse().unwrap(),
            num_rx:            1,
            rx_frequencies,
            sample_rate_index: 0,
            ring_capacity:     16_384,
            start_timeout:     Duration::from_millis(1_500),
        }
    }
}

/// Commands the owning task can push into a running session.
#[derive(Debug, Clone, Copy)]
pub enum SessionCommand {
    /// Retune one of the receivers (`rx` is a 0-based RX index).
    SetRxFrequency { rx: u8, hz: u32 },
    /// Change the sample rate (encoded value 0..=3).
    SetSampleRateIndex(u8),
}

/// Live statistics / health report. Snapshotted on every call to
/// [`Session::status`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionStatus {
    pub packets_received: u64,
    pub samples_received: u64,
    pub sequence_errors: u64,
    pub last_packet_at: Option<Instant>,
    pub running: bool,
}

impl SessionStatus {
    /// Heuristic connection check used by the UI / watchdog: the link is
    /// considered alive if we've seen a data packet in the last second.
    pub fn is_connected(&self, now: Instant) -> bool {
        match self.last_packet_at {
            Some(t) => now.saturating_duration_since(t) < Duration::from_secs(1),
            None    => false,
        }
    }
}

struct SharedState {
    status: Mutex<SessionStatus>,
    shutdown: AtomicBool,
}

/// A running RX session. Dropping it stops the threads and closes the
/// socket; call [`Session::stop`] explicitly if you want to check the
/// stop command was acknowledged.
pub struct Session {
    shared: Arc<SharedState>,
    rx_thread: Option<JoinHandle<()>>,
    ctrl_thread: Option<JoinHandle<()>>,
    command_tx: MpscTx<SessionCommand>,
    radio_addr: SocketAddr,
    socket: Arc<UdpSocket>,
}

impl Session {
    /// Open a UDP socket aimed at the given radio, run the HL2 start
    /// handshake (initial C&C frame → Start command → wait for first data
    /// frame), and spawn the RX + control threads. Returns the handle and
    /// an SPSC consumer the caller drains into its DSP stage.
    ///
    /// The handshake order matches upstream `networkproto1.c`:
    ///
    /// 1. Push one C&C data packet with register 0 (config/sample rate)
    ///    in USB frame 0 and register 2 (RX1 NCO) in USB frame 1. Before
    ///    this the HL2 has no idea what frequency we want and ignores the
    ///    Start command.
    /// 2. Send the Start command (`0xEF 0xFE 0x04 0x01`).
    /// 3. Wait up to 50 ms for a data frame. If nothing arrives, repeat
    ///    steps 1–2 up to five times.
    pub fn start(
        config: SessionConfig,
    ) -> Result<(Self, Vec<Consumer<IqSample>>), NetError> {
        let num_rx = config.num_rx as usize;
        if num_rx == 0 || num_rx > MAX_SESSION_RX {
            return Err(NetError::Io(io::Error::other(format!(
                "num_rx must be in 1..={MAX_SESSION_RX}, got {num_rx}"
            ))));
        }
        // We deliberately do NOT call `connect()` on this socket. On Linux
        // that would filter incoming packets to the exact `(radio_ip,
        // 1024)` 5-tuple, which is correct once streaming starts, but
        // hides anything useful during handshake debugging. We use
        // `send_to` / `recv_from` until the first data frame shows up, so
        // any stray reply (including wrong-port / wrong-size packets) is
        // visible to the tracing logs.
        //
        // Buffer sizes: upstream `nativeInitMetis` cranks SO_SNDBUF and
        // SO_RCVBUF to ~1 MB because a cold DSP pipeline can stall for
        // a few ms at a time and drop packets with the default 256 KB.
        // We do the same via `socket2`.
        let sock2 = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        sock2.set_recv_buffer_size(1 << 20)?;
        sock2.set_send_buffer_size(1 << 20)?;
        sock2.bind(
            &std::net::SocketAddr::from((
                std::net::Ipv4Addr::UNSPECIFIED,
                0,
            ))
            .into(),
        )?;
        let socket: UdpSocket = sock2.into();
        socket.set_read_timeout(Some(Duration::from_millis(500)))?;
        let local_port = socket.local_addr()?.port();
        tracing::info!(
            target = %config.radio_addr,
            local_port,
            "session socket bound"
        );

        let socket = Arc::new(socket);

        // One ring per RX so consumers can drive independent DSP chains
        // without having to demux on their side.
        let mut producers = Vec::with_capacity(num_rx);
        let mut consumers = Vec::with_capacity(num_rx);
        for _ in 0..num_rx {
            let (p, c) = RingBuffer::<IqSample>::new(config.ring_capacity);
            producers.push(p);
            consumers.push(c);
        }

        let shared = Arc::new(SharedState {
            status: Mutex::new(SessionStatus {
                running: true,
                ..SessionStatus::default()
            }),
            shutdown: AtomicBool::new(false),
        });

        // --- Handshake -------------------------------------------------
        //
        // We drive the handshake in this thread (no RX worker yet) so we
        // can read the first reply synchronously. Once we've seen a data
        // frame we hand the socket off to the RX worker.
        let attempts = perform_handshake(
            &socket,
            config.radio_addr,
            config.sample_rate_index,
            num_rx,
            &config.rx_frequencies[..num_rx],
            config.start_timeout,
        )?;
        tracing::info!(attempts, num_rx, "HL2 start handshake succeeded");

        // Relax the recv timeout now that we're streaming.
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;

        // --- Threads --------------------------------------------------
        let rx_thread = {
            let shared = Arc::clone(&shared);
            let socket = Arc::clone(&socket);
            let rx_producers = producers;
            thread::Builder::new()
                .name("hpsdr-rx".into())
                .spawn(move || rx_loop(socket, shared, rx_producers, num_rx))
                .map_err(|e| NetError::Io(io::Error::other(e.to_string())))?
        };

        let (command_tx, command_rx) = mpsc::channel::<SessionCommand>();
        let ctrl_thread = {
            let shared = Arc::clone(&shared);
            let socket = Arc::clone(&socket);
            let radio_addr = config.radio_addr;
            // Start the TX sequence number after the handshake's
            // pre-start packets. The handshake sent `num_rx + 1`
            // packets (TX NCO + one per RX NCO) per attempt; we don't
            // know how many attempts it took, so we play it safe with
            // a high-ish starting seq. The radio doesn't check inbound
            // sequence numbers anyway.
            let initial_tx_seq = (num_rx as u32 + 1) * 5;
            let initial = TxState {
                sample_rate_index: config.sample_rate_index,
                num_rx:            num_rx as u8,
                rx_frequencies:    config.rx_frequencies,
            };
            thread::Builder::new()
                .name("hpsdr-tx".into())
                .spawn(move || tx_loop(socket, radio_addr, shared, command_rx, initial, initial_tx_seq))
                .map_err(|e| NetError::Io(io::Error::other(e.to_string())))?
        };

        Ok((
            Session {
                shared,
                rx_thread:   Some(rx_thread),
                ctrl_thread: Some(ctrl_thread),
                command_tx,
                radio_addr:  config.radio_addr,
                socket,
            },
            consumers,
        ))
    }

    /// Retune one of the receivers. The change is applied by the TX
    /// thread on its next iteration through the C&C rotation.
    pub fn set_rx_frequency(&self, rx: u8, hz: u32) -> Result<(), NetError> {
        self.command_tx
            .send(SessionCommand::SetRxFrequency { rx, hz })
            .map_err(|_| NetError::AlreadyStopped)
    }

    /// Convenience wrapper that retunes RX1 (index 0).
    pub fn set_rx1_frequency(&self, hz: u32) -> Result<(), NetError> {
        self.set_rx_frequency(0, hz)
    }

    /// Change the sample rate. Takes effect on the next control frame.
    pub fn set_sample_rate_index(&self, idx: u8) -> Result<(), NetError> {
        self.command_tx
            .send(SessionCommand::SetSampleRateIndex(idx & 0x03))
            .map_err(|_| NetError::AlreadyStopped)
    }

    /// Get a snapshot of the current session stats.
    pub fn status(&self) -> SessionStatus {
        *self.shared.status.lock().unwrap()
    }

    /// Radio address this session is talking to.
    pub fn radio_addr(&self) -> SocketAddr {
        self.radio_addr
    }

    /// Send the Stop command, mark the session as shut down, and join
    /// both worker threads.
    pub fn stop(mut self) -> Result<(), NetError> {
        self.do_stop()
    }

    fn do_stop(&mut self) -> Result<(), NetError> {
        if self.shared.shutdown.swap(true, Ordering::AcqRel) {
            return Ok(()); // already stopped
        }
        let _ = self.socket.send_to(&StopCommand.encode(), self.radio_addr);
        if let Some(t) = self.rx_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.ctrl_thread.take() {
            let _ = t.join();
        }
        self.shared.status.lock().unwrap().running = false;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.do_stop();
    }
}

/// Number of receivers this crate's Session implementation supports.
/// `hpsdr-protocol::MAX_RX` is 8 to give the wire-format maths a
/// power-of-2 array, but only 7 RX NCO register addresses are
/// defined on HPSDR P1 (0x02..0x08); slot 0x09 is already claimed by
/// Alex. HL2 only goes to 2, ANAN-G2 / Saturn go up to 7.
pub const MAX_SESSION_RX: usize = 7;

/// Ordered list of per-RX NCO registers (register 2 = RX1, register 3
/// = RX2, etc.).
const RX_NCO_REGISTERS: [u8; MAX_SESSION_RX] = [
    register::RX1_NCO,
    register::RX2_NCO,
    register::RX3_NCO,
    register::RX4_NCO,
    register::RX5_NCO,
    register::RX6_NCO,
    register::RX7_NCO,
];

/// Perform the pre-streaming handshake against a fresh UDP socket.
///
/// Mirrors `SendStartToMetis` in upstream `networkproto1.c`, extended to
/// prime every RX NCO the session has configured (not just RX1):
///
/// ```text
/// loop up to 5 times {
///     send [config reg 0] + [reg 1 TX NCO = 0]
///     sleep 10 ms
///     for r in 0..num_rx {
///         send [config reg 0] + [reg (2+r) RXr NCO = rx_freqs[r]]
///         sleep 10 ms
///     }
///     send Start command
///     recv_from with 500 ms deadline
///     if got a data frame, break
/// }
/// ```
///
/// The extra RX NCO packets are critical for multi-RX sessions: without
/// them DDC1 has no tuned frequency and HL2 streams garbage on its
/// samples.
fn perform_handshake(
    socket: &UdpSocket,
    radio_addr: std::net::SocketAddr,
    sample_rate_index: u8,
    num_rx: usize,
    rx_frequencies: &[u32],
    total_timeout: Duration,
) -> Result<u32, NetError> {
    debug_assert_eq!(rx_frequencies.len(), num_rx);
    const MAX_ATTEMPTS: u32 = 5;
    let per_attempt_recv = (total_timeout / MAX_ATTEMPTS).max(Duration::from_millis(500));
    let mut buf = [0u8; 2048];
    let mut tx_seq: u32 = 0;

    for attempt in 1..=MAX_ATTEMPTS {
        // 1. [config reg 0 (with num_rx in C4)] + [reg 1 TX NCO = 0]
        let cc_tx_nco = build_handshake_cc_packet(
            tx_seq, sample_rate_index, num_rx, register::TX_NCO, 0,
        );
        tx_seq = tx_seq.wrapping_add(1);
        let n = socket.send_to(&cc_tx_nco, radio_addr)?;
        tracing::debug!(attempt, sent = n, "handshake: C&C TX NCO packet sent");
        std::thread::sleep(Duration::from_millis(10));

        // 2. One packet per RX NCO.
        for (r, &freq) in rx_frequencies.iter().enumerate() {
            let reg = RX_NCO_REGISTERS[r];
            let cc_rx_nco = build_handshake_cc_packet(
                tx_seq, sample_rate_index, num_rx, reg, freq,
            );
            tx_seq = tx_seq.wrapping_add(1);
            let n = socket.send_to(&cc_rx_nco, radio_addr)?;
            tracing::debug!(
                attempt, rx = r, freq, sent = n,
                "handshake: C&C RX NCO packet sent"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        // 3. Start command.
        let start = StartCommand.encode();
        let n = socket.send_to(&start, radio_addr)?;
        tracing::debug!(attempt, sent = n, "handshake: Start command sent");

        // 4. Wait for a data frame. We use recv_from so the source
        //    address of any reply is visible in the logs.
        let deadline = Instant::now() + per_attempt_recv;
        while Instant::now() < deadline {
            match socket.recv_from(&mut buf) {
                Ok((len, from)) => {
                    let magic = if len >= 3 {
                        format!("{:02x} {:02x} {:02x}", buf[0], buf[1], buf[2])
                    } else {
                        String::from("<short>")
                    };
                    tracing::info!(
                        attempt, len, %from, magic = %magic,
                        "handshake: got packet"
                    );
                    if len == METIS_PACKET_LEN {
                        if let Ok(packet) = MetisPacket::parse(&buf[..len]) {
                            // Real HL2 sends RX data on endpoint 6
                            // (`RadioRxAndStatus`). Endpoint 2 is only
                            // used for host → radio traffic. Accept any
                            // incoming sample-carrying endpoint as "the
                            // radio started streaming".
                            if packet.endpoint == Endpoint::RadioRxAndStatus {
                                return Ok(attempt);
                            } else {
                                tracing::debug!(
                                    endpoint = ?packet.endpoint,
                                    "handshake: got non-RX Metis packet, waiting"
                                );
                            }
                        } else {
                            tracing::warn!("handshake: 1032-byte packet failed to parse");
                        }
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut =>
                {
                    tracing::trace!(attempt, "handshake: recv timeout, polling");
                }
                Err(e) => return Err(e.into()),
            }
        }
        tracing::warn!(attempt, "handshake: no data frame received, retrying");
    }

    Err(NetError::StartFailed {
        attempts: MAX_ATTEMPTS,
    })
}

/// Build the 1032-byte data packet used during the Start handshake:
/// register 0 (config/sample rate/num_rx) in USB frame 0, a
/// caller-chosen frequency register in USB frame 1, zero IQ payload in
/// both.
fn build_handshake_cc_packet(
    tx_seq: u32,
    sample_rate_index: u8,
    num_rx: usize,
    frame1_register: u8,
    frame1_frequency: u32,
) -> [u8; METIS_PACKET_LEN] {
    let frame0_cmd = build_config_command(sample_rate_index, num_rx);
    let frame1_cmd = CommandFrame::set_frequency(frame1_register, frame1_frequency);

    let packet = MetisPacket {
        // Outbound: endpoint 2 (host → radio). HL2 accepts C&C writes on
        // this endpoint even with zero sample payload.
        endpoint: Endpoint::HostCommandAndTx,
        sequence: tx_seq,
        frame0: UsbFrame {
            command: frame0_cmd,
            samples: [0u8; hpsdr_protocol::metis::SAMPLES_SECTION_LEN],
        },
        frame1: UsbFrame {
            command: frame1_cmd,
            samples: [0u8; hpsdr_protocol::metis::SAMPLES_SECTION_LEN],
        },
    };
    packet.encode()
}

/// Build the C0..C4 payload for register 0 (config). C1 carries the
/// sample-rate index, C4 carries the duplex flag plus `(num_rx - 1)`
/// in bits 3..5.
fn build_config_command(sample_rate_index: u8, num_rx: usize) -> CommandFrame {
    // C4 = duplex (bit 2) | ((num_rx - 1) << 3)
    let c4 = 0b0000_0100u8 | ((((num_rx - 1) & 0x07) as u8) << 3);
    CommandFrame::raw(
        register::CONFIG,
        false,
        [sample_rate_index & 0x03, 0x00, 0x00, c4],
    )
}

/// Read loop for the RX thread. Exits as soon as `shared.shutdown` flips.
///
/// For a single-RX session we take a fast path (`UsbFrame::iq_samples`,
/// which avoids any runtime stride arithmetic). For multi-RX sessions
/// we use `iter_iq_multi(num_rx)` and demux each sample's per-RX I/Q
/// pair into its own producer.
fn rx_loop(
    socket: Arc<UdpSocket>,
    shared: Arc<SharedState>,
    mut producers: Vec<Producer<IqSample>>,
    num_rx: usize,
) {
    let mut buf = [0u8; 2048];
    let mut last_seq: Option<u32> = None;

    while !shared.shutdown.load(Ordering::Acquire) {
        match socket.recv_from(&mut buf).map(|(n, _from)| n) {
            Ok(len) if len == METIS_PACKET_LEN => {
                let Ok(packet) = MetisPacket::parse(&buf[..len]) else { continue };
                // Only consume RX data frames (endpoint 6). Anything else
                // (wideband, unknown endpoints) gets dropped — phase B
                // will add a wideband pipeline when the spectrum display
                // needs it.
                if packet.endpoint != Endpoint::RadioRxAndStatus {
                    continue;
                }

                let seq_error = matches!(
                    last_seq,
                    Some(prev) if packet.sequence != prev.wrapping_add(1)
                );
                last_seq = Some(packet.sequence);

                let mut samples_pushed = 0u64;
                push_frame_samples(
                    &mut producers,
                    num_rx,
                    &packet.frame0,
                    &mut samples_pushed,
                );
                push_frame_samples(
                    &mut producers,
                    num_rx,
                    &packet.frame1,
                    &mut samples_pushed,
                );

                let mut status = shared.status.lock().unwrap();
                status.packets_received += 1;
                status.samples_received += samples_pushed;
                if seq_error {
                    status.sequence_errors += 1;
                }
                status.last_packet_at = Some(Instant::now());
            }
            Ok(_) => {
                // Wrong-sized packet — ignore (upstream also prints and
                // drops in this case).
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock
                || e.kind() == io::ErrorKind::TimedOut =>
            {
                // Poll shutdown and retry.
            }
            Err(e) => {
                tracing::warn!(error = %e, "rx_loop recv error, shutting down");
                break;
            }
        }
    }
}

/// Demux a single USB frame's samples into the per-RX producers.
///
/// `samples_counted` is incremented by the number of *mono IqSamples*
/// that were accepted by the RX0 ring — it's used purely for stats, so
/// we only count one producer's worth even though multi-RX pushes to
/// all of them. If any producer's ring is full we bail and let the
/// caller's stats pick up the discrepancy.
fn push_frame_samples(
    producers: &mut [Producer<IqSample>],
    num_rx: usize,
    frame: &UsbFrame,
    samples_counted: &mut u64,
) {
    if num_rx == 1 {
        // Fast path: single RX, no multi-sample decode.
        for sample in frame.iq_samples() {
            if producers[0].push(sample).is_err() {
                return;
            }
            *samples_counted += 1;
        }
    } else {
        for ms in frame.iter_iq_multi(num_rx) {
            for (r, producer) in producers.iter_mut().enumerate().take(num_rx) {
                let (i, q) = ms.rx[r];
                // mic only goes to RX0 — it's a single physical input
                // shared by the radio regardless of DDC count.
                let mic = if r == 0 { ms.mic } else { 0 };
                if producer.push(IqSample { i, q, mic }).is_err() {
                    return;
                }
            }
            *samples_counted += 1;
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TxState {
    sample_rate_index: u8,
    num_rx:            u8,
    rx_frequencies:    [u32; MAX_RX],
}

/// TX keep-alive interval. HL2 expects the host to stream data packets
/// back to it at essentially the same rate as it sends RX data — if the
/// inbound flow stops for more than a few seconds, the FPGA watchdog
/// stops the RX stream. 48 kHz / 126 samples-per-packet ≈ 381 Hz, and a
/// tiny bit of headroom (2.625 ms period) absorbs scheduling jitter.
/// Upstream's `sendProtocol1Samples` is semaphore-driven from the RX
/// thread; we use a plain timer because it's robust against a brief RX
/// stall without collapsing the whole link.
const TX_PACKET_INTERVAL: Duration = Duration::from_micros(2_625);

/// TX thread. Starts from the post-handshake sequence number and sends a
/// zero-sample data packet every [`TX_PACKET_INTERVAL`]. Each packet's
/// two USB frames carry a rotating pair of C&C register writes that
/// keeps sample-rate / TX-NCO / RX-NCO primed on the radio; this matches
/// upstream `WriteMainLoop`'s `out_control_idx` rotation.
///
/// User commands (frequency / sample-rate changes) come in via the
/// [`mpsc::Receiver`] and update the local `TxState`; the next packet
/// the loop builds picks up the new value automatically.
fn tx_loop(
    socket: Arc<UdpSocket>,
    radio_addr: std::net::SocketAddr,
    shared: Arc<SharedState>,
    commands: MpscRx<SessionCommand>,
    initial: TxState,
    initial_tx_seq: u32,
) {
    let mut state  = initial;
    let mut tx_seq = initial_tx_seq;

    // Registers the C&C rotation cycles through. We always refresh
    // CONFIG (register 0) and TX_NCO (register 1), plus one RXn NCO
    // register per enabled receiver. Upstream cycles ALEX and a few
    // others too; phase B only needs the frequency-critical subset.
    let registers: Vec<u8> = {
        let num_rx = state.num_rx as usize;
        let mut r = Vec::with_capacity(2 + num_rx);
        r.push(register::CONFIG);
        r.push(register::TX_NCO);
        r.extend(RX_NCO_REGISTERS.iter().take(num_rx).copied());
        r
    };
    let mut reg_idx: usize = 0;

    let mut next_send = Instant::now();

    while !shared.shutdown.load(Ordering::Acquire) {
        // Drain any pending commands (non-blocking).
        loop {
            match commands.try_recv() {
                Ok(SessionCommand::SetRxFrequency { rx, hz }) => {
                    if (rx as usize) < state.num_rx as usize {
                        state.rx_frequencies[rx as usize] = hz;
                        tracing::debug!(rx, hz, "tx_loop: RX frequency updated");
                    } else {
                        tracing::warn!(
                            rx, num_rx = state.num_rx,
                            "tx_loop: ignoring SetRxFrequency for out-of-range rx",
                        );
                    }
                }
                Ok(SessionCommand::SetSampleRateIndex(idx)) => {
                    state.sample_rate_index = idx & 0x03;
                    tracing::debug!(idx = state.sample_rate_index, "tx_loop: sample rate updated");
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }

        // Pick the two registers this packet will carry.
        let reg0 = registers[reg_idx % registers.len()];
        let reg1 = registers[(reg_idx + 1) % registers.len()];
        reg_idx = (reg_idx + 1) % registers.len();

        let packet = build_tx_packet(tx_seq, &state, reg0, reg1);
        tx_seq = tx_seq.wrapping_add(1);

        if let Err(e) = socket.send_to(&packet, radio_addr) {
            tracing::warn!(error = %e, "tx_loop: send failed");
        }

        // Sleep until the next tick. Absolute wakeup times prevent drift
        // from accumulating when the scheduler pauses us for longer than
        // expected.
        next_send += TX_PACKET_INTERVAL;
        let now = Instant::now();
        if next_send > now {
            thread::sleep(next_send - now);
        } else {
            // We fell behind schedule — reset the anchor so we don't
            // burn CPU trying to catch up.
            next_send = now;
        }
    }
    tracing::debug!("tx_loop: exiting");
}

/// Build one TX keep-alive Metis packet: endpoint 2, zero IQ samples,
/// two register writes in the C&C slots (one per USB frame).
fn build_tx_packet(
    tx_seq: u32,
    state:  &TxState,
    frame0_register: u8,
    frame1_register: u8,
) -> [u8; METIS_PACKET_LEN] {
    let frame0_cmd = build_command(frame0_register, state);
    let frame1_cmd = build_command(frame1_register, state);

    let packet = MetisPacket {
        endpoint: Endpoint::HostCommandAndTx,
        sequence: tx_seq,
        frame0: UsbFrame {
            command: frame0_cmd,
            samples: [0u8; hpsdr_protocol::metis::SAMPLES_SECTION_LEN],
        },
        frame1: UsbFrame {
            command: frame1_cmd,
            samples: [0u8; hpsdr_protocol::metis::SAMPLES_SECTION_LEN],
        },
    };
    packet.encode()
}

/// Build a single command word for the given register, reading the
/// current `TxState` for registers that carry live data.
fn build_command(reg: u8, state: &TxState) -> CommandFrame {
    if reg == register::CONFIG {
        return build_config_command(state.sample_rate_index, state.num_rx as usize);
    }
    if reg == register::TX_NCO {
        // Phase B never asserts TX — keep the NCO parked at 0 Hz.
        return CommandFrame::set_frequency(register::TX_NCO, 0);
    }
    // RX NCO registers: map `reg` back to an RX index and emit that
    // receiver's tuned frequency. Unknown registers get a zero payload
    // so the packet layout stays well-formed.
    for (idx, &rx_reg) in RX_NCO_REGISTERS.iter().enumerate() {
        if rx_reg == reg && idx < state.num_rx as usize {
            return CommandFrame::set_frequency(reg, state.rx_frequencies[idx]);
        }
    }
    CommandFrame::raw(reg, false, [0, 0, 0, 0])
}

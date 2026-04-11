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
    Endpoint, IqSample, MetisPacket, UsbFrame, METIS_PACKET_LEN,
};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::NetError;

/// Configuration for [`Session::start`].
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Radio UDP endpoint, typically `<radio ip>:1024`.
    pub radio_addr: SocketAddr,
    /// Initial RX1 tuned frequency in Hz.
    pub rx1_frequency: u32,
    /// Sample rate index as encoded in register 0 `C1 & 0x03`
    /// (0 = 48 kHz, 1 = 96 kHz, 2 = 192 kHz, 3 = 384 kHz).
    pub sample_rate_index: u8,
    /// Size of the IQ ring buffer in samples. Must fit at least one
    /// burst of incoming data at the chosen sample rate; defaults to
    /// 16384 which gives roughly 340 ms of headroom at 48 kHz.
    pub ring_capacity: usize,
    /// How long [`Session::start`] waits for the first data packet after
    /// issuing the Start command before giving up.
    pub start_timeout: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            radio_addr: "127.0.0.1:1024".parse().unwrap(),
            rx1_frequency: 7_074_000,
            sample_rate_index: 0,
            ring_capacity: 16_384,
            start_timeout: Duration::from_millis(1_500),
        }
    }
}

/// Commands the owning task can push into a running session.
#[derive(Debug, Clone, Copy)]
pub enum SessionCommand {
    SetRx1Frequency(u32),
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
    pub fn start(config: SessionConfig) -> Result<(Self, Consumer<IqSample>), NetError> {
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

        let (producer, consumer) = RingBuffer::<IqSample>::new(config.ring_capacity);

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
            config.rx1_frequency,
            config.start_timeout,
        )?;
        tracing::info!(attempts, "HL2 start handshake succeeded");

        // Relax the recv timeout now that we're streaming.
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;

        // --- Threads --------------------------------------------------
        let rx_thread = {
            let shared = Arc::clone(&shared);
            let socket = Arc::clone(&socket);
            thread::Builder::new()
                .name("hpsdr-rx".into())
                .spawn(move || rx_loop(socket, shared, producer))
                .map_err(|e| NetError::Io(io::Error::other(e.to_string())))?
        };

        let (command_tx, command_rx) = mpsc::channel::<SessionCommand>();
        let ctrl_thread = {
            let shared = Arc::clone(&shared);
            let socket = Arc::clone(&socket);
            let radio_addr = config.radio_addr;
            // Start the TX sequence number after the handshake's initial
            // packets so the radio sees a monotonically increasing stream.
            let initial_tx_seq = 2;
            let initial = TxState {
                sample_rate_index: config.sample_rate_index,
                rx1_frequency:     config.rx1_frequency,
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
            consumer,
        ))
    }

    /// Ask the radio to tune RX1. The change is applied by the control
    /// thread on its next iteration.
    pub fn set_rx1_frequency(&self, hz: u32) -> Result<(), NetError> {
        self.command_tx
            .send(SessionCommand::SetRx1Frequency(hz))
            .map_err(|_| NetError::AlreadyStopped)
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

/// Perform the pre-streaming handshake against a fresh UDP socket.
///
/// Mirrors `SendStartToMetis` in upstream `networkproto1.c` byte-for-byte:
///
/// ```text
/// loop up to 5 times {
///     ForceCandCFrames(c0 = 2, freq = 0)   // [config reg 0] + [reg 1 TX NCO]
///     sleep 10 ms
///     ForceCandCFrames(c0 = 4, freq = rx1) // [config reg 0] + [reg 2 RX1 NCO]
///     sleep 10 ms
///     send_to(start_command, radio)
///     recv_from with 500 ms deadline
///     if got a data frame, break
/// }
/// ```
///
/// Everything is logged at `debug` / `info` / `warn` level so we can see
/// exactly what's happening on the wire when running against real HW.
fn perform_handshake(
    socket: &UdpSocket,
    radio_addr: std::net::SocketAddr,
    sample_rate_index: u8,
    rx1_frequency: u32,
    total_timeout: Duration,
) -> Result<u32, NetError> {
    const MAX_ATTEMPTS: u32 = 5;
    let per_attempt_recv = (total_timeout / MAX_ATTEMPTS).max(Duration::from_millis(500));
    let mut buf = [0u8; 2048];
    let mut tx_seq: u32 = 0;

    for attempt in 1..=MAX_ATTEMPTS {
        // 1. [config reg 0] + [reg 1 TX NCO = 0]
        let cc_tx_nco = build_handshake_cc_packet(
            tx_seq, sample_rate_index, register::TX_NCO, 0,
        );
        tx_seq = tx_seq.wrapping_add(1);
        let n = socket.send_to(&cc_tx_nco, radio_addr)?;
        tracing::debug!(attempt, sent = n, "handshake: C&C TX NCO packet sent");
        std::thread::sleep(Duration::from_millis(10));

        // 2. [config reg 0] + [reg 2 RX1 NCO = rx1_frequency]
        let cc_rx_nco = build_handshake_cc_packet(
            tx_seq, sample_rate_index, register::RX1_NCO, rx1_frequency,
        );
        tx_seq = tx_seq.wrapping_add(1);
        let n = socket.send_to(&cc_rx_nco, radio_addr)?;
        tracing::debug!(attempt, sent = n, "handshake: C&C RX NCO packet sent");
        std::thread::sleep(Duration::from_millis(10));

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
/// register 0 (config/sample rate) in USB frame 0, a caller-chosen
/// frequency register in USB frame 1, zero IQ payload in both.
fn build_handshake_cc_packet(
    tx_seq: u32,
    sample_rate_index: u8,
    frame1_register: u8,
    frame1_frequency: u32,
) -> [u8; METIS_PACKET_LEN] {
    let frame0_cmd = CommandFrame::raw(
        register::CONFIG,
        false,
        [sample_rate_index & 0x03, 0x00, 0x00, 0x00],
    );
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

/// Read loop for the RX thread. Exits as soon as `shared.shutdown` flips.
fn rx_loop(
    socket: Arc<UdpSocket>,
    shared: Arc<SharedState>,
    mut producer: Producer<IqSample>,
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
                push_samples(&mut producer, &packet.frame0, &mut samples_pushed);
                push_samples(&mut producer, &packet.frame1, &mut samples_pushed);

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

fn push_samples(producer: &mut Producer<IqSample>, frame: &UsbFrame, counter: &mut u64) {
    for sample in frame.iq_samples() {
        if producer.push(sample).is_ok() {
            *counter += 1;
        } else {
            // Ring full. Phase A treats this as "DSP stage is slower than
            // the radio" — we drop the oldest-that-didn't-fit and let the
            // caller spot the overrun via `samples_received` lagging
            // behind `packets_received`.
            return;
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TxState {
    sample_rate_index: u8,
    rx1_frequency:     u32,
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

    // Registers the C&C rotation cycles through. Upstream cycles CONFIG
    // / TX_NCO / RX1_NCO / ALEX / … ; phase A needs the first three.
    const REGISTERS: &[u8] = &[
        register::CONFIG,
        register::TX_NCO,
        register::RX1_NCO,
    ];
    let mut reg_idx: usize = 0;

    let mut next_send = Instant::now();

    while !shared.shutdown.load(Ordering::Acquire) {
        // Drain any pending commands (non-blocking).
        loop {
            match commands.try_recv() {
                Ok(SessionCommand::SetRx1Frequency(hz)) => {
                    state.rx1_frequency = hz;
                    tracing::debug!(hz, "tx_loop: RX1 frequency updated");
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
        let reg0 = REGISTERS[reg_idx % REGISTERS.len()];
        let reg1 = REGISTERS[(reg_idx + 1) % REGISTERS.len()];
        reg_idx = (reg_idx + 1) % REGISTERS.len();

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
    match reg {
        register::CONFIG => {
            // C1 = sample rate (bits 0-1)
            // C4 bit 2 = duplex (upstream always sets this)
            // C4 bits 3-5 = num_rx - 1 (0 for single RX)
            let c4 = 0b0000_0100u8;
            CommandFrame::raw(
                register::CONFIG,
                false,
                [state.sample_rate_index & 0x03, 0x00, 0x00, c4],
            )
        }
        register::TX_NCO => {
            // Phase A never asserts TX — keep the NCO parked at 0 Hz.
            CommandFrame::set_frequency(register::TX_NCO, 0)
        }
        register::RX1_NCO => {
            CommandFrame::set_frequency(register::RX1_NCO, state.rx1_frequency)
        }
        other => {
            // Unknown register — zero-payload write preserves layout.
            CommandFrame::raw(other, false, [0, 0, 0, 0])
        }
    }
}

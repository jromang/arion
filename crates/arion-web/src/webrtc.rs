//! WebRTC peer — Opus audio sender (phase W5a: 440 Hz tone).
//!
//! One blocking thread per peer. The thread owns a str0m `Rtc` and a
//! dedicated UDP socket. Signaling (offer in, answer out, remote ICE)
//! crosses into the thread via a `mpsc::Sender`. The outgoing audio
//! source is a synthetic tone — the real tap lands in W5b.
//!
//! The tokio side talks to this thread via channels; no tokio/str0m
//! entanglement.

use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use str0m::change::SdpOffer;
use str0m::media::MediaTime;
use str0m::net::Receive;
use str0m::{Candidate, Event, Input, Output, Rtc};

/// Messages driven from the WS (tokio) side into the peer thread.
pub enum PeerCmd {
    /// Remote SDP offer, answer returned on `reply`.
    Offer {
        sdp:   String,
        reply: tokio::sync::oneshot::Sender<Result<String>>,
    },
}

/// Handle kept by the WS session to dispatch signaling.
pub struct PeerHandle {
    pub tx: mpsc::Sender<PeerCmd>,
}

/// Spawn a new peer, returning a handle and the local bind address
/// (the address the browser will see as the ICE host candidate).
pub fn spawn(local_ip: IpAddr) -> Result<PeerHandle> {
    let socket = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))?;
    let local_port = socket.local_addr()?.port();
    let advertise = SocketAddr::new(local_ip, local_port);

    let (tx, rx) = mpsc::channel::<PeerCmd>();
    thread::Builder::new()
        .name(format!("arion-web-peer-{local_port}"))
        .spawn(move || {
            if let Err(e) = run(socket, advertise, rx) {
                tracing::warn!(error = %e, "peer thread exiting with error");
            }
        })?;
    Ok(PeerHandle { tx })
}

fn run(socket: UdpSocket, advertise: SocketAddr, rx: mpsc::Receiver<PeerCmd>) -> Result<()> {
    let mut rtc = Rtc::new();

    // Advertise ourselves as an ICE host candidate.
    let candidate = Candidate::host(advertise, "udp")
        .map_err(|e| anyhow!("candidate: {e:?}"))?;
    rtc.add_local_candidate(candidate);

    // Block until we get an offer.
    let Some(cmd) = rx.recv().ok() else { return Ok(()) };
    let PeerCmd::Offer { sdp, reply } = cmd;
    let offer = match SdpOffer::from_sdp_string(&sdp) {
        Ok(o) => o,
        Err(e) => {
            let _ = reply.send(Err(anyhow!("bad offer sdp: {e:?}")));
            return Ok(());
        }
    };
    let answer = match rtc.sdp_api().accept_offer(offer) {
        Ok(a) => a,
        Err(e) => {
            let _ = reply.send(Err(anyhow!("accept_offer: {e:?}")));
            return Ok(());
        }
    };
    let answer_sdp = answer.to_sdp_string();
    tracing::debug!(sdp = %answer_sdp, "sending answer");
    let _ = reply.send(Ok(answer_sdp));

    // Once the handshake completes we'll learn about the negotiated
    // audio mid via Event::MediaAdded. Until then, no writes.
    let mut audio_mid: Option<str0m::media::Mid> = None;
    let mut tone = ToneGenerator::new(440.0);
    let mut encoder = opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio)
        .map_err(|e| anyhow!("opus encoder: {e:?}"))?;
    encoder
        .set_bitrate(opus::Bitrate::Bits(96_000))
        .map_err(|e| anyhow!("opus bitrate: {e:?}"))?;

    // Media timeline at 48 kHz.
    let mut media_time: u64 = 0;
    // Next tick at which we should feed another 20 ms of audio.
    let mut next_audio_tick = Instant::now();

    let mut buf = [0u8; 2048];
    loop {
        // Drain signaling (future: remote ICE candidates).
        if let Ok(_cmd) = rx.try_recv() { /* no-op for W5a */ }

        // Drive str0m output until it asks us to wait.
        let timeout = loop {
            match rtc.poll_output() {
                Ok(Output::Timeout(t)) => break t,
                Ok(Output::Transmit(t)) => {
                    let _ = socket.send_to(&t.contents, t.destination);
                }
                Ok(Output::Event(ev)) => {
                    if let Event::MediaAdded(m) = &ev {
                        if matches!(m.kind, str0m::media::MediaKind::Audio) {
                            audio_mid = Some(m.mid);
                            tracing::info!(mid = ?m.mid, "audio track negotiated");
                        }
                    }
                    if matches!(ev, Event::IceConnectionStateChange(str0m::IceConnectionState::Disconnected)) {
                        tracing::info!("ice disconnected");
                        return Ok(());
                    }
                }
                Err(e) => return Err(anyhow!("poll_output: {e:?}")),
            }
        };

        // Feed audio frames whenever due (but only once the track is
        // negotiated, otherwise we'd just accumulate media time).
        let now = Instant::now();
        if audio_mid.is_none() {
            next_audio_tick = now + Duration::from_millis(20);
        }
        while now >= next_audio_tick {
            if let Some(mid) = audio_mid {
                let pcm = tone.next_20ms();
                let mut out = [0u8; 1500];
                match encoder.encode_float(&pcm, &mut out) {
                    Ok(n) => {
                        if let Some(w) = rtc.writer(mid) {
                            let pt_opt = w
                                .payload_params()
                                .find(|p: &&str0m::format::PayloadParams| {
                                    p.spec().codec == str0m::format::Codec::Opus
                                })
                                .map(|p| p.pt());
                            if let Some(pt) = pt_opt {
                                let time = MediaTime::from_90khz(media_time);
                                if let Err(e) = w.write(pt, now, time, out[..n].to_vec()) {
                                    tracing::debug!(error = ?e, "writer.write failed");
                                }
                            }
                        }
                    }
                    Err(e) => tracing::warn!(error = ?e, "opus encode failed"),
                }
            }
            // 20 ms × 48 kHz = 960 samples. In 90 kHz timebase that's 1800
            // ticks — str0m media time is 90 kHz for Opus per spec.
            media_time = media_time.wrapping_add(1800);
            next_audio_tick += Duration::from_millis(20);
        }

        // Block on socket or the audio tick, whichever comes first.
        let deadline = timeout.min(next_audio_tick);
        let wait = deadline.saturating_duration_since(Instant::now());
        socket.set_read_timeout(Some(wait.max(Duration::from_millis(1))))?;
        match socket.recv_from(&mut buf) {
            Ok((n, src)) => {
                let contents = (&buf[..n]).try_into().map_err(|e| anyhow!("packet parse: {e:?}"))?;
                rtc.handle_input(Input::Receive(
                    Instant::now(),
                    Receive {
                        proto:       str0m::net::Protocol::Udp,
                        source:      src,
                        destination: socket.local_addr()?,
                        contents,
                    },
                ))
                .map_err(|e| anyhow!("handle_input: {e:?}"))?;
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                rtc.handle_input(Input::Timeout(Instant::now()))
                    .map_err(|e| anyhow!("handle_input: {e:?}"))?;
            }
            Err(e) => return Err(anyhow!("udp recv: {e}")),
        }
    }
}

/// 440 Hz stereo sine at 48 kHz, 20 ms chunks (= 960 frames).
struct ToneGenerator {
    phase: f32,
    inc:   f32,
}

impl ToneGenerator {
    fn new(freq_hz: f32) -> Self {
        Self {
            phase: 0.0,
            inc:   freq_hz * std::f32::consts::TAU / 48_000.0,
        }
    }

    /// Returns 960 stereo samples (L0, R0, L1, R1, …).
    fn next_20ms(&mut self) -> Vec<f32> {
        let mut v = Vec::with_capacity(960 * 2);
        for _ in 0..960 {
            let s = (self.phase.sin()) * 0.2;
            v.push(s);
            v.push(s);
            self.phase += self.inc;
            if self.phase > std::f32::consts::TAU {
                self.phase -= std::f32::consts::TAU;
            }
        }
        v
    }
}

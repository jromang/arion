//! Discovery: broadcast a UDP request, collect every P1 reply within a
//! bounded time window.
//!
//! The call path matches what upstream `clsRadioDiscovery.cs` does — build
//! a 63-byte request, send it to every IPv4 broadcast target (limited +
//! directed), wait `timeout`, parse replies. We keep the C# code's split
//! of "limited broadcast" (`255.255.255.255`) vs "directed broadcast"
//! (subnet-specific) but default to the limited one because it Just Works
//! for most home LANs and for the integration tests against a loopback
//! mock.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use hpsdr_protocol::{DiscoveryReply, DiscoveryRequest};
use socket2::{Domain, Protocol, Socket, Type};

use crate::{NetError, HPSDR_PORT};

/// Trick to learn which local IPv4 the kernel would pick for outbound
/// traffic toward a well-known external address, without actually sending
/// anything. `UdpSocket::connect` on an unbound-but-connected UDP socket
/// just fills the 5-tuple and lets us read it back.
///
/// Returns `None` if the machine has no usable IPv4 routing (e.g. a CI
/// sandbox with only loopback).
fn default_local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    // 8.8.8.8 is a stable anchor — we only use it as a routing hint, no
    // packet is sent because UDP connect() is purely kernel state.
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
        _ => None,
    }
}

/// Compute the directed broadcast address for a /24 subnet, e.g.
/// `192.168.1.13` → `192.168.1.255`. We assume /24 because that's the
/// overwhelmingly common home-LAN case and because querying the kernel
/// for real netmasks would pull in another crate just to handle edge
/// cases that don't matter for phase A.
fn assumed_subnet_broadcast_slash24(local: Ipv4Addr) -> Ipv4Addr {
    let o = local.octets();
    Ipv4Addr::new(o[0], o[1], o[2], 255)
}

/// Options that tune a [`discover`] call.
#[derive(Debug, Clone)]
pub struct DiscoveryOptions {
    /// How long to wait for replies after sending the request.
    pub timeout: Duration,
    /// Explicit UDP destinations to probe. If empty, defaults to the
    /// limited broadcast `255.255.255.255:1024`.
    ///
    /// Tests pass `127.0.0.1:<mock_port>` here to talk to a loopback mock
    /// without needing broadcast permission.
    pub targets: Vec<SocketAddr>,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        DiscoveryOptions {
            timeout: Duration::from_millis(750),
            targets: Vec::new(),
        }
    }
}

/// One radio observed during a discovery sweep.
#[derive(Debug, Clone, Copy)]
pub struct RadioInfo {
    /// UDP address the reply came from. This is the radio's live IP plus
    /// (usually) its standard 1024 port, but a test mock will substitute
    /// an ephemeral port.
    pub addr: SocketAddr,
    /// Parsed contents of the reply.
    pub reply: DiscoveryReply,
}

/// Broadcast a discovery request and collect every Protocol 1 reply that
/// arrives before `opts.timeout` expires.
///
/// Results are de-duplicated by MAC address: if a radio's reply comes back
/// from both the limited and a directed broadcast we only keep one entry.
pub fn discover(opts: &DiscoveryOptions) -> Result<Vec<RadioInfo>, NetError> {
    // socket2 handle so we can set SO_BROADCAST and SO_REUSEADDR in a
    // portable way before binding.
    let sock2 = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock2.set_broadcast(true)?;
    sock2.set_reuse_address(true)?;
    sock2.bind(&SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).into())?;
    let socket: UdpSocket = sock2.into();

    let request = DiscoveryRequest.encode();

    let targets: Vec<SocketAddr> = if opts.targets.is_empty() {
        // Default: send to the limited broadcast AND to the /24 directed
        // broadcast of the default route. The directed form often works
        // on setups where the limited one doesn't — kernels sometimes
        // route `255.255.255.255` through an unexpected interface, and
        // stateful firewalls can drop replies to it.
        let mut t = vec![SocketAddr::new(
            IpAddr::V4(Ipv4Addr::BROADCAST),
            HPSDR_PORT,
        )];
        if let Some(local) = default_local_ipv4() {
            let directed = assumed_subnet_broadcast_slash24(local);
            tracing::debug!(%local, %directed, "adding directed subnet broadcast target");
            t.push(SocketAddr::new(IpAddr::V4(directed), HPSDR_PORT));
        }
        t
    } else {
        opts.targets.clone()
    };

    for target in &targets {
        // Swallow send failures: some networks reject broadcast to a
        // specific target but accept others. We still want to surface
        // replies from the ones that worked.
        match socket.send_to(&request, target) {
            Ok(_) => tracing::debug!(%target, "discovery request sent"),
            Err(e) => tracing::warn!(%target, error = %e, "discovery send failed"),
        }
    }

    socket.set_read_timeout(Some(Duration::from_millis(100)))?;

    let deadline = Instant::now() + opts.timeout;
    let mut radios: Vec<RadioInfo> = Vec::new();
    let mut buf = [0u8; 2048];

    while Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((len, addr)) => {
                match DiscoveryReply::parse(&buf[..len]) {
                    Ok(Some(reply)) => {
                        let info = RadioInfo { addr, reply };
                        if !radios.iter().any(|r| r.reply.mac == reply.mac) {
                            tracing::info!(%addr, ?reply, "discovered radio");
                            radios.push(info);
                        }
                    }
                    Ok(None) => {
                        // Protocol 2 reply — not ours.
                    }
                    Err(e) => {
                        tracing::debug!(%addr, error = %e, "ignoring bad reply");
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock
                || e.kind() == io::ErrorKind::TimedOut =>
            {
                // Loop and re-check the deadline.
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(radios)
}

//! Browser frontend for Arion.
//!
//! The server owns two handles:
//! - `SharedApp` — `Arc<Mutex<App>>` for controls and form state.
//! - `Arc<ArcSwap<Telemetry>>` — spectrum, S-meter, per-frame data.
//!
//! Each WebSocket session can additionally spawn a WebRTC peer for
//! audio playback (phase W5a: synthetic 440 Hz tone).

#![forbid(unsafe_code)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use arc_swap::ArcSwap;
use arion_app::App;
use arion_core::Telemetry;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, State,
    },
    response::Response,
    routing::get,
    Router,
};

mod assets;
mod protocol;
mod spectrum;
mod webrtc;

use protocol::{ClientEnvelope, Envelope, StateSnapshot, WebrtcClient, WebrtcServer};

pub use assets::StaticAssets;

const STATE_PUSH_INTERVAL: Duration = Duration::from_millis(100);
const SPECTRUM_PUSH_INTERVAL: Duration = Duration::from_millis(50);

pub type SharedApp = Arc<Mutex<App>>;
pub type SharedTelemetry = Arc<ArcSwap<Telemetry>>;

#[derive(Clone)]
struct AppState {
    app:       SharedApp,
    telemetry: SharedTelemetry,
    /// Fallback ICE host candidate IP when we can't derive one from
    /// the client's remote address.
    advertise_ip: IpAddr,
}

/// Run the web server on `addr` until the process exits.
pub fn serve_blocking(addr: SocketAddr, app: SharedApp, telemetry: SharedTelemetry) -> Result<()> {
    let advertise_ip = if addr.ip().is_unspecified() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        addr.ip()
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("arion-web")
        .build()?;
    rt.block_on(async move {
        let state = AppState { app, telemetry, advertise_ip };
        let router = Router::new()
            .route("/ws", get(ws_upgrade))
            .fallback(get(assets::serve_asset))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(%addr, "arion-web listening");
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await?;
        Ok::<_, anyhow::Error>(())
    })
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| ws_session(socket, state, remote))
}

/// Find the local interface IP used to reach `remote`. Uses the
/// UDP-connect trick: no packets are actually sent.
fn local_ip_for(remote: IpAddr, fallback: IpAddr) -> IpAddr {
    if let Ok(s) = std::net::UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)) {
        if s.connect(SocketAddr::new(remote, 1)).is_ok() {
            if let Ok(addr) = s.local_addr() {
                if !addr.ip().is_unspecified() {
                    return addr.ip();
                }
            }
        }
    }
    fallback
}

async fn ws_session(mut socket: WebSocket, state: AppState, remote: SocketAddr) {
    let advertise_ip = local_ip_for(remote.ip(), state.advertise_ip);
    let mut state_tick = tokio::time::interval(STATE_PUSH_INTERVAL);
    state_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut spec_tick = tokio::time::interval(SPECTRUM_PUSH_INTERVAL);
    spec_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut last_spec_update: Option<std::time::Instant> = None;
    let mut peer: Option<webrtc::PeerHandle> = None;

    loop {
        tokio::select! {
            _ = state_tick.tick() => {
                let text = {
                    let app = state.app.lock().unwrap_or_else(|p| p.into_inner());
                    let env = Envelope::State(StateSnapshot::from_app_and_telemetry(
                        &app,
                        &state.telemetry.load_full(),
                    ));
                    match serde_json::to_string(&env) {
                        Ok(s) => s,
                        Err(_) => continue,
                    }
                };
                if socket.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
            _ = spec_tick.tick() => {
                let snap = state.telemetry.load_full();
                if Some(snap.last_update) == last_spec_update { continue; }
                last_spec_update = Some(snap.last_update);
                for (rx_idx, rt) in snap.rx.iter().enumerate().take(snap.num_rx as usize) {
                    if !rt.enabled { continue; }
                    let frame = spectrum::encode(rx_idx as u8, rt);
                    if socket.send(Message::Binary(frame)).await.is_err() {
                        return;
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        if let Some(out) = handle_client_text(&state, advertise_ip, &t, &mut peer).await {
                            if socket.send(Message::Text(out)).await.is_err() { break; }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

async fn handle_client_text(
    state: &AppState,
    advertise_ip: IpAddr,
    text: &str,
    peer: &mut Option<webrtc::PeerHandle>,
) -> Option<String> {
    let env: ClientEnvelope = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(error = %e, "ignoring malformed client message");
            return None;
        }
    };
    match env {
        ClientEnvelope::Action(action) => {
            let mut app = state.app.lock().unwrap_or_else(|p| p.into_inner());
            action.apply(&mut app);
            None
        }
        ClientEnvelope::Webrtc(WebrtcClient::Offer { sdp }) => {
            handle_offer(advertise_ip, peer, sdp).await
        }
    }
}

async fn handle_offer(
    advertise_ip: IpAddr,
    peer: &mut Option<webrtc::PeerHandle>,
    sdp: String,
) -> Option<String> {
    if peer.is_none() {
        match webrtc::spawn(advertise_ip) {
            Ok(h) => *peer = Some(h),
            Err(e) => {
                return serde_json::to_string(&Envelope::Webrtc(WebrtcServer::Error {
                    message: &format!("spawn peer: {e}"),
                })).ok();
            }
        }
    }
    let handle = peer.as_ref().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    if handle.tx.send(webrtc::PeerCmd::Offer { sdp, reply: tx }).is_err() {
        return serde_json::to_string(&Envelope::Webrtc(WebrtcServer::Error {
            message: "peer thread gone",
        })).ok();
    }
    let answer = rx.await.ok()?;
    match answer {
        Ok(sdp) => serde_json::to_string(
            &Envelope::Webrtc(WebrtcServer::Answer { sdp: &sdp }),
        ).ok(),
        Err(e) => {
            let m = format!("{e}");
            serde_json::to_string(&Envelope::Webrtc(WebrtcServer::Error {
                message: &m,
            })).ok()
        }
    }
}

#[derive(rust_embed::RustEmbed)]
#[folder = "web/dist/"]
struct Dist;

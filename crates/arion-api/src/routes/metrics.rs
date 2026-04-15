use std::fmt::Write;

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::IntoResponse;

use crate::context::ApiContext;

/// Emit Prometheus text format `/metrics` exposition.
///
/// Hand-rolled writer — no dependency on a metrics crate. Gauges:
/// per-RX frequency / s-meter / volume / muted, plus radio_connected.
pub async fn get_metrics(State(ctx): State<ApiContext>) -> impl IntoResponse {
    let snap = ctx.snapshot.load_full();
    let tel = ctx.telemetry.load_full();

    let mut out = String::with_capacity(2048);

    let _ = writeln!(out, "# HELP arion_radio_connected 1 if the radio is currently connected, 0 otherwise.");
    let _ = writeln!(out, "# TYPE arion_radio_connected gauge");
    let _ = writeln!(out, "arion_radio_connected {}", snap.radio_connected as u8);

    let _ = writeln!(out, "# HELP arion_num_rx Number of active receivers.");
    let _ = writeln!(out, "# TYPE arion_num_rx gauge");
    let _ = writeln!(out, "arion_num_rx {}", snap.num_rx);

    let _ = writeln!(out, "# HELP arion_active_rx Index of the active receiver.");
    let _ = writeln!(out, "# TYPE arion_active_rx gauge");
    let _ = writeln!(out, "arion_active_rx {}", snap.active_rx);

    let _ = writeln!(out, "# HELP arion_rx_frequency_hz Current VFO frequency in Hz.");
    let _ = writeln!(out, "# TYPE arion_rx_frequency_hz gauge");
    for (i, r) in snap.rx.iter().enumerate() {
        let _ = writeln!(out, "arion_rx_frequency_hz{{rx=\"{i}\"}} {}", r.frequency_hz);
    }

    let _ = writeln!(out, "# HELP arion_rx_volume Linear AF gain (App-side units, typically 0..2).");
    let _ = writeln!(out, "# TYPE arion_rx_volume gauge");
    for (i, r) in snap.rx.iter().enumerate() {
        let _ = writeln!(out, "arion_rx_volume{{rx=\"{i}\"}} {}", r.volume);
    }

    let _ = writeln!(out, "# HELP arion_rx_s_meter_db RX S-meter in dBm (post-calibration).");
    let _ = writeln!(out, "# TYPE arion_rx_s_meter_db gauge");
    for (i, r) in snap.rx.iter().enumerate() {
        let _ = writeln!(out, "arion_rx_s_meter_db{{rx=\"{i}\"}} {}", r.s_meter_db);
    }

    let _ = writeln!(out, "# HELP arion_telemetry_age_seconds Seconds since the last DSP telemetry update.");
    let _ = writeln!(out, "# TYPE arion_telemetry_age_seconds gauge");
    let _ = writeln!(out, "arion_telemetry_age_seconds {:.3}", tel.last_update.elapsed().as_secs_f64());

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    (headers, out)
}

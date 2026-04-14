use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::context::ApiContext;
use crate::error::ApiResult;

pub async fn get_instance(State(ctx): State<ApiContext>) -> ApiResult<Json<Value>> {
    let snap = ctx.snapshot.load_full();
    let uptime = ctx.started_at.elapsed();
    Ok(Json(json!({
        "name":    "arion",
        "version": ctx.build_version,
        "uptime_seconds": uptime.as_secs(),
        "num_rx":  snap.num_rx,
        "active_rx": snap.active_rx,
        "radio_connected": snap.radio_connected,
        "features": {
            "scripts_enabled": ctx.script_tx.is_some(),
            "midi_enabled":    ctx.midi_mapping.is_some(),
        }
    })))
}

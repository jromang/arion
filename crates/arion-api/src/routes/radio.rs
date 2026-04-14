use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use arion_app::protocol::Action;

use crate::context::ApiContext;
use crate::error::{ApiError, ApiResult};

pub async fn get_radio(State(ctx): State<ApiContext>) -> ApiResult<Json<Value>> {
    let snap = ctx.snapshot.load_full();
    let tel = ctx.telemetry.load_full();
    Ok(Json(json!({
        "connected": snap.radio_connected,
        "num_rx":    snap.num_rx,
        "last_telemetry_age_ms": tel.last_update.elapsed().as_millis() as u64,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectBody {
    pub ip: Option<String>,
}

pub async fn post_connect(
    State(ctx): State<ApiContext>,
    body: Option<Json<ConnectBody>>,
) -> ApiResult<Json<Value>> {
    let ip = body.and_then(|b| b.0.ip);
    ctx.action_tx
        .send(Action::RadioConnect { ip })
        .map_err(|e| ApiError::Internal(format!("action dispatch: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn post_disconnect(State(ctx): State<ApiContext>) -> ApiResult<Json<Value>> {
    ctx.action_tx
        .send(Action::RadioDisconnect)
        .map_err(|e| ApiError::Internal(format!("action dispatch: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

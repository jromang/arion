use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use arion_app::protocol::Action;

use crate::context::ApiContext;
use crate::error::{ApiError, ApiResult};

pub async fn get_rigctld(State(_ctx): State<ApiContext>) -> ApiResult<Json<Value>> {
    // Live server state (running / port) is owned by the egui view;
    // we expose only the persisted settings-level intent here. A
    // future enhancement can add a `rigctld_running` field pushed
    // from the UI thread into the snapshot.
    Ok(Json(json!({
        "note": "use PATCH to enable/disable; current port visible in /instance"
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RigctldPatch {
    pub enabled: Option<bool>,
    pub port:    Option<u16>,
}

pub async fn patch_rigctld(
    State(ctx): State<ApiContext>,
    Json(body): Json<RigctldPatch>,
) -> ApiResult<Json<Value>> {
    if body.enabled.is_none() && body.port.is_none() {
        return Err(ApiError::Validation("nothing to patch".into()));
    }
    ctx.action_tx
        .send(Action::SetRigctldEnabled {
            enabled: body.enabled.unwrap_or(true),
            port:    body.port,
        })
        .map_err(|e| ApiError::Internal(format!("dispatch: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

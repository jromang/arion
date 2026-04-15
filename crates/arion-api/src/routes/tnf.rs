use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use arion_app::protocol::Action;

use crate::context::ApiContext;
use crate::error::{ApiError, ApiResult};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotchBody {
    pub freq_hz:  f64,
    pub width_hz: f64,
    #[serde(default = "default_true")]
    pub active:   bool,
}
fn default_true() -> bool { true }

pub async fn list_notches(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
) -> ApiResult<Json<Value>> {
    let snap = ctx.snapshot.load_full();
    if idx >= snap.num_rx as usize {
        return Err(ApiError::NotFound(format!("rx {idx} not found")));
    }
    // Notches aren't in StateSnapshot — surface a stub. Callers that
    // need the live list can scrape Settings or dispatch via scripting.
    // Keeping the response shape forward-compatible.
    Ok(Json(json!({ "rx": idx, "note": "see arion.toml rx.tnf_notches for the current list" })))
}

pub async fn post_notch(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
    Json(body): Json<NotchBody>,
) -> ApiResult<Json<Value>> {
    let snap = ctx.snapshot.load_full();
    if idx >= snap.num_rx as usize {
        return Err(ApiError::NotFound(format!("rx {idx} not found")));
    }
    ctx.action_tx
        .send(Action::AddRxTnfNotch {
            rx:       idx as u8,
            freq_hz:  body.freq_hz,
            width_hz: body.width_hz,
            active:   body.active,
        })
        .map_err(|e| ApiError::Internal(format!("dispatch: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_notch(
    State(ctx): State<ApiContext>,
    Path((idx, nidx)): Path<(usize, u32)>,
) -> ApiResult<Json<Value>> {
    let snap = ctx.snapshot.load_full();
    if idx >= snap.num_rx as usize {
        return Err(ApiError::NotFound(format!("rx {idx} not found")));
    }
    ctx.action_tx
        .send(Action::DeleteRxTnfNotch { rx: idx as u8, idx: nidx })
        .map_err(|e| ApiError::Internal(format!("dispatch: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn put_notch(
    State(ctx): State<ApiContext>,
    Path((idx, nidx)): Path<(usize, u32)>,
    Json(body): Json<NotchBody>,
) -> ApiResult<Json<Value>> {
    let snap = ctx.snapshot.load_full();
    if idx >= snap.num_rx as usize {
        return Err(ApiError::NotFound(format!("rx {idx} not found")));
    }
    ctx.action_tx
        .send(Action::EditRxTnfNotch {
            rx:       idx as u8,
            idx:      nidx,
            freq_hz:  body.freq_hz,
            width_hz: body.width_hz,
            active:   body.active,
        })
        .map_err(|e| ApiError::Internal(format!("dispatch: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

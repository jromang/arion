use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};

use arion_app::protocol::{band_from_label, Action};

use crate::context::ApiContext;
use crate::error::{ApiError, ApiResult};

const ALL_BANDS: &[&str] = &[
    "M160", "M80", "M60", "M40", "M30", "M20", "M17", "M15", "M12", "M10", "M6",
];

pub async fn list_bands() -> Json<Value> {
    Json(json!({ "bands": ALL_BANDS }))
}

pub async fn post_jump(
    State(ctx): State<ApiContext>,
    Path(band): Path<String>,
) -> ApiResult<Json<Value>> {
    if band_from_label(&band).is_none() {
        return Err(ApiError::NotFound(format!("unknown band: {band}")));
    }
    ctx.action_tx
        .send(Action::JumpBand { band })
        .map_err(|e| ApiError::Internal(format!("dispatch: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

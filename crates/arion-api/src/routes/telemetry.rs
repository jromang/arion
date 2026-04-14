use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::context::ApiContext;
use crate::error::ApiResult;

pub async fn get_telemetry(State(ctx): State<ApiContext>) -> ApiResult<Json<Value>> {
    let snap = ctx.snapshot.load_full();
    let tel = ctx.telemetry.load_full();
    let rx_tel: Vec<_> = tel
        .rx
        .iter()
        .take(tel.num_rx as usize)
        .map(|r| {
            json!({
                "enabled":     r.enabled,
                "s_meter_db":  r.s_meter_db,
            })
        })
        .collect();
    Ok(Json(json!({
        "state":     &*snap,
        "telemetry": {
            "num_rx":              tel.num_rx,
            "last_update_age_ms":  tel.last_update.elapsed().as_millis() as u64,
            "rx":                  rx_tel,
        }
    })))
}

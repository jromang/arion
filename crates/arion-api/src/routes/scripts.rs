use std::sync::mpsc::sync_channel;
use std::time::Duration;

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::context::{ApiContext, ScriptRequest};
use crate::error::{ApiError, ApiResult};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalBody {
    pub source: String,
}

pub async fn post_eval(
    State(ctx): State<ApiContext>,
    Json(body): Json<EvalBody>,
) -> ApiResult<Json<Value>> {
    let tx = ctx
        .script_tx
        .clone()
        .ok_or_else(|| ApiError::Disabled("scripting disabled".into()))?;

    let (reply_tx, reply_rx) = sync_channel(1);
    let req = ScriptRequest {
        source: body.source,
        reply:  reply_tx,
    };
    tx.send(req)
        .map_err(|e| ApiError::Internal(format!("script dispatch: {e}")))?;

    // Block up to 2s for the UI thread to evaluate. Runs inside a
    // blocking task so the tokio worker isn't pinned.
    let reply = tokio::task::spawn_blocking(move || reply_rx.recv_timeout(Duration::from_secs(2)))
        .await
        .map_err(|e| ApiError::Internal(format!("join: {e}")))?
        .map_err(|_| ApiError::Internal("script eval timed out".into()))?;

    Ok(Json(json!({
        "output": reply.output,
        "error":  reply.error,
    })))
}

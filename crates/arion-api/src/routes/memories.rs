use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use arion_app::protocol::{Action, MemorySnapshot};

use crate::context::ApiContext;
use crate::error::{ApiError, ApiResult};

pub async fn list_memories(State(ctx): State<ApiContext>) -> ApiResult<Json<Value>> {
    let snap = ctx.snapshot.load_full();
    Ok(Json(json!({ "memories": &snap.memories })))
}

pub async fn get_memory(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
) -> ApiResult<Json<MemorySnapshot>> {
    let snap = ctx.snapshot.load_full();
    snap.memories
        .get(idx)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("memory {idx} not found")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBody {
    pub rx:   u8,
    pub name: String,
    #[serde(default)]
    pub tag:  String,
}

pub async fn post_memory(
    State(ctx): State<ApiContext>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    let snap = ctx.snapshot.load_full();
    if body.rx as usize >= snap.num_rx as usize {
        return Err(ApiError::Validation(format!("rx {} out of range", body.rx)));
    }
    ctx.action_tx
        .send(Action::SaveMemory { rx: body.rx, name: body.name, tag: body.tag })
        .map_err(dispatch)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceBody {
    pub name:         String,
    #[serde(default)]
    pub tag:          String,
    pub frequency_hz: u32,
    pub mode:         String,
}

pub async fn put_memory(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
    Json(body): Json<ReplaceBody>,
) -> ApiResult<Json<Value>> {
    ctx.action_tx
        .send(Action::UpdateMemory {
            idx,
            name:         body.name,
            tag:          body.tag,
            frequency_hz: body.frequency_hz,
            mode:         body.mode,
        })
        .map_err(dispatch)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_memory(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
) -> ApiResult<Json<Value>> {
    ctx.action_tx
        .send(Action::DeleteMemory { idx })
        .map_err(dispatch)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn post_load_memory(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
) -> ApiResult<Json<Value>> {
    ctx.action_tx
        .send(Action::LoadMemory { idx })
        .map_err(dispatch)?;
    Ok(Json(json!({ "ok": true })))
}

fn dispatch(e: std::sync::mpsc::SendError<Action>) -> ApiError {
    ApiError::Internal(format!("action dispatch: {e}"))
}

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use arion_app::protocol::Action;
use arion_midi::{Binding, MappingTable};

use crate::context::ApiContext;
use crate::error::{ApiError, ApiResult};

pub async fn get_midi(State(ctx): State<ApiContext>) -> ApiResult<Json<Value>> {
    let devices = arion_midi::device::enum_inputs().unwrap_or_default();
    Ok(Json(json!({
        "available_devices": devices,
        "enabled":           ctx.midi_mapping.is_some(),
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MidiPatch {
    pub enabled:     Option<bool>,
    pub device_name: Option<String>,
}

pub async fn patch_midi(
    State(ctx): State<ApiContext>,
    Json(body): Json<MidiPatch>,
) -> ApiResult<Json<Value>> {
    if body.enabled.is_none() && body.device_name.is_none() {
        return Err(ApiError::Validation("nothing to patch".into()));
    }
    ctx.action_tx
        .send(Action::SetMidiEnabled {
            enabled:     body.enabled.unwrap_or(true),
            device_name: body.device_name,
        })
        .map_err(dispatch)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn list_bindings(State(ctx): State<ApiContext>) -> ApiResult<Json<Value>> {
    let mapping = mapping(&ctx)?;
    let snap = mapping.load_full();
    Ok(Json(json!({ "bindings": &snap.bindings })))
}

pub async fn post_binding(
    State(ctx): State<ApiContext>,
    Json(binding): Json<Binding>,
) -> ApiResult<Json<Value>> {
    let mapping = mapping(&ctx)?;
    let mut table = (*mapping.load_full()).clone();
    table.bindings.push(binding);
    commit(&ctx, mapping, table);
    Ok(Json(json!({ "ok": true })))
}

pub async fn put_binding(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
    Json(binding): Json<Binding>,
) -> ApiResult<Json<Value>> {
    let mapping = mapping(&ctx)?;
    let mut table = (*mapping.load_full()).clone();
    if idx >= table.bindings.len() {
        return Err(ApiError::NotFound(format!("binding {idx} not found")));
    }
    table.bindings[idx] = binding;
    commit(&ctx, mapping, table);
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_binding(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
) -> ApiResult<Json<Value>> {
    let mapping = mapping(&ctx)?;
    let mut table = (*mapping.load_full()).clone();
    if idx >= table.bindings.len() {
        return Err(ApiError::NotFound(format!("binding {idx} not found")));
    }
    table.bindings.remove(idx);
    commit(&ctx, mapping, table);
    Ok(Json(json!({ "ok": true })))
}

pub async fn get_last_event(State(ctx): State<ApiContext>) -> ApiResult<Json<Value>> {
    let ev = ctx.midi_last_event.load_full();
    Ok(Json(match (*ev).as_ref() {
        Some(e) => json!({
            "trigger": e.trigger,
            "value":   e.value,
        }),
        None => json!(null),
    }))
}

fn mapping(ctx: &ApiContext) -> ApiResult<arion_midi::SharedMapping> {
    ctx.midi_mapping
        .clone()
        .ok_or_else(|| ApiError::Disabled("MIDI not enabled".into()))
}

fn commit(ctx: &ApiContext, mapping: arion_midi::SharedMapping, table: MappingTable) {
    mapping.store(Arc::new(table));
    if ctx.midi_persist {
        let t = mapping.load_full();
        if let Err(e) = arion_midi::persist::save(&t) {
            tracing::warn!(error = %e, "midi: persist failed");
        }
    }
}

fn dispatch(e: std::sync::mpsc::SendError<Action>) -> ApiError {
    ApiError::Internal(format!("action dispatch: {e}"))
}

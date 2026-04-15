use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use arion_app::protocol::{Action, RxSnapshot};

use crate::context::ApiContext;
use crate::error::{ApiError, ApiResult};

pub async fn list_rx(State(ctx): State<ApiContext>) -> ApiResult<Json<Value>> {
    let snap = ctx.snapshot.load_full();
    Ok(Json(json!({
        "num_rx":    snap.num_rx,
        "active_rx": snap.active_rx,
        "rx":        &snap.rx,
    })))
}

pub async fn get_rx(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
) -> ApiResult<Json<RxSnapshot>> {
    let snap = ctx.snapshot.load_full();
    snap.rx
        .get(idx)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::NotFound(format!("rx {idx} not found")))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RxPatch {
    pub frequency_hz: Option<u32>,
    pub mode:         Option<String>,
    pub volume:       Option<f32>,
    pub muted:        Option<bool>,
    pub locked:       Option<bool>,
    pub rit_hz:       Option<i32>,
    pub nr3:          Option<bool>,
    pub nr4:          Option<bool>,
    pub anr:          Option<bool>,
    pub emnr:         Option<bool>,
    pub agc:          Option<String>,
}

pub async fn patch_rx(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
    Json(body): Json<RxPatch>,
) -> ApiResult<Json<Value>> {
    validate_rx(&ctx, idx)?;
    let rx = idx as u8;
    let send = |a: Action| ctx.action_tx.send(a).map_err(dispatch);

    if let Some(hz) = body.frequency_hz { send(Action::SetRxFrequency { rx, hz })?; }
    if let Some(mode) = body.mode { send(Action::SetRxMode { rx, mode })?; }
    if let Some(volume) = body.volume { send(Action::SetRxVolume { rx, volume })?; }
    if let Some(muted) = body.muted { send(Action::SetRxMuted { rx, muted })?; }
    if let Some(locked) = body.locked { send(Action::SetRxLocked { rx, locked })?; }
    if let Some(hz) = body.rit_hz { send(Action::SetRxRit { rx, hz })?; }
    if let Some(on) = body.nr3 { send(Action::SetRxNr3 { rx, on })?; }
    if let Some(on) = body.nr4 { send(Action::SetRxNr4 { rx, on })?; }
    if let Some(on) = body.anr { send(Action::SetRxAnr { rx, on })?; }
    if let Some(on) = body.emnr { send(Action::SetRxEmnr { rx, on })?; }
    if let Some(agc) = body.agc { send(Action::SetRxAgc { rx, agc })?; }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TuneBody {
    pub delta_hz: i32,
}

pub async fn post_tune(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
    Json(body): Json<TuneBody>,
) -> ApiResult<Json<Value>> {
    validate_rx(&ctx, idx)?;
    ctx.action_tx
        .send(Action::TuneRx { rx: idx as u8, delta_hz: body.delta_hz })
        .map_err(dispatch)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterBody {
    pub low:  Option<f64>,
    pub high: Option<f64>,
}

pub async fn patch_filter(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
    Json(body): Json<FilterBody>,
) -> ApiResult<Json<Value>> {
    validate_rx(&ctx, idx)?;
    let (low, high) = match (body.low, body.high) {
        (Some(l), Some(h)) => (l, h),
        _ => return Err(ApiError::Validation("both low and high are required".into())),
    };
    ctx.action_tx
        .send(Action::SetRxFilter { rx: idx as u8, low, high })
        .map_err(dispatch)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterPresetBody {
    pub preset: String,
}

pub async fn post_filter_preset(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
    Json(body): Json<FilterPresetBody>,
) -> ApiResult<Json<Value>> {
    validate_rx(&ctx, idx)?;
    ctx.action_tx
        .send(Action::SetRxFilterPreset { rx: idx as u8, preset: body.preset })
        .map_err(dispatch)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EqBody {
    pub gains: Vec<i32>,
}

pub async fn patch_eq(
    State(ctx): State<ApiContext>,
    Path(idx): Path<usize>,
    Json(body): Json<EqBody>,
) -> ApiResult<Json<Value>> {
    validate_rx(&ctx, idx)?;
    if body.gains.len() != 11 {
        return Err(ApiError::Validation(
            "gains must have 11 entries (preamp + 10 bands)".into(),
        ));
    }
    ctx.action_tx
        .send(Action::SetRxEq { rx: idx as u8, gains: body.gains })
        .map_err(dispatch)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveRxBody {
    pub rx: usize,
}

pub async fn post_active_rx(
    State(ctx): State<ApiContext>,
    Json(body): Json<ActiveRxBody>,
) -> ApiResult<Json<Value>> {
    validate_rx(&ctx, body.rx)?;
    ctx.action_tx
        .send(Action::SetActiveRx { rx: body.rx })
        .map_err(dispatch)?;
    Ok(Json(json!({ "ok": true })))
}

fn validate_rx(ctx: &ApiContext, idx: usize) -> ApiResult<()> {
    let snap = ctx.snapshot.load_full();
    if idx < snap.num_rx as usize {
        Ok(())
    } else {
        Err(ApiError::NotFound(format!("rx {idx} out of range (num_rx={})", snap.num_rx)))
    }
}

fn dispatch(e: std::sync::mpsc::SendError<Action>) -> ApiError {
    ApiError::Internal(format!("action dispatch failed: {e}"))
}

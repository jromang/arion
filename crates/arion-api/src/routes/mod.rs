use axum::routing::{get, patch, post, put};
use axum::Router;

use crate::context::ApiContext;

pub mod bands;
pub mod instance;
pub mod memories;
pub mod metrics;
pub mod midi;
pub mod radio;
pub mod rigctld;
pub mod rx;
pub mod scripts;
pub mod telemetry;

pub fn router() -> Router<ApiContext> {
    Router::new()
        // instance & radio
        .route("/instance",          get(instance::get_instance))
        .route("/radio",             get(radio::get_radio))
        .route("/radio/connect",     post(radio::post_connect))
        .route("/radio/disconnect",  post(radio::post_disconnect))
        // receivers
        .route("/rx",                get(rx::list_rx))
        .route("/rx/{idx}",          get(rx::get_rx).patch(rx::patch_rx))
        .route("/rx/{idx}/tune",     post(rx::post_tune))
        .route("/rx/{idx}/filter",   patch(rx::patch_filter))
        .route("/rx/{idx}/filter/preset", post(rx::post_filter_preset))
        .route("/rx/{idx}/eq",       patch(rx::patch_eq))
        .route("/active-rx",         post(rx::post_active_rx))
        // bands
        .route("/bands",             get(bands::list_bands))
        .route("/bands/{band}",      post(bands::post_jump))
        // memories
        .route(
            "/memories",
            get(memories::list_memories).post(memories::post_memory),
        )
        .route(
            "/memories/{idx}",
            get(memories::get_memory)
                .put(memories::put_memory)
                .delete(memories::delete_memory),
        )
        .route("/memories/{idx}/load", post(memories::post_load_memory))
        // midi
        .route("/midi",              get(midi::get_midi).patch(midi::patch_midi))
        .route(
            "/midi/bindings",
            get(midi::list_bindings).post(midi::post_binding),
        )
        .route(
            "/midi/bindings/{idx}",
            put(midi::put_binding).delete(midi::delete_binding),
        )
        .route("/midi/last-event",   get(midi::get_last_event))
        // services
        .route("/rigctld",           get(rigctld::get_rigctld).patch(rigctld::patch_rigctld))
        // scripting
        .route("/scripts/eval",      post(scripts::post_eval))
        // observability
        .route("/telemetry",         get(telemetry::get_telemetry))
        .route("/metrics",           get(metrics::get_metrics))
}

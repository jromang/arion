//! REST / JSON HTTP API for controlling Arion.
//!
//! Follows Richardson Maturity Model L2: resource-oriented URLs,
//! strict HTTP verb semantics (`GET` / `PATCH` / `PUT` / `POST` /
//! `DELETE`), uniform error envelope (RFC 7807 flavor), and
//! URL-prefix versioning under `/api/v1/*`.
//!
//! Architecture: the crate owns a dedicated tokio runtime on a
//! standalone OS thread. Handlers never touch `App` directly —
//! writes go through `mpsc::Sender<arion_app::protocol::Action>`
//! drained by the UI thread each frame, same pattern as
//! `arion-rigctld` and `arion-midi`.
//!
//! Reads go through `Arc<ArcSwap<StateSnapshot>>` and
//! `Arc<ArcSwap<Telemetry>>`, republished each frame by the UI.
//!
//! ```text
//!   HTTP client ─▶ axum handler ─┬─▶ mpsc Action ─▶ UI drain ─▶ App
//!                                └─▶ ArcSwap snapshot reads
//! ```

#![forbid(unsafe_code)]

pub mod context;
pub mod error;
pub mod routes;
pub mod server;

pub use context::{ApiContext, ScriptReply, ScriptRequest};
pub use error::ApiError;
pub use server::{start, ApiHandle};

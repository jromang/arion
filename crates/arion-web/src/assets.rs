//! Static asset serving for the embedded SPA.

use axum::{
    body::Body,
    extract::Request,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use crate::Dist;

/// Extension marker for alternative asset bundles in later phases.
pub struct StaticAssets;

pub async fn serve_asset(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    serve_path(path)
        .or_else(|| serve_path("index.html"))
        .unwrap_or_else(|| (StatusCode::NOT_FOUND, "not found").into_response())
}

fn serve_path(path: &str) -> Option<Response> {
    let file = Dist::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    Some(
        Response::builder()
            .header(header::CONTENT_TYPE, mime.as_ref())
            .body(Body::from(file.data.into_owned()))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    )
}

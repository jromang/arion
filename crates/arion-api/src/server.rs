use std::net::SocketAddr;
use std::thread::JoinHandle;

use axum::Router;
use tokio::sync::oneshot;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::context::ApiContext;
use crate::error::ApiError;
use crate::routes;

/// Running API server handle. Dropping it gracefully shuts down
/// the tokio runtime and joins the backing thread.
pub struct ApiHandle {
    shutdown: Option<oneshot::Sender<()>>,
    thread:   Option<JoinHandle<()>>,
    addr:     SocketAddr,
}

impl ApiHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ApiHandle {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

/// Spawn the REST API on a dedicated OS thread with its own tokio
/// runtime. Returns once the TCP listener is bound, so the caller
/// can log the actual address (useful with `:0` port).
pub fn start(addr: SocketAddr, ctx: ApiContext) -> Result<ApiHandle, ApiError> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<SocketAddr, String>>();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let thread = std::thread::Builder::new()
        .name("arion-api".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .thread_name("arion-api-rt")
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(format!("runtime build: {e}")));
                    return;
                }
            };
            rt.block_on(async move {
                let app: Router = Router::new()
                    .nest("/api/v1", routes::router())
                    .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
                    .layer(TraceLayer::new_for_http())
                    .with_state(ctx);

                let listener = match tokio::net::TcpListener::bind(addr).await {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("bind {addr}: {e}")));
                        return;
                    }
                };
                let local = match listener.local_addr() {
                    Ok(a) => a,
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("local_addr: {e}")));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(local));
                tracing::info!(addr = %local, "arion-api listening");

                let server = axum::serve(listener, app).with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                });
                if let Err(e) = server.await {
                    tracing::warn!(error = %e, "arion-api server exited with error");
                } else {
                    tracing::debug!("arion-api server exited cleanly");
                }
            });
        })
        .map_err(|e| ApiError::Internal(format!("spawn: {e}")))?;

    let bound = ready_rx
        .recv()
        .map_err(|e| ApiError::Internal(format!("ready channel: {e}")))?
        .map_err(ApiError::Internal)?;

    Ok(ApiHandle {
        shutdown: Some(shutdown_tx),
        thread:   Some(thread),
        addr:     bound,
    })
}

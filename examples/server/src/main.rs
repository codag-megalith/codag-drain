//! codag-drain-server - thin codag-drain template host.
//!
//! An axum service exposing:
//!   GET  /health                      (liveness)
//!   POST /v1/session/:id/ingest       (stream lines into a session)
//!   GET  /v1/session/:id/templates    (current template groups; text or json)
//!   POST /v1/template                 (stateless one-shot, mirrors the CLI)
//!
//! Each session is an in-memory `codag_drain::TemplateIndex`; idle sessions are
//! evicted by a background TTL sweeper.

use std::time::Instant;

use codag_drain_server::routes;
use codag_drain_server::session::{AppState, SESSION_TTL, SWEEP_INTERVAL};
use tokio::net::TcpListener;

/// Background task: drop sessions idle longer than [`SESSION_TTL`].
async fn ttl_sweeper(state: AppState) {
    let mut iv = tokio::time::interval(SWEEP_INTERVAL);
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        iv.tick().await;
        let now = Instant::now();
        let before = state.sessions.len();
        state.sessions.retain(|_, e| {
            let last_touch = *e.last_touch.lock().unwrap_or_else(|e| e.into_inner());
            now.duration_since(last_touch) < SESSION_TTL
        });
        let after = state.sessions.len();
        let evicted = before.saturating_sub(after);
        if evicted > 0 {
            tracing::info!(evicted, remaining = after, "ttl sweep");
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "codag_drain_server=info,tower_http=info".into()),
        )
        .init();

    let state = AppState::new();
    tokio::spawn(ttl_sweeper(state.clone()));

    let app = routes::router(state);

    let addr = std::env::var("CODAG_SERVER_ADDR").unwrap_or_else(|_| {
        std::env::var("PORT")
            .map(|port| format!("0.0.0.0:{port}"))
            .unwrap_or_else(|_| "0.0.0.0:8088".to_string())
    });
    let listener = TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));
    tracing::info!(%addr, "server listening");

    axum::serve(listener, app).await.expect("server error");
}

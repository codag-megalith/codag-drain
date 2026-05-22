mod api;
mod config;
mod error;
mod persistence;
mod state;
mod template_miner;

use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::persistence::file::FilePersistence;
use crate::persistence::memory::MemoryPersistence;
use crate::persistence::redis::RedisPersistence;
use crate::persistence::PersistenceHandler;
use crate::template_miner::TemplateMiner;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env();

    let persistence: Arc<dyn PersistenceHandler> = match config.persistence_backend.as_str() {
        "file" => {
            tracing::info!("Using file persistence at {}", config.persistence_file_path);
            Arc::new(FilePersistence::new(&config.persistence_file_path))
        }
        "redis" => {
            tracing::info!("Using Redis persistence at {}", config.redis_url);
            Arc::new(RedisPersistence::new(&config.redis_url, config.redis_key.clone()).await?)
        }
        _ => {
            tracing::info!("Using in-memory persistence");
            Arc::new(MemoryPersistence::new())
        }
    };

    let miner = TemplateMiner::new(&config, persistence).await?;
    let state = Arc::new(RwLock::new(miner));

    let app = api::router(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let addr = format!("0.0.0.0:{}", config.port);
    tracing::info!("Starting server on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install CTRL+C handler");
    tracing::info!("Shutdown signal received");
}

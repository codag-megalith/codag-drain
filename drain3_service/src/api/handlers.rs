use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use crate::error::AppError;
use crate::state::AppState;

use super::models::*;

/// Parse a single log message.
#[utoipa::path(
    post,
    path = "/parse",
    request_body = ParseRequest,
    responses(
        (status = 200, description = "Parsed log message", body = ParseResponse)
    )
)]
pub async fn parse(
    State(state): State<AppState>,
    Json(req): Json<ParseRequest>,
) -> Result<Json<ParseResponse>, AppError> {
    let mut miner = state.write().await;
    let (cluster, change_type) = miner.add_log_message(&req.log_message);
    miner.save_if_dirty().await?;
    Ok(Json(ParseResponse {
        cluster_id: cluster.cluster_id,
        cluster_size: cluster.size,
        template: cluster.get_template(),
        change_type,
    }))
}

/// Parse multiple log messages in batch.
#[utoipa::path(
    post,
    path = "/parse_batch",
    request_body = ParseBatchRequest,
    responses(
        (status = 200, description = "Parsed log messages", body = Vec<ParseResponse>)
    )
)]
pub async fn parse_batch(
    State(state): State<AppState>,
    Json(req): Json<ParseBatchRequest>,
) -> Result<Json<Vec<ParseResponse>>, AppError> {
    let mut miner = state.write().await;
    let mut results = Vec::with_capacity(req.log_messages.len());
    for msg in &req.log_messages {
        let (cluster, change_type) = miner.add_log_message(msg);
        results.push(ParseResponse {
            cluster_id: cluster.cluster_id,
            cluster_size: cluster.size,
            template: cluster.get_template(),
            change_type,
        });
    }
    miner.save_if_dirty().await?;
    Ok(Json(results))
}

/// List all clusters.
#[utoipa::path(
    get,
    path = "/clusters",
    responses(
        (status = 200, description = "All clusters", body = Vec<ClusterResponse>)
    )
)]
pub async fn list_clusters(
    State(state): State<AppState>,
) -> Json<Vec<ClusterResponse>> {
    let miner = state.read().await;
    let clusters: Vec<ClusterResponse> = miner
        .get_clusters()
        .into_iter()
        .map(|c| ClusterResponse {
            cluster_id: c.cluster_id,
            cluster_size: c.size,
            template: c.get_template(),
        })
        .collect();
    Json(clusters)
}

/// Get a specific cluster by ID.
#[utoipa::path(
    get,
    path = "/clusters/{id}",
    params(
        ("id" = usize, Path, description = "Cluster ID")
    ),
    responses(
        (status = 200, description = "Cluster found", body = ClusterResponse),
        (status = 404, description = "Cluster not found")
    )
)]
pub async fn get_cluster(
    State(state): State<AppState>,
    Path(id): Path<usize>,
) -> Result<Json<ClusterResponse>, StatusCode> {
    let miner = state.read().await;
    miner
        .get_clusters()
        .into_iter()
        .find(|c| c.cluster_id == id)
        .map(|c| {
            Json(ClusterResponse {
                cluster_id: c.cluster_id,
                cluster_size: c.size,
                template: c.get_template(),
            })
        })
        .ok_or(StatusCode::NOT_FOUND)
}

/// Health check endpoint.
#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthResponse)
    )
)]
pub async fn health(
    State(state): State<AppState>,
) -> Json<HealthResponse> {
    let miner = state.read().await;
    Json(HealthResponse {
        status: "ok".to_string(),
        cluster_count: miner.cluster_count(),
    })
}

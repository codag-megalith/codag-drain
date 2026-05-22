use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub struct ParseRequest {
    /// The raw log message to parse.
    pub log_message: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ParseBatchRequest {
    /// List of raw log messages to parse.
    pub log_messages: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ParseResponse {
    pub cluster_id: usize,
    pub cluster_size: usize,
    pub template: String,
    pub change_type: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ClusterResponse {
    pub cluster_id: usize,
    pub cluster_size: usize,
    pub template: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    pub status: String,
    pub cluster_count: usize,
}

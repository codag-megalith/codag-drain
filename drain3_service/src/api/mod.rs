pub mod handlers;
pub mod models;

use axum::Router;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::state::AppState;

use self::handlers::*;
use self::models::*;

#[derive(OpenApi)]
#[openapi(
    paths(parse, parse_batch, list_clusters, get_cluster, health),
    components(schemas(
        ParseRequest,
        ParseBatchRequest,
        ParseResponse,
        ClusterResponse,
        HealthResponse,
    ))
)]
struct ApiDoc;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/parse", axum::routing::post(parse))
        .route("/parse_batch", axum::routing::post(parse_batch))
        .route("/clusters", axum::routing::get(list_clusters))
        .route("/clusters/{id}", axum::routing::get(get_cluster))
        .route("/health", axum::routing::get(health))
        .merge(SwaggerUi::new("/swagger-ui").url("/openapi.json", ApiDoc::openapi()))
        .with_state(state)
}

//! Integration tests for the codag-drain template host, driving the axum
//! `Router` directly via `tower::ServiceExt::oneshot` (no real network).

use axum::body::Body;
use axum::http::{HeaderValue, Request, StatusCode};
use http_body_util::BodyExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tower::ServiceExt;
use dashmap::DashMap;

use codag_drain::{template_logs, LogLine, TemplaterConfig};
use codag_drain_server::routes;
use codag_drain_server::session::{self, AppState, Limits};

const TEST_TOKEN: &str = "test-drain-token";

fn sample_text() -> String {
    [
        "worker ready shard=1",
        "worker ready shard=2",
        "worker ready shard=3",
        "node phase changed to Succeeded now",
        "node phase changed to Failed now",
        "node phase changed to Skipped now",
    ]
    .join("\n")
}

fn parsed_lines() -> Vec<LogLine> {
    sample_text().lines().map(session::parse_line).collect()
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn app() -> axum::Router {
    routes::router(state_with_limits(Limits::from_env()))
}

fn limited_app(limits: Limits) -> axum::Router {
    routes::router(state_with_limits(limits))
}

fn state_with_limits(limits: Limits) -> AppState {
    let max_inflight = limits.max_inflight;
    AppState {
        sessions: Arc::new(DashMap::new()),
        limits: Arc::new(limits),
        template_slots: Arc::new(Semaphore::new(max_inflight)),
        auth_token: Some(Arc::<str>::from(TEST_TOKEN)),
    }
}

fn authed(req: Request<Body>) -> Request<Body> {
    let (mut parts, body) = req.into_parts();
    parts.headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer test-drain-token"),
    );
    Request::from_parts(parts, body)
}

#[tokio::test]
async fn health_returns_ok() {
    let resp = app()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_string(resp).await, "ok");
}

#[tokio::test]
async fn templates_404_for_unknown_session() {
    let resp = app()
        .oneshot(authed(
            Request::get("/v1/session/nope/templates")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn v1_routes_require_bearer_auth() {
    let resp = app()
        .oneshot(
            Request::post("/v1/template")
                .body(Body::from("one\ntwo\n"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn v1_routes_fail_closed_without_configured_auth_token() {
    let limits = Limits::from_env();
    let max_inflight = limits.max_inflight;
    let state = AppState {
        sessions: Arc::new(DashMap::new()),
        limits: Arc::new(limits),
        template_slots: Arc::new(Semaphore::new(max_inflight)),
        auth_token: None,
    };
    let resp = routes::router(state)
        .oneshot(authed(
            Request::post("/v1/template")
                .body(Body::from("one\ntwo\n"))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn ingest_then_templates_text_parity() {
    let app = app();
    let body = sample_text();

    let resp = app
        .clone()
        .oneshot(authed(
            Request::post("/v1/session/s1/ingest")
                .body(Body::from(body.clone()))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ingest_json: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    let expected_n = parsed_lines().len();
    assert_eq!(
        ingest_json["ingested"].as_u64().unwrap() as usize,
        expected_n
    );
    assert_eq!(ingest_json["total"].as_u64().unwrap() as usize, expected_n);

    let resp = app
        .oneshot(authed(
            Request::get("/v1/session/s1/templates?format=text&grouper=drain-stock")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let got = body_string(resp).await;

    let cfg = TemplaterConfig {
        grouper: codag_drain::GrouperKind::DrainStock,
        ..TemplaterConfig::default()
    };
    let expected = template_logs(&parsed_lines(), &cfg).render();
    assert_eq!(got, expected, "templates text != library render");
}

#[tokio::test]
async fn template_oneshot_parity() {
    let body = sample_text();
    let resp = app()
        .oneshot(authed(
            Request::post("/v1/template?format=text")
                .body(Body::from(body))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let got = body_string(resp).await;

    let cfg = TemplaterConfig::default();
    let expected = template_logs(&parsed_lines(), &cfg).render();
    assert_eq!(got, expected, "/v1/template != library render");
}

#[tokio::test]
async fn template_oneshot_json_can_parse_text_body() {
    let body = sample_text();
    let resp = app()
        .oneshot(authed(
            Request::post("/v1/template?format=json")
                .header("content-type", "text/plain")
                .body(Body::from(body))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert_eq!(
        json["original_count"].as_u64().unwrap() as usize,
        parsed_lines().len()
    );
    assert!(!json.get("groups").unwrap().as_array().unwrap().is_empty());
}

#[tokio::test]
async fn templates_json_shape() {
    let app = app();
    let body = sample_text();
    app.clone()
        .oneshot(authed(
            Request::post("/v1/session/sj/ingest")
                .body(Body::from(body))
                .unwrap(),
        ))
        .await
        .unwrap();

    let resp = app
        .oneshot(authed(
            Request::get("/v1/session/sj/templates?format=json")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(json.get("groups").unwrap().is_array());
    assert!(json.get("original_count").is_some());
    assert!(json.get("template_count").is_some());
    assert!(json.get("line_compression").is_some());
}

#[tokio::test]
async fn ingest_counts_accumulate_across_two_calls() {
    let app = app();

    let r1 = app
        .clone()
        .oneshot(authed(
            Request::post("/v1/session/acc/ingest")
                .body(Body::from("alpha one\nbeta two\n"))
                .unwrap(),
        ))
        .await
        .unwrap();
    let j1: serde_json::Value = serde_json::from_str(&body_string(r1).await).unwrap();
    assert_eq!(j1["ingested"].as_u64().unwrap(), 2);
    assert_eq!(j1["total"].as_u64().unwrap(), 2);

    let r2 = app
        .clone()
        .oneshot(authed(
            Request::post("/v1/session/acc/ingest")
                .body(Body::from("gamma three\n"))
                .unwrap(),
        ))
        .await
        .unwrap();
    let j2: serde_json::Value = serde_json::from_str(&body_string(r2).await).unwrap();
    assert_eq!(j2["ingested"].as_u64().unwrap(), 1);
    assert_eq!(j2["total"].as_u64().unwrap(), 3);
}

#[tokio::test]
async fn ingest_ndjson_format() {
    let app = app();
    let body =
        "{\"message\":\"boom\",\"level\":\"error\"}\n{\"message\":\"again\",\"level\":\"error\"}\n";
    let r = app
        .oneshot(authed(
            Request::post("/v1/session/nd/ingest?body=ndjson")
                .body(Body::from(body))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let j: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
    assert_eq!(j["ingested"].as_u64().unwrap(), 2);
}

#[tokio::test]
async fn invalid_ndjson_returns_400() {
    let r = app()
        .oneshot(authed(
            Request::post("/v1/template?body=ndjson")
                .body(Body::from("{\"message\":\"ok\"}\nnot json\n"))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn line_limit_returns_413() {
    let r = limited_app(Limits {
        max_body_bytes: 1024,
        max_lines: 1,
        max_line_chars: 100,
        max_samples: 3,
        max_inflight: 1,
        template_timeout: Duration::from_secs(1),
    })
    .oneshot(authed(
        Request::post("/v1/template")
            .body(Body::from("one\ntwo\n"))
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn samples_limit_returns_413() {
    let r = limited_app(Limits {
        max_body_bytes: 1024,
        max_lines: 10,
        max_line_chars: 100,
        max_samples: 1,
        max_inflight: 1,
        template_timeout: Duration::from_secs(1),
    })
    .oneshot(authed(
        Request::post("/v1/template?samples=2")
            .body(Body::from("one\ntwo\n"))
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn concurrency_overflow_returns_429() {
    let limits = Limits {
        max_body_bytes: 1024,
        max_lines: 10,
        max_line_chars: 100,
        max_samples: 3,
        max_inflight: 1,
        template_timeout: Duration::from_secs(1),
    };
    let state = AppState {
        sessions: Arc::new(DashMap::new()),
        limits: Arc::new(limits),
        template_slots: Arc::new(Semaphore::new(1)),
        auth_token: Some(Arc::<str>::from(TEST_TOKEN)),
    };
    let _held = state.template_slots.clone().try_acquire_owned().unwrap();
    let r = routes::router(state)
        .oneshot(authed(
            Request::post("/v1/template")
                .body(Body::from("one\ntwo\n"))
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
}

//! HTTP routes for the thin codag-drain template host.
//!
//! ```text
//! GET  /health                         -> "ok"
//! POST /v1/session/:id/ingest          -> {"ingested": n, "total": m}
//! GET  /v1/session/:id/templates       -> rendered text | json template result
//! POST /v1/template                    -> stateless one-shot template result
//! ```
//!
//! `grouper=drain|drain-stock|drain-delimited|drain-fullsearch|statistical`
//! selects the grouping algorithm; `samples=N` controls raw examples per group;
//! `format=text|json` selects the response shape, and `body=text|ndjson`
//! selects the input format when the Content-Type is not enough.

use std::time::Instant;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{
        header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE},
        HeaderMap, StatusCode,
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use codag_drain::{
    parse_json_line, parse_line, template_logs, GrouperKind, LogLine, TemplaterConfig,
};
use serde::Deserialize;
use serde_json::json;
use tokio::task;
use tokio::time;

use crate::session::{AppState, BodyFormat, Limits, SessionEntry};

/// Build the application router.
pub fn router(state: AppState) -> Router {
    let max_body_bytes = state.limits.max_body_bytes;
    let protected_routes = Router::new()
        .route("/v1/session/:id/ingest", post(ingest))
        .route("/v1/session/:id/templates", get(templates))
        .route("/v1/template", post(template_oneshot))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer_auth,
        ));

    Router::new()
        .route("/health", get(health))
        .merge(protected_routes)
        .layer(DefaultBodyLimit::max(max_body_bytes))
        .with_state(state)
}

/// Common query string: `?grouper=...&samples=...&format=...`.
#[derive(Debug, Deserialize, Default)]
pub struct TemplateQuery {
    grouper: Option<String>,
    samples: Option<usize>,
    format: Option<String>,
    body: Option<String>,
}

fn parse_grouper(s: Option<&str>) -> Option<GrouperKind> {
    match s.map(|m| m.to_ascii_lowercase()).as_deref() {
        Some("drain") => Some(GrouperKind::Drain),
        Some("drain-stock") | Some("stock") => Some(GrouperKind::DrainStock),
        Some("drain-delimited") | Some("delimited") => Some(GrouperKind::DrainDelimited),
        Some("drain-fullsearch") | Some("fullsearch") => Some(GrouperKind::DrainFullSearch),
        Some("statistical") => Some(GrouperKind::Statistical),
        _ => None,
    }
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, msg.into()).into_response()
}

async fn require_bearer_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.auth_token.as_deref() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "drain auth token not configured",
        );
    };

    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    if !presented.is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes())) {
        return (
            StatusCode::UNAUTHORIZED,
            [(WWW_AUTHENTICATE, "Bearer")],
            "unauthorized",
        )
            .into_response();
    }

    next.run(request).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let left = a.get(i).copied().unwrap_or(0);
        let right = b.get(i).copied().unwrap_or(0);
        diff |= (left ^ right) as usize;
    }
    diff == 0
}

fn config_from_query(q: &TemplateQuery, limits: &Limits) -> Result<TemplaterConfig, Response> {
    let mut cfg = TemplaterConfig::default();
    if let Some(g) = parse_grouper(q.grouper.as_deref()) {
        cfg.grouper = g;
    }
    if let Some(samples) = q.samples {
        if samples > limits.max_samples {
            return Err(error(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("samples exceeds {}", limits.max_samples),
            ));
        }
        cfg.sample_cap = samples;
    }
    Ok(cfg)
}

fn wants_json(format: Option<&str>) -> bool {
    matches!(
        format.map(|f| f.to_ascii_lowercase()).as_deref(),
        Some("json")
    )
}

/// Decide the ingest body format: explicit `?body=text|ndjson`, else sniff the
/// `Content-Type` header (`application/x-ndjson` / `application/json`), else text.
fn body_format(headers: &HeaderMap, body: Option<&str>) -> BodyFormat {
    if let Some(value) = body.map(|b| b.to_ascii_lowercase()) {
        match value.as_str() {
            "json" | "ndjson" => return BodyFormat::Ndjson,
            "text" => return BodyFormat::Text,
            _ => {}
        }
    }
    let ct = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ct.contains("ndjson") || ct.contains("application/json") {
        BodyFormat::Ndjson
    } else {
        BodyFormat::Text
    }
}

fn parse_limited_body(
    headers: &HeaderMap,
    body_format_query: Option<&str>,
    body: &Bytes,
    limits: &Limits,
) -> Result<Vec<LogLine>, Response> {
    if body.len() > limits.max_body_bytes {
        return Err(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("body exceeds {} bytes", limits.max_body_bytes),
        ));
    }
    let fmt = body_format(headers, body_format_query);
    let text = String::from_utf8_lossy(body);
    let mut lines = Vec::new();
    for raw in text.lines().filter(|line| !line.trim().is_empty()) {
        let line = match fmt {
            BodyFormat::Text => parse_line(raw),
            BodyFormat::Ndjson => parse_json_line(raw)
                .ok_or_else(|| error(StatusCode::BAD_REQUEST, "invalid ndjson log line"))?,
        };
        if line.message.chars().count() > limits.max_line_chars {
            return Err(error(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("line exceeds {} chars", limits.max_line_chars),
            ));
        }
        lines.push(line);
        if lines.len() > limits.max_lines {
            return Err(error(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("line count exceeds {}", limits.max_lines),
            ));
        }
    }
    Ok(lines)
}

// --------------------------------------------------------------------------
// Handlers
// --------------------------------------------------------------------------

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn ingest(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<TemplateQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let new_lines = match parse_limited_body(&headers, q.body.as_deref(), &body, &state.limits) {
        Ok(lines) => lines,
        Err(resp) => return resp,
    };
    let ingested = new_lines.len();

    let entry = if let Some(e) = state.sessions.get(&id) {
        e.clone()
    } else {
        state.sessions.entry(id.clone()).or_insert_with(|| std::sync::Arc::new(SessionEntry::new())).value().clone()
    };

    let mut index = entry.index.write().await;
    for l in new_lines {
        index.push(l);
    }
    *entry.last_touch.lock().unwrap() = Instant::now();
    let total = index.len();

    tracing::info!(session = %id, ingested, total, "ingest");
    Json(json!({ "ingested": ingested, "total": total })).into_response()
}

async fn templates(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<TemplateQuery>,
) -> Response {
    let cfg = match config_from_query(&q, &state.limits) {
        Ok(cfg) => cfg,
        Err(resp) => return resp,
    };
    let entry = match state.sessions.get(&id) {
        Some(e) => e.clone(),
        None => return (StatusCode::NOT_FOUND, format!("no such session: {id}")).into_response(),
    };
    let index = entry.index.read().await;
    let result = index.templates_with(&cfg);
    if wants_json(q.format.as_deref()) {
        Json(result).into_response()
    } else {
        result.render().into_response()
    }
}

async fn template_oneshot(
    State(state): State<AppState>,
    Query(q): Query<TemplateQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let lines = match parse_limited_body(&headers, q.body.as_deref(), &body, &state.limits) {
        Ok(lines) => lines,
        Err(resp) => return resp,
    };
    let cfg = match config_from_query(&q, &state.limits) {
        Ok(cfg) => cfg,
        Err(resp) => return resp,
    };
    let Ok(permit) = state.template_slots.clone().try_acquire_owned() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "5")],
            "too many in-flight template requests",
        )
            .into_response();
    };
    let timeout = state.limits.template_timeout;
    let join = task::spawn_blocking(move || {
        let _permit = permit;
        template_logs(&lines, &cfg)
    });
    let result = match time::timeout(timeout, join).await {
        Ok(Ok(result)) => result,
        Ok(Err(_join_err)) => {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "template worker failed")
        }
        Err(_elapsed) => return error(StatusCode::SERVICE_UNAVAILABLE, "template timed out"),
    };
    if wants_json(q.format.as_deref()) {
        Json(result).into_response()
    } else {
        result.render().into_response()
    }
}

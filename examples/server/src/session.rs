//! Session state for the codag-drain template host.
//!
//! A *session* is a long-lived [`TemplateIndex`] keyed by an opaque id. Agents
//! `POST` log lines into it incrementally and `GET` projected templates. State
//! lives entirely in memory; sessions idle longer than [`SESSION_TTL`] are
//! evicted by a background task (see `main.rs`).

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use codag_drain::{TemplateIndex, TemplaterConfig};
use dashmap::DashMap;
use tokio::sync::{RwLock, Semaphore};

pub use codag_drain::{parse_body, parse_json_line, parse_line, BodyFormat};

/// Sessions idle longer than this are evicted by the background sweeper.
pub const SESSION_TTL: Duration = Duration::from_secs(30 * 60);

/// How often the background sweeper runs.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Public-host safety limits. Tuned by env in production.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_body_bytes: usize,
    pub max_lines: usize,
    pub max_line_chars: usize,
    pub max_samples: usize,
    pub max_inflight: usize,
    pub template_timeout: Duration,
}

impl Limits {
    pub fn from_env() -> Self {
        Limits {
            max_body_bytes: env_usize("CODAG_DRAIN_MAX_BODY_BYTES", 4 * 1024 * 1024),
            max_lines: env_usize("CODAG_DRAIN_MAX_LINES", 5_000),
            max_line_chars: env_usize("CODAG_DRAIN_MAX_LINE_CHARS", 20_000),
            max_samples: env_usize("CODAG_DRAIN_MAX_SAMPLES", 10),
            max_inflight: env_usize("CODAG_DRAIN_MAX_INFLIGHT", 1_000),
            template_timeout: Duration::from_millis(env_u64(
                "CODAG_DRAIN_TEMPLATE_TIMEOUT_MS",
                10_000,
            )),
        }
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// One live session: a streaming index plus its last-touch timestamp.
#[derive(Debug)]
pub struct SessionEntry {
    pub index: RwLock<TemplateIndex>,
    pub last_touch: std::sync::Mutex<Instant>,
}

impl SessionEntry {
    pub fn new() -> Self {
        SessionEntry::default()
    }
}

impl Default for SessionEntry {
    fn default() -> Self {
        SessionEntry {
            index: RwLock::new(TemplateIndex::new(TemplaterConfig::default())),
            last_touch: std::sync::Mutex::new(Instant::now()),
        }
    }
}

/// Shared, cloneable application state.
#[derive(Clone)]
pub struct AppState {
    pub sessions: Arc<DashMap<String, Arc<SessionEntry>>>,
    pub limits: Arc<Limits>,
    pub template_slots: Arc<Semaphore>,
    pub auth_token: Option<Arc<str>>,
}

impl AppState {
    pub fn new() -> Self {
        let limits = Limits::from_env();
        let max_inflight = limits.max_inflight;
        AppState {
            sessions: Arc::new(DashMap::new()),
            limits: Arc::new(limits),
            template_slots: Arc::new(Semaphore::new(max_inflight)),
            auth_token: auth_token_from_env(),
        }
    }
}

fn auth_token_from_env() -> Option<Arc<str>> {
    env::var("CODAG_DRAIN_AUTH_TOKEN")
        .or_else(|_| env::var("CODAG_DRAIN_TOKEN"))
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .map(Arc::<str>::from)
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::from_env()
    }
}

use std::env;

/// Application configuration loaded from environment variables.
pub struct Config {
    pub drain_depth: usize,
    pub drain_sim_th: f64,
    pub drain_max_children: usize,
    pub drain_max_clusters: Option<usize>,
    pub persistence_backend: String,
    pub persistence_file_path: String,
    pub redis_url: String,
    pub redis_key: String,
    pub port: u16,
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            drain_depth: env_parse("DRAIN_DEPTH", 4),
            drain_sim_th: env_parse("DRAIN_SIM_TH", 0.4),
            drain_max_children: env_parse("DRAIN_MAX_CHILDREN", 100),
            drain_max_clusters: env::var("DRAIN_MAX_CLUSTERS")
                .ok()
                .and_then(|v| v.parse().ok()),
            persistence_backend: env::var("PERSISTENCE_BACKEND")
                .unwrap_or_else(|_| "memory".to_string()),
            persistence_file_path: env::var("PERSISTENCE_FILE_PATH")
                .unwrap_or_else(|_| "/data/drain3_state.json".to_string()),
            redis_url: env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://localhost:6379".to_string()),
            redis_key: env::var("REDIS_KEY")
                .unwrap_or_else(|_| "drain3:state".to_string()),
            port: env_parse("PORT", 3000),
        }
    }
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

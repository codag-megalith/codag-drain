/// A single log message cluster (template group).
///
/// Tracks a generalized template and the number of messages that matched it.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LogCluster {
    pub log_template_tokens: Vec<String>,
    pub cluster_id: usize,
    pub size: usize,
}

impl LogCluster {
    pub fn new(tokens: Vec<String>, cluster_id: usize) -> Self {
        Self {
            log_template_tokens: tokens,
            cluster_id,
            size: 1,
        }
    }

    pub fn get_template(&self) -> String {
        self.log_template_tokens.join(" ")
    }
}

impl std::fmt::Display for LogCluster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ID={:<5} : size={:<10}: {}",
            self.cluster_id,
            self.size,
            self.get_template()
        )
    }
}

/// Describes what changed when a log message was ingested.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum UpdateType {
    ClusterCreated,
    ClusterTemplateChanged,
    None,
}

impl UpdateType {
    pub fn as_str(&self) -> &'static str {
        match self {
            UpdateType::ClusterCreated => "cluster_created",
            UpdateType::ClusterTemplateChanged => "cluster_template_changed",
            UpdateType::None => "none",
        }
    }
}

use std::sync::Arc;

use drain3_rust::cluster::LogCluster;
use drain3_rust::masking::{default_masking_instructions, LogMasker};
use drain3_rust::Drain;

use crate::config::Config;
use crate::persistence::PersistenceHandler;

/// Orchestrates a `Drain` instance with persistence.
///
/// Loads state on init, re-attaches the masker from config (since Regex can't
/// be serialized), and saves state after mutations.
pub struct TemplateMiner {
    drain: Drain,
    persistence: Arc<dyn PersistenceHandler>,
    dirty: bool,
}

impl TemplateMiner {
    /// Create a new TemplateMiner, loading persisted state if available.
    pub async fn new(
        config: &Config,
        persistence: Arc<dyn PersistenceHandler>,
    ) -> anyhow::Result<Self> {
        let mut drain = match persistence.load_state().await? {
            Some(data) => {
                tracing::info!("Restoring Drain state from persistence");
                Drain::from_json(&data)?
            }
            None => {
                tracing::info!("Starting fresh Drain instance");
                Drain::new(
                    config.drain_depth,
                    config.drain_sim_th,
                    config.drain_max_children,
                    config.drain_max_clusters,
                    vec![],
                    "<*>".to_string(),
                    true,
                )
            }
        };

        // Re-attach masker (Regex can't be serialized)
        drain.set_masker(LogMasker::new(default_masking_instructions()));

        Ok(Self {
            drain,
            persistence,
            dirty: false,
        })
    }

    /// Parse a log message, returning the cluster it matched/created.
    pub fn add_log_message(&mut self, content: &str) -> (LogCluster, String) {
        let (cluster, update_type) = self.drain.add_log_message(content);
        let change_type = update_type.as_str().to_string();
        if change_type != "none" {
            self.dirty = true;
        }
        (cluster, change_type)
    }

    /// Read-only match against existing clusters.
    pub fn match_log(&self, content: &str) -> Option<LogCluster> {
        self.drain.match_default(content)
    }

    /// Return all current clusters.
    pub fn get_clusters(&self) -> Vec<LogCluster> {
        self.drain.clusters().into_iter().cloned().collect()
    }

    /// Return cluster count.
    pub fn cluster_count(&self) -> usize {
        self.drain.cluster_count()
    }

    /// Persist state if dirty.
    pub async fn save_if_dirty(&mut self) -> anyhow::Result<()> {
        if self.dirty {
            let data = self.drain.to_json()?;
            self.persistence.save_state(&data).await?;
            self.dirty = false;
            tracing::debug!("State persisted");
        }
        Ok(())
    }
}

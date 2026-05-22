use std::path::{Path, PathBuf};

use async_trait::async_trait;

use super::PersistenceHandler;

/// File-based persistence with atomic writes (write to .tmp, then rename).
/// Ideal for Railway volumes or any persistent filesystem.
pub struct FilePersistence {
    path: PathBuf,
}

impl FilePersistence {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[async_trait]
impl PersistenceHandler for FilePersistence {
    async fn save_state(&self, state: &[u8]) -> anyhow::Result<()> {
        let tmp_path = self.path.with_extension("json.tmp");

        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tokio::fs::write(&tmp_path, state).await?;
        tokio::fs::rename(&tmp_path, &self.path).await?;

        tracing::debug!("State saved to {}", self.path.display());
        Ok(())
    }

    async fn load_state(&self) -> anyhow::Result<Option<Vec<u8>>> {
        if !Path::new(&self.path).exists() {
            return Ok(None);
        }
        let data = tokio::fs::read(&self.path).await?;
        tracing::info!("State loaded from {}", self.path.display());
        Ok(Some(data))
    }
}

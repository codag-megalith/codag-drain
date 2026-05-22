use async_trait::async_trait;
use tokio::sync::RwLock;

use super::PersistenceHandler;

/// In-memory persistence — useful for testing and ephemeral deployments.
pub struct MemoryPersistence {
    data: RwLock<Option<Vec<u8>>>,
}

impl MemoryPersistence {
    pub fn new() -> Self {
        Self {
            data: RwLock::new(None),
        }
    }
}

#[async_trait]
impl PersistenceHandler for MemoryPersistence {
    async fn save_state(&self, state: &[u8]) -> anyhow::Result<()> {
        *self.data.write().await = Some(state.to_vec());
        Ok(())
    }

    async fn load_state(&self) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.data.read().await.clone())
    }
}

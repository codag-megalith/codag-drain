pub mod file;
pub mod memory;
pub mod redis;

use async_trait::async_trait;

/// Pluggable persistence backend (mirrors Python Drain3's PersistenceHandler).
#[async_trait]
pub trait PersistenceHandler: Send + Sync {
    async fn save_state(&self, state: &[u8]) -> anyhow::Result<()>;
    async fn load_state(&self) -> anyhow::Result<Option<Vec<u8>>>;
}

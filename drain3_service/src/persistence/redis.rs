use async_trait::async_trait;
use redis::AsyncCommands;

use super::PersistenceHandler;

pub struct RedisPersistence {
    conn: redis::aio::MultiplexedConnection,
    key: String,
}

impl RedisPersistence {
    pub async fn new(url: &str, key: String) -> anyhow::Result<Self> {
        let client = redis::Client::open(url)?;
        let conn = client.get_multiplexed_async_connection().await?;
        Ok(Self { conn, key })
    }
}

#[async_trait]
impl PersistenceHandler for RedisPersistence {
    async fn save_state(&self, state: &[u8]) -> anyhow::Result<()> {
        let mut conn = self.conn.clone();
        conn.set::<_, _, ()>(&self.key, state).await?;
        Ok(())
    }

    async fn load_state(&self) -> anyhow::Result<Option<Vec<u8>>> {
        let mut conn = self.conn.clone();
        let result: Option<Vec<u8>> = conn.get(&self.key).await?;
        Ok(result)
    }
}

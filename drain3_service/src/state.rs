use std::sync::Arc;
use tokio::sync::RwLock;

use crate::template_miner::TemplateMiner;

/// Shared application state accessible from all handlers.
pub type AppState = Arc<RwLock<TemplateMiner>>;

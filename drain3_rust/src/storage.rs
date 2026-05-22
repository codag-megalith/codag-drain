use std::collections::HashMap;
use std::num::NonZeroUsize;

use lru::LruCache;

use crate::cluster::LogCluster;

/// Backing store for log clusters.
///
/// - `Unlimited` — plain `HashMap`, no eviction.
/// - `Limited`   — `LruCache` that evicts the least-recently-used cluster
///                 once `max_clusters` is reached.
pub enum ClusterStorage {
    Unlimited(HashMap<usize, LogCluster>),
    Limited(LruCache<usize, LogCluster>),
}

#[cfg(feature = "serde")]
mod serde_impl {
    use super::*;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    struct ClusterStorageSurrogate {
        max_clusters: Option<usize>,
        entries: Vec<(usize, LogCluster)>,
    }

    impl Serialize for ClusterStorage {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let surrogate = match self {
                ClusterStorage::Unlimited(map) => ClusterStorageSurrogate {
                    max_clusters: None,
                    entries: map.iter().map(|(&k, v)| (k, v.clone())).collect(),
                },
                ClusterStorage::Limited(cache) => ClusterStorageSurrogate {
                    max_clusters: Some(cache.cap().get()),
                    entries: cache.iter().map(|(&k, v)| (k, v.clone())).collect(),
                },
            };
            surrogate.serialize(serializer)
        }
    }

    impl<'de> Deserialize<'de> for ClusterStorage {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let surrogate = ClusterStorageSurrogate::deserialize(deserializer)?;
            match surrogate.max_clusters {
                None => {
                    let map: HashMap<usize, LogCluster> =
                        surrogate.entries.into_iter().collect();
                    Ok(ClusterStorage::Unlimited(map))
                }
                Some(cap) => {
                    let mut cache =
                        LruCache::new(NonZeroUsize::new(cap).unwrap());
                    for (k, v) in surrogate.entries {
                        cache.put(k, v);
                    }
                    Ok(ClusterStorage::Limited(cache))
                }
            }
        }
    }
}

impl ClusterStorage {
    pub fn new(max_clusters: Option<usize>) -> Self {
        match max_clusters {
            None => ClusterStorage::Unlimited(HashMap::new()),
            Some(cap) => {
                ClusterStorage::Limited(LruCache::new(NonZeroUsize::new(cap).unwrap()))
            }
        }
    }

    /// Insert or update a cluster (touches LRU).
    pub fn insert(&mut self, id: usize, cluster: LogCluster) {
        match self {
            ClusterStorage::Unlimited(map) => {
                map.insert(id, cluster);
            }
            ClusterStorage::Limited(cache) => {
                cache.put(id, cluster);
            }
        }
    }

    /// Peek at a cluster **without** promoting it in the LRU.
    pub fn peek(&self, id: &usize) -> Option<&LogCluster> {
        match self {
            ClusterStorage::Unlimited(map) => map.get(id),
            ClusterStorage::Limited(cache) => cache.peek(id),
        }
    }

    /// Get a mutable reference, **promoting** the entry in the LRU.
    pub fn get_mut(&mut self, id: &usize) -> Option<&mut LogCluster> {
        match self {
            ClusterStorage::Unlimited(map) => map.get_mut(id),
            ClusterStorage::Limited(cache) => cache.get_mut(id),
        }
    }

    /// Mark a cluster as recently used (LRU promotion only).
    pub fn touch(&mut self, id: &usize) {
        match self {
            ClusterStorage::Unlimited(_) => {}
            ClusterStorage::Limited(cache) => {
                let _ = cache.get(id);
            }
        }
    }

    pub fn contains(&self, id: &usize) -> bool {
        match self {
            ClusterStorage::Unlimited(map) => map.contains_key(id),
            ClusterStorage::Limited(cache) => cache.peek(id).is_some(),
        }
    }

    pub fn values(&self) -> Vec<&LogCluster> {
        match self {
            ClusterStorage::Unlimited(map) => map.values().collect(),
            ClusterStorage::Limited(cache) => cache.iter().map(|(_, v)| v).collect(),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            ClusterStorage::Unlimited(map) => map.len(),
            ClusterStorage::Limited(cache) => cache.len(),
        }
    }
}

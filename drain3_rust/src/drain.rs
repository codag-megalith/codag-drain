use crate::cluster::{LogCluster, UpdateType};
use crate::masking::LogMasker;
use crate::node::Node;
use crate::similarity::{create_template, get_seq_distance};
use crate::storage::ClusterStorage;

/// Core Drain log-parsing engine.
///
/// Maintains a prefix tree that groups log messages into clusters sharing a
/// common template.  Tokens that vary across messages in the same cluster are
/// replaced by a configurable wildcard string (`param_str`).
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Drain {
    max_node_depth: usize,
    sim_th: f64,
    max_children: usize,
    root_node: Node,
    extra_delimiters: Vec<String>,
    param_str: String,
    parametrize_numeric_tokens: bool,
    #[cfg_attr(feature = "serde", serde(skip))]
    masker: Option<LogMasker>,
    id_to_cluster: ClusterStorage,
    clusters_counter: usize,
}

impl Drain {
    pub fn new(
        depth: usize,
        sim_th: f64,
        max_children: usize,
        max_clusters: Option<usize>,
        extra_delimiters: Vec<String>,
        param_str: String,
        parametrize_numeric_tokens: bool,
    ) -> Self {
        assert!(depth >= 3, "depth argument must be at least 3");
        Self {
            max_node_depth: depth - 2,
            sim_th,
            max_children,
            root_node: Node::new(),
            extra_delimiters,
            param_str,
            parametrize_numeric_tokens,
            masker: None,
            id_to_cluster: ClusterStorage::new(max_clusters),
            clusters_counter: 0,
        }
    }

    /// Create a Drain instance with default parameters.
    pub fn default() -> Self {
        Self::new(4, 0.4, 100, None, vec![], "<*>".to_string(), true)
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// Ingest a raw log line.
    ///
    /// Returns the (possibly new) cluster the message was assigned to and a tag
    /// describing what changed.
    pub fn add_log_message(&mut self, content: &str) -> (LogCluster, UpdateType) {
        let masked = match &self.masker {
            Some(m) => m.mask(content),
            None => content.to_string(),
        };
        let content_tokens = self.get_content_as_tokens(&masked);
        self.add_log_message_from_tokens(content_tokens)
    }

    /// Ingest a pre-tokenized log line.
    ///
    /// Same as [`add_log_message`] but skips tokenization — useful when
    /// preprocessing has already been done (e.g. in the concurrent pipeline).
    pub fn add_log_message_from_tokens(
        &mut self,
        content_tokens: Vec<String>,
    ) -> (LogCluster, UpdateType) {
        let match_cluster_id = self.tree_search(&content_tokens, self.sim_th, false);

        if let Some(cluster_id) = match_cluster_id {
            let cluster = self.id_to_cluster.get_mut(&cluster_id).unwrap();
            let new_template = create_template(
                &content_tokens,
                &cluster.log_template_tokens,
                &self.param_str,
            );
            let update_type = if new_template == cluster.log_template_tokens {
                UpdateType::None
            } else {
                cluster.log_template_tokens = new_template;
                UpdateType::ClusterTemplateChanged
            };
            cluster.size += 1;
            let ret = cluster.clone();
            self.id_to_cluster.touch(&cluster_id);
            (ret, update_type)
        } else {
            self.clusters_counter += 1;
            let cluster_id = self.clusters_counter;
            let cluster = LogCluster::new(content_tokens, cluster_id);
            self.id_to_cluster.insert(cluster_id, cluster.clone());
            self.add_seq_to_prefix_tree(&cluster);
            (cluster, UpdateType::ClusterCreated)
        }
    }

    /// Read-only match against existing clusters (sim_th = 1.0, no mutations).
    pub fn match_log(&self, content: &str, full_search_strategy: &str) -> Option<LogCluster> {
        assert!(
            full_search_strategy == "always"
                || full_search_strategy == "never"
                || full_search_strategy == "fallback"
        );

        let required_sim_th = 1.0;
        let content_tokens = self.get_content_as_tokens(content);

        let full_search = || -> Option<LogCluster> {
            let all_ids = self.get_clusters_ids_for_seq_len(content_tokens.len());
            self.fast_match(&all_ids, &content_tokens, required_sim_th, true)
        };

        if full_search_strategy == "always" {
            return full_search();
        }

        if let Some(cluster_id) = self.tree_search(&content_tokens, required_sim_th, true) {
            return self.id_to_cluster.peek(&cluster_id).cloned();
        }

        if full_search_strategy == "never" {
            return None;
        }

        full_search()
    }

    /// Convenience wrapper — `match_log` with strategy `"never"`.
    pub fn match_default(&self, content: &str) -> Option<LogCluster> {
        self.match_log(content, "never")
    }

    /// Sum of `size` across all live clusters.
    pub fn get_total_cluster_size(&self) -> usize {
        self.id_to_cluster.values().iter().map(|c| c.size).sum()
    }

    /// Number of live clusters.
    pub fn cluster_count(&self) -> usize {
        self.id_to_cluster.len()
    }

    /// Return all live clusters.
    pub fn clusters(&self) -> Vec<&LogCluster> {
        self.id_to_cluster.values()
    }

    /// Public template-creation helper (mainly for testing).
    pub fn create_template_pub(&self, seq1: &[String], seq2: &[String]) -> Vec<String> {
        create_template(seq1, seq2, &self.param_str)
    }

    // -----------------------------------------------------------------------
    // Serialization
    // -----------------------------------------------------------------------

    /// Serialize the Drain state to JSON bytes.
    #[cfg(feature = "serde")]
    pub fn to_json(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec(self)
    }

    /// Deserialize a Drain instance from JSON bytes.
    ///
    /// Note: the masker field is not serialized (Regex can't be serialized).
    /// Callers must re-attach a masker via `set_masker()` after loading.
    #[cfg(feature = "serde")]
    pub fn from_json(data: &[u8]) -> serde_json::Result<Self> {
        serde_json::from_slice(data)
    }

    // -----------------------------------------------------------------------
    // Tokenisation
    // -----------------------------------------------------------------------

    /// Return a reference to the extra delimiters used for tokenization.
    pub fn extra_delimiters(&self) -> &[String] {
        &self.extra_delimiters
    }

    /// Set the log masker (applied before tokenization).
    pub fn set_masker(&mut self, masker: LogMasker) {
        self.masker = Some(masker);
    }

    /// Return a reference to the configured masker, if any.
    pub fn masker(&self) -> Option<&LogMasker> {
        self.masker.as_ref()
    }

    pub fn get_content_as_tokens(&self, content: &str) -> Vec<String> {
        let mut content = content.trim().to_string();
        for delim in &self.extra_delimiters {
            content = content.replace(delim.as_str(), " ");
        }
        content
            .split_whitespace()
            .map(|s| s.to_string())
            .collect()
    }

    fn has_numbers(s: &str) -> bool {
        s.chars().any(|c| c.is_ascii_digit())
    }

    // -----------------------------------------------------------------------
    // Tree search
    // -----------------------------------------------------------------------

    /// Walk the prefix tree to find the best matching cluster.
    fn tree_search(
        &self,
        tokens: &[String],
        sim_th: f64,
        include_params: bool,
    ) -> Option<usize> {
        let token_count = tokens.len();
        let token_count_str = token_count.to_string();

        let cur_node = self.root_node.key_to_child_node.get(&token_count_str)?;

        if token_count == 0 {
            let cid = cur_node.cluster_ids.first()?;
            return if self.id_to_cluster.contains(cid) {
                Some(*cid)
            } else {
                None
            };
        }

        let mut cur_node = cur_node;
        let mut cur_node_depth: usize = 1;

        for token in tokens {
            if cur_node_depth >= self.max_node_depth {
                break;
            }
            if cur_node_depth >= token_count {
                break;
            }

            if let Some(child) = cur_node.key_to_child_node.get(token.as_str()) {
                cur_node = child;
            } else if let Some(child) = cur_node.key_to_child_node.get(&self.param_str) {
                cur_node = child;
            } else {
                return None;
            }

            cur_node_depth += 1;
        }

        self.fast_match(&cur_node.cluster_ids, tokens, sim_th, include_params)
            .map(|c| c.cluster_id)
    }

    // -----------------------------------------------------------------------
    // Candidate scoring
    // -----------------------------------------------------------------------

    /// Score every candidate cluster and return the best one above `sim_th`.
    fn fast_match(
        &self,
        cluster_ids: &[usize],
        tokens: &[String],
        sim_th: f64,
        include_params: bool,
    ) -> Option<LogCluster> {
        let mut max_sim: f64 = -1.0;
        let mut max_param_count: i64 = -1;
        let mut max_cluster: Option<&LogCluster> = None;

        for &cluster_id in cluster_ids {
            let cluster = match self.id_to_cluster.peek(&cluster_id) {
                Some(c) => c,
                None => continue,
            };
            let (cur_sim, param_count) = get_seq_distance(
                &cluster.log_template_tokens,
                tokens,
                include_params,
                &self.param_str,
            );
            if cur_sim > max_sim
                || (cur_sim == max_sim && param_count as i64 > max_param_count)
            {
                max_sim = cur_sim;
                max_param_count = param_count as i64;
                max_cluster = Some(cluster);
            }
        }

        if max_sim >= sim_th {
            max_cluster.cloned()
        } else {
            None
        }
    }

    // -----------------------------------------------------------------------
    // Tree insertion
    // -----------------------------------------------------------------------

    /// Insert a newly created cluster into the prefix tree.
    fn add_seq_to_prefix_tree(&mut self, cluster: &LogCluster) {
        let token_count = cluster.log_template_tokens.len();
        let token_count_str = token_count.to_string();

        if !self.root_node.key_to_child_node.contains_key(&token_count_str) {
            self.root_node
                .key_to_child_node
                .insert(token_count_str.clone(), Node::new());
        }

        let first_layer_node = self
            .root_node
            .key_to_child_node
            .get_mut(&token_count_str)
            .unwrap();

        let mut cur_node = first_layer_node as *mut Node;

        if token_count == 0 {
            unsafe {
                (*cur_node).cluster_ids = vec![cluster.cluster_id];
            }
            return;
        }

        let mut current_depth: usize = 1;
        for token in &cluster.log_template_tokens {
            let node = unsafe { &mut *cur_node };

            if current_depth >= self.max_node_depth || current_depth >= token_count {
                // Leaf: clean stale IDs, then append.
                let mut new_cluster_ids: Vec<usize> = node
                    .cluster_ids
                    .iter()
                    .copied()
                    .filter(|id| self.id_to_cluster.contains(id))
                    .collect();
                new_cluster_ids.push(cluster.cluster_id);
                node.cluster_ids = new_cluster_ids;
                break;
            }

            if !node.key_to_child_node.contains_key(token.as_str()) {
                if self.parametrize_numeric_tokens && Self::has_numbers(token) {
                    if !node.key_to_child_node.contains_key(&self.param_str) {
                        node.key_to_child_node
                            .insert(self.param_str.clone(), Node::new());
                    }
                    cur_node = node
                        .key_to_child_node
                        .get_mut(&self.param_str)
                        .unwrap() as *mut Node;
                } else if node.key_to_child_node.contains_key(&self.param_str) {
                    if node.key_to_child_node.len() < self.max_children {
                        node.key_to_child_node.insert(token.clone(), Node::new());
                        cur_node =
                            node.key_to_child_node.get_mut(token.as_str()).unwrap() as *mut Node;
                    } else {
                        cur_node = node
                            .key_to_child_node
                            .get_mut(&self.param_str)
                            .unwrap() as *mut Node;
                    }
                } else {
                    let children_count = node.key_to_child_node.len();
                    if children_count + 1 < self.max_children {
                        node.key_to_child_node.insert(token.clone(), Node::new());
                        cur_node =
                            node.key_to_child_node.get_mut(token.as_str()).unwrap() as *mut Node;
                    } else if children_count + 1 == self.max_children {
                        node.key_to_child_node
                            .insert(self.param_str.clone(), Node::new());
                        cur_node = node
                            .key_to_child_node
                            .get_mut(&self.param_str)
                            .unwrap() as *mut Node;
                    } else {
                        cur_node = node
                            .key_to_child_node
                            .get_mut(&self.param_str)
                            .unwrap() as *mut Node;
                    }
                }
            } else {
                cur_node =
                    node.key_to_child_node.get_mut(token.as_str()).unwrap() as *mut Node;
            }

            current_depth += 1;
        }
    }

    // -----------------------------------------------------------------------
    // Cluster collection helpers
    // -----------------------------------------------------------------------

    /// Collect every cluster ID reachable under the subtree for a given token count.
    fn get_clusters_ids_for_seq_len(&self, seq_len: usize) -> Vec<usize> {
        fn collect_recursive(node: &Node, ids: &mut Vec<usize>) {
            ids.extend_from_slice(&node.cluster_ids);
            for child in node.key_to_child_node.values() {
                collect_recursive(child, ids);
            }
        }

        let key = seq_len.to_string();
        match self.root_node.key_to_child_node.get(&key) {
            Some(node) => {
                let mut ids = Vec::new();
                collect_recursive(node, &mut ids);
                ids
            }
            None => Vec::new(),
        }
    }
}

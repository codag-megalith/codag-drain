//! Groupers.
//!
//! Deterministic groupers, ported / adapted from
//! `v2/src/v2/templater/grouping.py`:
//!   - `DrainGrouper`             - Drain3-compatible online template mining.
//!   - `DrainFullSearchGrouper`   - Drain similarity without prefix-tree routing.
//!   - `StatisticalGrouper`       - generic lexical co-occurrence experiment.
//!
//! Determinism is mandatory: groups are emitted in ascending first-member-index
//! order, members ascending. We never iterate a `HashMap` for emission; blocks
//! are keyed in deterministic containers.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::compress::lex::{anchor_chars, anchor_count, lex, token_is_anchor, LexToken};
use crate::compress::{GrouperKind, LogLine, TemplaterConfig, DEFAULT_MIN_STATIC_CHARS};
use drain3_rust::masking::{default_masking_instructions, LogMasker};
use drain3_rust::similarity::{create_template, get_seq_distance};
use drain3_rust::Drain;

/// One group of similar lines. `member_indices` is ascending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub member_indices: Vec<usize>,
}

/// A grouper assigns each line to a group.
pub trait Grouper {
    fn group(&self, lines: &[LogLine]) -> Vec<Group>;
}

/// Build a `Box<dyn Grouper>` from the config.
pub fn make_grouper(config: &TemplaterConfig) -> Box<dyn Grouper> {
    match config.grouper {
        GrouperKind::Drain => Box::new(DrainGrouper {
            depth: config.drain_depth,
            sim_th: config.drain_sim_th,
            max_children: config.drain_max_children,
            mode: DrainTokenMode::CompactFallback,
        }),
        GrouperKind::DrainStock => Box::new(DrainGrouper {
            depth: config.drain_depth,
            sim_th: config.drain_sim_th,
            max_children: config.drain_max_children,
            mode: DrainTokenMode::Whitespace,
        }),
        GrouperKind::DrainDelimited => Box::new(DrainGrouper {
            depth: config.drain_depth,
            sim_th: config.drain_sim_th,
            max_children: config.drain_max_children,
            mode: DrainTokenMode::Delimited,
        }),
        GrouperKind::DrainFullSearch => Box::new(DrainFullSearchGrouper {
            sim_th: config.drain_sim_th,
            mode: DrainTokenMode::Whitespace,
        }),
        GrouperKind::Statistical => Box::new(StatisticalGrouper::default()),
    }
}

/// Emit groups sorted by their smallest member index, members ascending.
fn finalize(mut buckets: Vec<Vec<usize>>) -> Vec<Group> {
    for b in &mut buckets {
        b.sort_unstable();
    }
    buckets.sort_by_key(|b| b.first().copied().unwrap_or(usize::MAX));
    buckets
        .into_iter()
        .filter(|b| !b.is_empty())
        .map(|member_indices| Group { member_indices })
        .collect()
}

// ---------------------------------------------------------------------------
// DrainGrouper
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrainTokenMode {
    Whitespace,
    CompactFallback,
    Delimited,
}

/// Drain3-compatible grouping with a compact-line tokenizer fallback.
///
/// This is the primary codag-drain path. Drain performs data-driven variable
/// discovery by positional similarity, which generalizes to alpha/enum/name
/// variables that structural regex masking misses. codag keeps Drain3-compatible
/// tokenization for normal whitespace-delimited logs, but switches to a generic
/// character-class tokenizer for compact, punctuation-heavy one-token lines
/// such as compact JSON/logfmt. The adaptation layer then derives richer
/// templates, raw samples, and slot summaries from the resulting groups.
#[derive(Debug, Clone)]
pub struct DrainGrouper {
    pub depth: usize,
    pub sim_th: f64,
    pub max_children: usize,
    mode: DrainTokenMode,
}

impl Default for DrainGrouper {
    fn default() -> Self {
        DrainGrouper {
            depth: 4,
            sim_th: 0.4,
            max_children: 100,
            mode: DrainTokenMode::CompactFallback,
        }
    }
}

impl Grouper for DrainGrouper {
    fn group(&self, lines: &[LogLine]) -> Vec<Group> {
        let mut drain = Drain::new(
            self.depth,
            self.sim_th,
            self.max_children,
            None,
            vec![],
            "<*>".into(),
            true,
        );
        let masker = LogMasker::new(default_masking_instructions());

        let mut by_cluster: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (idx, line) in lines.iter().enumerate() {
            let tokens = drain_tokens(&line.message, self.mode, &masker);
            let (cluster, _update) = drain.add_log_message_from_tokens(tokens);
            by_cluster.entry(cluster.cluster_id).or_default().push(idx);
        }

        finalize(by_cluster.into_values().collect())
    }
}

fn generic_delimiters() -> &'static [&'static str] {
    &["=", ":", ",", "{", "}", "[", "]", "(", ")", "\"", "'", ";"]
}

fn whitespace_tokens(line: &str, masker: &LogMasker) -> Vec<String> {
    masker
        .mask(line)
        .split_whitespace()
        .map(|s| s.to_string())
        .collect()
}

fn delimited_tokens(line: &str, masker: &LogMasker) -> Vec<String> {
    let mut masked = masker.mask(line);
    for delim in generic_delimiters() {
        masked = masked.replace(delim, " ");
    }
    masked.split_whitespace().map(|s| s.to_string()).collect()
}

fn drain_tokens(line: &str, mode: DrainTokenMode, masker: &LogMasker) -> Vec<String> {
    match mode {
        DrainTokenMode::Whitespace => whitespace_tokens(line, masker),
        DrainTokenMode::CompactFallback => {
            if let Some(tokens) = json_tokens(line) {
                tokens
            } else if use_compact_lexing(line) {
                lex(line).into_iter().map(|t| t.text).collect()
            } else {
                whitespace_tokens(line, masker)
            }
        }
        DrainTokenMode::Delimited => delimited_tokens(line, masker),
    }
}

/// Tokenize JSON by its parsed structure, keeping every scalar value in one
/// token. In particular, spaces inside a JSON string must not change the token
/// count: prompts, stack traces, and other free-form fields commonly grow while
/// the surrounding event schema remains stable.
fn json_tokens(line: &str) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if !value.is_object() && !value.is_array() {
        return None;
    }

    fn append(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, value) in fields {
                    out.push(format!("key:{key}"));
                    append(value, out);
                }
            }
            serde_json::Value::Array(items) => {
                out.push("[array]".to_string());
                for item in items {
                    append(item, out);
                }
                out.push("[/array]".to_string());
            }
            serde_json::Value::String(value) => out.push(format!("string:{value}")),
            serde_json::Value::Number(value) => out.push(format!("number:{value}")),
            serde_json::Value::Bool(value) => out.push(format!("bool:{value}")),
            serde_json::Value::Null => out.push("null".to_string()),
        }
    }

    let mut tokens = Vec::new();
    append(&value, &mut tokens);
    Some(tokens)
}

fn use_compact_lexing(line: &str) -> bool {
    let whitespace_tokens = line.split_whitespace().count();
    if whitespace_tokens > 1 {
        return false;
    }
    let toks = lex(line);
    let punct = toks
        .iter()
        .filter(|t| matches!(t.kind, crate::compress::lex::LexKind::Punct))
        .count();
    toks.len() >= 6 && punct >= 2
}

/// Drain's similarity scorer over every existing same-length cluster.
///
/// This keeps Drain's template update rule and threshold, but removes the fixed
/// prefix-tree routing. It is slower and intended as a quality ablation for
/// bounded `codag wrap` windows where a few thousand lines is normal.
#[derive(Debug, Clone)]
pub struct DrainFullSearchGrouper {
    pub sim_th: f64,
    mode: DrainTokenMode,
}

impl Default for DrainFullSearchGrouper {
    fn default() -> Self {
        DrainFullSearchGrouper {
            sim_th: 0.4,
            mode: DrainTokenMode::Whitespace,
        }
    }
}

#[derive(Debug, Clone)]
struct FullSearchCluster {
    template: Vec<String>,
    members: Vec<usize>,
}

impl Grouper for DrainFullSearchGrouper {
    fn group(&self, lines: &[LogLine]) -> Vec<Group> {
        let masker = LogMasker::new(default_masking_instructions());
        let mut clusters: Vec<FullSearchCluster> = Vec::new();

        for (idx, line) in lines.iter().enumerate() {
            let tokens = drain_tokens(&line.message, self.mode, &masker);
            let mut best: Option<(usize, f64, usize)> = None;

            for (cidx, cluster) in clusters.iter().enumerate() {
                if cluster.template.len() != tokens.len() {
                    continue;
                }
                let (sim, params) = get_seq_distance(&cluster.template, &tokens, false, "<*>");
                let better = match best {
                    None => true,
                    Some((best_idx, best_sim, best_params)) => {
                        sim > best_sim
                            || (sim == best_sim && params > best_params)
                            || (sim == best_sim && params == best_params && cidx < best_idx)
                    }
                };
                if better {
                    best = Some((cidx, sim, params));
                }
            }

            if let Some((cidx, sim, _params)) = best {
                if sim >= self.sim_th {
                    let template = create_template(&tokens, &clusters[cidx].template, "<*>");
                    clusters[cidx].template = template;
                    clusters[cidx].members.push(idx);
                    continue;
                }
            }

            clusters.push(FullSearchCluster {
                template: tokens,
                members: vec![idx],
            });
        }

        finalize(clusters.into_iter().map(|c| c.members).collect())
    }
}

// ---------------------------------------------------------------------------
// StatisticalGrouper
// ---------------------------------------------------------------------------

/// Data-driven grouping based on generic lexical co-occurrence.
///
/// This grouper has no log-domain shape rules. It learns variable positions
/// from the current batch: a position stays static only while all members share
/// the same generic lexical token, and mismatches become wildcards. Candidate
/// membership is decided by shared non-punctuation anchors.
#[derive(Debug, Clone)]
pub struct StatisticalGrouper {
    pub sim_th: f64,
    pub min_shared_anchors: usize,
    pub min_remaining_anchors: usize,
    pub min_static_chars: usize,
}

impl Default for StatisticalGrouper {
    fn default() -> Self {
        StatisticalGrouper {
            sim_th: 0.65,
            min_shared_anchors: 2,
            min_remaining_anchors: 2,
            min_static_chars: DEFAULT_MIN_STATIC_CHARS,
        }
    }
}

#[derive(Debug, Clone)]
struct StatCluster {
    len: usize,
    template: Vec<Option<String>>,
    members: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct StatScore {
    anchor_sim: f64,
    match_weight: f64,
    match_count: usize,
    remaining_anchors: usize,
    remaining_chars: usize,
}

fn idf_table(tokenized: &[Vec<LexToken>]) -> HashMap<String, f64> {
    let mut df: BTreeMap<String, usize> = BTreeMap::new();
    for toks in tokenized {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for tok in toks {
            if token_is_anchor(tok) {
                seen.insert(tok.text.as_str());
            }
        }
        for t in seen {
            *df.entry(t.to_string()).or_insert(0) += 1;
        }
    }
    let n = tokenized.len() as f64;
    df.into_iter()
        .map(|(tok, c)| {
            let weight = ((n + 1.0) / (c as f64 + 1.0)).ln() + 1.0;
            (tok, weight)
        })
        .collect()
}

fn anchor_weight(tok: &LexToken, idf: &HashMap<String, f64>) -> f64 {
    if token_is_anchor(tok) {
        idf.get(&tok.text).copied().unwrap_or(1.0)
    } else {
        0.0
    }
}

impl StatisticalGrouper {
    fn score(
        &self,
        cluster: &StatCluster,
        toks: &[LexToken],
        idf: &HashMap<String, f64>,
    ) -> Option<StatScore> {
        if toks.len() != cluster.len {
            return None;
        }

        let mut line_anchor_count = 0usize;
        let mut match_weight = 0.0;
        let mut match_count = 0usize;
        let mut merged_static = vec![false; toks.len()];

        for (pos, tok) in toks.iter().enumerate() {
            let w = anchor_weight(tok, idf);
            if w > 0.0 {
                line_anchor_count += 1;
            }
            if let Some(static_tok) = &cluster.template[pos] {
                if static_tok == &tok.text {
                    merged_static[pos] = true;
                    if w > 0.0 {
                        match_weight += w;
                        match_count += 1;
                    }
                }
            }
        }

        if line_anchor_count == 0 {
            return None;
        }

        let anchor_sim = match_count as f64 / line_anchor_count as f64;
        let remaining_anchors = anchor_count(toks, &merged_static);
        let remaining_chars = anchor_chars(toks, &merged_static);
        let required_remaining = self
            .min_remaining_anchors
            .min(toks.iter().filter(|t| token_is_anchor(t)).count().max(1));

        if match_count < self.min_shared_anchors.min(required_remaining) {
            return None;
        }
        if anchor_sim < self.sim_th {
            return None;
        }
        if remaining_anchors < required_remaining {
            return None;
        }
        if remaining_chars < self.min_static_chars {
            return None;
        }

        Some(StatScore {
            anchor_sim,
            match_weight,
            match_count,
            remaining_anchors,
            remaining_chars,
        })
    }

    fn merge(cluster: &mut StatCluster, idx: usize, toks: &[LexToken]) {
        for (pos, tok) in toks.iter().enumerate() {
            if cluster.template[pos].as_deref() != Some(tok.text.as_str()) {
                cluster.template[pos] = None;
            }
        }
        cluster.members.push(idx);
    }
}

impl Grouper for StatisticalGrouper {
    fn group(&self, lines: &[LogLine]) -> Vec<Group> {
        let tokenized: Vec<Vec<LexToken>> = lines.iter().map(|l| lex(&l.message)).collect();
        let idf = idf_table(&tokenized);
        let mut clusters: Vec<StatCluster> = Vec::new();

        for (idx, toks) in tokenized.iter().enumerate() {
            let mut best: Option<(usize, StatScore)> = None;
            for (cidx, cluster) in clusters.iter().enumerate() {
                let Some(score) = self.score(cluster, toks, &idf) else {
                    continue;
                };
                let better = match best {
                    None => true,
                    Some((best_idx, best_score)) => {
                        score.match_weight > best_score.match_weight
                            || (score.match_weight == best_score.match_weight
                                && score.anchor_sim > best_score.anchor_sim)
                            || (score.match_weight == best_score.match_weight
                                && score.anchor_sim == best_score.anchor_sim
                                && score.match_count > best_score.match_count)
                            || (score.match_weight == best_score.match_weight
                                && score.anchor_sim == best_score.anchor_sim
                                && score.match_count == best_score.match_count
                                && cidx < best_idx)
                    }
                };
                if better {
                    best = Some((cidx, score));
                }
            }

            if let Some((cidx, _score)) = best {
                StatisticalGrouper::merge(&mut clusters[cidx], idx, toks);
            } else {
                clusters.push(StatCluster {
                    len: toks.len(),
                    template: toks.iter().map(|t| Some(t.text.clone())).collect(),
                    members: vec![idx],
                });
            }
        }

        let buckets: Vec<Vec<usize>> = clusters.into_iter().map(|c| c.members).collect();
        finalize(buckets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(msgs: &[&str]) -> Vec<LogLine> {
        msgs.iter().map(|m| LogLine::new(m.to_string())).collect()
    }

    #[test]
    fn member_indices_ascending() {
        let lines = lines_of(&["x same", "x same", "x same"]);
        let groups = DrainGrouper::default().group(&lines);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, vec![0, 1, 2]);
    }

    #[test]
    fn drain_groups_json_with_whitespace_inside_string_values() {
        let lines = lines_of(&[
            r#"{"timestamp":1,"data":{"type":"llm.prediction.input","input":"hello world","model":"qwen"}}"#,
            r#"{"timestamp":2,"data":{"type":"llm.prediction.input","input":"hello world with a longer history","model":"qwen"}}"#,
            r#"{"timestamp":3,"data":{"type":"llm.prediction.input","input":"another prompt containing many words","model":"qwen"}}"#,
        ]);
        let groups = DrainGrouper::default().group(&lines);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, vec![0, 1, 2]);
    }

    #[test]
    fn statistical_groups_alpha_variables_from_anchors() {
        let lines = lines_of(&[
            r#"File "worker.py", line 149, in requires_inputs"#,
            r#"File "worker.py", line 207, in schema_transforms"#,
            r#"File "worker.py", line 344, in lambda_fn"#,
        ]);
        let groups = StatisticalGrouper::default().group(&lines);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].member_indices, vec![0, 1, 2]);
    }

    #[test]
    fn statistical_groups_compact_json_without_shape_rules() {
        let lines = lines_of(&[
            r#"{"level":"warning","time":"2025-01-08T08:18:33Z","msg":"ready"}"#,
            r#"{"level":"warning","time":"2025-01-08T08:18:23Z","msg":"ready"}"#,
            r#"{"level":"warning","time":"2025-01-08T08:18:10Z","msg":"ready"}"#,
        ]);
        let groups = StatisticalGrouper::default().group(&lines);
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn statistical_rejects_weak_anchor_merge() {
        let lines = lines_of(&[
            "alpha beta gamma",
            "delta echo foxtrot",
            "hotel india juliet",
        ]);
        let groups = StatisticalGrouper::default().group(&lines);
        assert_eq!(groups.len(), 3);
    }
}

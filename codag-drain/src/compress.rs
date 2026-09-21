//! Deterministic log templating.
//!
//! This crate is intentionally narrow: it groups log lines, derives templates,
//! and emits template groups with bounded samples and slot summaries. Incident
//! understanding belongs to the inference pipeline, not this deterministic
//! adapter.

pub mod grouper;
pub mod lex;
pub mod profile;
pub mod template;

use grouper::{make_grouper, Group};
use profile::{distinct_samples, Profile};

/// Default minimum static (non-placeholder) chars for a useful template.
pub const DEFAULT_MIN_STATIC_CHARS: usize = 3;
/// Default number of raw lines retained as examples for each template group.
pub const DEFAULT_SAMPLE_CAP: usize = 3;
/// Default cap on distinct values shown for each template slot.
pub const DEFAULT_SLOT_SAMPLE_CAP: usize = 6;
/// Default maximum template chars used by text rendering.
pub const DEFAULT_TEMPLATE_CLIP: usize = 120;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single log line. `level` / `timestamp` are optional metadata parsed by the
/// CLI/server wrappers; the templater itself groups by `message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub message: String,
    pub level: Option<String>,
    pub timestamp: Option<String>,
}

impl LogLine {
    /// Construct a line with no level/timestamp.
    pub fn new(message: String) -> Self {
        LogLine {
            message,
            level: None,
            timestamp: None,
        }
    }
}

/// Which grouping algorithm to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GrouperKind {
    #[default]
    Drain,
    DrainStock,
    DrainDelimited,
    DrainFullSearch,
    Statistical,
}

/// Tunable knobs for deterministic templating.
#[derive(Debug, Clone)]
pub struct TemplaterConfig {
    pub grouper: GrouperKind,
    pub drain_depth: usize,
    pub drain_sim_th: f64,
    pub drain_max_children: usize,
    pub template_clip: usize,
    pub min_static_chars: usize,
    pub sample_cap: usize,
    pub slot_sample_cap: usize,
}

impl Default for TemplaterConfig {
    fn default() -> Self {
        TemplaterConfig {
            grouper: GrouperKind::Drain,
            drain_depth: 4,
            drain_sim_th: 0.4,
            drain_max_children: 100,
            template_clip: DEFAULT_TEMPLATE_CLIP,
            min_static_chars: DEFAULT_MIN_STATIC_CHARS,
            sample_cap: DEFAULT_SAMPLE_CAP,
            slot_sample_cap: DEFAULT_SLOT_SAMPLE_CAP,
        }
    }
}

/// One sampled raw line from a template group.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TemplateSample {
    pub index: usize,
    pub text: String,
    pub level: Option<String>,
    pub timestamp: Option<String>,
}

/// Summary of one `<*>` slot in a template.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SlotSummary {
    pub numeric: bool,
    pub min: f64,
    pub max: f64,
    pub median: f64,
    pub unit: String,
    pub distinct: usize,
    pub samples: Vec<String>,
}

/// One deterministic template group.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TemplateGroup {
    pub id: usize,
    pub first_index: usize,
    pub count: usize,
    pub template: String,
    pub samples: Vec<TemplateSample>,
    pub slots: Vec<SlotSummary>,
}

/// Full templating result for a log window.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TemplateResult {
    pub groups: Vec<TemplateGroup>,
    pub original_count: usize,
    pub template_count: usize,
    pub line_compression: f64,
}

impl TemplateResult {
    /// Render one compact text line per template group.
    pub fn render(&self) -> String {
        self.groups
            .iter()
            .map(render_group)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ---------------------------------------------------------------------------
// Rendering and group construction
// ---------------------------------------------------------------------------

/// Format an f64 without trailing `.0` noise (`20.0` -> `20`, `0.8` -> `0.8`).
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.is_finite() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let mut s = format!("{v}");
        if s.contains('.') {
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
        s
    }
}

fn fmt_slot(slot: &SlotSummary) -> String {
    if slot.numeric {
        let unit = &slot.unit;
        if slot.min == slot.max {
            format!("[{}{}]", fmt_num(slot.min), unit)
        } else {
            format!(
                "[{}..{}{} p50={}{}]",
                fmt_num(slot.min),
                fmt_num(slot.max),
                unit,
                fmt_num(slot.median),
                unit
            )
        }
    } else if slot.distinct <= slot.samples.len() {
        format!("[{}]", slot.samples.join(","))
    } else {
        format!("[{}x {}]", slot.distinct, slot.samples.join(","))
    }
}

fn clip(template: &str, max: usize) -> String {
    if template.chars().count() <= max {
        template.to_string()
    } else {
        template.chars().take(max).collect()
    }
}

fn render_group(group: &TemplateGroup) -> String {
    let mut s = format!("[x{}] {}", group.count, group.template);
    for slot in &group.slots {
        s.push(' ');
        s.push_str(&fmt_slot(slot));
    }
    if !group.samples.is_empty() {
        let samples = group
            .samples
            .iter()
            .map(|sample| sample.text.replace('\n', "\\n"))
            .collect::<Vec<_>>()
            .join(" | ");
        s.push_str(" samples: ");
        s.push_str(&samples);
    }
    s
}

fn build_samples(lines: &[LogLine], group: &Group, cap: usize) -> Vec<TemplateSample> {
    group
        .member_indices
        .iter()
        .take(cap)
        .map(|&idx| TemplateSample {
            index: idx,
            text: lines[idx].message.clone(),
            level: lines[idx].level.clone(),
            timestamp: lines[idx].timestamp.clone(),
        })
        .collect()
}

fn build_slot_summary(profile: &Profile, g_idx: usize, si: usize, cap: usize) -> SlotSummary {
    let prof = &profile.profiles[g_idx];
    let values: Vec<Option<String>> = prof
        .raw_slots
        .iter()
        .map(|m| m.get(si).cloned().flatten())
        .collect();
    let (samples, distinct) = distinct_samples(&values, cap);
    let unit = samples
        .first()
        .map(|s| trailing_alpha_unit(s))
        .unwrap_or_default();

    match &prof.numeric[si] {
        Some(num) => SlotSummary {
            numeric: true,
            min: num.min,
            max: num.max,
            median: num.median,
            unit,
            distinct,
            samples,
        },
        None => SlotSummary {
            numeric: false,
            min: 0.0,
            max: 0.0,
            median: 0.0,
            unit,
            distinct,
            samples,
        },
    }
}

/// Trailing-alphabetic unit of a value (e.g. "ms" from "45ms", "MB" from
/// "512MB"). Empty if the value does not end in letters or is purely alphabetic.
fn trailing_alpha_unit(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut i = chars.len();
    while i > 0 && chars[i - 1].is_ascii_alphabetic() {
        i -= 1;
    }
    if i == chars.len() || i == 0 {
        return String::new();
    }
    chars[i..].iter().collect()
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Template a batch of log lines.
pub fn template_logs(lines: &[LogLine], config: &TemplaterConfig) -> TemplateResult {
    if lines.is_empty() {
        return TemplateResult {
            groups: Vec::new(),
            original_count: 0,
            template_count: 0,
            line_compression: 0.0,
        };
    }

    let grouper = make_grouper(config);
    let groups = grouper.group(lines);
    template_groups(lines, &groups, config)
}

/// Build a template result from precomputed groups. Used by [`TemplateIndex`]
/// and tests that need exact control over grouping.
pub fn template_groups(
    lines: &[LogLine],
    groups: &[Group],
    config: &TemplaterConfig,
) -> TemplateResult {
    if lines.is_empty() {
        return TemplateResult {
            groups: Vec::new(),
            original_count: 0,
            template_count: 0,
            line_compression: 0.0,
        };
    }

    let profile = Profile::build(lines, groups);
    let rendered_groups = groups
        .iter()
        .enumerate()
        .map(|(g_idx, group)| {
            let prof = &profile.profiles[g_idx];
            let slots = (0..prof.slot_count)
                .map(|si| build_slot_summary(&profile, g_idx, si, config.slot_sample_cap))
                .collect();
            TemplateGroup {
                id: g_idx,
                first_index: group.member_indices[0],
                count: group.member_indices.len(),
                template: clip(&prof.template, config.template_clip),
                samples: build_samples(lines, group, config.sample_cap),
                slots,
            }
        })
        .collect::<Vec<_>>();

    let template_count = rendered_groups.len();
    TemplateResult {
        groups: rendered_groups,
        original_count: lines.len(),
        template_count,
        line_compression: if template_count == 0 {
            0.0
        } else {
            lines.len() as f64 / template_count as f64
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(msg: &str) -> LogLine {
        LogLine::new(msg.to_string())
    }

    #[test]
    fn templates_compact_json_with_learned_slots() {
        let mut v = Vec::new();
        for sec in [10, 11, 12, 13, 14, 15, 16, 17] {
            v.push(line(&format!(
                r#"{{"level":"warning","time":"2025-01-08T08:18:{sec}Z","msg":"ready"}}"#
            )));
        }
        let result = template_logs(&v, &TemplaterConfig::default());
        assert_eq!(result.original_count, 8);
        assert_eq!(result.template_count, 1);
        assert_eq!(result.groups[0].count, 8);
        assert!(
            result.groups[0]
                .template
                .contains(r#""time":"2025-01-08T08:18:<*>""#),
            "{:?}",
            result.groups[0].template
        );
        assert!(result.render().contains("[x8]"));
    }

    #[test]
    fn templates_structured_json_with_free_form_text() {
        let lines = vec![
            line(
                r#"{"timestamp":1,"data":{"type":"llm.prediction.input","input":"hello world","model":"qwen"}}"#,
            ),
            line(
                r#"{"timestamp":2,"data":{"type":"llm.prediction.output","output":"hello there","tokens":2}}"#,
            ),
            line(
                r#"{"timestamp":3,"data":{"type":"llm.prediction.input","input":"hello world with a much longer history","model":"qwen"}}"#,
            ),
            line(
                r#"{"timestamp":4,"data":{"type":"llm.prediction.output","output":"a substantially longer answer with spaces","tokens":7}}"#,
            ),
        ];
        let result = template_logs(&lines, &TemplaterConfig::default());
        assert_eq!(result.template_count, 2);
        assert_eq!(result.groups[0].count, 2);
        assert_eq!(result.groups[1].count, 2);
        assert!(result.groups[0]
            .template
            .contains(r#""type":"llm.prediction.input""#));
        assert!(result.groups[0].template.contains(r#""input":"<*>""#));
        assert!(result.groups[1]
            .template
            .contains(r#""type":"llm.prediction.output""#));
        assert!(result.groups[1].template.contains(r#""output":"<*>""#));
    }

    #[test]
    fn alpha_enum_values_are_slot_metadata() {
        let states = [
            "Succeeded",
            "Failed",
            "Skipped",
            "Succeeded",
            "Failed",
            "Skipped",
            "Succeeded",
            "Failed",
        ];
        let lines: Vec<LogLine> = states
            .iter()
            .map(|s| line(&format!("node phase changed to {s} now")))
            .collect();
        let result = template_logs(&lines, &TemplaterConfig::default());
        assert_eq!(result.template_count, 1);
        let samples = &result.groups[0].slots[0].samples;
        assert!(samples.contains(&"Succeeded".to_string()), "{samples:?}");
        assert!(samples.contains(&"Failed".to_string()), "{samples:?}");
        assert!(samples.contains(&"Skipped".to_string()), "{samples:?}");
    }

    #[test]
    fn numeric_slot_summary_uses_all_values() {
        let lines = vec![
            line("latency 20ms"),
            line("latency 20ms"),
            line("latency 20ms"),
            line("latency 8400ms"),
        ];
        let result = template_logs(&lines, &TemplaterConfig::default());
        assert_eq!(result.template_count, 1);
        let slot = &result.groups[0].slots[0];
        assert!(slot.numeric);
        assert_eq!(slot.min, 20.0);
        assert_eq!(slot.max, 8400.0);
        assert_eq!(slot.median, 20.0);
        assert_eq!(slot.unit, "ms");
    }

    #[test]
    fn sample_cap_limits_raw_examples() {
        let cfg = TemplaterConfig {
            sample_cap: 2,
            ..TemplaterConfig::default()
        };
        let lines = vec![
            line("worker ready 1"),
            line("worker ready 2"),
            line("worker ready 3"),
        ];
        let result = template_logs(&lines, &cfg);
        assert_eq!(result.template_count, 1);
        assert_eq!(result.groups[0].samples.len(), 2);
    }

    #[test]
    fn empty_input() {
        let result = template_logs(&[], &TemplaterConfig::default());
        assert_eq!(result.original_count, 0);
        assert_eq!(result.template_count, 0);
        assert_eq!(result.line_compression, 0.0);
        assert_eq!(result.render(), "");
    }

    #[test]
    fn deterministic_render() {
        let lines = vec![
            line("GET /users/1 200"),
            line("GET /users/2 200"),
            line("GET /users/3 200"),
        ];
        let cfg = TemplaterConfig::default();
        assert_eq!(
            template_logs(&lines, &cfg).render(),
            template_logs(&lines, &cfg).render()
        );
    }

    #[test]
    fn json_serializes() {
        let lines = vec![line("worker ready 1"), line("worker ready 2")];
        let result = template_logs(&lines, &TemplaterConfig::default());
        let json = serde_json::to_value(&result).expect("serialize");
        assert!(json.get("groups").unwrap().is_array());
        assert_eq!(json["original_count"].as_u64().unwrap(), 2);
        assert!(json.get("template_count").is_some());
        assert!(json.get("line_compression").is_some());
    }

    #[test]
    fn trailing_unit_extraction() {
        assert_eq!(trailing_alpha_unit("45ms"), "ms");
        assert_eq!(trailing_alpha_unit("512MB"), "MB");
        assert_eq!(trailing_alpha_unit("100"), "");
        assert_eq!(trailing_alpha_unit("word"), "");
    }
}

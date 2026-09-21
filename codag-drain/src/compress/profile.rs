//! Per-group template + per-slot profiling.
//!
//! Adapted from the `Profile` class in
//! `codag-bench/scripts/det_compressors.py`. For each group we derive a template
//! (`derive_pair_template` of members[0] vs members[1]; member[0] alone for size
//! 1), compile a `regex_from_placeholder` capture regex, capture each line's raw
//! slot strings, and compute per-slot numeric `(min, median, max)` over ALL
//! parsed values (a slot is numeric iff >= 50% of present values parse as a
//! number). The median is over the FULL list (including duplicates), not
//! distinct.

use regex::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::compress::grouper::Group;
use crate::compress::lex::{derive_lex_template, lex};
use crate::compress::template::{derive_multi_template, regex_from_placeholder};
use crate::compress::LogLine;

/// Derive a canonical JSON template when lexical token counts differ because
/// free-form string values have different lengths. Returns the raw value for
/// each placeholder in member order so slot summaries do not depend on a regex
/// matching canonicalized object-key order.
fn derive_json_profile(messages: &[&str]) -> Option<(String, Vec<Vec<Option<String>>>)> {
    let values: Vec<serde_json::Value> = messages
        .iter()
        .map(|message| serde_json::from_str(message))
        .collect::<Result<_, _>>()
        .ok()?;
    if values.is_empty() || (!values[0].is_object() && !values[0].is_array()) {
        return None;
    }

    let mut raw_slots = vec![Vec::new(); values.len()];

    fn scalar_text(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::String(value) => value.clone(),
            _ => value.to_string(),
        }
    }

    fn derive(
        values: &[&serde_json::Value],
        raw_slots: &mut [Vec<Option<String>>],
    ) -> Option<String> {
        match values[0] {
            serde_json::Value::Object(first) => {
                let objects: Vec<_> = values
                    .iter()
                    .map(|value| value.as_object())
                    .collect::<Option<_>>()?;
                if objects
                    .iter()
                    .any(|object| object.len() != first.len() || object.keys().ne(first.keys()))
                {
                    return None;
                }

                let mut fields = Vec::with_capacity(first.len());
                for key in first.keys() {
                    let children: Vec<_> = objects.iter().map(|object| &object[key]).collect();
                    fields.push(format!(
                        "{}:{}",
                        serde_json::to_string(key).ok()?,
                        derive(&children, raw_slots)?
                    ));
                }
                Some(format!("{{{}}}", fields.join(",")))
            }
            serde_json::Value::Array(first) => {
                let arrays: Vec<_> = values
                    .iter()
                    .map(|value| value.as_array())
                    .collect::<Option<_>>()?;
                if arrays.iter().any(|array| array.len() != first.len()) {
                    return None;
                }

                let mut items = Vec::with_capacity(first.len());
                for index in 0..first.len() {
                    let children: Vec<_> = arrays.iter().map(|array| &array[index]).collect();
                    items.push(derive(&children, raw_slots)?);
                }
                Some(format!("[{}]", items.join(",")))
            }
            first => {
                let same = values.iter().all(|value| *value == first);
                if same {
                    serde_json::to_string(first).ok()
                } else {
                    for (row, value) in raw_slots.iter_mut().zip(values.iter()) {
                        row.push(Some(scalar_text(value)));
                    }
                    Some(match first {
                        serde_json::Value::String(_) => {
                            format!("\"{}\"", crate::compress::template::PLACEHOLDER)
                        }
                        _ => crate::compress::template::PLACEHOLDER.to_string(),
                    })
                }
            }
        }
    }

    let refs: Vec<_> = values.iter().collect();
    let template = derive(&refs, &mut raw_slots)?;
    Some((template, raw_slots))
}

/// `NUM = re.compile(r"-?\d+(?:\.\d+)?")` - first numeric substring of a slot.
fn num_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"-?\d+(?:\.\d+)?").unwrap())
}

/// Numeric stats for one slot of one group.
#[derive(Debug, Clone)]
pub struct SlotNumeric {
    pub min: f64,
    pub median: f64,
    pub max: f64,
}

/// Per-group profile data.
#[derive(Debug, Clone)]
pub struct GroupProfile {
    /// Derived `<*>` template string.
    pub template: String,
    /// Compiled capture regex (None if it didn't compile / no static anchor).
    pub regex: Option<Regex>,
    /// Number of `<*>` slots in the template.
    pub slot_count: usize,
    /// raw_slots[member_position][slot_index] = captured raw string (or None).
    /// member_position indexes into the group's ascending member_indices.
    pub raw_slots: Vec<Vec<Option<String>>>,
    /// numeric[slot] = Some((min,median,max)) iff the slot is numeric.
    pub numeric: Vec<Option<SlotNumeric>>,
}

/// One `GroupProfile` per group, plus the per-line group assignment.
#[derive(Debug, Clone)]
pub struct Profile {
    /// gid[line_index] = group index (into `groups` / `profiles`).
    pub gid: Vec<usize>,
    pub profiles: Vec<GroupProfile>,
}

/// Median over the FULL slice (incl. duplicates). Sorts a copy. Even-length =
/// mean of the two middle elements (Python `statistics.median`).
fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

impl Profile {
    /// Build the profile. `groups` are emitted in ascending first-member-index
    /// order; `gid[i]` maps line `i` to its group index.
    pub fn build(lines: &[LogLine], groups: &[Group]) -> Profile {
        let mut gid = vec![0usize; lines.len()];
        for (g_idx, g) in groups.iter().enumerate() {
            for &m in &g.member_indices {
                gid[m] = g_idx;
            }
        }

        let mut profiles: Vec<GroupProfile> = Vec::with_capacity(groups.len());
        for g in groups {
            let members = &g.member_indices;
            // Derive a multi-member lexical template first so compact JSON/logfmt
            // and alpha-only variables are visible without domain regexes.
            let a_raw = &lines[members[0]].message;
            let members_lex: Vec<_> = members.iter().map(|&m| lex(&lines[m].message)).collect();
            let mut json_raw_slots = None;
            let template = match derive_lex_template(a_raw, &members_lex) {
                Some(t) => t,
                None => match derive_json_profile(
                    &members
                        .iter()
                        .map(|&m| lines[m].message.as_str())
                        .collect::<Vec<_>>(),
                ) {
                    Some((template, raw_slots)) => {
                        json_raw_slots = Some(raw_slots);
                        template
                    }
                    None => {
                        let members_norm: Vec<Vec<String>> = members
                            .iter()
                            .map(|&m| whitespace_tokens(&lines[m].message))
                            .collect();
                        derive_multi_template(a_raw, &members_norm)
                    }
                },
            };
            let regex = regex_from_placeholder(&template);
            let slot_count = template
                .matches(crate::compress::template::PLACEHOLDER)
                .count();

            // Capture raw slot strings per member.
            let mut raw_slots: Vec<Vec<Option<String>>> = Vec::with_capacity(members.len());
            // numeric_vals[slot] = list of parsed numeric values (present only)
            let mut numeric_vals: Vec<Vec<f64>> = vec![Vec::new(); slot_count];
            // present_count[slot] = number of members with a captured (non-None) value
            let mut present_count: Vec<usize> = vec![0; slot_count];

            for (member_position, &m) in members.iter().enumerate() {
                let msg = &lines[m].message;
                let caps = json_raw_slots
                    .as_ref()
                    .map(|slots| slots[member_position].clone())
                    .unwrap_or_else(|| capture_slots(regex.as_ref(), msg, slot_count));
                for (si, cap) in caps.iter().enumerate() {
                    if let Some(raw) = cap {
                        present_count[si] += 1;
                        if let Some(mm) = num_re().find(raw) {
                            if let Ok(val) = mm.as_str().parse::<f64>() {
                                numeric_vals[si].push(val);
                            }
                        }
                    }
                }
                raw_slots.push(caps);
            }

            // A slot is numeric iff >= 50% of *present* values parse as a number.
            let mut numeric: Vec<Option<SlotNumeric>> = Vec::with_capacity(slot_count);
            for si in 0..slot_count {
                let present = present_count[si];
                let parsed = numeric_vals[si].len();
                if present > 0 && parsed * 2 >= present && parsed > 0 {
                    let med = median(&numeric_vals[si]);
                    let mn = numeric_vals[si]
                        .iter()
                        .cloned()
                        .fold(f64::INFINITY, f64::min);
                    let mx = numeric_vals[si]
                        .iter()
                        .cloned()
                        .fold(f64::NEG_INFINITY, f64::max);
                    numeric.push(Some(SlotNumeric {
                        min: mn,
                        median: med,
                        max: mx,
                    }));
                } else {
                    numeric.push(None);
                }
            }

            profiles.push(GroupProfile {
                template,
                regex,
                slot_count,
                raw_slots,
                numeric,
            });
        }

        Profile { gid, profiles }
    }

    /// Parse the numeric value of a captured slot string (first numeric match).
    pub fn slot_numeric_value(s: &str) -> Option<f64> {
        num_re()
            .find(s)
            .and_then(|m| m.as_str().parse::<f64>().ok())
    }
}

fn whitespace_tokens(line: &str) -> Vec<String> {
    line.split_whitespace().map(|s| s.to_string()).collect()
}

/// Capture the `<*>` slot raw strings for a line. Returns a vector of length
/// `slot_count`; entries are None when the regex doesn't match. LibreLog comma /
/// whitespace normalization is applied before matching.
pub fn capture_slots(regex: Option<&Regex>, msg: &str, slot_count: usize) -> Vec<Option<String>> {
    let re = match regex {
        Some(r) => r,
        None => return vec![None; slot_count],
    };
    let prepared = msg.replace(',', "");
    let prepared = prepared.trim();
    match re.captures(prepared) {
        Some(caps) => (1..=slot_count)
            .map(|i| caps.get(i).map(|m| m.as_str().to_string()))
            .collect(),
        None => vec![None; slot_count],
    }
}

/// Build distinct first-seen samples (cap N) and a distinct count for a slot.
pub fn distinct_samples(values: &[Option<String>], cap: usize) -> (Vec<String>, usize) {
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut distinct = 0usize;
    for v in values.iter().flatten() {
        if seen.insert(v.clone(), ()).is_none() {
            distinct += 1;
            if order.len() < cap {
                order.push(v.clone());
            }
        }
    }
    (order, distinct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compress::grouper::Group;

    fn lines_of(msgs: &[&str]) -> Vec<LogLine> {
        msgs.iter().map(|m| LogLine::new(m.to_string())).collect()
    }

    #[test]
    fn median_odd_and_even() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
    }

    #[test]
    fn median_over_all_values_not_distinct() {
        // The key correctness check from the spec: median of [20,20,20,8400]
        // must be ~20 (over ALL values), NOT (20+8400)/2 = 4210 (distinct).
        let m = median(&[20.0, 20.0, 20.0, 8400.0]);
        assert!((m - 20.0).abs() < 1e-9, "median was {m}");
    }

    #[test]
    fn numeric_detection_majority() {
        // slot mostly numeric -> numeric profile
        let lines = lines_of(&["count is 1", "count is 2", "count is 3"]);
        let groups = vec![Group {
            member_indices: vec![0, 1, 2],
        }];
        let p = Profile::build(&lines, &groups);
        assert!(p.profiles[0].numeric[0].is_some());
    }
}

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use serde_json::Value;

use crate::model_aliases::preferred_model;
use crate::types::{TokenBreakdown, SessionTotals, SessionParsePlan};

pub struct AntigravitySessionParser;

impl AntigravitySessionParser {
    pub fn new() -> Self {
        Self
    }

    pub fn parse(&self, plan: &SessionParsePlan) -> Result<SessionTotals, std::io::Error> {
        let mut reported = TokenBreakdown::default();
        let mut model_totals = HashMap::new();
        let mut model_breakdowns = HashMap::new();
        let mut message_count = 0;
        let mut evidence_count = 0;
        let mut estimated_text_length = 0;

        for file_path in &plan.token_file_paths {
            let path = Path::new(file_path);
            let ext = path.extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();

            if ext != "jsonl" && ext != "json" && ext != "md" && ext != "txt" && ext != "log" && ext != "yaml" && ext != "yml" {
                continue;
            }

            let metadata = match std::fs::metadata(path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let file_size = metadata.len();

            if ext == "jsonl" {
                let file = match File::open(path) {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                let reader = BufReader::new(file);
                let file_name = path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                
                let is_steps_file = file_name == "steps.jsonl" || file_name == "transcript.jsonl";

                for line in reader.lines() {
                    let trimmed = match line {
                        Ok(l) => l.trim().to_string(),
                        Err(_) => continue,
                    };
                    if trimmed.is_empty() {
                        continue;
                    }

                    estimated_text_length += trimmed.len() as u64 + 1;
                    if let Ok(parsed) = serde_json::from_str::<Value>(&trimmed) {
                        message_count += count_step_rows(&parsed);
                        if !is_steps_file {
                            let signal = walk_for_token_signals(&parsed, None);
                            merge_breakdown(&mut reported, &signal.breakdown);
                            merge_model_totals(&mut model_totals, &signal.model_totals);
                            merge_model_breakdowns(&mut model_breakdowns, &signal.model_breakdowns);
                            evidence_count += signal.hits;
                        }
                    }
                }
            } else if ext == "json" {
                let content = match std::fs::read_to_string(path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                estimated_text_length += content.len() as u64;
                if let Ok(parsed) = serde_json::from_str::<Value>(&content) {
                    message_count += count_messages(&parsed);
                    let signal = walk_for_token_signals(&parsed, None);
                    merge_breakdown(&mut reported, &signal.breakdown);
                    merge_model_totals(&mut model_totals, &signal.model_totals);
                    merge_model_breakdowns(&mut model_breakdowns, &signal.model_breakdowns);
                    evidence_count += signal.hits;
                }
            } else {
                // For .md, .txt, .log, .yaml, .yml: Zero-I/O metadata only!
                estimated_text_length += file_size;
            }
        }

        let label = self.resolve_label(plan);

        if evidence_count > 0 {
            reported.total_tokens = normalize_total(&reported);
            // Finalize model breakdowns
            for breakdown in model_breakdowns.values_mut() {
                breakdown.total_tokens = normalize_total(breakdown);
            }
            Ok(SessionTotals {
                session_id: plan.session_id.clone(),
                label,
                file_path: plan.session_dir.clone(),
                last_modified_ms: plan.last_modified_ms,
                mode: "reported".to_string(),
                source: plan.source.clone(),
                evidence_count,
                message_count,
                model_totals,
                model_breakdowns,
                breakdown: reported,
            })
        } else {
            let estimated_total = std::cmp::max(1, estimated_text_length / 4);
            let input_tokens = (estimated_total as f64 * 0.62) as u64;
            let output_tokens = (estimated_total as f64 * 0.38) as u64;
            Ok(SessionTotals {
                session_id: plan.session_id.clone(),
                label,
                file_path: plan.session_dir.clone(),
                last_modified_ms: plan.last_modified_ms,
                mode: "estimated".to_string(),
                source: plan.source.clone(),
                evidence_count: 0,
                message_count,
                model_totals: HashMap::new(),
                model_breakdowns: HashMap::new(),
                breakdown: TokenBreakdown {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                    total_tokens: estimated_total,
                },
            })
        }
    }

    fn resolve_label(&self, plan: &SessionParsePlan) -> String {
        let label_files = ["task.md", "implementation_plan.md", "walkthrough.md"];
        for label_file in &label_files {
            let file_path = Path::new(&plan.session_dir).join(label_file);
            if let Ok(content) = std::fs::read_to_string(file_path) {
                // Find first header
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with('#') {
                        let raw_title = trimmed.trim_start_matches('#').trim();
                        // Strip prefix like "Task:" if present
                        let title = if raw_title.to_lowercase().starts_with("task:") {
                            raw_title["task:".len()..].trim()
                        } else {
                            raw_title
                        };
                        if !title.is_empty() {
                            return title.to_string();
                        }
                    }
                }
            }
        }
        plan.label_hint.clone()
    }
}

struct SignalResult {
    breakdown: TokenBreakdown,
    hits: u64,
    model_totals: HashMap<String, u64>,
    model_breakdowns: HashMap<String, TokenBreakdown>,
}

fn walk_for_token_signals(value: &Value, inherited_model: Option<&str>) -> SignalResult {
    let mut res = SignalResult {
        breakdown: TokenBreakdown::default(),
        hits: 0,
        model_totals: HashMap::new(),
        model_breakdowns: HashMap::new(),
    };

    visit(value, inherited_model, &mut res);
    res.breakdown.total_tokens = normalize_total(&res.breakdown);
    res
}

fn visit(val: &Value, inherited_model: Option<&str>, res: &mut SignalResult) {
    match val {
        Value::Array(arr) => {
            for item in arr {
                visit(item, inherited_model, res);
            }
        }
        Value::Object(map) => {
            // Check if this is a canonical usage signal
            if let Some(usage_signal) = extract_canonical_usage_signal(map, inherited_model) {
                merge_breakdown(&mut res.breakdown, &usage_signal.breakdown);
                merge_model_totals(&mut res.model_totals, &usage_signal.model_totals);
                merge_model_breakdowns(&mut res.model_breakdowns, &usage_signal.model_breakdowns);
                res.hits += usage_signal.hits;
                return;
            }

            let current_model = preferred_model(
                &[
                    map.get("responseModel").and_then(|v| v.as_str()),
                    map.get("model").and_then(|v| v.as_str()),
                    map.get("modelId").and_then(|v| v.as_str()),
                    map.get("modelName").and_then(|v| v.as_str()),
                ],
                inherited_model,
            );

            for (key, raw_val) in map {
                let normalized_key = key.to_lowercase();
                if let Some(numeric_value) = to_finite_u64(raw_val) {
                    let model = current_model.as_deref().unwrap_or("unknown");
                    
                    if normalized_key.starts_with("input") || (normalized_key.starts_with("prompt") && normalized_key.contains("token")) {
                        res.breakdown.input_tokens += numeric_value;
                        *res.model_totals.entry(model.to_string()).or_insert(0) += numeric_value;
                        merge_model_category(&mut res.model_breakdowns, model, |b| b.input_tokens += numeric_value);
                        res.hits += 1;
                    } else if normalized_key.starts_with("output") || (normalized_key.starts_with("completion") && normalized_key.contains("token")) {
                        res.breakdown.output_tokens += numeric_value;
                        *res.model_totals.entry(model.to_string()).or_insert(0) += numeric_value;
                        merge_model_category(&mut res.model_breakdowns, model, |b| b.output_tokens += numeric_value);
                        res.hits += 1;
                    } else if normalized_key.contains("cache") && normalized_key.contains("read") && normalized_key.contains("token") {
                        res.breakdown.cache_read_tokens += numeric_value;
                        *res.model_totals.entry(model.to_string()).or_insert(0) += numeric_value;
                        merge_model_category(&mut res.model_breakdowns, model, |b| b.cache_read_tokens += numeric_value);
                        res.hits += 1;
                    } else if normalized_key.contains("cache") && normalized_key.contains("write") && normalized_key.contains("token") {
                        res.breakdown.cache_write_tokens += numeric_value;
                        *res.model_totals.entry(model.to_string()).or_insert(0) += numeric_value;
                        merge_model_category(&mut res.model_breakdowns, model, |b| b.cache_write_tokens += numeric_value);
                        res.hits += 1;
                    } else if (normalized_key.contains("reasoning") || normalized_key.contains("thinking")) && normalized_key.contains("token") {
                        res.breakdown.reasoning_tokens += numeric_value;
                        *res.model_totals.entry(model.to_string()).or_insert(0) += numeric_value;
                        merge_model_category(&mut res.model_breakdowns, model, |b| b.reasoning_tokens += numeric_value);
                        res.hits += 1;
                    } else if normalized_key == "totaltokens" || normalized_key == "total_tokens" {
                        res.breakdown.total_tokens += numeric_value;
                        *res.model_totals.entry(model.to_string()).or_insert(0) += numeric_value;
                        merge_model_category(&mut res.model_breakdowns, model, |b| b.total_tokens += numeric_value);
                        res.hits += 1;
                    }
                }
                visit(raw_val, current_model.as_deref(), res);
            }
        }
        _ => {}
    }
}

fn extract_canonical_usage_signal(record: &serde_json::Map<String, Value>, inherited_model: Option<&str>) -> Option<SignalResult> {
    if record.get("recordType").and_then(|v| v.as_str()) != Some("usage") {
        return None;
    }

    let raw = record.get("raw");
    let raw_record = raw.and_then(|v| v.as_object());
    let chat_model = raw_record.and_then(|o| o.get("chatModel"));
    let chat_model_record = chat_model.and_then(|v| v.as_object());
    let usage = chat_model_record.and_then(|o| o.get("usage"));
    let usage_record = usage.and_then(|v| v.as_object());

    let model = preferred_model(
        &[
            record.get("model").and_then(|v| v.as_str()),
            record.get("responseModel").and_then(|v| v.as_str()),
            chat_model_record.and_then(|o| o.get("responseModel")).and_then(|v| v.as_str()),
            chat_model_record.and_then(|o| o.get("modelName")).and_then(|v| v.as_str()),
            chat_model_record.and_then(|o| o.get("model")).and_then(|v| v.as_str()),
            usage_record.and_then(|o| o.get("responseModel")).and_then(|v| v.as_str()),
            usage_record.and_then(|o| o.get("modelName")).and_then(|v| v.as_str()),
            usage_record.and_then(|o| o.get("model")).and_then(|v| v.as_str()),
        ],
        inherited_model,
    ).unwrap_or_else(|| "unknown".to_string());

    let input_tokens = to_finite_u64_opt(record.get("inputTokens"))
        .or_else(|| usage_record.and_then(|o| o.get("inputTokens")).and_then(to_finite_u64))
        .unwrap_or(0);

    let output_tokens = prefer_observed_number(
        usage_record.and_then(|o| o.get("outputTokens")).and_then(to_finite_u64),
        usage_record.and_then(|o| o.get("responseOutputTokens")).and_then(to_finite_u64),
        to_finite_u64_opt(record.get("outputTokens")),
    );

    let cache_read_tokens = to_finite_u64_opt(record.get("cacheReadTokens"))
        .or_else(|| usage_record.and_then(|o| o.get("cacheReadTokens")).and_then(to_finite_u64))
        .unwrap_or(0);

    let cache_write_tokens = to_finite_u64_opt(record.get("cacheWriteTokens"))
        .or_else(|| usage_record.and_then(|o| o.get("cacheWriteTokens")).and_then(to_finite_u64))
        .unwrap_or(0);

    let reasoning_tokens = to_finite_u64_opt(record.get("reasoningTokens"))
        .or_else(|| usage_record.and_then(|o| o.get("thinkingOutputTokens")).and_then(to_finite_u64))
        .or_else(|| usage_record.and_then(|o| o.get("reasoningTokens")).and_then(to_finite_u64))
        .unwrap_or(0);

    let mut total_tokens = to_finite_u64_opt(record.get("totalTokens"))
        .or_else(|| usage_record.and_then(|o| o.get("totalTokens")).and_then(to_finite_u64))
        .unwrap_or(0);

    let mut breakdown = TokenBreakdown {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
        total_tokens,
    };

    breakdown.total_tokens = normalize_total(&breakdown);
    total_tokens = breakdown.total_tokens;

    let subtotal = input_tokens + output_tokens + cache_read_tokens + cache_write_tokens + reasoning_tokens;
    let mut model_totals = HashMap::new();
    let mut model_breakdowns = HashMap::new();

    if subtotal > 0 || total_tokens > 0 {
        if subtotal > 0 {
            model_totals.insert(model.clone(), subtotal);
            model_breakdowns.insert(model, breakdown.clone());
        }
        Some(SignalResult {
            breakdown,
            hits: 1,
            model_totals,
            model_breakdowns,
        })
    } else {
        None
    }
}

fn count_step_rows(value: &Value) -> u64 {
    if let Some(record) = value.as_object() {
        if record.get("recordType").and_then(|v| v.as_str()) == Some("step") {
            return 1;
        }
    }
    0
}

fn count_messages(value: &Value) -> u64 {
    if let Some(record) = value.as_object() {
        if let Some(records) = record.get("records").and_then(|v| v.as_array()) {
            let mut count = 0;
            for item in records {
                count += count_step_rows(item);
            }
            return count;
        }
        return count_step_rows(value);
    }
    0
}

fn normalize_total(breakdown: &TokenBreakdown) -> u64 {
    let subtotal = breakdown.input_tokens
        + breakdown.output_tokens
        + breakdown.cache_read_tokens
        + breakdown.cache_write_tokens
        + breakdown.reasoning_tokens;
    std::cmp::max(breakdown.total_tokens, subtotal)
}

fn merge_breakdown(target: &mut TokenBreakdown, source: &TokenBreakdown) {
    target.input_tokens += source.input_tokens;
    target.output_tokens += source.output_tokens;
    target.cache_read_tokens += source.cache_read_tokens;
    target.cache_write_tokens += source.cache_write_tokens;
    target.reasoning_tokens += source.reasoning_tokens;
    target.total_tokens += source.total_tokens;
}

fn merge_model_totals(target: &mut HashMap<String, u64>, source: &HashMap<String, u64>) {
    for (model, &total) in source {
        if total == 0 {
            continue;
        }
        *target.entry(model.clone()).or_insert(0) += total;
    }
}

fn merge_model_breakdowns(target: &mut HashMap<String, TokenBreakdown>, source: &HashMap<String, TokenBreakdown>) {
    for (model, source_breakdown) in source {
        let entry = target.entry(model.clone()).or_insert_with(TokenBreakdown::default);
        merge_breakdown(entry, source_breakdown);
        entry.total_tokens = normalize_total(entry);
    }
}

fn merge_model_category<F>(target: &mut HashMap<String, TokenBreakdown>, model: &str, mutate: F)
where
    F: FnOnce(&mut TokenBreakdown),
{
    let entry = target.entry(model.to_string()).or_insert_with(TokenBreakdown::default);
    mutate(entry);
}

fn to_finite_u64(v: &Value) -> Option<u64> {
    if let Some(n) = v.as_u64() {
        Some(n)
    } else if let Some(s) = v.as_str() {
        s.trim().parse::<u64>().ok()
    } else {
        None
    }
}

fn to_finite_u64_opt(val: Option<&Value>) -> Option<u64> {
    val.and_then(to_finite_u64)
}

fn prefer_observed_number(a: Option<u64>, b: Option<u64>, c: Option<u64>) -> u64 {
    if let Some(n) = a {
        if n > 0 { return n; }
    }
    if let Some(n) = b {
        if n > 0 { return n; }
    }
    if let Some(n) = c {
        if n > 0 { return n; }
    }
    a.or(b).or(c).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_to_finite_u64() {
        let val_num = json!(123);
        let val_str = json!("456");
        let val_bad = json!("abc");
        let val_bool = json!(true);

        assert_eq!(to_finite_u64(&val_num), Some(123));
        assert_eq!(to_finite_u64(&val_str), Some(456));
        assert_eq!(to_finite_u64(&val_bad), None);
        assert_eq!(to_finite_u64(&val_bool), None);
    }

    #[test]
    fn test_prefer_observed_number() {
        assert_eq!(prefer_observed_number(Some(10), Some(20), Some(30)), 10);
        assert_eq!(prefer_observed_number(None, Some(20), Some(30)), 20);
        assert_eq!(prefer_observed_number(None, None, Some(30)), 30);
        assert_eq!(prefer_observed_number(None, None, None), 0);
    }

    #[test]
    fn test_normalize_total() {
        let b1 = TokenBreakdown {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 5,
            cache_write_tokens: 5,
            reasoning_tokens: 10,
            total_tokens: 100,
        };
        assert_eq!(normalize_total(&b1), 100);

        let b2 = TokenBreakdown {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 5,
            cache_write_tokens: 5,
            reasoning_tokens: 10,
            total_tokens: 10,
        };
        assert_eq!(normalize_total(&b2), 50);
    }
}

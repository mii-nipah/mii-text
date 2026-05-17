use std::fmt::Write as _;

use serde_json::{Value, json};

/// Normalizes a Responses-API `usage` object into the Chat Completions shape
/// (`prompt_tokens`, `completion_tokens`, `total_tokens`, and
/// `completion_tokens_details.reasoning_tokens`) so the rest of the program
/// can treat both providers uniformly.
pub fn normalize_responses_usage(u: &Value) -> Value {
    let prompt = u
        .get("input_tokens")
        .or_else(|| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64());
    let completion = u
        .get("output_tokens")
        .or_else(|| u.get("completion_tokens"))
        .and_then(|v| v.as_u64());
    let total =
        u.get("total_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| match (prompt, completion) {
                (Some(p), Some(c)) => Some(p + c),
                _ => None,
            });
    let reasoning = u
        .get("output_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .or_else(|| {
            u.get("completion_tokens_details")
                .and_then(|d| d.get("reasoning_tokens"))
                .and_then(|v| v.as_u64())
        });

    let mut out = serde_json::Map::new();
    if let Some(p) = prompt {
        out.insert("prompt_tokens".to_string(), json!(p));
    }
    if let Some(c) = completion {
        out.insert("completion_tokens".to_string(), json!(c));
    }
    if let Some(t) = total {
        out.insert("total_tokens".to_string(), json!(t));
    }
    if let Some(r) = reasoning {
        out.insert(
            "completion_tokens_details".to_string(),
            json!({ "reasoning_tokens": r }),
        );
    }
    Value::Object(out)
}

pub fn format_stats(
    model: &Option<String>,
    usage: &Option<Value>,
    total: std::time::Duration,
    first_token: Option<std::time::Duration>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n--- stats ---");
    if let Some(m) = model {
        let _ = writeln!(out, "model: {}", m);
    }
    let _ = writeln!(out, "latency: {:.3}s", total.as_secs_f64());
    if let Some(ft) = first_token {
        let _ = writeln!(out, "time to first token: {:.3}s", ft.as_secs_f64());
    }
    match usage {
        Some(u) => {
            push_usage(&mut out, u);
            let completion = u.get("completion_tokens").and_then(|v| v.as_u64());
            let secs = total.as_secs_f64();
            if let Some(c) = completion {
                if secs > 0.0 {
                    let _ = writeln!(out, "throughput: {:.1} tok/s", c as f64 / secs);
                }
            }
        }
        None => {
            let _ = writeln!(out, "usage: <not reported by server>");
        }
    }
    out
}

pub fn format_cached_stats(
    model: &Option<String>,
    usage: &Option<Value>,
    total: std::time::Duration,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\n--- stats (cached) ---");
    if let Some(m) = model {
        let _ = writeln!(out, "model: {}", m);
    }
    let _ = writeln!(out, "latency: {:.3}s", total.as_secs_f64());
    if let Some(u) = usage {
        push_usage(&mut out, u);
    }
    out
}

fn push_usage(out: &mut String, u: &Value) {
    let prompt = u.get("prompt_tokens").and_then(|v| v.as_u64());
    let completion = u.get("completion_tokens").and_then(|v| v.as_u64());
    let total_tok = u.get("total_tokens").and_then(|v| v.as_u64());
    let reasoning = u
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_u64());
    if let Some(p) = prompt {
        let _ = writeln!(out, "prompt tokens: {}", p);
    }
    if let Some(c) = completion {
        let _ = writeln!(out, "completion tokens: {}", c);
    }
    if let Some(r) = reasoning {
        let _ = writeln!(out, "  reasoning tokens: {}", r);
    }
    if let Some(t) = total_tok {
        let _ = writeln!(out, "total tokens: {}", t);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn normalizes_responses_usage_and_derives_total_tokens() {
        let usage = normalize_responses_usage(&json!({
            "input_tokens": 3,
            "output_tokens": 7,
            "output_tokens_details": { "reasoning_tokens": 2 }
        }));

        assert_eq!(usage["prompt_tokens"], 3);
        assert_eq!(usage["completion_tokens"], 7);
        assert_eq!(usage["total_tokens"], 10);
        assert_eq!(usage["completion_tokens_details"]["reasoning_tokens"], 2);
    }

    #[test]
    fn normalizes_chat_shaped_usage_without_losing_existing_total() {
        let usage = normalize_responses_usage(&json!({
            "prompt_tokens": 3,
            "completion_tokens": 7,
            "total_tokens": 99,
            "completion_tokens_details": { "reasoning_tokens": 4 }
        }));

        assert_eq!(usage["prompt_tokens"], 3);
        assert_eq!(usage["completion_tokens"], 7);
        assert_eq!(usage["total_tokens"], 99);
        assert_eq!(usage["completion_tokens_details"]["reasoning_tokens"], 4);
    }

    #[test]
    fn format_stats_includes_usage_latency_first_token_and_throughput() {
        let text = format_stats(
            &Some("model-a".to_string()),
            &Some(json!({
                "prompt_tokens": 4,
                "completion_tokens": 8,
                "total_tokens": 12,
                "completion_tokens_details": { "reasoning_tokens": 3 }
            })),
            Duration::from_secs(2),
            Some(Duration::from_millis(250)),
        );

        assert!(text.contains("--- stats ---"));
        assert!(text.contains("model: model-a"));
        assert!(text.contains("latency: 2.000s"));
        assert!(text.contains("time to first token: 0.250s"));
        assert!(text.contains("prompt tokens: 4"));
        assert!(text.contains("completion tokens: 8"));
        assert!(text.contains("reasoning tokens: 3"));
        assert!(text.contains("total tokens: 12"));
        assert!(text.contains("throughput: 4.0 tok/s"));
    }

    #[test]
    fn format_stats_reports_missing_usage_and_cached_stats_do_not_claim_live_run() {
        let missing = format_stats(&None, &None, Duration::ZERO, None);
        assert!(missing.contains("usage: <not reported by server>"));
        assert!(!missing.contains("throughput:"));

        let zero_latency = format_stats(
            &None,
            &Some(json!({ "completion_tokens": 8 })),
            Duration::ZERO,
            None,
        );
        assert!(!zero_latency.contains("throughput:"));

        let cached = format_cached_stats(
            &Some("model-b".to_string()),
            &Some(json!({ "total_tokens": 5 })),
            Duration::from_millis(12),
        );
        assert!(cached.contains("--- stats (cached) ---"));
        assert!(cached.contains("model: model-b"));
        assert!(cached.contains("total tokens: 5"));
    }
}

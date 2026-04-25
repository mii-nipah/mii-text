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
    let total = u
        .get("total_tokens")
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

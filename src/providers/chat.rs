use std::time::Instant;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::args::map_reasoning;
use crate::constraints;
use crate::conversation::build_chat_messages;
use crate::output::OutputWriter;
use crate::sink::Sink;
use crate::tools;

use super::{CallOutcome, CallParams, should_include_reasoning};

pub async fn call(
    client: &Client<OpenAIConfig>,
    sink: &mut Sink,
    params: CallParams<'_>,
) -> Result<CallOutcome, (u8, String)> {
    let req_messages = build_chat_messages(params.system, params.messages);
    let mut body = json!({
        "model": params.model,
        "messages": req_messages,
        "stream": params.stream,
    });
    if let Some(level) = params.reasoning {
        let mapped = map_reasoning(level).map_err(|e| (1u8, e))?;
        body["reasoning_effort"] = Value::String(mapped.to_string());
    }
    if let Some(t) = params.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(tool_defs) = params.tools {
        body["tools"] = Value::Array(tools::for_chat(tool_defs));
    }
    if let Some(schema) = params.schema {
        body["response_format"] = constraints::response_format_for_chat(schema);
    }
    body["max_completion_tokens"] = json!(params.max_tokens);
    // Note: `--reasoning-summary` doesn't add a request field for chat
    // completions. OpenAI only emits reasoning text via the Responses API,
    // and providers that surface it on chat (DeepSeek-R1, Qwen3-thinking,
    // GLM, etc.) emit `reasoning_content` / `reasoning` unconditionally.
    if params.stream && params.stats {
        body["stream_options"] = json!({ "include_usage": true });
    }

    let mut full_output = String::new();
    let emit_reasoning =
        should_include_reasoning(params.reasoning_summary, params.stream, params.simple);
    let mut output = OutputWriter::with_reasoning(params.simple, params.stream, emit_reasoning);
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut usage: Option<Value> = None;
    let mut model_used: Option<String> = None;
    let started = Instant::now();
    let mut first_token_at = None;

    if params.stream {
        let mut stream = client
            .chat()
            .create_stream_byot::<Value, Value>(body)
            .await
            .map_err(|e| (2u8, format!("api error: {}", e)))?;

        while let Some(item) = stream.next().await {
            let chunk = item.map_err(|e| (2u8, format!("api stream error: {}", e)))?;
            if model_used.is_none()
                && let Some(m) = chunk.get("model").and_then(|m| m.as_str())
            {
                model_used = Some(m.to_string());
            }
            if let Some(u) = chunk.get("usage")
                && !u.is_null()
            {
                usage = Some(u.clone());
            }
            let delta = chunk
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"));

            if let Some(d) = delta {
                let r = d
                    .get("reasoning_content")
                    .and_then(|v| v.as_str())
                    .or_else(|| d.get("reasoning").and_then(|v| v.as_str()));
                if let Some(r) = r {
                    let wrote = output
                        .push_reasoning(sink, &mut full_output, r)
                        .await
                        .map_err(|e| (1u8, e))?;
                    if wrote && first_token_at.is_none() {
                        first_token_at = Some(started.elapsed());
                    }
                }
            }
            if let Some(d) = delta {
                if d.get("tool_calls").and_then(Value::as_array).is_some()
                    && first_token_at.is_none()
                {
                    first_token_at = Some(started.elapsed());
                }
                merge_tool_call_deltas(&mut tool_calls, d);
            }

            if let Some(content) = delta
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
                && !content.is_empty()
            {
                if first_token_at.is_none() {
                    first_token_at = Some(started.elapsed());
                }
                output
                    .push_content(sink, &mut full_output, content)
                    .await
                    .map_err(|e| (1u8, e))?;
            }
        }
        output.set_tool_calls(tool_calls);
        output
            .finish(sink, &mut full_output)
            .await
            .map_err(|e| (1u8, e))?;
    } else {
        let resp: Value = client
            .chat()
            .create_byot(body)
            .await
            .map_err(|e| (2u8, format!("api error: {}", e)))?;
        if let Some(m) = resp.get("model").and_then(|m| m.as_str()) {
            model_used = Some(m.to_string());
        }
        if let Some(u) = resp.get("usage")
            && !u.is_null()
        {
            usage = Some(u.clone());
        }
        let message = resp
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"));
        if let Some(calls) = message
            .and_then(|m| m.get("tool_calls"))
            .and_then(Value::as_array)
        {
            tool_calls = calls.clone();
        }
        let reasoning_text = message.and_then(|m| {
            m.get("reasoning_content")
                .and_then(|v| v.as_str())
                .or_else(|| m.get("reasoning").and_then(|v| v.as_str()))
        });
        if let Some(r) = reasoning_text {
            output
                .push_reasoning(sink, &mut full_output, r)
                .await
                .map_err(|e| (1u8, e))?;
        }
        let content = message
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        output
            .push_content(sink, &mut full_output, content)
            .await
            .map_err(|e| (1u8, e))?;
        output.set_tool_calls(tool_calls);
        output
            .finish(sink, &mut full_output)
            .await
            .map_err(|e| (1u8, e))?;
    }
    let prospect = output.prospect().clone();
    let events = output.events().to_vec();
    let assistant_buf = prospect.content.clone();

    Ok(CallOutcome {
        assistant_buf,
        full_output,
        prospect,
        events,
        usage,
        model_used,
        first_token_at,
    })
}

fn merge_tool_call_deltas(tool_calls: &mut Vec<Value>, delta: &Value) {
    let Some(items) = delta.get("tool_calls").and_then(Value::as_array) else {
        return;
    };

    for item in items {
        let index = item
            .get("index")
            .and_then(Value::as_u64)
            .map(|i| i as usize)
            .unwrap_or(tool_calls.len());
        while tool_calls.len() <= index {
            tool_calls.push(json!({}));
        }
        merge_delta_value(&mut tool_calls[index], item);
    }
}

fn merge_delta_value(target: &mut Value, delta: &Value) {
    if !target.is_object() || !delta.is_object() {
        *target = delta.clone();
        return;
    }

    let target = target.as_object_mut().expect("checked object");
    let delta = delta.as_object().expect("checked object");
    for (key, value) in delta {
        if key == "index" || value.is_null() {
            continue;
        }

        match (target.get_mut(key), value) {
            (Some(Value::String(existing)), Value::String(part))
                if key == "arguments" || key == "input" =>
            {
                existing.push_str(part)
            }
            (Some(existing @ Value::Object(_)), Value::Object(_)) => {
                merge_delta_value(existing, value);
            }
            (Some(Value::String(existing)), Value::String(part)) => {
                if existing.is_empty() {
                    existing.push_str(part);
                } else if existing != part {
                    *existing = part.clone();
                }
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn merges_streamed_tool_call_deltas() {
        let mut calls = Vec::new();
        merge_tool_call_deltas(
            &mut calls,
            &json!({
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "search", "arguments": "{\"q\"" }
                }]
            }),
        );
        merge_tool_call_deltas(
            &mut calls,
            &json!({
                "tool_calls": [{
                    "index": 0,
                    "function": { "arguments": ":\"rust\"}" }
                }]
            }),
        );

        assert_eq!(calls[0]["id"], "call_1");
        assert_eq!(calls[0]["function"]["name"], "search");
        assert_eq!(calls[0]["function"]["arguments"], "{\"q\":\"rust\"}");
        assert!(calls[0].get("index").is_none());
    }

    #[test]
    fn merges_tool_call_input_deltas_and_overwrites_changed_metadata() {
        let mut calls = Vec::new();
        merge_tool_call_deltas(
            &mut calls,
            &json!({
                "tool_calls": [{
                    "index": 1,
                    "id": "call_1",
                    "type": "function",
                    "name": "draft",
                    "input": "{\"city\""
                }]
            }),
        );
        merge_tool_call_deltas(
            &mut calls,
            &json!({
                "tool_calls": [{
                    "index": 1,
                    "name": "weather",
                    "input": ":\"Paris\"}"
                }]
            }),
        );

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], json!({}));
        assert_eq!(calls[1]["id"], "call_1");
        assert_eq!(calls[1]["type"], "function");
        assert_eq!(calls[1]["name"], "weather");
        assert_eq!(calls[1]["input"], "{\"city\":\"Paris\"}");
    }

    #[test]
    fn merge_delta_value_replaces_non_object_targets() {
        let mut target = json!("old");
        merge_delta_value(&mut target, &json!({ "id": "call_1" }));

        assert_eq!(target, json!({ "id": "call_1" }));
    }
}

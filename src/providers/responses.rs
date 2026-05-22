use std::time::Instant;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::args::map_reasoning;
use crate::constraints;
use crate::conversation::is_provider_continuation_value;
use crate::output::{OutputWriter, ProviderContinuation};
use crate::sink::Sink;
use crate::stats::normalize_responses_usage;
use crate::tools;

use super::{CallOutcome, CallParams, should_include_reasoning};

fn build_input(
    system: &Option<String>,
    msgs: &[crate::conversation::Message],
) -> Result<Vec<Value>, String> {
    let mut out: Vec<Value> = Vec::with_capacity(msgs.len() + 1);
    if let Some(sys) = system {
        out.push(json!({ "role": "system", "content": sys }));
    }
    for m in msgs {
        append_input_item(&mut out, m.to_api_value())?;
    }
    Ok(out)
}

fn append_input_item(out: &mut Vec<Value>, item: Value) -> Result<(), String> {
    if !is_provider_continuation_value(&item) {
        out.push(item);
        return Ok(());
    }

    let provider = item
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("openai");
    if provider != "openai" {
        return Err(format!(
            "provider_continuation for provider '{}' cannot be sent to OpenAI",
            provider
        ));
    }

    let reasoning_items = item
        .get("reasoning_items")
        .or_else(|| item.get("items"))
        .and_then(Value::as_array)
        .ok_or_else(|| "provider_continuation requires a reasoning_items array".to_string())?;
    out.extend(reasoning_items.iter().cloned());
    Ok(())
}

pub async fn call(
    client: &Client<OpenAIConfig>,
    sink: &mut Sink,
    params: CallParams<'_>,
) -> Result<CallOutcome, (u8, String)> {
    let input = build_input(params.system, params.messages).map_err(|e| (1u8, e))?;

    let mut body = json!({
        "model": params.model,
        "input": input,
        "stream": true,
        "store": false,
        "include": ["reasoning.encrypted_content"],
        "max_output_tokens": params.max_tokens,
    });
    let emit_reasoning =
        should_include_reasoning(params.reasoning_summary, params.stream, params.simple);
    let mut reasoning_obj = json!({ "summary": "auto" });
    if let Some(level) = params.reasoning {
        let mapped = map_reasoning(level).map_err(|e| (1u8, e))?;
        reasoning_obj["effort"] = Value::String(mapped.to_string());
    }
    body["reasoning"] = reasoning_obj;
    if let Some(t) = params.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(tool_defs) = params.tools {
        body["tools"] = Value::Array(tools::for_responses(tool_defs));
    }
    if let Some(schema) = params.schema {
        body["text"] = constraints::text_format_for_responses(schema);
    }

    let mut full_output = String::new();
    let mut output = OutputWriter::with_done(
        params.simple,
        params.stream,
        emit_reasoning,
        params.emit_done,
    );
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut usage: Option<Value> = None;
    let mut model_used: Option<String> = None;
    let started = Instant::now();
    let mut first_token_at = None;

    let mut stream = client
        .responses()
        .create_stream_byot::<Value, Value>(body)
        .await
        .map_err(|e| (2u8, format!("api error: {}", e)))?;

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| (2u8, format!("api stream error: {}", e)))?;
        let event_type = chunk.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event_type {
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(delta) = reasoning_event_text(event_type, &chunk) {
                    push_reasoning_event(
                        &mut output,
                        sink,
                        &mut full_output,
                        delta,
                        &started,
                        &mut first_token_at,
                    )
                    .await?;
                }
            }
            "response.reasoning_summary_text.done"
            | "response.reasoning_text.done"
            | "response.reasoning_summary_part.done" => {
                if output.prospect().reasoning.is_none()
                    && let Some(text) = reasoning_event_text(event_type, &chunk)
                {
                    push_reasoning_event(
                        &mut output,
                        sink,
                        &mut full_output,
                        text,
                        &started,
                        &mut first_token_at,
                    )
                    .await?;
                }
            }
            "response.output_text.delta" => {
                if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str())
                    && !delta.is_empty()
                {
                    if first_token_at.is_none() {
                        first_token_at = Some(started.elapsed());
                    }
                    output
                        .push_content(sink, &mut full_output, delta)
                        .await
                        .map_err(|e| (1u8, e))?;
                }
            }
            "response.created" | "response.completed" => {
                if let Some(resp) = chunk.get("response") {
                    if model_used.is_none()
                        && let Some(m) = resp.get("model").and_then(|m| m.as_str())
                    {
                        model_used = Some(m.to_string());
                    }
                    if let Some(u) = resp.get("usage")
                        && !u.is_null()
                    {
                        usage = Some(normalize_responses_usage(u));
                    }
                    if event_type == "response.completed"
                        && let Some(output_items) = resp.get("output").and_then(|o| o.as_array())
                    {
                        let completed_tool_calls = extract_tool_calls(output_items);
                        if !completed_tool_calls.is_empty() {
                            tool_calls = completed_tool_calls;
                        }
                        if !tool_calls.is_empty() && first_token_at.is_none() {
                            first_token_at = Some(started.elapsed());
                        }
                        if output.prospect().reasoning.is_none()
                            && let Some(reasoning) = extract_reasoning_summary(output_items)
                        {
                            push_reasoning_event(
                                &mut output,
                                sink,
                                &mut full_output,
                                &reasoning,
                                &started,
                                &mut first_token_at,
                            )
                            .await?;
                        }
                        if let Some(continuation) =
                            extract_provider_continuation(resp, output_items)
                        {
                            output.set_provider_continuation(continuation);
                        }
                    }
                }
            }
            "response.function_call_arguments.done" => {
                if let Some(item) = chunk.get("item") {
                    if first_token_at.is_none() {
                        first_token_at = Some(started.elapsed());
                    }
                    tool_calls.push(item.clone());
                }
            }
            _ => {}
        }
    }
    output.set_tool_calls(tool_calls);
    output
        .finish(sink, &mut full_output)
        .await
        .map_err(|e| (1u8, e))?;
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

fn reasoning_event_text<'a>(event_type: &str, chunk: &'a Value) -> Option<&'a str> {
    match event_type {
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            chunk.get("delta").and_then(Value::as_str)
        }
        "response.reasoning_summary_text.done" | "response.reasoning_text.done" => chunk
            .get("text")
            .and_then(Value::as_str)
            .or_else(|| chunk.get("delta").and_then(Value::as_str)),
        "response.reasoning_summary_part.done" => chunk
            .get("part")
            .and_then(|p| p.get("text"))
            .and_then(Value::as_str),
        _ => None,
    }
}

async fn push_reasoning_event(
    output: &mut OutputWriter,
    sink: &mut Sink,
    full_output: &mut String,
    text: &str,
    started: &std::time::Instant,
    first_token_at: &mut Option<std::time::Duration>,
) -> Result<(), (u8, String)> {
    let wrote = output
        .push_reasoning(sink, full_output, text)
        .await
        .map_err(|e| (1u8, e))?;
    if wrote && first_token_at.is_none() {
        *first_token_at = Some(started.elapsed());
    }
    Ok(())
}

fn extract_tool_calls(output: &[Value]) -> Vec<Value> {
    output
        .iter()
        .filter(|item| {
            item.get("type")
                .and_then(Value::as_str)
                .map(|kind| kind == "function_call" || kind == "custom_tool_call")
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn extract_reasoning_summary(output: &[Value]) -> Option<String> {
    let mut text = String::new();
    for item in output {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        if item_type != "reasoning" {
            continue;
        }

        append_reasoning_text(item.get("summary"), &mut text);
        append_reasoning_text(item.get("content"), &mut text);
    }

    if text.is_empty() { None } else { Some(text) }
}

fn extract_provider_continuation(
    response: &Value,
    output: &[Value],
) -> Option<ProviderContinuation> {
    let reasoning_items = output
        .iter()
        .filter(|item| is_encrypted_continuation_item(item))
        .cloned()
        .collect::<Vec<_>>();
    if reasoning_items.is_empty() {
        return None;
    }

    Some(ProviderContinuation {
        provider: "openai".to_string(),
        response_id: response
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        reasoning_items,
    })
}

fn is_encrypted_continuation_item(item: &Value) -> bool {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    matches!(item_type, "reasoning" | "compaction")
        && item
            .get("encrypted_content")
            .and_then(Value::as_str)
            .map(|content| !content.is_empty())
            .unwrap_or(false)
}

fn append_reasoning_text(value: Option<&Value>, out: &mut String) {
    match value {
        Some(Value::Array(items)) => {
            for item in items {
                append_reasoning_text(Some(item), out);
            }
        }
        Some(Value::Object(item)) => {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                out.push_str(text);
            } else {
                append_reasoning_text(item.get("summary"), out);
                append_reasoning_text(item.get("content"), out);
            }
        }
        Some(Value::String(text)) => out.push_str(text),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::conversation::Message;

    #[test]
    fn extracts_response_tool_call_items() {
        let calls = extract_tool_calls(&[
            json!({ "type": "reasoning" }),
            json!({ "type": "function_call", "call_id": "call_1" }),
            json!({ "type": "custom_tool_call", "call_id": "call_2" }),
            json!({ "type": "message", "content": [] }),
        ]);

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["call_id"], "call_1");
        assert_eq!(calls[1]["call_id"], "call_2");
    }

    #[test]
    fn build_input_prepends_system_and_preserves_response_tool_messages() {
        let system = Some("system prompt".to_string());
        let messages = vec![
            Message::user("hello".to_string()),
            serde_json::from_value(json!({
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "done"
            }))
            .unwrap(),
        ];

        let input = build_input(&system, &messages).unwrap();

        assert_eq!(input.len(), 3);
        assert_eq!(
            input[0],
            json!({ "role": "system", "content": "system prompt" })
        );
        assert_eq!(input[1], json!({ "role": "user", "content": "hello" }));
        assert_eq!(
            input[2],
            json!({
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "done"
            })
        );
    }

    #[test]
    fn build_input_expands_provider_continuation_items() {
        let messages = vec![
            serde_json::from_value(json!({
                "provider": "openai",
                "response_id": "resp_1",
                "reasoning_items": [
                    {
                        "type": "reasoning",
                        "id": "rs_1",
                        "summary": [],
                        "encrypted_content": "opaque"
                    }
                ]
            }))
            .unwrap(),
            Message::user("continue".to_string()),
        ];

        let input = build_input(&None, &messages).unwrap();

        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["encrypted_content"], "opaque");
        assert_eq!(input[1], json!({ "role": "user", "content": "continue" }));
    }

    #[test]
    fn build_input_accepts_streamed_provider_continuation_event_shape() {
        let messages = vec![
            serde_json::from_value(json!({
                "type": "provider_continuation",
                "provider": "openai",
                "reasoning_items": [
                    {
                        "type": "reasoning",
                        "encrypted_content": "opaque"
                    }
                ]
            }))
            .unwrap(),
        ];

        let input = build_input(&None, &messages).unwrap();

        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["encrypted_content"], "opaque");
    }

    #[test]
    fn build_input_does_not_treat_provider_metadata_as_continuation() {
        let messages = vec![
            serde_json::from_value(json!({
                "provider": "openai",
                "content": "ordinary metadata"
            }))
            .unwrap(),
        ];

        let input = build_input(&None, &messages).unwrap();

        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["provider"], "openai");
        assert_eq!(input[0]["content"], "ordinary metadata");
    }

    #[test]
    fn build_input_requires_provider_marker_for_untyped_continuation() {
        let messages = vec![
            serde_json::from_value(json!({
                "reasoning_items": [
                    {
                        "type": "reasoning",
                        "encrypted_content": "opaque"
                    }
                ]
            }))
            .unwrap(),
        ];

        let input = build_input(&None, &messages).unwrap();

        assert_eq!(input.len(), 1);
        assert_eq!(
            input[0]["reasoning_items"][0]["encrypted_content"],
            "opaque"
        );
    }

    #[test]
    fn extracts_encrypted_reasoning_continuation() {
        let continuation = extract_provider_continuation(
            &json!({ "id": "resp_1" }),
            &[
                json!({
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [],
                    "encrypted_content": "opaque"
                }),
                json!({ "type": "reasoning", "summary": [] }),
                json!({ "type": "function_call", "call_id": "call_1" }),
            ],
        )
        .unwrap();

        assert_eq!(continuation.provider, "openai");
        assert_eq!(continuation.response_id.as_deref(), Some("resp_1"));
        assert_eq!(continuation.reasoning_items.len(), 1);
        assert_eq!(
            continuation.reasoning_items[0]["encrypted_content"],
            "opaque"
        );
    }

    #[test]
    fn extract_tool_calls_ignores_messages_and_reasoning_items() {
        let calls = extract_tool_calls(&[
            json!({ "type": "message", "content": [{ "type": "output_text", "text": "hi" }] }),
            json!({ "type": "reasoning", "summary": [] }),
        ]);

        assert!(calls.is_empty());
    }

    #[test]
    fn extracts_reasoning_summary_from_response_output_items() {
        let reasoning = extract_reasoning_summary(&[
            json!({
                "type": "reasoning",
                "summary": [
                    { "type": "summary_text", "text": "first" },
                    { "type": "summary_text", "text": " second" }
                ]
            }),
            json!({ "type": "function_call", "name": "echo" }),
        ]);

        assert_eq!(reasoning.as_deref(), Some("first second"));
    }

    #[test]
    fn extracts_reasoning_text_from_content_fallbacks() {
        let reasoning = extract_reasoning_summary(&[
            json!({
                "type": "reasoning",
                "content": [
                    { "type": "reasoning_text", "text": "fallback" }
                ]
            }),
            json!({
                "type": "reasoning",
                "summary": " plus string summary"
            }),
        ]);

        assert_eq!(reasoning.as_deref(), Some("fallback plus string summary"));
    }

    #[test]
    fn extracts_reasoning_text_from_stream_event_variants() {
        assert_eq!(
            reasoning_event_text(
                "response.reasoning_summary_text.delta",
                &json!({ "delta": "delta summary" })
            ),
            Some("delta summary")
        );
        assert_eq!(
            reasoning_event_text(
                "response.reasoning_summary_text.done",
                &json!({ "text": "done summary" })
            ),
            Some("done summary")
        );
        assert_eq!(
            reasoning_event_text(
                "response.reasoning_summary_part.done",
                &json!({ "part": { "type": "summary_text", "text": "part summary" } })
            ),
            Some("part summary")
        );
        assert_eq!(
            reasoning_event_text("response.output_text.delta", &json!({ "delta": "nope" })),
            None
        );
    }
}

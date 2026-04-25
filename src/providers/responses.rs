use std::time::Instant;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::args::map_reasoning;
use crate::sink::{Sink, ThinkWriter};
use crate::stats::normalize_responses_usage;

use super::{CallOutcome, CallParams};

fn build_input(system: &Option<String>, msgs: &[crate::conversation::Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(msgs.len() + 1);
    if let Some(sys) = system {
        out.push(json!({ "role": "system", "content": sys }));
    }
    for m in msgs {
        out.push(json!({ "role": m.role, "content": m.content }));
    }
    out
}

pub async fn call(
    client: &Client<OpenAIConfig>,
    sink: &mut Sink,
    params: CallParams<'_>,
) -> Result<CallOutcome, (u8, String)> {
    let input = build_input(params.system, params.messages);

    let mut reasoning_obj = json!({ "summary": "auto" });
    if let Some(level) = params.reasoning {
        let mapped = map_reasoning(level).map_err(|e| (1u8, e))?;
        reasoning_obj["effort"] = Value::String(mapped.to_string());
    }

    let mut body = json!({
        "model": params.model,
        "input": input,
        "stream": params.stream,
        "max_output_tokens": params.max_tokens,
        "reasoning": reasoning_obj,
    });
    if let Some(t) = params.temperature {
        body["temperature"] = json!(t);
    }

    let mut assistant_buf = String::new();
    let mut full_output = String::new();
    let mut usage: Option<Value> = None;
    let mut model_used: Option<String> = None;
    let started = Instant::now();
    let mut first_token_at = None;
    // ThinkWriter is always enabled here — this provider only runs when
    // --reasoning-summary is set.
    let mut think = ThinkWriter::new(true);

    if params.stream {
        let mut stream = client
            .responses()
            .create_stream_byot::<Value, Value>(body)
            .await
            .map_err(|e| (2u8, format!("api error: {}", e)))?;

        while let Some(item) = stream.next().await {
            let chunk = item.map_err(|e| (2u8, format!("api stream error: {}", e)))?;
            let event_type = chunk.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match event_type {
                "response.reasoning_summary_text.delta" => {
                    if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                        let opened = think
                            .write_reasoning(sink, &mut full_output, delta)
                            .await
                            .map_err(|e| (1u8, e))?;
                        if opened && first_token_at.is_none() {
                            first_token_at = Some(started.elapsed());
                        }
                    }
                }
                "response.output_text.delta" => {
                    if let Some(delta) = chunk.get("delta").and_then(|d| d.as_str()) {
                        if !delta.is_empty() {
                            if first_token_at.is_none() {
                                first_token_at = Some(started.elapsed());
                            }
                            think
                                .write_content(sink, &mut full_output, delta)
                                .await
                                .map_err(|e| (1u8, e))?;
                            assistant_buf.push_str(delta);
                        }
                    }
                }
                "response.created" | "response.completed" => {
                    if let Some(resp) = chunk.get("response") {
                        if model_used.is_none() {
                            if let Some(m) = resp.get("model").and_then(|m| m.as_str()) {
                                model_used = Some(m.to_string());
                            }
                        }
                        if let Some(u) = resp.get("usage") {
                            if !u.is_null() {
                                usage = Some(normalize_responses_usage(u));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        think
            .close_if_open(sink, &mut full_output)
            .await
            .map_err(|e| (1u8, e))?;
    } else {
        let resp: Value = client
            .responses()
            .create_byot(body)
            .await
            .map_err(|e| (2u8, format!("api error: {}", e)))?;
        if let Some(m) = resp.get("model").and_then(|m| m.as_str()) {
            model_used = Some(m.to_string());
        }
        if let Some(u) = resp.get("usage") {
            if !u.is_null() {
                usage = Some(normalize_responses_usage(u));
            }
        }
        if let Some(output) = resp.get("output").and_then(|o| o.as_array()) {
            // Reasoning summary blocks come first in the output array.
            for item in output {
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if item_type != "reasoning" {
                    continue;
                }
                if let Some(summary) = item.get("summary").and_then(|s| s.as_array()) {
                    for s in summary {
                        if let Some(text) = s.get("text").and_then(|t| t.as_str()) {
                            think
                                .write_reasoning(sink, &mut full_output, text)
                                .await
                                .map_err(|e| (1u8, e))?;
                        }
                    }
                }
            }
            for item in output {
                let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if item_type != "message" {
                    continue;
                }
                if let Some(content) = item.get("content").and_then(|c| c.as_array()) {
                    for piece in content {
                        let piece_type =
                            piece.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if piece_type != "output_text" {
                            continue;
                        }
                        if let Some(text) = piece.get("text").and_then(|t| t.as_str()) {
                            think
                                .write_content(sink, &mut full_output, text)
                                .await
                                .map_err(|e| (1u8, e))?;
                            assistant_buf.push_str(text);
                        }
                    }
                }
            }
        }
    }

    Ok(CallOutcome {
        assistant_buf,
        full_output,
        usage,
        model_used,
        first_token_at,
    })
}

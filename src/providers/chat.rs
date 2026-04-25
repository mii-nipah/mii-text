use std::time::Instant;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::args::map_reasoning;
use crate::conversation::build_chat_messages;
use crate::sink::{Sink, ThinkWriter};

use super::{CallOutcome, CallParams};

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
    body["max_completion_tokens"] = json!(params.max_tokens);
    // Note: `--reasoning-summary` doesn't add a request field for chat
    // completions. OpenAI only emits reasoning text via the Responses API,
    // and providers that surface it on chat (DeepSeek-R1, Qwen3-thinking,
    // GLM, etc.) emit `reasoning_content` / `reasoning` unconditionally.
    if params.stream && params.stats {
        body["stream_options"] = json!({ "include_usage": true });
    }

    let mut assistant_buf = String::new();
    let mut full_output = String::new();
    let mut usage: Option<Value> = None;
    let mut model_used: Option<String> = None;
    let started = Instant::now();
    let mut first_token_at = None;
    let mut think = ThinkWriter::new(params.reasoning_summary);

    if params.stream {
        let mut stream = client
            .chat()
            .create_stream_byot::<Value, Value>(body)
            .await
            .map_err(|e| (2u8, format!("api error: {}", e)))?;

        while let Some(item) = stream.next().await {
            let chunk = item.map_err(|e| (2u8, format!("api stream error: {}", e)))?;
            if model_used.is_none() {
                if let Some(m) = chunk.get("model").and_then(|m| m.as_str()) {
                    model_used = Some(m.to_string());
                }
            }
            if let Some(u) = chunk.get("usage") {
                if !u.is_null() {
                    usage = Some(u.clone());
                }
            }
            let delta = chunk
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"));

            if params.reasoning_summary {
                if let Some(d) = delta {
                    let r = d
                        .get("reasoning_content")
                        .and_then(|v| v.as_str())
                        .or_else(|| d.get("reasoning").and_then(|v| v.as_str()));
                    if let Some(r) = r {
                        let opened = think
                            .write_reasoning(sink, &mut full_output, r)
                            .await
                            .map_err(|e| (1u8, e))?;
                        if opened && first_token_at.is_none() {
                            first_token_at = Some(started.elapsed());
                        }
                    }
                }
            }

            if let Some(content) = delta
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
            {
                if !content.is_empty() {
                    if first_token_at.is_none() {
                        first_token_at = Some(started.elapsed());
                    }
                    think
                        .write_content(sink, &mut full_output, content)
                        .await
                        .map_err(|e| (1u8, e))?;
                    assistant_buf.push_str(content);
                }
            }
        }
        think
            .close_if_open(sink, &mut full_output)
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
        if let Some(u) = resp.get("usage") {
            if !u.is_null() {
                usage = Some(u.clone());
            }
        }
        let message = resp
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"));
        if params.reasoning_summary {
            let reasoning_text = message.and_then(|m| {
                m.get("reasoning_content")
                    .and_then(|v| v.as_str())
                    .or_else(|| m.get("reasoning").and_then(|v| v.as_str()))
            });
            if let Some(r) = reasoning_text {
                think
                    .write_reasoning(sink, &mut full_output, r)
                    .await
                    .map_err(|e| (1u8, e))?;
            }
        }
        let content = message
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        think
            .write_content(sink, &mut full_output, content)
            .await
            .map_err(|e| (1u8, e))?;
        assistant_buf.push_str(content);
    }

    Ok(CallOutcome {
        assistant_buf,
        full_output,
        usage,
        model_used,
        first_token_at,
    })
}

use std::time::Duration;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use serde_json::Value;

use crate::conversation::Message;
use crate::sink::Sink;

pub mod chat;
pub mod responses;

pub struct CallParams<'a> {
    pub model: &'a str,
    pub system: &'a Option<String>,
    pub messages: &'a [Message],
    pub stream: bool,
    pub reasoning: &'a Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: u32,
    pub reasoning_summary: bool,
    pub stats: bool,
}

pub struct CallOutcome {
    pub assistant_buf: String,
    pub full_output: String,
    pub usage: Option<Value>,
    pub model_used: Option<String>,
    pub first_token_at: Option<Duration>,
}

/// Returns true when the configured base URL points at OpenAI's API (i.e. the
/// default endpoint is in use, or `--url` explicitly references api.openai.com).
pub fn is_openai(base_url: Option<&str>) -> bool {
    match base_url {
        None => true,
        Some(u) => u.contains("api.openai.com"),
    }
}

pub async fn call(
    client: &Client<OpenAIConfig>,
    sink: &mut Sink,
    params: CallParams<'_>,
    base_url: Option<&str>,
) -> Result<CallOutcome, (u8, String)> {
    if params.reasoning_summary && is_openai(base_url) {
        responses::call(client, sink, params).await
    } else {
        chat::call(client, sink, params).await
    }
}

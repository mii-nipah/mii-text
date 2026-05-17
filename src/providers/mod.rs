use std::time::Duration;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use serde_json::Value;

use crate::conversation::Message;
use crate::output::Prospect;
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
    pub tools: &'a Option<Vec<Value>>,
    pub completions: bool,
    pub simple: bool,
}

pub struct CallOutcome {
    pub assistant_buf: String,
    pub full_output: String,
    pub prospect: Prospect,
    pub events: Vec<Value>,
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

fn use_chat_completions(completions: bool, base_url: Option<&str>) -> bool {
    completions || !is_openai(base_url)
}

pub(crate) fn should_include_reasoning(
    reasoning_summary: bool,
    stream: bool,
    simple: bool,
) -> bool {
    reasoning_summary || (!stream && !simple)
}

pub async fn call(
    client: &Client<OpenAIConfig>,
    sink: &mut Sink,
    params: CallParams<'_>,
    base_url: Option<&str>,
) -> Result<CallOutcome, (u8, String)> {
    if use_chat_completions(params.completions, base_url) {
        chat::call(client, sink, params).await
    } else {
        responses::call(client, sink, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_openai_to_responses_by_default() {
        assert!(!use_chat_completions(false, None));
        assert!(!use_chat_completions(
            false,
            Some("https://api.openai.com/v1")
        ));
    }

    #[test]
    fn keeps_chat_completions_for_compatibility() {
        assert!(use_chat_completions(true, None));
        assert!(use_chat_completions(false, Some("https://example.test/v1")));
    }

    #[test]
    fn includes_reasoning_by_default_only_for_structured_non_streaming_output() {
        assert!(should_include_reasoning(false, false, false));
        assert!(should_include_reasoning(true, false, false));
        assert!(should_include_reasoning(true, true, false));
        assert!(should_include_reasoning(true, false, true));
        assert!(!should_include_reasoning(false, true, false));
        assert!(!should_include_reasoning(false, false, true));
    }
}

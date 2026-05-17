use std::env;
use std::process::ExitCode;
use std::time::Instant;

use async_openai::Client;
use async_openai::config::OpenAIConfig;

use crate::args::{Args, DEFAULT_MAX_TOKENS, parse, usage};
use crate::conversation::{
    Message, load_input_messages, load_stateful, push_assistant_turn, save_stateful,
};
use crate::output::ProviderContinuation;
use crate::output::render_cached;
use crate::providers::{CallParams, call as call_provider, is_openai, should_include_reasoning};
use crate::sink::{ErrSink, Sink};
use crate::stats::{format_cached_stats, format_stats};

mod args;
mod cache;
mod client;
mod conversation;
mod ipc;
mod output;
mod providers;
mod server;
mod sink;
mod stats;
mod tools;

#[tokio::main]
async fn main() -> ExitCode {
    let parsed = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}\n{}", e, usage());
            return ExitCode::from(1);
        }
    };

    if parsed.serve {
        return match server::serve(parsed).await {
            Ok(()) => ExitCode::from(0),
            Err(e) => {
                eprintln!("{}", e);
                ExitCode::from(1)
            }
        };
    }
    if parsed.ipc {
        let result = if parsed.status {
            client::run_status(parsed).await
        } else {
            client::run_ipc(parsed).await
        };
        return match result {
            Ok(code) => ExitCode::from(code),
            Err(e) => {
                eprintln!("{}", e);
                ExitCode::from(1)
            }
        };
    }

    match run_local(parsed).await {
        Ok(code) => ExitCode::from(code),
        Err((code, msg)) => {
            eprintln!("{}", msg);
            ExitCode::from(code)
        }
    }
}

async fn run_local(args: Args) -> Result<u8, (u8, String)> {
    let mut sink = Sink::open(&args.out).await.map_err(|e| (1u8, e))?;
    let err = ErrSink::Local;
    let outcome = run(&args, &mut sink, &err, None).await?;
    Ok(outcome.exit_code)
}

pub struct RunOutcome {
    pub exit_code: u8,
    /// Final assistant message text (no `<think>` wrapping). Empty when no
    /// content was produced.
    pub assistant_buf: String,
    pub provider_continuation: Option<ProviderContinuation>,
}

/// Core request execution shared by local, server, and (via the server)
/// IPC-client invocations. The caller owns sink lifecycle and decides how to
/// surface diagnostic output.
pub async fn run(
    args: &Args,
    sink: &mut Sink,
    err: &ErrSink,
    stdin_override: Option<String>,
) -> Result<RunOutcome, (u8, String)> {
    let model = args
        .model
        .clone()
        .or_else(|| env::var("OPENAI_MODEL_NAME").ok())
        .ok_or((
            1u8,
            "missing model (--model or OPENAI_MODEL_NAME)".to_string(),
        ))?;

    let mut conversation: Vec<Message> = match &args.stateful {
        Some(p) => load_stateful(p).await.map_err(|e| (1u8, e))?,
        None => Vec::new(),
    };
    let new_messages = load_input_messages(&args.messages, args.quick, stdin_override.as_deref())
        .await
        .map_err(|e| (1u8, e))?;
    conversation.extend(new_messages);

    let max_tokens = args.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let tools = tools::resolve(&args.tools).await.map_err(|e| (1u8, e))?;
    let key_hash = cache::key(cache::KeyParts {
        model: &model,
        system: &args.system,
        messages: &conversation,
        reasoning: &args.reasoning,
        temperature: args.temperature,
        max_tokens,
        tools: &tools,
        completions: args.completions,
    });
    let cache_conn = match &args.cache {
        Some(p) => Some(cache::open(p).map_err(|e| (1u8, e))?),
        None => None,
    };

    if let Some(conn) = &cache_conn
        && let Some(hit) = cache::lookup(conn, &key_hash).map_err(|e| (1u8, e))?
    {
        return replay_cached(args, sink, err, &mut conversation, hit).await;
    }

    let base_url = args
        .url
        .clone()
        .or_else(|| env::var("OPENAI_BASE_URL").ok());
    let api_key = api_key_for_base_url(
        args.key.clone().and_then(non_empty).as_deref(),
        env::var("OPENAI_API_KEY")
            .ok()
            .and_then(non_empty)
            .as_deref(),
        base_url.as_deref(),
    )
    .map_err(|e| (1u8, e))?;

    let mut config = OpenAIConfig::new().with_api_key(api_key);
    if let Some(u) = &base_url {
        config = config.with_api_base(u.clone());
    }
    let client = Client::with_config(config);

    let started = Instant::now();
    let outcome = call_provider(
        &client,
        sink,
        CallParams {
            model: &model,
            system: &args.system,
            messages: &conversation,
            stream: args.stream,
            reasoning: &args.reasoning,
            temperature: args.temperature,
            max_tokens,
            reasoning_summary: args.reasoning_summary,
            stats: args.stats,
            tools: &tools,
            completions: args.completions,
            simple: args.simple,
        },
        base_url.as_deref(),
    )
    .await?;
    let total_elapsed = started.elapsed();

    sink.finish()
        .await
        .map_err(|e| (1u8, format!("flush output: {}", e)))?;

    if let Some(conn) = &cache_conn {
        cache::store(
            conn,
            cache::StoreEntry {
                key: &key_hash,
                content: &outcome.full_output,
                assistant: &outcome.assistant_buf,
                prospect: &outcome.prospect,
                events: &outcome.events,
                usage: &outcome.usage,
                model: &outcome.model_used,
            },
        )
        .map_err(|e| (1u8, e))?;
    }

    if args.stats {
        err.emit(&format_stats(
            &outcome.model_used,
            &outcome.usage,
            total_elapsed,
            outcome.first_token_at,
        ));
    }

    if let Some(p) = &args.stateful {
        push_assistant_turn(
            &mut conversation,
            outcome.assistant_buf.clone(),
            outcome.prospect.provider_continuation.as_ref(),
        )
        .map_err(|e| (1u8, e))?;
        save_stateful(p, &conversation)
            .await
            .map_err(|e| (1u8, e))?;
    }

    Ok(RunOutcome {
        exit_code: 0,
        assistant_buf: outcome.assistant_buf,
        provider_continuation: outcome.prospect.provider_continuation,
    })
}

async fn replay_cached(
    args: &Args,
    sink: &mut Sink,
    err: &ErrSink,
    conversation: &mut Vec<Message>,
    hit: cache::CachedEntry,
) -> Result<RunOutcome, (u8, String)> {
    let started = Instant::now();
    let (rendered, assistant, continuation) = match hit.prospect {
        Some(prospect) => {
            let rendered = render_cached(
                &prospect,
                hit.events.as_deref(),
                args.simple,
                args.stream,
                should_include_reasoning(args.reasoning_summary, args.stream, args.simple),
            )
            .map_err(|e| (1u8, e))?;
            let assistant = prospect.content.clone();
            let continuation = prospect.provider_continuation.clone();
            (rendered, assistant, continuation)
        }
        None => {
            let assistant = hit.assistant.unwrap_or_else(|| hit.content.clone());
            (hit.content, assistant, None)
        }
    };
    sink.write_str(&rendered)
        .await
        .map_err(|e| (1u8, format!("write output: {}", e)))?;
    sink.finish()
        .await
        .map_err(|e| (1u8, format!("flush output: {}", e)))?;
    if args.stats {
        err.emit(&format_cached_stats(
            &hit.model,
            &hit.usage,
            started.elapsed(),
        ));
    }
    if let Some(p) = &args.stateful {
        push_assistant_turn(conversation, assistant.clone(), continuation.as_ref())
            .map_err(|e| (1u8, e))?;
        save_stateful(p, conversation).await.map_err(|e| (1u8, e))?;
    }
    Ok(RunOutcome {
        exit_code: 0,
        assistant_buf: assistant,
        provider_continuation: continuation,
    })
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

fn api_key_for_base_url(
    explicit_key: Option<&str>,
    env_key: Option<&str>,
    base_url: Option<&str>,
) -> Result<String, String> {
    if let Some(key) = explicit_key.or(env_key) {
        return Ok(key.to_string());
    }
    if !is_openai(base_url) {
        return Ok("mii-text-local".to_string());
    }
    Err("missing API key (--key or OPENAI_API_KEY)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_is_required_for_openai_base_urls() {
        assert!(api_key_for_base_url(None, None, None).is_err());
        assert!(api_key_for_base_url(None, None, Some("https://api.openai.com/v1")).is_err());
        assert_eq!(
            api_key_for_base_url(Some("sk-explicit"), None, None).unwrap(),
            "sk-explicit"
        );
        assert_eq!(
            api_key_for_base_url(None, Some("sk-env"), None).unwrap(),
            "sk-env"
        );
    }

    #[test]
    fn custom_base_urls_can_run_without_a_key() {
        assert_eq!(
            api_key_for_base_url(None, None, Some("http://localhost:8080/v1")).unwrap(),
            "mii-text-local"
        );
        assert_eq!(
            api_key_for_base_url(
                None,
                Some("real-local-key"),
                Some("http://localhost:8080/v1")
            )
            .unwrap(),
            "real-local-key"
        );
    }
}

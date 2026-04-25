use std::env;
use std::process::ExitCode;
use std::time::Instant;

use async_openai::Client;
use async_openai::config::OpenAIConfig;

use crate::args::{Args, DEFAULT_MAX_TOKENS, parse, usage};
use crate::conversation::{Message, load_input_messages, load_stateful, save_stateful};
use crate::providers::{CallParams, call as call_provider};
use crate::sink::Sink;
use crate::stats::{print_stats, print_usage_only};

mod args;
mod cache;
mod conversation;
mod providers;
mod sink;
mod stats;

#[tokio::main]
async fn main() -> ExitCode {
    let parsed = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}\n{}", e, usage());
            return ExitCode::from(1);
        }
    };
    match run(parsed).await {
        Ok(code) => ExitCode::from(code),
        Err((code, msg)) => {
            eprintln!("{}", msg);
            ExitCode::from(code)
        }
    }
}

async fn run(args: Args) -> Result<u8, (u8, String)> {
    let model = args
        .model
        .clone()
        .or_else(|| env::var("OPENAI_MODEL_NAME").ok())
        .ok_or((1u8, "missing model (--model or OPENAI_MODEL_NAME)".to_string()))?;

    let mut conversation: Vec<Message> = match &args.stateful {
        Some(p) => load_stateful(p).await.map_err(|e| (1u8, e))?,
        None => Vec::new(),
    };
    let new_messages = load_input_messages(&args.messages, args.quick)
        .await
        .map_err(|e| (1u8, e))?;
    conversation.extend(new_messages);

    let max_tokens = args.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);
    let key_hash = cache::key(
        &model,
        &args.system,
        &conversation,
        &args.reasoning,
        args.temperature,
        max_tokens,
        args.reasoning_summary,
    );
    let cache_conn = match &args.cache {
        Some(p) => Some(cache::open(p).map_err(|e| (1u8, e))?),
        None => None,
    };

    if let Some(conn) = &cache_conn {
        if let Some(hit) = cache::lookup(conn, &key_hash).map_err(|e| (1u8, e))? {
            return replay_cached(&args, &mut conversation, hit).await;
        }
    }

    let api_key = args
        .key
        .clone()
        .or_else(|| env::var("OPENAI_API_KEY").ok())
        .ok_or((1u8, "missing API key (--key or OPENAI_API_KEY)".to_string()))?;
    let base_url = args
        .url
        .clone()
        .or_else(|| env::var("OPENAI_BASE_URL").ok());

    let mut config = OpenAIConfig::new().with_api_key(api_key);
    if let Some(u) = &base_url {
        config = config.with_api_base(u.clone());
    }
    let client = Client::with_config(config);

    let mut sink = Sink::open(&args.out).await.map_err(|e| (1u8, e))?;
    let started = Instant::now();
    let outcome = call_provider(
        &client,
        &mut sink,
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
            &key_hash,
            &outcome.full_output,
            &outcome.usage,
            &outcome.model_used,
        )
        .map_err(|e| (1u8, e))?;
    }

    if args.stats {
        print_stats(
            &outcome.model_used,
            &outcome.usage,
            total_elapsed,
            outcome.first_token_at,
        );
    }

    if let Some(p) = &args.stateful {
        conversation.push(Message {
            role: "assistant".to_string(),
            content: outcome.assistant_buf,
        });
        save_stateful(p, &conversation).await.map_err(|e| (1u8, e))?;
    }

    Ok(0)
}

async fn replay_cached(
    args: &Args,
    conversation: &mut Vec<Message>,
    hit: cache::CachedEntry,
) -> Result<u8, (u8, String)> {
    let started = Instant::now();
    let mut sink = Sink::open(&args.out).await.map_err(|e| (1u8, e))?;
    sink.write_str(&hit.content)
        .await
        .map_err(|e| (1u8, format!("write output: {}", e)))?;
    sink.finish()
        .await
        .map_err(|e| (1u8, format!("flush output: {}", e)))?;
    if args.stats {
        eprintln!("\n--- stats (cached) ---");
        if let Some(m) = &hit.model {
            eprintln!("model: {}", m);
        }
        eprintln!("latency: {:.3}s", started.elapsed().as_secs_f64());
        if let Some(u) = &hit.usage {
            print_usage_only(u);
        }
    }
    if let Some(p) = &args.stateful {
        conversation.push(Message {
            role: "assistant".to_string(),
            content: hit.content,
        });
        save_stateful(p, conversation).await.map_err(|e| (1u8, e))?;
    }
    Ok(0)
}

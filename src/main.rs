use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use async_openai::Client;
use async_openai::config::OpenAIConfig;
use futures::StreamExt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Debug, Default)]
struct Args {
    key: Option<String>,
    url: Option<String>,
    model: Option<String>,
    stream: bool,
    out: Option<PathBuf>,
    system: Option<String>,
    messages: Option<String>,
    quick: bool,
    stateful: Option<PathBuf>,
    reasoning: Option<String>,
    stats: bool,
    cache: Option<PathBuf>,
}

fn usage() -> &'static str {
    "usage: mii-text [--key <s>] [--url <s>] [--model <s>] [--stream] [--out <path>]\n\
                    [--system <s>] [--messages <json>] [--quick] [--stateful <path>]\n\
                    [--reasoning <none|low|medium|high|xhigh>] [--stats] [--cache <path>]"
}

fn map_reasoning(level: &str) -> Result<&'static str, String> {
    match level {
        "none" => Ok("minimal"),
        "low" => Ok("low"),
        "medium" => Ok("medium"),
        "high" => Ok("high"),
        "xhigh" => Ok("high"),
        other => Err(format!(
            "invalid --reasoning value '{}': expected none|low|medium|high|xhigh",
            other
        )),
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = env::args().skip(1);
    fn need(
        it: &mut std::iter::Skip<env::Args>,
        flag: &str,
    ) -> Result<String, String> {
        it.next().ok_or_else(|| format!("missing value for {}", flag))
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "--key" => args.key = Some(need(&mut it, "--key")?),
            "--url" => args.url = Some(need(&mut it, "--url")?),
            "--model" => args.model = Some(need(&mut it, "--model")?),
            "--stream" => args.stream = true,
            "--out" => args.out = Some(PathBuf::from(need(&mut it, "--out")?)),
            "--system" => args.system = Some(need(&mut it, "--system")?),
            "--messages" => args.messages = Some(need(&mut it, "--messages")?),
            "--quick" => args.quick = true,
            "--stateful" => args.stateful = Some(PathBuf::from(need(&mut it, "--stateful")?)),
            "--reasoning" => args.reasoning = Some(need(&mut it, "--reasoning")?),
            "--stats" => args.stats = true,
            "--cache" => args.cache = Some(PathBuf::from(need(&mut it, "--cache")?)),
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {}", other)),
        }
    }
    Ok(args)
}

async fn read_stdin_to_string() -> std::io::Result<String> {
    let mut buf = String::new();
    tokio::io::stdin().read_to_string(&mut buf).await?;
    Ok(buf)
}

async fn load_input_messages(args: &Args) -> Result<Vec<Message>, String> {
    let raw = match &args.messages {
        Some(s) => s.clone(),
        None => read_stdin_to_string()
            .await
            .map_err(|e| format!("failed to read stdin: {}", e))?,
    };

    if args.quick {
        let content = raw.trim().to_string();
        if content.is_empty() {
            return Err("quick mode requires non-empty input".to_string());
        }
        return Ok(vec![Message {
            role: "user".to_string(),
            content,
        }]);
    }

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("no input messages provided".to_string());
    }
    serde_json::from_str::<Vec<Message>>(trimmed)
        .map_err(|e| format!("failed to parse messages json: {}", e))
}

async fn load_stateful(path: &PathBuf) -> Result<Vec<Message>, String> {
    if !fs::try_exists(path).await.unwrap_or(false) {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)
        .await
        .map_err(|e| format!("failed to read stateful file: {}", e))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<Vec<Message>>(trimmed)
        .map_err(|e| format!("failed to parse stateful file json: {}", e))
}

async fn save_stateful(path: &PathBuf, msgs: &[Message]) -> Result<(), String> {
    let serialized =
        serde_json::to_string_pretty(msgs).map_err(|e| format!("serialize stateful: {}", e))?;
    fs::write(path, serialized)
        .await
        .map_err(|e| format!("write stateful file: {}", e))
}

fn build_request_messages(system: &Option<String>, msgs: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(msgs.len() + 1);
    if let Some(sys) = system {
        out.push(json!({ "role": "system", "content": sys }));
    }
    for m in msgs {
        out.push(json!({ "role": m.role, "content": m.content }));
    }
    out
}

/// Computes the cache key. Deliberately excludes secrets (api key) and
/// transport-only fields (base URL, --stream, --out, --stateful, --stats)
/// since they don't affect the model's output.
fn cache_key(
    model: &str,
    system: &Option<String>,
    messages: &[Message],
    reasoning: &Option<String>,
) -> String {
    let canonical = json!({
        "v": 1,
        "model": model,
        "system": system,
        "messages": messages,
        "reasoning": reasoning,
    });
    let serialized = serde_json::to_vec(&canonical).expect("canonical json");
    let mut hasher = Sha256::new();
    hasher.update(&serialized);
    let bytes = hasher.finalize();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{:02x}", b);
    }
    hex
}

struct CachedEntry {
    content: String,
    usage: Option<Value>,
    model: Option<String>,
}

fn cache_open(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create cache dir: {}", e))?;
        }
    }
    let conn = Connection::open(path).map_err(|e| format!("open cache db: {}", e))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS responses (\n             key TEXT PRIMARY KEY,\n             content TEXT NOT NULL,\n             usage TEXT,\n             model TEXT,\n             created_at INTEGER NOT NULL\n         );",
    )
    .map_err(|e| format!("init cache schema: {}", e))?;
    Ok(conn)
}

fn cache_lookup(conn: &Connection, key: &str) -> Result<Option<CachedEntry>, String> {
    let row = conn
        .query_row(
            "SELECT content, usage, model FROM responses WHERE key = ?1",
            params![key],
            |r| {
                let content: String = r.get(0)?;
                let usage: Option<String> = r.get(1)?;
                let model: Option<String> = r.get(2)?;
                Ok((content, usage, model))
            },
        )
        .optional()
        .map_err(|e| format!("cache lookup: {}", e))?;
    match row {
        None => Ok(None),
        Some((content, usage_str, model)) => {
            let usage = match usage_str {
                Some(s) => serde_json::from_str(&s).ok(),
                None => None,
            };
            Ok(Some(CachedEntry { content, usage, model }))
        }
    }
}

fn cache_store(
    conn: &Connection,
    key: &str,
    content: &str,
    usage: &Option<Value>,
    model: &Option<String>,
) -> Result<(), String> {
    let usage_str = usage
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT OR REPLACE INTO responses (key, content, usage, model, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![key, content, usage_str, model, now],
    )
    .map_err(|e| format!("cache store: {}", e))?;
    Ok(())
}

enum Sink {
    Stdout(tokio::io::Stdout),
    File(tokio::fs::File),
}

impl Sink {
    async fn open(out: &Option<PathBuf>) -> Result<Self, String> {
        match out {
            Some(p) => {
                let f = tokio::fs::File::create(p)
                    .await
                    .map_err(|e| format!("open output file: {}", e))?;
                Ok(Sink::File(f))
            }
            None => Ok(Sink::Stdout(tokio::io::stdout())),
        }
    }

    async fn write_str(&mut self, s: &str) -> std::io::Result<()> {
        match self {
            Sink::Stdout(o) => {
                o.write_all(s.as_bytes()).await?;
                o.flush().await
            }
            Sink::File(f) => f.write_all(s.as_bytes()).await,
        }
    }

    async fn finish(&mut self) -> std::io::Result<()> {
        match self {
            Sink::Stdout(o) => o.flush().await,
            Sink::File(f) => f.flush().await,
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

    let new_messages = load_input_messages(&args).await.map_err(|e| (1u8, e))?;
    conversation.extend(new_messages);

    let key_hash = cache_key(&model, &args.system, &conversation, &args.reasoning);
    let cache_conn = match &args.cache {
        Some(p) => Some(cache_open(p).map_err(|e| (1u8, e))?),
        None => None,
    };

    // Try cache hit first (no API key required for hits).
    if let Some(conn) = &cache_conn {
        if let Some(hit) = cache_lookup(conn, &key_hash).map_err(|e| (1u8, e))? {
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
                save_stateful(p, &conversation).await.map_err(|e| (1u8, e))?;
            }
            return Ok(0);
        }
    }

    // Cache miss → real API call.
    let key = args
        .key
        .clone()
        .or_else(|| env::var("OPENAI_API_KEY").ok())
        .ok_or((1u8, "missing API key (--key or OPENAI_API_KEY)".to_string()))?;
    let base_url = args
        .url
        .clone()
        .or_else(|| env::var("OPENAI_BASE_URL").ok());

    let mut config = OpenAIConfig::new().with_api_key(key);
    if let Some(u) = base_url {
        config = config.with_api_base(u);
    }
    let client = Client::with_config(config);

    let req_messages = build_request_messages(&args.system, &conversation);
    let mut body = json!({
        "model": model,
        "messages": req_messages,
        "stream": args.stream,
    });
    if let Some(level) = &args.reasoning {
        let mapped = map_reasoning(level).map_err(|e| (1u8, e))?;
        body["reasoning_effort"] = Value::String(mapped.to_string());
    }
    if args.stream && args.stats {
        body["stream_options"] = json!({ "include_usage": true });
    }

    let mut sink = Sink::open(&args.out).await.map_err(|e| (1u8, e))?;
    let mut assistant_buf = String::new();
    let mut usage: Option<Value> = None;
    let mut model_used: Option<String> = None;
    let started = Instant::now();
    let mut first_token_at: Option<std::time::Duration> = None;

    if args.stream {
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
            if let Some(delta) = chunk
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
            {
                if !delta.is_empty() {
                    if first_token_at.is_none() {
                        first_token_at = Some(started.elapsed());
                    }
                    assistant_buf.push_str(delta);
                    sink.write_str(delta)
                        .await
                        .map_err(|e| (1u8, format!("write output: {}", e)))?;
                }
            }
        }
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
        let content = resp
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        assistant_buf.push_str(content);
        sink.write_str(content)
            .await
            .map_err(|e| (1u8, format!("write output: {}", e)))?;
    }

    let total_elapsed = started.elapsed();

    sink.finish()
        .await
        .map_err(|e| (1u8, format!("flush output: {}", e)))?;

    if let Some(conn) = &cache_conn {
        cache_store(conn, &key_hash, &assistant_buf, &usage, &model_used)
            .map_err(|e| (1u8, e))?;
    }

    if args.stats {
        print_stats(&model_used, &usage, total_elapsed, first_token_at);
    }

    if let Some(p) = &args.stateful {
        conversation.push(Message {
            role: "assistant".to_string(),
            content: assistant_buf,
        });
        save_stateful(p, &conversation).await.map_err(|e| (1u8, e))?;
    }

    Ok(0)
}

fn print_stats(
    model: &Option<String>,
    usage: &Option<Value>,
    total: std::time::Duration,
    first_token: Option<std::time::Duration>,
) {
    eprintln!("\n--- stats ---");
    if let Some(m) = model {
        eprintln!("model: {}", m);
    }
    eprintln!("latency: {:.3}s", total.as_secs_f64());
    if let Some(ft) = first_token {
        eprintln!("time to first token: {:.3}s", ft.as_secs_f64());
    }
    match usage {
        Some(u) => {
            print_usage_only(u);
            let completion = u.get("completion_tokens").and_then(|v| v.as_u64());
            let secs = total.as_secs_f64();
            if let Some(c) = completion {
                if secs > 0.0 {
                    eprintln!("throughput: {:.1} tok/s", c as f64 / secs);
                }
            }
        }
        None => {
            eprintln!("usage: <not reported by server>");
        }
    }
}

fn print_usage_only(u: &Value) {
    let prompt = u.get("prompt_tokens").and_then(|v| v.as_u64());
    let completion = u.get("completion_tokens").and_then(|v| v.as_u64());
    let total_tok = u.get("total_tokens").and_then(|v| v.as_u64());
    let reasoning = u
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_u64());
    if let Some(p) = prompt {
        eprintln!("prompt tokens: {}", p);
    }
    if let Some(c) = completion {
        eprintln!("completion tokens: {}", c);
    }
    if let Some(r) = reasoning {
        eprintln!("  reasoning tokens: {}", r);
    }
    if let Some(t) = total_tok {
        eprintln!("total tokens: {}", t);
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}\n{}", e, usage());
            return ExitCode::from(1);
        }
    };

    match run(args).await {
        Ok(code) => ExitCode::from(code),
        Err((code, msg)) => {
            eprintln!("{}", msg);
            ExitCode::from(code)
        }
    }
}

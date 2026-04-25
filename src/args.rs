use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_TOKENS: u32 = 128_000;

/// Returns the default IPC socket path. Prefers `$XDG_RUNTIME_DIR/mii-text.sock`
/// (per-user, tmpfs, mode 0700) and falls back to `/tmp/mii-text.sock` when
/// the environment variable is unset.
pub fn default_ipc_socket() -> PathBuf {
    match env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("mii-text.sock"),
        _ => PathBuf::from("/tmp/mii-text.sock"),
    }
}

/// Subset of `Args` that may be sent over IPC from a client to a `--serve`
/// instance. Excludes secrets (`key`, `url`) and mode flags (`serve`, `ipc`).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ClientArgs {
    pub model: Option<String>,
    pub stream: bool,
    pub out: Option<PathBuf>,
    pub system: Option<String>,
    pub messages: Option<String>,
    pub quick: bool,
    pub stateful: Option<PathBuf>,
    pub reasoning: Option<String>,
    pub stats: bool,
    pub cache: Option<PathBuf>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub reasoning_summary: bool,
}

#[derive(Debug, Default)]
pub struct Args {
    pub key: Option<String>,
    pub url: Option<String>,
    pub model: Option<String>,
    pub stream: bool,
    pub out: Option<PathBuf>,
    pub system: Option<String>,
    pub messages: Option<String>,
    pub quick: bool,
    pub stateful: Option<PathBuf>,
    pub reasoning: Option<String>,
    pub stats: bool,
    pub cache: Option<PathBuf>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub reasoning_summary: bool,
    pub serve: bool,
    pub ipc: bool,
    pub ipc_path: Option<PathBuf>,
    pub status: bool,
    pub quiet: bool,
}

impl Args {
    /// Returns the IPC-shareable subset of these args.
    pub fn to_client(&self) -> ClientArgs {
        ClientArgs {
            model: self.model.clone(),
            stream: self.stream,
            out: self.out.clone(),
            system: self.system.clone(),
            messages: self.messages.clone(),
            quick: self.quick,
            stateful: self.stateful.clone(),
            reasoning: self.reasoning.clone(),
            stats: self.stats,
            cache: self.cache.clone(),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            reasoning_summary: self.reasoning_summary,
        }
    }

    /// Merges a client's args on top of the server's defaults. Client
    /// `Option<T>` values override when `Some`; client booleans override when
    /// `true` (bools cannot be unset by the client).
    pub fn merge_client(&mut self, c: ClientArgs) {
        if c.model.is_some() {
            self.model = c.model;
        }
        if c.stream {
            self.stream = true;
        }
        if c.out.is_some() {
            self.out = c.out;
        }
        if c.system.is_some() {
            self.system = c.system;
        }
        if c.messages.is_some() {
            self.messages = c.messages;
        }
        if c.quick {
            self.quick = true;
        }
        if c.stateful.is_some() {
            self.stateful = c.stateful;
        }
        if c.reasoning.is_some() {
            self.reasoning = c.reasoning;
        }
        if c.stats {
            self.stats = true;
        }
        if c.cache.is_some() {
            self.cache = c.cache;
        }
        if c.temperature.is_some() {
            self.temperature = c.temperature;
        }
        if c.max_tokens.is_some() {
            self.max_tokens = c.max_tokens;
        }
        if c.reasoning_summary {
            self.reasoning_summary = true;
        }
    }
}

pub fn usage() -> &'static str {
    "usage: mii-text [--key <s>] [--url <s>] [--model <s>] [--stream] [--out <path>]\n\
                    [--system <s>] [--messages <json>] [--quick] [--stateful <path>]\n\
                    [--reasoning <none|low|medium|high|xhigh>] [--stats] [--cache <path>]\n\
                    [--temperature <float>] [--max-tokens <int>] [--reasoning-summary]\n\
                    [--serve] [--ipc [<path>]] [--status] [--quiet]"
}

/// Validates the user-facing reasoning level and returns it unchanged for use
/// as the `reasoning_effort` (chat) / `reasoning.effort` (responses) value.
pub fn map_reasoning(level: &str) -> Result<&'static str, String> {
    match level {
        "none" => Ok("none"),
        "low" => Ok("low"),
        "medium" => Ok("medium"),
        "high" => Ok("high"),
        "xhigh" => Ok("xhigh"),
        other => Err(format!(
            "invalid --reasoning value '{}': expected none|low|medium|high|xhigh",
            other
        )),
    }
}

pub fn parse() -> Result<Args, String> {
    parse_from(env::args().skip(1).collect())
}

fn parse_from(tokens: Vec<String>) -> Result<Args, String> {
    let mut args = Args::default();
    let mut i = 0;
    fn need(i: &mut usize, tokens: &[String], flag: &str) -> Result<String, String> {
        *i += 1;
        tokens
            .get(*i)
            .cloned()
            .ok_or_else(|| format!("missing value for {}", flag))
    }
    while i < tokens.len() {
        let a = tokens[i].clone();
        match a.as_str() {
            "--key" => args.key = Some(need(&mut i, &tokens, "--key")?),
            "--url" => args.url = Some(need(&mut i, &tokens, "--url")?),
            "--model" => args.model = Some(need(&mut i, &tokens, "--model")?),
            "--stream" => args.stream = true,
            "--out" => args.out = Some(PathBuf::from(need(&mut i, &tokens, "--out")?)),
            "--system" => args.system = Some(need(&mut i, &tokens, "--system")?),
            "--messages" => args.messages = Some(need(&mut i, &tokens, "--messages")?),
            "--quick" => args.quick = true,
            "--stateful" => {
                args.stateful = Some(PathBuf::from(need(&mut i, &tokens, "--stateful")?))
            }
            "--reasoning" => args.reasoning = Some(need(&mut i, &tokens, "--reasoning")?),
            "--stats" => args.stats = true,
            "--cache" => args.cache = Some(PathBuf::from(need(&mut i, &tokens, "--cache")?)),
            "--temperature" => {
                let v = need(&mut i, &tokens, "--temperature")?;
                args.temperature = Some(v.parse::<f32>().map_err(|_| {
                    format!("invalid --temperature value '{}': expected float", v)
                })?);
            }
            "--max-tokens" => {
                let v = need(&mut i, &tokens, "--max-tokens")?;
                args.max_tokens = Some(v.parse::<u32>().map_err(|_| {
                    format!("invalid --max-tokens value '{}': expected positive integer", v)
                })?);
            }
            "--reasoning-summary" => args.reasoning_summary = true,
            "--quiet" => args.quiet = true,
            "--status" => args.status = true,
            "--serve" => args.serve = true,
            "--ipc" => {
                args.ipc = true;
                if let Some(next) = tokens.get(i + 1) {
                    if !next.starts_with('-') {
                        i += 1;
                        args.ipc_path = Some(PathBuf::from(next));
                    }
                }
            }
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {}", other)),
        }
        i += 1;
    }
    Ok(args)
}

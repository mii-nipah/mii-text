use std::env;
use std::ffi::OsStr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::tools::ToolSource;

pub const DEFAULT_MAX_TOKENS: u32 = 128_000;

/// Returns the default IPC socket path. Prefers `$XDG_RUNTIME_DIR/mii-text.sock`
/// (per-user, tmpfs, mode 0700) and falls back to `/tmp/mii-text.sock` when
/// the environment variable is unset.
pub fn default_ipc_socket() -> PathBuf {
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR");
    default_ipc_socket_from(runtime_dir.as_deref())
}

fn default_ipc_socket_from(runtime_dir: Option<&OsStr>) -> PathBuf {
    match runtime_dir {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("mii-text.sock"),
        _ => PathBuf::from("/tmp/mii-text.sock"),
    }
}

/// Subset of `Args` that may be sent over IPC from a client to a `--serve`
/// instance. Excludes secrets (`key`, `url`) and mode flags (`serve`, `ipc`).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
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
    pub tools: Vec<ToolSource>,
    pub completions: bool,
    pub simple: bool,
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
    pub tools: Vec<ToolSource>,
    pub completions: bool,
    pub simple: bool,
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
            tools: self.tools.clone(),
            completions: self.completions,
            simple: self.simple,
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
        if !c.tools.is_empty() {
            self.tools = c.tools;
        }
        if c.completions {
            self.completions = true;
        }
        if c.simple {
            self.simple = true;
        }
    }
}

pub fn usage() -> &'static str {
    "usage: mii-text [--key <s>] [--url <s>] [--model <s>] [--stream] [--out <path>]\n\
                    [--system <s>] [--messages <json>] [--quick] [--stateful <path>]\n\
                    [--reasoning <none|low|medium|high|xhigh>] [--stats] [--cache <path>]\n\
                    [--temperature <float>] [--max-tokens <int>] [--reasoning-summary]\n\
                    [--tool <json>] [--tools <path>] [--completions] [--simple]\n\
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
    let mut steps = 0usize;
    let max_steps = tokens.len().saturating_mul(2).saturating_add(1);
    fn need(i: &mut usize, tokens: &[String], flag: &str) -> Result<String, String> {
        *i += 1;
        tokens
            .get(*i)
            .cloned()
            .ok_or_else(|| format!("missing value for {}", flag))
    }
    while i < tokens.len() {
        steps += 1;
        if steps > max_steps {
            return Err("argument parser made no progress".to_string());
        }
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
                args.temperature =
                    Some(v.parse::<f32>().map_err(|_| {
                        format!("invalid --temperature value '{}': expected float", v)
                    })?);
            }
            "--max-tokens" => {
                let v = need(&mut i, &tokens, "--max-tokens")?;
                args.max_tokens = Some(v.parse::<u32>().map_err(|_| {
                    format!(
                        "invalid --max-tokens value '{}': expected positive integer",
                        v
                    )
                })?);
            }
            "--reasoning-summary" => args.reasoning_summary = true,
            "--tool" => args
                .tools
                .push(ToolSource::Inline(need(&mut i, &tokens, "--tool")?)),
            "--tools" => args.tools.push(ToolSource::File(PathBuf::from(need(
                &mut i, &tokens, "--tools",
            )?))),
            "--completions" => args.completions = true,
            "--simple" => args.simple = true,
            "--quiet" => args.quiet = true,
            "--status" => args.status = true,
            "--serve" => args.serve = true,
            "--ipc" => {
                args.ipc = true;
                if let Some(next) = tokens.get(i + 1)
                    && !next.starts_with('-')
                {
                    i += 1;
                    args.ipc_path = Some(PathBuf::from(next));
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

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::PathBuf;

    use super::*;

    fn parse_tokens(tokens: &[&str]) -> Result<Args, String> {
        parse_from(tokens.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn parses_generation_provider_output_and_tool_flags() {
        let args = parse_tokens(&[
            "--key",
            "sk-test",
            "--url",
            "https://example.test/v1",
            "--model",
            "model-a",
            "--stream",
            "--out",
            "out.txt",
            "--system",
            "sys",
            "--messages",
            "[]",
            "--quick",
            "--stateful",
            "state.json",
            "--reasoning",
            "high",
            "--stats",
            "--cache",
            "cache.db",
            "--temperature",
            "0.25",
            "--max-tokens",
            "42",
            "--reasoning-summary",
            "--tool",
            "{\"name\":\"echo\",\"input_schema\":{\"type\":\"object\"}}",
            "--tools",
            "tools.json",
            "--completions",
            "--simple",
            "--serve",
            "--ipc",
            "sock",
            "--status",
            "--quiet",
        ])
        .unwrap();

        assert_eq!(args.key.as_deref(), Some("sk-test"));
        assert_eq!(args.url.as_deref(), Some("https://example.test/v1"));
        assert_eq!(args.model.as_deref(), Some("model-a"));
        assert!(args.stream);
        assert_eq!(
            args.out.as_deref(),
            Some(PathBuf::from("out.txt").as_path())
        );
        assert_eq!(args.system.as_deref(), Some("sys"));
        assert_eq!(args.messages.as_deref(), Some("[]"));
        assert!(args.quick);
        assert_eq!(
            args.stateful.as_deref(),
            Some(PathBuf::from("state.json").as_path())
        );
        assert_eq!(args.reasoning.as_deref(), Some("high"));
        assert!(args.stats);
        assert_eq!(
            args.cache.as_deref(),
            Some(PathBuf::from("cache.db").as_path())
        );
        assert_eq!(args.temperature, Some(0.25));
        assert_eq!(args.max_tokens, Some(42));
        assert!(args.reasoning_summary);
        assert_eq!(args.tools.len(), 2);
        assert!(matches!(args.tools[0], ToolSource::Inline(_)));
        assert!(matches!(args.tools[1], ToolSource::File(_)));
        assert!(args.completions);
        assert!(args.simple);
        assert!(args.serve);
        assert!(args.ipc);
        assert_eq!(
            args.ipc_path.as_deref(),
            Some(PathBuf::from("sock").as_path())
        );
        assert!(args.status);
        assert!(args.quiet);
    }

    #[test]
    fn default_ipc_socket_uses_runtime_dir_when_present_and_tmp_otherwise() {
        assert_eq!(
            default_ipc_socket_from(Some(OsStr::new("/run/user/test"))),
            PathBuf::from("/run/user/test").join("mii-text.sock")
        );
        assert_eq!(
            default_ipc_socket_from(Some(OsStr::new(""))),
            PathBuf::from("/tmp/mii-text.sock")
        );
        assert_eq!(
            default_ipc_socket_from(None),
            PathBuf::from("/tmp/mii-text.sock")
        );

        let expected = match env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("mii-text.sock"),
            _ => PathBuf::from("/tmp/mii-text.sock"),
        };
        assert_eq!(default_ipc_socket(), expected);
    }

    #[test]
    fn ipc_without_path_does_not_consume_the_next_flag() {
        let args = parse_tokens(&["--ipc", "--status"]).unwrap();

        assert!(args.ipc);
        assert!(args.status);
        assert_eq!(args.ipc_path, None);
    }

    #[test]
    fn parse_rejects_missing_values_unknown_flags_and_bad_numbers() {
        assert_eq!(
            parse_tokens(&["--model"]).unwrap_err(),
            "missing value for --model"
        );
        assert_eq!(
            parse_tokens(&["--temperature", "warm"]).unwrap_err(),
            "invalid --temperature value 'warm': expected float"
        );
        assert_eq!(
            parse_tokens(&["--max-tokens", "many"]).unwrap_err(),
            "invalid --max-tokens value 'many': expected positive integer"
        );
        assert_eq!(
            parse_tokens(&["--surprise"]).unwrap_err(),
            "unknown argument: --surprise"
        );
    }

    #[test]
    fn maps_reasoning_levels_and_rejects_unknown_values() {
        for level in ["none", "low", "medium", "high", "xhigh"] {
            assert_eq!(map_reasoning(level).unwrap(), level);
        }

        assert_eq!(
            map_reasoning("maximum").unwrap_err(),
            "invalid --reasoning value 'maximum': expected none|low|medium|high|xhigh"
        );
    }

    #[test]
    fn to_client_carries_every_shareable_field() {
        let args = parse_tokens(&[
            "--model",
            "model-a",
            "--stream",
            "--out",
            "out.txt",
            "--system",
            "sys",
            "--messages",
            "[]",
            "--quick",
            "--stateful",
            "state.json",
            "--reasoning",
            "low",
            "--stats",
            "--cache",
            "cache.db",
            "--temperature",
            "0.5",
            "--max-tokens",
            "99",
            "--reasoning-summary",
            "--tool",
            "{\"name\":\"echo\",\"input_schema\":{\"type\":\"object\"}}",
            "--completions",
            "--simple",
        ])
        .unwrap();

        let client = args.to_client();

        assert_eq!(client.model.as_deref(), Some("model-a"));
        assert!(client.stream);
        assert_eq!(
            client.out.as_deref(),
            Some(PathBuf::from("out.txt").as_path())
        );
        assert_eq!(client.system.as_deref(), Some("sys"));
        assert_eq!(client.messages.as_deref(), Some("[]"));
        assert!(client.quick);
        assert_eq!(
            client.stateful.as_deref(),
            Some(PathBuf::from("state.json").as_path())
        );
        assert_eq!(client.reasoning.as_deref(), Some("low"));
        assert!(client.stats);
        assert_eq!(
            client.cache.as_deref(),
            Some(PathBuf::from("cache.db").as_path())
        );
        assert_eq!(client.temperature, Some(0.5));
        assert_eq!(client.max_tokens, Some(99));
        assert!(client.reasoning_summary);
        assert_eq!(client.tools.len(), 1);
        assert!(client.completions);
        assert!(client.simple);
    }

    #[test]
    fn merge_client_only_enables_booleans_and_replaces_client_tools() {
        let mut server = parse_tokens(&[
            "--model",
            "server-model",
            "--stream",
            "--system",
            "server-system",
            "--tool",
            "{\"name\":\"server\",\"input_schema\":{\"type\":\"object\"}}",
            "--simple",
        ])
        .unwrap();

        server.merge_client(ClientArgs {
            model: Some("client-model".to_string()),
            stream: false,
            system: None,
            messages: Some("[]".to_string()),
            tools: vec![ToolSource::Inline(
                "{\"name\":\"client\",\"input_schema\":{\"type\":\"object\"}}".to_string(),
            )],
            completions: true,
            simple: false,
            ..ClientArgs::default()
        });

        assert_eq!(server.model.as_deref(), Some("client-model"));
        assert!(server.stream);
        assert_eq!(server.system.as_deref(), Some("server-system"));
        assert_eq!(server.messages.as_deref(), Some("[]"));
        assert_eq!(server.tools.len(), 1);
        match &server.tools[0] {
            ToolSource::Inline(raw) => assert!(raw.contains("client")),
            other => panic!("unexpected tool source: {other:?}"),
        }
        assert!(server.completions);
        assert!(server.simple);
    }

    #[test]
    fn usage_mentions_the_structural_output_flags() {
        let text = usage();

        assert!(text.contains("--tool <json>"));
        assert!(text.contains("--tools <path>"));
        assert!(text.contains("--completions"));
        assert!(text.contains("--simple"));
    }
}

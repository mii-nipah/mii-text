use std::env;
use std::path::PathBuf;

pub const DEFAULT_MAX_TOKENS: u32 = 128_000;

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
}

pub fn usage() -> &'static str {
    "usage: mii-text [--key <s>] [--url <s>] [--model <s>] [--stream] [--out <path>]\n\
                    [--system <s>] [--messages <json>] [--quick] [--stateful <path>]\n\
                    [--reasoning <none|low|medium|high|xhigh>] [--stats] [--cache <path>]\n\
                    [--temperature <float>] [--max-tokens <int>] [--reasoning-summary]"
}

/// Maps the user-facing reasoning level to the OpenAI `reasoning_effort` value.
pub fn map_reasoning(level: &str) -> Result<&'static str, String> {
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

pub fn parse() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = env::args().skip(1);
    fn need(it: &mut std::iter::Skip<env::Args>, flag: &str) -> Result<String, String> {
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
            "--temperature" => {
                let v = need(&mut it, "--temperature")?;
                args.temperature = Some(
                    v.parse::<f32>()
                        .map_err(|_| format!("invalid --temperature value '{}': expected float", v))?,
                );
            }
            "--max-tokens" => {
                let v = need(&mut it, "--max-tokens")?;
                args.max_tokens = Some(v.parse::<u32>().map_err(|_| {
                    format!("invalid --max-tokens value '{}': expected positive integer", v)
                })?);
            }
            "--reasoning-summary" => args.reasoning_summary = true,
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {}", other)),
        }
    }
    Ok(args)
}

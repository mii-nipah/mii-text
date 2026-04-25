# mii-text

A small, unix-friendly CLI for talking to OpenAI-compatible LLM APIs. Pipe text in, get text out, compose it with the rest of your shell.

```bash
echo 'the capital of France is...' | mii-text --quick --model gpt-5-mini
```

It is stateless by default, with an opt-in "stateful illusion" for continuous conversations, an optional SQLite response cache, and an IPC server mode so other processes can use a single warm configuration.

---

## Index

- [Features](#features)
- [Quick start](#quick-start)
- [Usage](#usage)
  - [Inputs](#inputs)
  - [Generation knobs](#generation-knobs)
  - [Output and stats](#output-and-stats)
  - [Stateful conversations](#stateful-conversations)
  - [Cache](#cache)
- [Server / client mode](#server--client-mode)
- [Environment variables](#environment-variables)
- [Exit codes](#exit-codes)
- [Architecture](#architecture)
- [Contributing](#contributing)

---

## Features

- Stateless by design: every invocation is self-contained, scriptable, and pipeable.
- Optional stateful file: keeps conversation history in a JSON file you can read, edit, or pipe back in.
- Streaming output (`--stream`) for incremental responses.
- Reasoning controls (`--reasoning`, `--reasoning-summary`) for models that support them.
- SQLite response cache (`--cache`) keyed on the full request shape.
- IPC server (`--serve`) and client (`--ipc`) over a Unix domain socket so secrets and defaults live in one place.
- Single static binary — only `tokio`, `async-openai`, `rusqlite`, and `interprocess` under the hood.

## Quick start

Install:

```bash
cargo install --locked mii-text
```

Build:

```bash
cargo build --release
# binary at ./target/release/mii-text
```

One-shot prompt:

```bash
export OPENAI_API_KEY=sk-...
export OPENAI_MODEL_NAME=gpt-5-mini

echo 'write a haiku about cargo' | mii-text --quick --stream
```

Pass an explicit message list instead of stdin:

```bash
mii-text --messages '[
  {"role":"user","content":"hi"},
  {"role":"assistant","content":"hello!"},
  {"role":"user","content":"what did i just say?"}
]'
```

Continue a conversation across invocations:

```bash
echo 'remember the number 42' | mii-text --quick --stateful chat.json
echo 'what number?'           | mii-text --quick --stateful chat.json
```

## Usage

### Inputs

- `--quick` — read stdin (or `--messages`) as a single user message string.
- `--messages <json>` — explicit message list:
  ```json
  [{"role": "user|assistant", "content": "text"}]
  ```
  When omitted, the same JSON is read from stdin.
- `--system <string>` — system prompt prepended to the conversation.

### Generation knobs

- `--model <string>` — model name (or `OPENAI_MODEL_NAME`).
- `--temperature <float>`
- `--max-tokens <int>` — defaults to `128000`.
- `--reasoning <none|low|medium|high|xhigh>` — silently ignored by models that don't support it.
- `--reasoning-summary` — emits the model's reasoning summary wrapped in `<think>…</think>` tags before the answer. The summary is **not** stored in the stateful conversation history.
- `--stream` — stream tokens as they arrive.

### Output and stats

- `--out <path>` — write the response to a file instead of stdout.
- `--stats` — print token counts, latency, and time-to-first-token to stderr after completion. In `--serve` mode this enables stats logging on the server, not on clients.

### Stateful conversations

`--stateful <path>` keeps a JSON file of the conversation messages on disk. Each invocation:

1. Loads the file (if it exists) as the prior history.
2. Appends new messages from `--messages` / stdin / `--quick`.
3. Sends the whole thing to the model.
4. Appends the assistant reply and writes the file back.

The file format is the same as `--messages`, so you can hand-edit it or pipe it through other tooling.

### Cache

`--cache <path>` opens (or creates) a SQLite database. The cache key is a hash of model, system prompt, conversation, reasoning level, temperature, max tokens, and reasoning-summary flag. Cache hits replay the stored output without contacting the API.

## Server / client mode

`--serve` runs a long-lived process holding the API key, base URL, and any default flags. Other invocations attach via `--ipc` and inherit those defaults; client-supplied flags override them.

```bash
# server
OPENAI_API_KEY=$KEY mii-text --serve \
  --model gpt-5-mini \
  --reasoning xhigh \
  --cache /tmp/mii-text.db

# client (anywhere on the same machine)
echo 'the capital of France is...' | mii-text --ipc --quick --reasoning low
```

The server listens on `$XDG_RUNTIME_DIR/mii-text.sock` by default, falling back to `/tmp/mii-text.sock`. Override with `--ipc <path>` on either side.

Useful client commands:

- `mii-text --ipc --status` — check server liveness and basic info (pid, uptime).
- `--quiet` (server side) — suppress per-request server logs.

Override semantics:

- `Option<T>` flags: client value wins when present, else server's value is used.
- Boolean flags: clients can only enable, not disable, server-set flags.
- Secrets (`--key`, `--url`) are server-only and never travel over the socket.

## Environment variables

| Variable            | Equivalent flag |
| ------------------- | --------------- |
| `OPENAI_API_KEY`    | `--key`         |
| `OPENAI_BASE_URL`   | `--url`         |
| `OPENAI_MODEL_NAME` | `--model`       |
| `XDG_RUNTIME_DIR`   | IPC socket dir  |

`--url` makes mii-text usable with any OpenAI-compatible endpoint (local llama.cpp servers, OpenRouter, Groq, etc.).

## Exit codes

| Code | Meaning                                    |
| ---- | ------------------------------------------ |
| 0    | Success                                    |
| 1    | Invalid arguments (details on stderr)      |
| 2    | API error (details on stderr)              |

## Architecture

```
                    ┌──────────────────┐
   stdin / args ──▶ │  args + input    │
                    │  parsing         │
                    └────────┬─────────┘
                             │
                    ┌────────▼─────────┐         ┌──────────────┐
                    │  cache lookup    │ ──hit──▶│  replay      │
                    └────────┬─────────┘         └──────────────┘
                             │ miss
                    ┌────────▼─────────┐
                    │  provider call   │  chat completions
                    │  (async-openai)  │  or responses API
                    └────────┬─────────┘
                             │
                    ┌────────▼─────────┐
                    │  sink (stdout    │
                    │  or file) +      │
                    │  optional cache  │
                    │  store + stats   │
                    └──────────────────┘
```

Source layout (`src/`):

- [main.rs](src/main.rs) — entry point and shared `run` pipeline used by local, server, and IPC paths.
- [args.rs](src/args.rs) — argument parsing, `Args` / `ClientArgs`, server↔client merge rules.
- [conversation.rs](src/conversation.rs) — message types, stateful file I/O, stdin/JSON loading.
- [providers/](src/providers/) — chat completions and responses API adapters behind a single `call` entry point.
- [cache.rs](src/cache.rs) — SQLite cache (`bundled` rusqlite, no system dep).
- [sink.rs](src/sink.rs) — stdout/file output with streaming flush.
- [stats.rs](src/stats.rs) — formatted token / latency reports.
- [server.rs](src/server.rs), [client.rs](src/client.rs), [ipc.rs](src/ipc.rs) — UDS server, client connector, and shared framing.

## Contributing

Issues and pull requests are welcome.

- Run `cargo fmt` and `cargo clippy --all-targets` before submitting.
- Keep the CLI surface stable; if you add a flag, mirror it in `ClientArgs` and the server↔client merge logic.
- Prefer small, focused PRs with a short rationale in the description.

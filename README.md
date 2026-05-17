# mii-text

A small, unix-friendly CLI for talking to OpenAI-compatible LLM APIs. Pipe text in, get structured output, compose it with the rest of your shell.

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
  - [Tools](#tools)
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
- Structured JSON output by default: `{ "reasoning": ..., "content": ..., "tool_calls": [...] }`.
- Streaming JSONL (`--stream`) for incremental responses.
- Plain text compatibility mode (`--simple`) for scripts that only want the model content.
- Tool schemas from repeated `--tool <json>` flags or a `--tools <path>` file.
- Reasoning controls (`--reasoning`, `--reasoning-summary`) for models that support them.
- SQLite response cache (`--cache`) keyed on the canonical model request and reusable across output modes.
- IPC server (`--serve`) and client (`--ipc`) over a Unix domain socket so secrets and defaults live in one place.
- Responses API by default for OpenAI models, using streaming internally and `--completions` for Chat Completions compatibility.
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

echo 'write a haiku about cargo' | mii-text --quick
```

For custom OpenAI-compatible endpoints, such as local model servers, `--key` / `OPENAI_API_KEY` is optional unless that endpoint requires one.

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

Give the model a tool schema:

```bash
echo 'what is the weather in Paris?' | mii-text --quick \
  --tool '{"type":"function","name":"get_weather","description":"Gets weather for a city","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}'
```

## Usage

### Inputs

- `--quick` — read stdin (or `--messages`) as a single user message string.
- `--messages <json>` — explicit message list:
  ```json
  [{"role": "user|assistant", "content": "text"}]
  ```
  When omitted, the same JSON is read from stdin. Extra JSON fields are preserved, so tool-response messages can carry provider fields such as `tool_call_id`, `type`, `call_id`, or `output`.
- `--system <string>` — system prompt prepended to the conversation.

### Generation knobs

- `--model <string>` — model name (or `OPENAI_MODEL_NAME`).
- `--temperature <float>`
- `--max-tokens <int>` — defaults to `128000`.
- `--reasoning <none|low|medium|high|xhigh>` — silently ignored by models that don't support it.
- `--reasoning-summary` — explicitly asks the model for a reasoning summary. Structured non-streaming output asks for this by default; streaming and `--simple` output only include reasoning when this flag is present. The summary is **not** stored in the stateful conversation history.
- `--stream` — stream tokens as they arrive.
- `--completions` — force the legacy Chat Completions API. OpenAI requests use the Responses API by default; non-OpenAI `--url` endpoints keep using Chat Completions for compatibility.
- `--simple` — print only the old plain-text answer format. With `--reasoning-summary`, this restores the old `<think>…</think>` prefix behavior.

### Tools

- `--tool <json>` — add one inline tool definition. Repeat it to pass multiple tools.
- `--tools <path>` — read tools from a JSON file. The file may contain an array, a single tool object, or an object with a `tools` array.

Tool JSON is sent to the provider's `tools` request field. Function tools may use mii-text's compact shape:

```json
{
  "name": "get_weather",
  "description": "Gets weather for a city",
  "input_schema": {
    "type": "object",
    "properties": { "city": { "type": "string" } },
    "required": ["city"]
  }
}
```

They may also use the OpenAI Responses-style shape:

```json
{
  "type": "function",
  "name": "get_weather",
  "description": "Gets weather for a city",
  "parameters": {
    "type": "object",
    "properties": { "city": { "type": "string" } },
    "required": ["city"]
  }
}
```

or the Chat Completions-style nested `function` shape. mii-text normalizes `input_schema` to OpenAI's `parameters` field and adapts the wrapper for the active provider. Other tool types are left untouched.

Tool calls are returned in the `tool_calls` output field. In `--simple` mode, if the model returns tool calls instead of text, mii-text prints the tool-call JSON array to stdout so it can be piped into your own executor.

### Output and stats

- Default stdout is one JSON object:
  ```json
  {
    "reasoning": "summary text, or null if the provider did not return one",
    "content": "answer text",
    "tool_calls": [],
    "provider_continuation": null
  }
  ```
  OpenAI Responses requests ask for a reasoning summary by default in this mode. They also request encrypted reasoning continuation data; when the provider returns it, `provider_continuation` contains opaque OpenAI items that can be passed back on a later turn.
- `--stream` changes stdout to JSONL events:
  ```jsonl
  {"type":"content_delta","delta":"answer "}
  {"type":"content_delta","delta":"text"}
  {"type":"done","reasoning":null,"content":"answer text","tool_calls":[],"provider_continuation":null}
  ```
  Reasoning summaries are omitted by default while streaming; pass `--reasoning-summary` to receive `reasoning_delta` events. Tool calls use a `tool_calls` event before `done`. OpenAI encrypted continuation data uses a `provider_continuation` event before `done` when present. The final `done` event repeats the complete accumulated `tool_calls` and `provider_continuation` values as part of the final prospect snapshot.
- `--simple` restores the previous text-only stdout contract. `--simple --stream` streams text chunks as before. Pass `--reasoning-summary` to restore the old `<think>…</think>` prefix behavior.
- `--out <path>` — write the response to a file instead of stdout.
- `--stats` — print token counts, latency, and time-to-first-token to stderr after completion. In `--serve` mode this enables stats logging on the server, not on clients.

For OpenAI Responses requests, mii-text uses the provider's streaming API internally even when `--stream` is not set. Without `--stream`, it buffers the events and renders the final prospect as one JSON object or simple text.

### Provider continuation

For OpenAI Responses requests, mii-text sends `store: false` and `include: ["reasoning.encrypted_content"]`. If OpenAI returns encrypted reasoning items, mii-text exposes them without interpreting them:

```json
{
  "provider": "openai",
  "response_id": "resp_...",
  "reasoning_items": [
    {
      "type": "reasoning",
      "id": "rs_...",
      "summary": [],
      "encrypted_content": "..."
    }
  ]
}
```

To continue a stateless Responses conversation, include that object as an item in your next `--messages` array. mii-text also accepts the streamed event shape with `"type": "provider_continuation"`, and expands `reasoning_items` into the OpenAI `input` array before sending the request. Chat Completions-compatible providers, including local `--url` backends, ignore these provider-private continuation items.

### Stateful conversations

`--stateful <path>` keeps a JSON file of the conversation messages on disk. Each invocation:

1. Loads the file (if it exists) as the prior history.
2. Appends new messages from `--messages` / stdin / `--quick`.
3. Sends the whole thing to the model.
4. Appends the assistant reply and any provider continuation item, then writes the file back.

The file format is the same as `--messages`, so you can hand-edit it or pipe it through other tooling.

### Cache

`--cache <path>` opens (or creates) a SQLite database. The cache key is a hash of model, system prompt, conversation, reasoning level, temperature, max tokens, tools, and provider mode. Cache entries store the canonical prospect and the captured stream event log, so the same model result can replay as structured JSON, JSONL, or `--simple` text without contacting the API. Cached JSONL keeps the original delta boundaries when the event log is available.

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
- Boolean flags, including `--completions` and `--simple`: clients can only enable, not disable, server-set flags.
- Tools: client-supplied `--tool` / `--tools` replace server default tools; otherwise server tools are inherited.
- Secrets (`--key`, `--url`) are server-only and never travel over the socket.

## Environment variables

| Variable            | Equivalent flag |
| ------------------- | --------------- |
| `OPENAI_API_KEY`    | `--key`         |
| `OPENAI_BASE_URL`   | `--url`         |
| `OPENAI_MODEL_NAME` | `--model`       |
| `XDG_RUNTIME_DIR`   | IPC socket dir  |

`--url` makes mii-text usable with any OpenAI-compatible endpoint (local llama.cpp servers, OpenRouter, Groq, etc.). API keys are required for OpenAI's default endpoint, but optional for custom URLs so local servers can run without fake secrets.

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
                    │  provider call   │  responses API
                    │  (async-openai)  │  or chat compat
                    │  + tools         │
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
- [output.rs](src/output.rs) — structured prospect rendering, JSONL streaming, and `--simple` compatibility output.
- [tools.rs](src/tools.rs) — tool JSON loading, provider wrapper normalization, and tool-call formatting.
- [cache.rs](src/cache.rs) — SQLite cache (`bundled` rusqlite, no system dep).
- [sink.rs](src/sink.rs) — stdout/file output with streaming flush.
- [stats.rs](src/stats.rs) — formatted token / latency reports.
- [server.rs](src/server.rs), [client.rs](src/client.rs), [ipc.rs](src/ipc.rs) — UDS server, client connector, and shared framing.

## Contributing

Issues and pull requests are welcome.

- Run `cargo fmt` and `cargo clippy --all-targets` before submitting.
- Keep the CLI surface stable; if you add a flag, mirror it in `ClientArgs` and the server↔client merge logic.
- Prefer small, focused PRs with a short rationale in the description.

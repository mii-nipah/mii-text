# mii-text

A unix-like text generation utility for a nice and composable way of using llms.

It's completely stateless in principle, so the user may invoke it without worries.
A semi-stateful API exists as a convenient "illusion" so users can have a continuous experience with it if.

* `--key <string>`: specifies your API key
You can also pass OPENAI_API_KEY as an env var to the process.

* `--url <string>`: specifies the base URL of your API
(optional if you want to use openai models directly)
You can also pass OPENAI_BASE_URL as an env var to the process.

* `--model <string>`: specifies the model name you want to use
You can also pass OPENAI_MODEL_NAME as an env var to the process.

* `--stream`: enables streaming of the responses

* `--out <path>`: changes the output mode from stdout to a file

* `--system`: specifies the system prompt to use with the AI

* `--messages`: specifies a json with the messages in the shape
```json
[
    {
        "role": "user|assistant",
        "content": "text"
    }
]
```
If no `--messages` flag is specified, it will consume from stdin instead.

* `--quick`: enables quick response mode, in which case both `--messages` and stdin (in the absence of `--messages`) will be read as a single string and automatically be processed as a normal user message to the model

* `--stateful <path>`: the stateful illusion feature, allows you to specify a file to keep the state of the conversation, content of messages will be understood as a continuation (both in `--messages`|stdin multiple mode as json or with `--quick` as a next prompt)
format of the stateful file is the same as the input json, so you could even pipe it back in a stateless manner if you wanted to

* `--reasoning <none|low|medium|high|xhigh>`
specifies the reasoning level of the model, if it's not supported by the model it will be ignored.

* `--stats`: prints some stats about the request after it's done (tokens, latency, etc) in stderr
(if used with `--serve` it implies server logs will contain stats, not the clients)

* `--cache <path>`: enables caching of the responses using a simple sqlite database to speed up repeated requests without requiring additional inferences

* `--temperature <float>`: specifies the temperature to use for the generation

* `--max-tokens <int>`: specifies the maximum number of tokens to generate (default: 128_000)

* `--reasoning-summary`: enables 'auto' reasoning summary for models, which will be appended to the output within special `<think>` `</think>` tags independent of the provider, and will not be included in the stateful conversation history

## serve mode / client mode
* `--serve`: starts a simple IPC server using the *interprocess* crate with some default configurations, so other processes can have an easier experience with mii-text without having to worry about the arguments or API keys
in this mode it's required to have an url and api key (either by argument or env), the other arguments are all optional. If you provide an argument in the server and the "clients" don't it will use the server's argument as the default, otherwise client will dominate
Example:
```bash
OPENAI_API_KEY=$OPENAI_KEY_THING mii-text --serve --model 'gpt-5.4-mini' --reasoning xhigh --cache /tmp/cache.db

# in other terminal
echo 'the capital of France is...' | mii-text --ipc --reasoning low --quick
# will use the server arguments for model and cache, reasoning is overwritten and --quick mode is enabled
```
by default serve mode will emit logs related to actions being invoked. If you use the `--quiet` flag it will be silent instead.

* `--ipc <path?>`: the client mode, by default will connect to the default UDS socket of the server, but you can specify a custom path optionally
if `--ipc` is provided with `--serve`, it will act as the specifier of the socket path to use for the server instead of the default one

Clients can also invoke:
* `mii-text --ipc --status` to check if the server is alive and get some basic info about it (pid, uptime, etc).

## exit codes
0. success
1. invalid arguments (read stderr for details)
2. api error (read stderr for details)

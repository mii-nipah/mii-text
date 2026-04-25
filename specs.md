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

## exit codes
0. success
1. invalid arguments (read stderr for details)
2. api error (read stderr for details)

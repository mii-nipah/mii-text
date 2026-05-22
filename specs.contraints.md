# mii-text - constraints, aka structured outputs

Allows you to define schemas and force models to output their anwers in said format.

Differently than the native support given by llama.cpp, this approach works even with reaosning models!

It works in two passes:
1. A normal prompt is sent to the model, which allows it to reason and to produce the best answer it can
2. The output of the first pass is then fed to a second prompt, which has structured outputs send to the model as a json schema (OpenAI API already supports it)

This way, the model can reason freely in the first pass, and then be forced to output in a structured format in the second pass.

You may pass a schema using:
* `mii-text ... --schema <path>|<json_schema>`: the schema can be a path to a json file, or a json string

And the constrained response will be returned in the same object as the original response, in a special `"constrained"` field.
For `--simple` mode, only the constrained response will be returned.

If you are using `--stateful` mode, the constrained response will be stored in the state normally.

However, for either directly providing the context or `--stateful` mode, the constrained responses will *not* be sent to the model as context, as a means to avoid confusion and produce the best possible results.
If you want to use the constrained response as context, you are free to extract the `"constrained"` field and feed it back to the model as you see fit.

## prompts
For the first pass, we extract the structure and descriptions from the schema and produce a prompt telling the model to structure it's answer in a nice way (not JSON, but simple structured information). Example:
```json
{
    "type": "object",
    "description": "Movie data",
    "properties": {
        "title": {
            "type": "string",
            "description": "The title of the movie"
        },
        "director": {
            "type": "string",
            "description": "The director of the movie"
        },
        "year": {
            "type": "integer",
            "description": "The year the movie was released"
        }
    }
}
```
will produce a prompt like:
```
Question: <the original question>
Please answer the above question with the following structure:
Movie data:
- title: The title of the movie
- director: The director of the movie
- year: The year the movie was released
```

For the second pass, you may use the exact same prompt (+ the results of the first pass), but passing the proper json schema response_format to the request, which will both give the model the context of the information and force it to output it properly.

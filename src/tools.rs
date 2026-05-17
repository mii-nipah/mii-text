use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::fs;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum ToolSource {
    Inline(String),
    File(PathBuf),
    Resolved(Vec<Value>),
}

pub async fn resolve(sources: &[ToolSource]) -> Result<Option<Vec<Value>>, String> {
    if sources.is_empty() {
        return Ok(None);
    }

    let mut tools = Vec::new();
    for source in sources {
        match source {
            ToolSource::Inline(raw) => {
                let value = serde_json::from_str::<Value>(raw)
                    .map_err(|e| format!("failed to parse --tool json: {}", e))?;
                append_tool_value(&mut tools, value, "--tool")?;
            }
            ToolSource::File(path) => {
                let raw = fs::read_to_string(path)
                    .await
                    .map_err(|e| format!("failed to read --tools {}: {}", path.display(), e))?;
                let value = serde_json::from_str::<Value>(&raw)
                    .map_err(|e| format!("failed to parse --tools {}: {}", path.display(), e))?;
                append_tool_value(&mut tools, value, &format!("--tools {}", path.display()))?;
            }
            ToolSource::Resolved(items) => {
                for item in items {
                    push_tool(&mut tools, item.clone(), "resolved tools")?;
                }
            }
        }
    }

    if tools.is_empty() {
        Ok(None)
    } else {
        Ok(Some(tools))
    }
}

pub fn resolved_sources(tools: Vec<Value>) -> Vec<ToolSource> {
    if tools.is_empty() {
        Vec::new()
    } else {
        vec![ToolSource::Resolved(tools)]
    }
}

pub fn for_chat(tools: &[Value]) -> Vec<Value> {
    tools.iter().map(normalize_for_chat).collect()
}

pub fn for_responses(tools: &[Value]) -> Vec<Value> {
    tools.iter().map(normalize_for_responses).collect()
}

pub fn format_tool_calls(calls: &[Value]) -> Result<String, String> {
    let mut text =
        serde_json::to_string_pretty(calls).map_err(|e| format!("serialize tool calls: {}", e))?;
    text.push('\n');
    Ok(text)
}

fn append_tool_value(out: &mut Vec<Value>, value: Value, label: &str) -> Result<(), String> {
    match value {
        Value::Array(items) => {
            for item in items {
                push_tool(out, item, label)?;
            }
            Ok(())
        }
        Value::Object(mut map) => {
            if let Some(tools) = map.remove("tools") {
                match tools {
                    Value::Array(items) => {
                        for item in items {
                            push_tool(out, item, label)?;
                        }
                        Ok(())
                    }
                    _ => Err(format!("{}: expected `tools` to be an array", label)),
                }
            } else {
                push_tool(out, Value::Object(map), label)
            }
        }
        _ => Err(format!(
            "{}: expected a tool object, an array of tool objects, or an object with a `tools` array",
            label
        )),
    }
}

fn push_tool(out: &mut Vec<Value>, value: Value, label: &str) -> Result<(), String> {
    if !value.is_object() {
        return Err(format!("{}: each tool must be a JSON object", label));
    }
    out.push(value);
    Ok(())
}

fn normalize_for_chat(tool: &Value) -> Value {
    let Some(obj) = tool.as_object() else {
        return tool.clone();
    };
    if obj.get("type").and_then(Value::as_str) == Some("function") {
        if let Some(function) = obj.get("function").and_then(Value::as_object) {
            let mut out = obj.clone();
            out.insert(
                "function".to_string(),
                Value::Object(normalize_function_fields(function)),
            );
            return Value::Object(out);
        }
        if obj.contains_key("name") {
            return json!({
                "type": "function",
                "function": Value::Object(normalize_function_fields(obj)),
            });
        }
        return tool.clone();
    }

    if looks_like_plain_function(obj) {
        return json!({
            "type": "function",
            "function": Value::Object(normalize_function_fields(obj)),
        });
    }

    tool.clone()
}

fn normalize_for_responses(tool: &Value) -> Value {
    let Some(obj) = tool.as_object() else {
        return tool.clone();
    };
    if obj.get("type").and_then(Value::as_str) != Some("function") {
        if looks_like_plain_function(obj) {
            let mut out = normalize_function_fields(obj);
            out.insert("type".to_string(), Value::String("function".to_string()));
            return Value::Object(out);
        }
        return tool.clone();
    }

    let Some(function) = obj.get("function").and_then(Value::as_object) else {
        let mut out = normalize_function_fields(obj);
        out.insert("type".to_string(), Value::String("function".to_string()));
        return Value::Object(out);
    };

    let mut out = Map::new();
    out.insert("type".to_string(), Value::String("function".to_string()));
    for (key, value) in obj {
        if key != "type" && key != "function" {
            out.insert(key.clone(), value.clone());
        }
    }
    let function = normalize_function_fields(function);
    for (key, value) in &function {
        out.insert(key.clone(), value.clone());
    }

    Value::Object(out)
}

fn looks_like_plain_function(obj: &Map<String, Value>) -> bool {
    obj.contains_key("name") && (obj.contains_key("input_schema") || obj.contains_key("parameters"))
}

fn normalize_function_fields(obj: &Map<String, Value>) -> Map<String, Value> {
    let mut function = Map::new();
    for (key, value) in obj {
        match key.as_str() {
            "type" | "function" => {}
            "input_schema" => {
                if !obj.contains_key("parameters") {
                    function.insert("parameters".to_string(), value.clone());
                }
            }
            _ => {
                function.insert(key.clone(), value.clone());
            }
        }
    }
    function
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::*;

    fn temp_tools_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mii-text-tools-{name}-{}-{unique}.json",
            std::process::id()
        ))
    }

    #[test]
    fn accepts_inline_tool_arrays_and_wrappers() {
        let mut out = Vec::new();
        append_tool_value(
            &mut out,
            json!({ "type": "function", "name": "one" }),
            "--tool",
        )
        .unwrap();
        append_tool_value(
            &mut out,
            json!({ "tools": [{ "type": "function", "name": "two" }] }),
            "--tools tools.json",
        )
        .unwrap();
        append_tool_value(
            &mut out,
            json!([{ "type": "function", "name": "three" }]),
            "--tools tools.json",
        )
        .unwrap();

        assert_eq!(out.len(), 3);
    }

    #[test]
    fn normalizes_function_tools_for_chat_and_responses() {
        let responses_style = json!({
            "type": "function",
            "name": "get_weather",
            "description": "Gets weather",
            "parameters": { "type": "object" },
            "strict": true
        });
        let chat_style = for_chat(&[responses_style])[0].clone();

        assert_eq!(chat_style["type"], "function");
        assert_eq!(chat_style["function"]["name"], "get_weather");
        assert_eq!(chat_style["function"]["strict"], true);

        let responses_again = for_responses(&[chat_style])[0].clone();
        assert_eq!(responses_again["type"], "function");
        assert_eq!(responses_again["name"], "get_weather");
        assert_eq!(responses_again["parameters"]["type"], "object");
    }

    #[test]
    fn maps_plain_input_schema_tools_to_openai_shapes() {
        let plain = json!({
            "name": "echo",
            "description": "Echo a message back to the user.",
            "input_schema": {
                "type": "object",
                "properties": { "message": { "type": "string" } },
                "required": ["message"]
            }
        });

        let chat = for_chat(std::slice::from_ref(&plain))[0].clone();
        assert_eq!(chat["type"], "function");
        assert_eq!(chat["function"]["name"], "echo");
        assert!(chat["function"].get("input_schema").is_none());
        assert_eq!(chat["function"]["parameters"]["required"][0], "message");

        let responses = for_responses(&[plain])[0].clone();
        assert_eq!(responses["type"], "function");
        assert_eq!(responses["name"], "echo");
        assert!(responses.get("input_schema").is_none());
        assert_eq!(
            responses["parameters"]["properties"]["message"]["type"],
            "string"
        );
    }

    #[test]
    fn maps_input_schema_inside_openai_function_wrappers() {
        let chat_style = json!({
            "type": "function",
            "function": {
                "name": "echo",
                "input_schema": { "type": "object" }
            }
        });

        let chat = for_chat(std::slice::from_ref(&chat_style))[0].clone();
        assert!(chat["function"].get("input_schema").is_none());
        assert_eq!(chat["function"]["parameters"]["type"], "object");

        let responses = for_responses(&[chat_style])[0].clone();
        assert!(responses.get("input_schema").is_none());
        assert_eq!(responses["parameters"]["type"], "object");
    }

    #[tokio::test]
    async fn resolve_loads_inline_file_and_pre_resolved_sources_in_order() {
        let path = temp_tools_path("resolve");
        fs::write(
            &path,
            r#"{"tools":[{"name":"from_file","input_schema":{"type":"object"}}]}"#,
        )
        .await
        .unwrap();

        let tools = resolve(&[
            ToolSource::Inline(r#"{"name":"inline","input_schema":{"type":"object"}}"#.into()),
            ToolSource::File(path.clone()),
            ToolSource::Resolved(vec![json!({
                "name": "resolved",
                "input_schema": { "type": "object" }
            })]),
        ])
        .await
        .unwrap()
        .unwrap();

        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0]["name"], "inline");
        assert_eq!(tools[1]["name"], "from_file");
        assert_eq!(tools[2]["name"], "resolved");

        let _ = fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn resolve_returns_none_for_empty_sources_and_rejects_bad_shapes() {
        assert!(resolve(&[]).await.unwrap().is_none());

        let err = resolve(&[ToolSource::Inline("42".into())])
            .await
            .unwrap_err();
        assert!(err.contains("expected a tool object"));

        let err = resolve(&[ToolSource::Inline(r#"{"tools":42}"#.into())])
            .await
            .unwrap_err();
        assert!(err.contains("expected `tools` to be an array"));

        let err = resolve(&[ToolSource::Inline("[42]".into())])
            .await
            .unwrap_err();
        assert!(err.contains("each tool must be a JSON object"));
    }

    #[test]
    fn resolved_sources_and_tool_call_formatting_are_stable() {
        assert!(resolved_sources(Vec::new()).is_empty());

        let sources = resolved_sources(vec![json!({ "name": "echo" })]);
        assert_eq!(sources.len(), 1);
        match &sources[0] {
            ToolSource::Resolved(items) => assert_eq!(items[0]["name"], "echo"),
            other => panic!("unexpected source: {other:?}"),
        }

        let text = format_tool_calls(&[json!({ "call_id": "call_1" })]).unwrap();
        assert!(text.ends_with('\n'));
        let calls: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(calls[0]["call_id"], "call_1");
    }

    #[test]
    fn response_normalization_leaves_non_function_tools_untouched() {
        let custom = json!({
            "type": "custom",
            "name": "raw_exec",
            "input": { "type": "text" }
        });

        assert_eq!(for_chat(std::slice::from_ref(&custom))[0], custom);
        assert_eq!(for_responses(std::slice::from_ref(&custom))[0], custom);
    }

    #[test]
    fn responses_normalization_flattens_nested_function_without_leaking_wrapper_fields() {
        let chat_style = json!({
            "type": "function",
            "metadata": { "owner": "tests" },
            "function": {
                "type": "function",
                "function": { "should": "not leak" },
                "name": "echo",
                "description": "Echo",
                "parameters": { "type": "object" }
            }
        });

        let normalized = for_responses(&[chat_style])[0].clone();

        assert_eq!(normalized["type"], "function");
        assert_eq!(normalized["name"], "echo");
        assert_eq!(normalized["description"], "Echo");
        assert_eq!(normalized["parameters"]["type"], "object");
        assert_eq!(normalized["metadata"]["owner"], "tests");
        assert!(normalized.get("function").is_none());
    }
}

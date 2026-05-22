use std::path::PathBuf;

use serde_json::Value;
use tokio::fs;

use crate::conversation::Message;
use crate::output::Prospect;

pub async fn resolve_schema(raw: &str) -> Result<Value, String> {
    if let Ok(schema) = serde_json::from_str::<Value>(raw) {
        validate_schema(&schema)?;
        return Ok(schema);
    }

    let path = PathBuf::from(raw);
    let contents = fs::read_to_string(&path).await.map_err(|e| {
        format!(
            "failed to parse --schema as JSON or read {}: {}",
            path.display(),
            e
        )
    })?;
    let schema = serde_json::from_str::<Value>(&contents)
        .map_err(|e| format!("failed to parse --schema {}: {}", path.display(), e))?;
    validate_schema(&schema)?;
    Ok(schema)
}

pub fn schema_guidance(schema: &Value) -> String {
    let mut lines =
        vec!["Please answer the above question with the following structure:".to_string()];
    append_schema_lines(schema, None, 0, &mut lines);
    lines.join("\n")
}

pub fn apply_first_pass_prompt(messages: &[Message], schema: &Value) -> Vec<Message> {
    let guidance = schema_guidance(schema);
    let mut out = messages.to_vec();
    if let Some((_, message)) = out
        .iter_mut()
        .enumerate()
        .rev()
        .find(|(_, message)| message.role == "user")
        && let Some(Value::String(question)) = &message.content
    {
        message.content = Some(Value::String(format!(
            "Question: {}\n{}",
            question.trim(),
            guidance
        )));
        return out;
    }

    out.push(Message::user(guidance));
    out
}

pub fn second_pass_messages(first_pass_messages: &[Message], first: &Prospect) -> Vec<Message> {
    let mut out = first_pass_messages.to_vec();
    out.push(Message::assistant(draft_text(first)));
    out.push(Message::user(
        "Convert the assistant answer into JSON that validates against the provided schema. Return only the JSON value.".to_string(),
    ));
    out
}

pub fn parse_constrained_response(content: &str) -> Result<Value, String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err("schema-constrained pass returned empty content".to_string());
    }
    serde_json::from_str::<Value>(trimmed)
        .map_err(|e| format!("schema-constrained pass returned invalid JSON: {}", e))
}

pub fn response_format_for_chat(schema: &Value) -> Value {
    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "mii_text_constrained_response",
            "schema": schema,
        }
    })
}

pub fn text_format_for_responses(schema: &Value) -> Value {
    serde_json::json!({
        "format": {
            "type": "json_schema",
            "name": "mii_text_constrained_response",
            "schema": schema,
        }
    })
}

fn validate_schema(schema: &Value) -> Result<(), String> {
    if schema.is_object() {
        Ok(())
    } else {
        Err("--schema must be a JSON schema object".to_string())
    }
}

fn draft_text(first: &Prospect) -> String {
    if !first.content.trim().is_empty() {
        return first.content.clone();
    }
    if first.tool_calls.is_empty() {
        return String::new();
    }
    serde_json::to_string_pretty(&first.tool_calls).unwrap_or_else(|_| "[]".to_string())
}

fn append_schema_lines(schema: &Value, name: Option<&str>, depth: usize, lines: &mut Vec<String>) {
    let indent = "  ".repeat(depth);
    let description = schema
        .get("description")
        .and_then(Value::as_str)
        .or_else(|| schema.get("title").and_then(Value::as_str));
    let schema_type = schema.get("type").and_then(Value::as_str);

    match (name, description, schema_type) {
        (None, Some(description), _) => lines.push(format!("{indent}{description}:")),
        (None, None, Some(kind)) => lines.push(format!("{indent}{kind}:")),
        (None, None, None) => lines.push(format!("{indent}response:")),
        (Some(name), Some(description), _) => {
            lines.push(format!("{indent}- {name}: {description}"))
        }
        (Some(name), None, Some(kind)) => lines.push(format!("{indent}- {name}: {kind}")),
        (Some(name), None, None) => lines.push(format!("{indent}- {name}")),
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (property, property_schema) in properties {
            append_schema_lines(property_schema, Some(property), depth + 1, lines);
        }
    }

    if let Some(items) = schema.get("items") {
        append_schema_lines(items, Some("items[]"), depth + 1, lines);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn schema_guidance_extracts_descriptions_and_properties() {
        let guidance = schema_guidance(&json!({
            "type": "object",
            "description": "Movie data",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "The title of the movie"
                },
                "year": {
                    "type": "integer",
                    "description": "The year the movie was released"
                }
            }
        }));

        assert!(guidance.contains("Please answer the above question"));
        assert!(guidance.contains("Movie data:"));
        assert!(guidance.contains("- title: The title of the movie"));
        assert!(guidance.contains("- year: The year the movie was released"));
    }

    #[test]
    fn first_pass_prompt_wraps_the_latest_user_string() {
        let messages = vec![
            Message::user("old".to_string()),
            Message::assistant("answer".to_string()),
            Message::user("new question".to_string()),
        ];

        let prompted = apply_first_pass_prompt(&messages, &json!({"type":"object"}));

        assert_eq!(prompted.len(), 3);
        let content = prompted[2]
            .content
            .as_ref()
            .and_then(Value::as_str)
            .unwrap();
        assert!(content.contains("Question: new question"));
        assert!(content.contains("Please answer the above question"));
        assert_eq!(
            prompted[0].content.as_ref().and_then(Value::as_str),
            Some("old")
        );
    }

    #[test]
    fn parses_constrained_json_and_rejects_empty_or_invalid_content() {
        assert_eq!(
            parse_constrained_response(r#"{"title":"Brazil"}"#).unwrap()["title"],
            "Brazil"
        );
        assert!(
            parse_constrained_response("")
                .unwrap_err()
                .contains("empty")
        );
        assert!(
            parse_constrained_response("not json")
                .unwrap_err()
                .contains("invalid JSON")
        );
    }

    #[test]
    fn response_formats_use_the_openai_json_schema_shape() {
        let schema = json!({"type":"object"});
        let chat = response_format_for_chat(&schema);
        let responses = text_format_for_responses(&schema);

        assert_eq!(chat["type"], "json_schema");
        assert_eq!(chat["json_schema"]["schema"]["type"], "object");
        assert_eq!(responses["format"]["type"], "json_schema");
        assert_eq!(responses["format"]["schema"]["type"], "object");
    }
}

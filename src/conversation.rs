use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::fs;
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Message {
    pub fn user(content: String) -> Self {
        Self {
            role: "user".to_string(),
            content: Some(Value::String(content)),
            extra: Map::new(),
        }
    }

    pub fn assistant(content: String) -> Self {
        Self {
            role: "assistant".to_string(),
            content: Some(Value::String(content)),
            extra: Map::new(),
        }
    }

    pub fn to_api_value(&self) -> Value {
        let mut object = self.extra.clone();
        if !self.role.is_empty() {
            object.insert("role".to_string(), Value::String(self.role.clone()));
        }
        if let Some(content) = &self.content {
            object.insert("content".to_string(), content.clone());
        }
        Value::Object(object)
    }
}

pub async fn read_stdin_to_string() -> std::io::Result<String> {
    let mut buf = String::new();
    tokio::io::stdin().read_to_string(&mut buf).await?;
    Ok(buf)
}

/// Loads the user-supplied messages from `--messages`, the provided stdin
/// override, or actual stdin (in that order), applying `--quick` semantics if
/// enabled.
pub async fn load_input_messages(
    messages_arg: &Option<String>,
    quick: bool,
    stdin_override: Option<&str>,
) -> Result<Vec<Message>, String> {
    let raw = match messages_arg {
        Some(s) => s.clone(),
        None => match stdin_override {
            Some(s) => s.to_string(),
            None => read_stdin_to_string()
                .await
                .map_err(|e| format!("failed to read stdin: {}", e))?,
        },
    };

    if quick {
        let content = raw.trim().to_string();
        if content.is_empty() {
            return Err("quick mode requires non-empty input".to_string());
        }
        return Ok(vec![Message::user(content)]);
    }

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("no input messages provided".to_string());
    }
    serde_json::from_str::<Vec<Message>>(trimmed)
        .map_err(|e| format!("failed to parse messages json: {}", e))
}

pub async fn load_stateful(path: &PathBuf) -> Result<Vec<Message>, String> {
    if !fs::try_exists(path).await.unwrap_or(false) {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)
        .await
        .map_err(|e| format!("failed to read stateful file: {}", e))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<Vec<Message>>(trimmed)
        .map_err(|e| format!("failed to parse stateful file json: {}", e))
}

pub async fn save_stateful(path: &PathBuf, msgs: &[Message]) -> Result<(), String> {
    let serialized =
        serde_json::to_string_pretty(msgs).map_err(|e| format!("serialize stateful: {}", e))?;
    fs::write(path, serialized)
        .await
        .map_err(|e| format!("write stateful file: {}", e))
}

/// Builds the message array shape used by the chat completions API.
pub fn build_chat_messages(system: &Option<String>, msgs: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(msgs.len() + 1);
    if let Some(sys) = system {
        out.push(json!({ "role": "system", "content": sys }));
    }
    for m in msgs {
        out.push(m.to_api_value());
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::*;

    fn temp_state_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mii-text-state-{name}-{}-{unique}.json",
            std::process::id()
        ))
    }

    #[test]
    fn preserves_tool_message_fields() {
        let raw = r#"[
            {"role":"tool","tool_call_id":"call_1","content":"ok"},
            {"type":"function_call_output","call_id":"call_2","output":"done"}
        ]"#;
        let messages = serde_json::from_str::<Vec<Message>>(raw).unwrap();
        let values: Vec<Value> = messages.iter().map(Message::to_api_value).collect();

        assert_eq!(values[0]["role"], "tool");
        assert_eq!(values[0]["tool_call_id"], "call_1");
        assert_eq!(values[0]["content"], "ok");
        assert_eq!(
            values[1],
            json!({
                "type": "function_call_output",
                "call_id": "call_2",
                "output": "done"
            })
        );
    }

    #[test]
    fn accepts_null_content_tool_calls() {
        let raw = r#"[{"role":"assistant","content":null,"tool_calls":[]}]"#;
        let messages = serde_json::from_str::<Vec<Message>>(raw).unwrap();
        let value = messages[0].to_api_value();

        assert_eq!(value["role"], "assistant");
        assert!(value.get("content").is_none());
        assert_eq!(value["tool_calls"], json!([]));
    }

    #[tokio::test]
    async fn quick_input_trims_stdin_override_and_rejects_empty_prompts() {
        let messages = load_input_messages(&None, true, Some("  hello shell  \n"))
            .await
            .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].to_api_value()["role"], "user");
        assert_eq!(messages[0].to_api_value()["content"], "hello shell");
        assert_eq!(
            load_input_messages(&None, true, Some(" \n\t "))
                .await
                .unwrap_err(),
            "quick mode requires non-empty input"
        );
    }

    #[tokio::test]
    async fn json_messages_are_loaded_from_argument_before_stdin_override() {
        let arg = Some(r#"[{"role":"user","content":"from args"}]"#.to_string());
        let messages =
            load_input_messages(&arg, false, Some(r#"[{"role":"user","content":"stdin"}]"#))
                .await
                .unwrap();

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].to_api_value()["content"], "from args");
    }

    #[tokio::test]
    async fn stateful_files_round_trip_and_missing_files_start_empty() {
        let path = temp_state_path("roundtrip");
        assert!(load_stateful(&path).await.unwrap().is_empty());

        let messages = vec![
            Message::user("hello".to_string()),
            Message::assistant("hi".to_string()),
        ];
        save_stateful(&path, &messages).await.unwrap();
        let loaded = load_stateful(&path).await.unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].to_api_value()["role"], "user");
        assert_eq!(loaded[0].to_api_value()["content"], "hello");
        assert_eq!(loaded[1].to_api_value()["role"], "assistant");
        assert_eq!(loaded[1].to_api_value()["content"], "hi");

        let _ = fs::remove_file(path).await;
    }

    #[test]
    fn build_chat_messages_prepends_system_and_preserves_extra_fields() {
        let system = Some("system prompt".to_string());
        let messages = serde_json::from_str::<Vec<Message>>(
            r#"[
                {"role":"assistant","content":null,"tool_calls":[{"id":"call_1"}]},
                {"role":"tool","tool_call_id":"call_1","content":"ok"}
            ]"#,
        )
        .unwrap();

        let api = build_chat_messages(&system, &messages);

        assert_eq!(api.len(), 3);
        assert_eq!(
            api[0],
            json!({ "role": "system", "content": "system prompt" })
        );
        assert_eq!(api[1]["role"], "assistant");
        assert!(api[1].get("content").is_none());
        assert_eq!(api[1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(api[2]["tool_call_id"], "call_1");
        assert_eq!(api[2]["content"], "ok");
    }
}

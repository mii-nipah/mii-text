use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::fs;
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

pub async fn read_stdin_to_string() -> std::io::Result<String> {
    let mut buf = String::new();
    tokio::io::stdin().read_to_string(&mut buf).await?;
    Ok(buf)
}

/// Loads the user-supplied messages from `--messages` or stdin, applying
/// `--quick` semantics if enabled.
pub async fn load_input_messages(
    messages_arg: &Option<String>,
    quick: bool,
) -> Result<Vec<Message>, String> {
    let raw = match messages_arg {
        Some(s) => s.clone(),
        None => read_stdin_to_string()
            .await
            .map_err(|e| format!("failed to read stdin: {}", e))?,
    };

    if quick {
        let content = raw.trim().to_string();
        if content.is_empty() {
            return Err("quick mode requires non-empty input".to_string());
        }
        return Ok(vec![Message {
            role: "user".to_string(),
            content,
        }]);
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
        out.push(json!({ "role": m.role, "content": m.content }));
    }
    out
}

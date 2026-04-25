use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::conversation::Message;

pub struct CachedEntry {
    pub content: String,
    pub usage: Option<Value>,
    pub model: Option<String>,
}

/// Computes the cache key. Deliberately excludes secrets (api key) and
/// transport-only fields (base URL, --stream, --out, --stateful, --stats)
/// since they don't affect the model's output.
pub fn key(
    model: &str,
    system: &Option<String>,
    messages: &[Message],
    reasoning: &Option<String>,
    temperature: Option<f32>,
    max_tokens: u32,
    reasoning_summary: bool,
) -> String {
    let canonical = json!({
        "v": 4,
        "model": model,
        "system": system,
        "messages": messages,
        "reasoning": reasoning,
        "temperature": temperature,
        "max_tokens": max_tokens,
        "reasoning_summary": reasoning_summary,
    });
    let serialized = serde_json::to_vec(&canonical).expect("canonical json");
    let mut hasher = Sha256::new();
    hasher.update(&serialized);
    let bytes = hasher.finalize();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{:02x}", b);
    }
    hex
}

pub fn open(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create cache dir: {}", e))?;
        }
    }
    let conn = Connection::open(path).map_err(|e| format!("open cache db: {}", e))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS responses (\n             key TEXT PRIMARY KEY,\n             content TEXT NOT NULL,\n             usage TEXT,\n             model TEXT,\n             created_at INTEGER NOT NULL\n         );",
    )
    .map_err(|e| format!("init cache schema: {}", e))?;
    Ok(conn)
}

pub fn lookup(conn: &Connection, key: &str) -> Result<Option<CachedEntry>, String> {
    let row = conn
        .query_row(
            "SELECT content, usage, model FROM responses WHERE key = ?1",
            params![key],
            |r| {
                let content: String = r.get(0)?;
                let usage: Option<String> = r.get(1)?;
                let model: Option<String> = r.get(2)?;
                Ok((content, usage, model))
            },
        )
        .optional()
        .map_err(|e| format!("cache lookup: {}", e))?;
    match row {
        None => Ok(None),
        Some((content, usage_str, model)) => {
            let usage = usage_str.and_then(|s| serde_json::from_str(&s).ok());
            Ok(Some(CachedEntry { content, usage, model }))
        }
    }
}

pub fn store(
    conn: &Connection,
    key: &str,
    content: &str,
    usage: &Option<Value>,
    model: &Option<String>,
) -> Result<(), String> {
    let usage_str = usage
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT OR REPLACE INTO responses (key, content, usage, model, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![key, content, usage_str, model, now],
    )
    .map_err(|e| format!("cache store: {}", e))?;
    Ok(())
}

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::conversation::Message;
use crate::output::Prospect;

pub struct CachedEntry {
    pub content: String,
    pub assistant: Option<String>,
    pub prospect: Option<Prospect>,
    pub events: Option<Vec<Value>>,
    pub usage: Option<Value>,
    pub model: Option<String>,
}

/// Computes the cache key. Deliberately excludes secrets (api key) and
/// transport-only and rendering-only fields (base URL, --out, --stateful,
/// --stats, --stream, --simple, --reasoning-summary) since they don't affect
/// the canonical model prospect.
pub struct KeyParts<'a> {
    pub model: &'a str,
    pub system: &'a Option<String>,
    pub messages: &'a [Message],
    pub reasoning: &'a Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: u32,
    pub tools: &'a Option<Vec<Value>>,
    pub completions: bool,
}

pub struct StoreEntry<'a> {
    pub key: &'a str,
    pub content: &'a str,
    pub assistant: &'a str,
    pub prospect: &'a Prospect,
    pub events: &'a [Value],
    pub usage: &'a Option<Value>,
    pub model: &'a Option<String>,
}

pub fn key(parts: KeyParts<'_>) -> String {
    let canonical = json!({
        "v": 10,
        "model": parts.model,
        "system": parts.system,
        "messages": parts.messages,
        "reasoning": parts.reasoning,
        "temperature": parts.temperature,
        "max_tokens": parts.max_tokens,
        "tools": parts.tools,
        "completions": parts.completions,
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
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| format!("create cache dir: {}", e))?;
    }
    let conn = Connection::open(path).map_err(|e| format!("open cache db: {}", e))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS responses (\n             key TEXT PRIMARY KEY,\n             content TEXT NOT NULL,\n             assistant TEXT,\n             prospect TEXT,\n             events TEXT,\n             usage TEXT,\n             model TEXT,\n             created_at INTEGER NOT NULL\n         );",
    )
    .map_err(|e| format!("init cache schema: {}", e))?;
    ensure_column(&conn, "assistant", "TEXT")?;
    ensure_column(&conn, "prospect", "TEXT")?;
    ensure_column(&conn, "events", "TEXT")?;
    Ok(conn)
}

pub fn lookup(conn: &Connection, key: &str) -> Result<Option<CachedEntry>, String> {
    let row = conn
        .query_row(
            "SELECT content, assistant, prospect, events, usage, model FROM responses WHERE key = ?1",
            params![key],
            |r| {
                let content: String = r.get(0)?;
                let assistant: Option<String> = r.get(1)?;
                let prospect: Option<String> = r.get(2)?;
                let events: Option<String> = r.get(3)?;
                let usage: Option<String> = r.get(4)?;
                let model: Option<String> = r.get(5)?;
                Ok((content, assistant, prospect, events, usage, model))
            },
        )
        .optional()
        .map_err(|e| format!("cache lookup: {}", e))?;
    match row {
        None => Ok(None),
        Some((content, assistant, prospect_str, events_str, usage_str, model)) => {
            let prospect = prospect_str.and_then(|s| serde_json::from_str(&s).ok());
            let events = events_str.and_then(|s| serde_json::from_str(&s).ok());
            let usage = usage_str.and_then(|s| serde_json::from_str(&s).ok());
            Ok(Some(CachedEntry {
                content,
                assistant,
                prospect,
                events,
                usage,
                model,
            }))
        }
    }
}

pub fn store(conn: &Connection, entry: StoreEntry<'_>) -> Result<(), String> {
    let prospect_str = serde_json::to_string(entry.prospect)
        .map_err(|e| format!("serialize cache prospect: {}", e))?;
    let events_str = serde_json::to_string(entry.events)
        .map_err(|e| format!("serialize cache events: {}", e))?;
    let usage_str = entry
        .usage
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT OR REPLACE INTO responses (key, content, assistant, prospect, events, usage, model, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            entry.key,
            entry.content,
            entry.assistant,
            prospect_str,
            events_str,
            usage_str,
            entry.model,
            now
        ],
    )
    .map_err(|e| format!("cache store: {}", e))?;
    Ok(())
}

fn ensure_column(conn: &Connection, column: &str, sql_type: &str) -> Result<(), String> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(responses)")
        .map_err(|e| format!("inspect cache schema: {}", e))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| format!("inspect cache schema: {}", e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("inspect cache schema: {}", e))?;
    if names.iter().any(|name| name == column) {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE responses ADD COLUMN {column} {sql_type}"),
        [],
    )
    .map_err(|e| format!("migrate cache schema: {}", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::*;
    use crate::conversation::Message;

    fn key_for(tools: Option<Vec<Value>>, completions: bool) -> String {
        let system = Some("system".to_string());
        let reasoning = Some("low".to_string());
        let messages = vec![Message::user("hello".to_string())];
        key(KeyParts {
            model: "model-a",
            system: &system,
            messages: &messages,
            reasoning: &reasoning,
            temperature: Some(0.5),
            max_tokens: 123,
            tools: &tools,
            completions,
        })
    }

    fn temp_cache_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mii-text-{name}-{}-{unique}.db",
            std::process::id()
        ))
    }

    #[test]
    fn cache_key_changes_for_tools_and_provider_but_not_output_modes() {
        let base = key_for(None, false);

        assert_ne!(base, key_for(Some(vec![json!({ "name": "echo" })]), false));
        assert_ne!(base, key_for(None, true));
        assert_eq!(base, key_for(None, false));
        assert_eq!(base.len(), 64);
        assert!(base.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn store_and_lookup_preserve_prospect_assistant_usage_and_model() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE responses (
                key TEXT PRIMARY KEY,
                content TEXT NOT NULL,
                assistant TEXT,
                prospect TEXT,
                events TEXT,
                usage TEXT,
                model TEXT,
                created_at INTEGER NOT NULL
            );",
        )
        .unwrap();
        let usage = Some(json!({ "prompt_tokens": 3, "completion_tokens": 5 }));
        let model = Some("model-a".to_string());
        let prospect = Prospect {
            reasoning: Some("because".to_string()),
            content: "rendered".to_string(),
            tool_calls: vec![json!({ "call_id": "call_1" })],
            provider_continuation: None,
        };
        let events = vec![json!({ "type": "content_delta", "delta": "rendered" })];

        store(
            &conn,
            StoreEntry {
                key: "key-a",
                content: "{\"content\":\"rendered\"}\n",
                assistant: "rendered",
                prospect: &prospect,
                events: &events,
                usage: &usage,
                model: &model,
            },
        )
        .unwrap();

        let hit = lookup(&conn, "key-a").unwrap().unwrap();
        assert_eq!(hit.content, "{\"content\":\"rendered\"}\n");
        assert_eq!(hit.assistant.as_deref(), Some("rendered"));
        let cached_prospect = hit.prospect.unwrap();
        assert_eq!(cached_prospect.reasoning.as_deref(), Some("because"));
        assert_eq!(cached_prospect.content, "rendered");
        assert_eq!(cached_prospect.tool_calls[0]["call_id"], "call_1");
        assert_eq!(hit.events.unwrap(), events);
        assert_eq!(hit.usage, usage);
        assert_eq!(hit.model, model);
        assert!(lookup(&conn, "missing").unwrap().is_none());
    }

    #[test]
    fn open_migrates_old_cache_schema_without_losing_rows() {
        let path = temp_cache_path("migration");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE responses (
                    key TEXT PRIMARY KEY,
                    content TEXT NOT NULL,
                    usage TEXT,
                    model TEXT,
                    created_at INTEGER NOT NULL
                );
                INSERT INTO responses (key, content, usage, model, created_at)
                VALUES ('old-key', 'old rendered', '{\"total_tokens\":8}', 'old-model', 1);",
            )
            .unwrap();
        }

        let conn = open(&path).unwrap();
        let hit = lookup(&conn, "old-key").unwrap().unwrap();

        assert_eq!(hit.content, "old rendered");
        assert_eq!(hit.assistant, None);
        assert!(hit.prospect.is_none());
        assert!(hit.events.is_none());
        assert_eq!(hit.usage, Some(json!({ "total_tokens": 8 })));
        assert_eq!(hit.model.as_deref(), Some("old-model"));
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(responses)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(columns.iter().any(|name| name == "assistant"));
        assert!(columns.iter().any(|name| name == "prospect"));
        assert!(columns.iter().any(|name| name == "events"));

        drop(conn);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn open_accepts_relative_paths_without_parent_directory_creation() {
        let path = PathBuf::from(format!(
            "mii-text-relative-cache-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let conn = open(&path).unwrap();
        let prospect = Prospect {
            reasoning: None,
            content: "assistant".to_string(),
            tool_calls: Vec::new(),
            provider_continuation: None,
        };
        store(
            &conn,
            StoreEntry {
                key: "key-a",
                content: "rendered",
                assistant: "assistant",
                prospect: &prospect,
                events: &[],
                usage: &None,
                model: &None,
            },
        )
        .unwrap();
        assert_eq!(
            lookup(&conn, "key-a")
                .unwrap()
                .unwrap()
                .assistant
                .as_deref(),
            Some("assistant")
        );

        drop(conn);
        let _ = std::fs::remove_file(path);
    }
}

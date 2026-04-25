use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};

use crate::args::ClientArgs;

/// Newline-delimited JSON request from the client to the server.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Request {
    /// Standard text-generation request: forwards the client's overrideable
    /// args plus a captured stdin buffer to the server.
    Run {
        args: ClientArgs,
        /// Stdin buffer captured by the client. The server uses this in place
        /// of its own stdin when `--messages` is not supplied.
        stdin: String,
    },
    /// Health/info ping. The server responds with a single `Status` frame
    /// followed by `Exit { code: 0 }`.
    Status,
}

/// Server-side info returned in response to a `Request::Status` ping.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusInfo {
    pub pid: u32,
    pub uptime_ms: u64,
    pub requests_served: u64,
    pub model: Option<String>,
    pub socket: String,
}

/// Newline-delimited JSON frame streamed from the server back to the client.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Frame {
    /// A piece of model output (or cached replay) destined for the client's
    /// stdout sink.
    Stdout { text: String },
    /// A piece of stats / diagnostic text destined for the client's stderr.
    Stderr { text: String },
    /// Reply to a `Request::Status` ping.
    Status { info: StatusInfo },
    /// Terminal frame indicating the request is complete and providing the
    /// shell exit code the client should return. `assistant` carries the
    /// final assistant message text (without `<think>` wrapping) so the
    /// client can append it to its stateful conversation file.
    Exit { code: u8, assistant: Option<String> },
}

pub async fn write_json_line<T, W>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    T: Serialize,
    W: AsyncWriteExt + Unpin,
{
    let mut buf = serde_json::to_vec(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    buf.push(b'\n');
    writer.write_all(&buf).await?;
    writer.flush().await
}

pub async fn read_json_line<T, R>(reader: &mut BufReader<R>) -> std::io::Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
    R: AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
    serde_json::from_str(trimmed)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

use std::path::PathBuf;

use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::UnboundedSender;

/// Sink for diagnostic / stats text. Either eprint!-equivalent or a channel
/// that buffers lines for forwarding to a connected IPC client.
pub enum ErrSink {
    Local,
    Channel(UnboundedSender<String>),
    /// Server-side variant: forwards to the client only when the client
    /// requested it, and always logs locally to stderr unless `quiet`.
    Server {
        tx: UnboundedSender<String>,
        forward_to_client: bool,
        quiet: bool,
        conn_id: u64,
    },
}

impl ErrSink {
    pub fn emit(&self, s: &str) {
        match self {
            ErrSink::Local => {
                eprint!("{}", s);
            }
            ErrSink::Channel(tx) => {
                let _ = tx.send(s.to_string());
            }
            ErrSink::Server {
                tx,
                forward_to_client,
                quiet,
                conn_id,
            } => {
                if !*quiet {
                    for line in s.lines() {
                        eprintln!("#{} {}", conn_id, line);
                    }
                }
                if *forward_to_client {
                    let _ = tx.send(s.to_string());
                }
            }
        }
    }
}

pub enum Sink {
    Stdout(tokio::io::Stdout),
    File(tokio::fs::File),
    /// Sends each write as a separate string into a channel. Used by the
    /// `--serve` mode to forward streamed text frames to a connected client.
    Channel(UnboundedSender<String>),
}

impl Sink {
    pub async fn open(out: &Option<PathBuf>) -> Result<Self, String> {
        match out {
            Some(p) => {
                let f = tokio::fs::File::create(p)
                    .await
                    .map_err(|e| format!("open output file: {}", e))?;
                Ok(Sink::File(f))
            }
            None => Ok(Sink::Stdout(tokio::io::stdout())),
        }
    }

    pub fn channel(tx: UnboundedSender<String>) -> Self {
        Sink::Channel(tx)
    }

    pub async fn write_str(&mut self, s: &str) -> std::io::Result<()> {
        match self {
            Sink::Stdout(o) => {
                o.write_all(s.as_bytes()).await?;
                o.flush().await
            }
            Sink::File(f) => f.write_all(s.as_bytes()).await,
            Sink::Channel(tx) => {
                tx.send(s.to_string()).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "ipc client disconnected",
                    )
                })
            }
        }
    }

    pub async fn finish(&mut self) -> std::io::Result<()> {
        match self {
            Sink::Stdout(o) => o.flush().await,
            Sink::File(f) => f.flush().await,
            Sink::Channel(_) => Ok(()),
        }
    }
}

/// Wraps incremental writes that may include reasoning text and answer text,
/// emitting `<think>...</think>` tags around the reasoning portion (when
/// enabled) and writing both to a sink and an in-memory mirror buffer used for
/// caching.
pub struct ThinkWriter {
    enabled: bool,
    open: bool,
    closed: bool,
}

impl ThinkWriter {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            open: false,
            closed: false,
        }
    }

    /// Returns true if a `<think>` tag was emitted as a side-effect.
    pub async fn write_reasoning(
        &mut self,
        sink: &mut Sink,
        mirror: &mut String,
        text: &str,
    ) -> Result<bool, String> {
        if !self.enabled || self.closed || text.is_empty() {
            return Ok(false);
        }
        let opened = if !self.open {
            self.open = true;
            sink.write_str("<think>")
                .await
                .map_err(|e| format!("write output: {}", e))?;
            mirror.push_str("<think>");
            true
        } else {
            false
        };
        sink.write_str(text)
            .await
            .map_err(|e| format!("write output: {}", e))?;
        mirror.push_str(text);
        Ok(opened)
    }

    pub async fn write_content(
        &mut self,
        sink: &mut Sink,
        mirror: &mut String,
        text: &str,
    ) -> Result<(), String> {
        if text.is_empty() {
            return Ok(());
        }
        self.close_if_open(sink, mirror).await?;
        sink.write_str(text)
            .await
            .map_err(|e| format!("write output: {}", e))?;
        mirror.push_str(text);
        Ok(())
    }

    /// Closes the think block if it was opened but not yet closed. Idempotent.
    pub async fn close_if_open(
        &mut self,
        sink: &mut Sink,
        mirror: &mut String,
    ) -> Result<(), String> {
        if self.open && !self.closed {
            sink.write_str("</think>\n")
                .await
                .map_err(|e| format!("write output: {}", e))?;
            mirror.push_str("</think>\n");
            self.closed = true;
        }
        Ok(())
    }
}

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
    Memory(String),
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

    pub fn memory() -> Self {
        Sink::Memory(String::new())
    }

    pub fn into_memory(self) -> Option<String> {
        match self {
            Sink::Memory(text) => Some(text),
            _ => None,
        }
    }

    pub async fn write_str(&mut self, s: &str) -> std::io::Result<()> {
        match self {
            Sink::Stdout(o) => {
                o.write_all(s.as_bytes()).await?;
                o.flush().await
            }
            Sink::File(f) => f.write_all(s.as_bytes()).await,
            Sink::Channel(tx) => tx.send(s.to_string()).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "ipc client disconnected")
            }),
            Sink::Memory(text) => {
                text.push_str(s);
                Ok(())
            }
        }
    }

    pub async fn finish(&mut self) -> std::io::Result<()> {
        match self {
            Sink::Stdout(o) => o.flush().await,
            Sink::File(f) => f.flush().await,
            Sink::Channel(_) => Ok(()),
            Sink::Memory(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::{self, error::TryRecvError};

    use super::*;

    #[tokio::test]
    async fn channel_sink_writes_each_chunk_and_finish_is_a_noop() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);

        sink.write_str("hello").await.unwrap();
        sink.write_str(" world").await.unwrap();
        sink.finish().await.unwrap();

        assert_eq!(rx.try_recv().unwrap(), "hello");
        assert_eq!(rx.try_recv().unwrap(), " world");
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test]
    async fn memory_sink_buffers_written_text() {
        let mut sink = Sink::memory();

        sink.write_str("hello").await.unwrap();
        sink.write_str(" world").await.unwrap();
        sink.finish().await.unwrap();

        assert_eq!(sink.into_memory().as_deref(), Some("hello world"));
    }

    #[test]
    fn error_sink_channel_emits_diagnostic_text() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = ErrSink::Channel(tx);

        sink.emit("stats\n");

        assert_eq!(rx.try_recv().unwrap(), "stats\n");
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn server_error_sink_respects_forwarding_flag() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = ErrSink::Server {
            tx,
            forward_to_client: false,
            quiet: true,
            conn_id: 7,
        };

        sink.emit("hidden from client\n");

        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }
}

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use interprocess::local_socket::ListenerOptions;
use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::{GenericFilePath, ToFsName};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::args::{Args, ClientArgs, default_ipc_socket};
use crate::ipc::{Frame, Request, StatusInfo, read_json_line, write_json_line};
use crate::sink::{ErrSink, Sink};

static CONN_COUNTER: AtomicU64 = AtomicU64::new(0);

fn ts() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let ms = now.subsec_millis();
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms)
}

macro_rules! log {
    ($quiet:expr, $($arg:tt)*) => {
        if !$quiet {
            eprintln!("[{}] {}", ts(), format_args!($($arg)*));
        }
    };
}

pub async fn serve(server_args: Args) -> Result<(), String> {
    if server_args.key.is_none() && std::env::var("OPENAI_API_KEY").is_err() {
        return Err("--serve requires an API key (--key or OPENAI_API_KEY)".to_string());
    }
    let socket_path: PathBuf = server_args
        .ipc_path
        .clone()
        .unwrap_or_else(default_ipc_socket);

    // Best-effort cleanup of a stale socket file from a prior crash.
    let _ = std::fs::remove_file(&socket_path);

    let name = socket_path
        .as_path()
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| format!("invalid socket path: {}", e))?;
    let listener = ListenerOptions::new()
        .name(name)
        .create_tokio()
        .map_err(|e| format!("bind socket {}: {}", socket_path.display(), e))?;

    eprintln!("mii-text serving on {}", socket_path.display());

    let quiet = server_args.quiet;
    let started_at = Instant::now();
    let socket_str = socket_path.display().to_string();
    let server_args = Arc::new(server_args);
    loop {
        let conn = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("accept error: {}", e);
                continue;
            }
        };
        let id = CONN_COUNTER.fetch_add(1, Ordering::Relaxed);
        log!(quiet, "#{} accepted", id);
        let sa = Arc::clone(&server_args);
        let socket_str = socket_str.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(sa, conn, id, quiet, started_at, socket_str).await {
                eprintln!("[{}] #{} connection error: {}", ts(), id, e);
            }
        });
    }
}

async fn handle_connection(
    server_args: Arc<Args>,
    conn: Stream,
    id: u64,
    quiet: bool,
    started_at: Instant,
    socket: String,
) -> std::io::Result<()> {
    let started = Instant::now();
    let (recv, mut send) = conn.split();
    let mut reader = BufReader::new(recv);

    let req: Request = match read_json_line(&mut reader).await? {
        Some(r) => r,
        None => {
            log!(quiet, "#{} closed without request", id);
            return Ok(());
        }
    };

    let (args, stdin) = match req {
        Request::Run { args, stdin } => (*args, stdin),
        Request::Status => {
            log!(quiet, "#{} status ping", id);
            let info = StatusInfo {
                pid: std::process::id(),
                uptime_ms: started_at.elapsed().as_millis() as u64,
                requests_served: CONN_COUNTER.load(Ordering::Relaxed),
                model: server_args.model.clone(),
                socket,
            };
            write_json_line(&mut send, &Frame::Status { info }).await?;
            write_json_line(
                &mut send,
                &Frame::Exit {
                    code: 0,
                    assistant: None,
                    provider_continuation: None,
                },
            )
            .await?;
            return Ok(());
        }
    };

    // Build per-request args by merging the client's overrides on top of the
    // server's defaults. Strip client-side-only fields (out, stateful) that
    // the server should not act on — those are handled by the client.
    let ClientArgs {
        model,
        stream,
        out: _,
        system,
        messages,
        quick,
        stateful: _,
        reasoning,
        stats,
        cache,
        temperature,
        max_tokens,
        reasoning_summary,
        tools,
        completions,
        simple,
    } = args;
    let merged_client = ClientArgs {
        model,
        stream,
        out: None,
        system,
        messages,
        quick,
        stateful: None,
        reasoning,
        stats,
        cache,
        temperature,
        max_tokens,
        reasoning_summary,
        tools,
        completions,
        simple,
    };
    let client_wants_stats = merged_client.stats;

    let mut effective: Args = clone_args(&server_args);
    // Cache and stateful are server-side-only / client-side-only respectively;
    // the merge wipes any server-side stateful since the client will manage it
    // locally.
    effective.out = None;
    effective.stateful = None;
    effective.merge_client(merged_client);

    // The server always computes stats so it can log them; whether they are
    // also forwarded to the client depends on the client's --stats flag.
    effective.stats = true;

    log!(
        quiet,
        "#{} request model={} reasoning={} stream={} cache={}",
        id,
        effective.model.as_deref().unwrap_or("<unset>"),
        effective.reasoning.as_deref().unwrap_or("<none>"),
        effective.stream,
        effective.cache.is_some(),
    );

    let (text_tx, mut text_rx) = mpsc::unbounded_channel::<String>();
    let (err_tx, mut err_rx) = mpsc::unbounded_channel::<String>();

    let stdin_override = Some(stdin);

    // Run the request and the frame-forwarder in parallel: the run task
    // produces stdout/stderr text via the channels; the forwarder drains them
    // and writes Frame::Stdout / Frame::Stderr to the socket. The sink and
    // error sink are scoped to this block so they (and thus the channel
    // senders) are dropped before we drain remaining buffered messages.
    //
    // We also poll the read half for EOF: if the client disconnects mid-
    // generation (especially in the non-streaming case where no Frame writes
    // happen until the provider returns), `fill_buf` resolves with an empty
    // slice and we abort the in-flight request by dropping `run_fut`. This
    // cancels the underlying reqwest call so we don't burn tokens for a
    // client that's already gone.
    let outcome = {
        let mut sink = Sink::channel(text_tx);
        let err_sink = ErrSink::Server {
            tx: err_tx,
            forward_to_client: client_wants_stats,
            quiet,
            conn_id: id,
        };
        let mut run_fut = Box::pin(crate::run(&effective, &mut sink, &err_sink, stdin_override));
        let mut client_alive = true;
        loop {
            tokio::select! {
                biased;
                text = text_rx.recv() => if let Some(t) = text {
                    if write_json_line(&mut send, &Frame::Stdout { text: t }).await.is_err() {
                        log!(quiet, "#{} client gone (write failed); cancelling generation", id);
                        return Ok(());
                    }
                },
                err = err_rx.recv() => if let Some(t) = err {
                    if write_json_line(&mut send, &Frame::Stderr { text: t }).await.is_err() {
                        log!(quiet, "#{} client gone (write failed); cancelling generation", id);
                        return Ok(());
                    }
                },
                peek = reader.fill_buf(), if client_alive => match peek {
                    Ok(buf) if buf.is_empty() => {
                        log!(quiet, "#{} client disconnected; cancelling generation", id);
                        return Ok(());
                    }
                    Ok(_) => {
                        // Unexpected post-request bytes from the client; ignore
                        // them but stop polling so we don't spin.
                        client_alive = false;
                    }
                    Err(_) => {
                        log!(quiet, "#{} client read error; cancelling generation", id);
                        return Ok(());
                    }
                },
                r = &mut run_fut => break r,
            }
        }
    };

    while let Ok(t) = text_rx.try_recv() {
        write_json_line(&mut send, &Frame::Stdout { text: t }).await?;
    }
    while let Ok(t) = err_rx.try_recv() {
        write_json_line(&mut send, &Frame::Stderr { text: t }).await?;
    }

    let (code, assistant, provider_continuation) = match outcome {
        Ok(o) => (o.exit_code, Some(o.assistant_buf), o.provider_continuation),
        Err((code, msg)) => {
            write_json_line(
                &mut send,
                &Frame::Stderr {
                    text: format!("{}\n", msg),
                },
            )
            .await?;
            (code, None, None)
        }
    };
    write_json_line(
        &mut send,
        &Frame::Exit {
            code,
            assistant,
            provider_continuation,
        },
    )
    .await?;
    log!(
        quiet,
        "#{} done code={} elapsed={}ms",
        id,
        code,
        started.elapsed().as_millis()
    );
    Ok(())
}

/// `Args` does not derive `Clone` (it carries non-clonable runtime fields in
/// principle), so manually shallow-clone it for per-connection effective args.
fn clone_args(a: &Args) -> Args {
    Args {
        key: a.key.clone(),
        url: a.url.clone(),
        model: a.model.clone(),
        stream: a.stream,
        out: a.out.clone(),
        system: a.system.clone(),
        messages: a.messages.clone(),
        quick: a.quick,
        stateful: a.stateful.clone(),
        reasoning: a.reasoning.clone(),
        stats: a.stats,
        cache: a.cache.clone(),
        temperature: a.temperature,
        max_tokens: a.max_tokens,
        reasoning_summary: a.reasoning_summary,
        tools: a.tools.clone(),
        completions: a.completions,
        simple: a.simple,
        serve: false,
        ipc: false,
        ipc_path: None,
        status: false,
        quiet: a.quiet,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::tools::ToolSource;

    #[test]
    fn clone_args_preserves_defaults_but_strips_runtime_modes() {
        let args = Args {
            key: Some("key".to_string()),
            url: Some("https://example.test/v1".to_string()),
            model: Some("model-a".to_string()),
            stream: true,
            out: Some(PathBuf::from("out.txt")),
            system: Some("system".to_string()),
            messages: Some("[]".to_string()),
            quick: true,
            stateful: Some(PathBuf::from("state.json")),
            reasoning: Some("low".to_string()),
            stats: true,
            cache: Some(PathBuf::from("cache.db")),
            temperature: Some(0.2),
            max_tokens: Some(123),
            reasoning_summary: true,
            tools: vec![ToolSource::Inline(
                "{\"name\":\"echo\",\"input_schema\":{\"type\":\"object\"}}".to_string(),
            )],
            completions: true,
            simple: true,
            serve: true,
            ipc: true,
            ipc_path: Some(PathBuf::from("sock")),
            status: true,
            quiet: true,
        };

        let cloned = clone_args(&args);

        assert_eq!(cloned.key.as_deref(), Some("key"));
        assert_eq!(cloned.url.as_deref(), Some("https://example.test/v1"));
        assert_eq!(cloned.model.as_deref(), Some("model-a"));
        assert!(cloned.stream);
        assert_eq!(
            cloned.out.as_deref(),
            Some(PathBuf::from("out.txt").as_path())
        );
        assert_eq!(cloned.system.as_deref(), Some("system"));
        assert_eq!(cloned.messages.as_deref(), Some("[]"));
        assert!(cloned.quick);
        assert_eq!(
            cloned.stateful.as_deref(),
            Some(PathBuf::from("state.json").as_path())
        );
        assert_eq!(cloned.reasoning.as_deref(), Some("low"));
        assert!(cloned.stats);
        assert_eq!(
            cloned.cache.as_deref(),
            Some(PathBuf::from("cache.db").as_path())
        );
        assert_eq!(cloned.temperature, Some(0.2));
        assert_eq!(cloned.max_tokens, Some(123));
        assert!(cloned.reasoning_summary);
        assert_eq!(cloned.tools.len(), 1);
        assert!(cloned.completions);
        assert!(cloned.simple);
        assert!(!cloned.serve);
        assert!(!cloned.ipc);
        assert_eq!(cloned.ipc_path, None);
        assert!(!cloned.status);
        assert!(cloned.quiet);
    }
}

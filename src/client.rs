use std::path::{Path, PathBuf};

use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::{GenericFilePath, ToFsName};
use tokio::io::{AsyncWriteExt, BufReader};

use crate::args::{Args, default_ipc_socket};
use crate::conversation::{
    Message, load_input_messages, load_stateful, push_assistant_turn, read_stdin_to_string,
    save_stateful,
};
use crate::ipc::{Frame, Request, read_json_line, write_json_line};
use crate::sink::Sink;
use crate::tools;

pub async fn run_status(args: Args) -> Result<u8, String> {
    let socket_path: PathBuf = args.ipc_path.clone().unwrap_or_else(default_ipc_socket);

    let conn = connect(&socket_path).await?;
    let (recv, mut send) = conn.split();
    let mut reader = BufReader::new(recv);

    write_json_line(&mut send, &Request::Status)
        .await
        .map_err(|e| format!("send status request: {}", e))?;
    send.shutdown()
        .await
        .map_err(|e| format!("flush status request: {}", e))?;

    let mut exit_code: u8 = 1;
    while let Some(frame) = read_json_line::<Frame, _>(&mut reader)
        .await
        .map_err(|e| format!("read frame: {}", e))?
    {
        match frame {
            Frame::Status { info } => {
                println!("socket:           {}", info.socket);
                println!("pid:              {}", info.pid);
                println!("uptime:           {} ms", info.uptime_ms);
                println!("requests served:  {}", info.requests_served);
                println!(
                    "model:            {}",
                    info.model.as_deref().unwrap_or("<unset>")
                );
            }
            Frame::Exit { code, .. } => {
                exit_code = code;
                break;
            }
            Frame::Stdout { text } => print!("{}", text),
            Frame::Stderr { text } => eprint!("{}", text),
        }
    }
    Ok(exit_code)
}

pub async fn run_ipc(args: Args) -> Result<u8, String> {
    let socket_path: PathBuf = args.ipc_path.clone().unwrap_or_else(default_ipc_socket);

    // Resolve the full conversation locally (history + new turns) so the
    // server doesn't need access to the client's filesystem for stateful or
    // stdin handling.
    let history: Vec<Message> = match &args.stateful {
        Some(p) => load_stateful(p).await?,
        None => Vec::new(),
    };
    let stdin_buf = match &args.messages {
        Some(_) => String::new(),
        None => read_stdin_to_string()
            .await
            .map_err(|e| format!("failed to read stdin: {}", e))?,
    };
    let new_turns =
        load_input_messages(&args.messages, args.quick, Some(stdin_buf.as_str())).await?;

    let mut conversation = history;
    conversation.extend(new_turns);

    // Build the request: forward all overridable args except those handled
    // client-side (`stateful`, `out`); collapse messages/stdin into a single
    // pre-merged messages JSON.
    let mut client_args = args.to_client();
    client_args.stateful = None;
    client_args.out = None;
    client_args.quick = false;
    client_args.messages = Some(
        serde_json::to_string(&conversation).map_err(|e| format!("serialize messages: {}", e))?,
    );
    if !args.tools.is_empty() {
        let resolved = tools::resolve(&args.tools).await?.unwrap_or_default();
        client_args.tools = tools::resolved_sources(resolved);
    }

    let conn = connect(&socket_path).await?;
    let (recv, mut send) = conn.split();
    let mut reader = BufReader::new(recv);

    write_json_line(
        &mut send,
        &Request::Run {
            args: Box::new(client_args),
            stdin: String::new(),
        },
    )
    .await
    .map_err(|e| format!("send request: {}", e))?;
    send.shutdown()
        .await
        .map_err(|e| format!("flush request: {}", e))?;

    let mut sink = Sink::open(&args.out).await?;
    let mut stderr = tokio::io::stderr();
    let mut exit_code: u8 = 1;
    let mut assistant: Option<String> = None;
    let mut provider_continuation = None;

    while let Some(frame) = read_json_line::<Frame, _>(&mut reader)
        .await
        .map_err(|e| format!("read frame: {}", e))?
    {
        match frame {
            Frame::Stdout { text } => sink
                .write_str(&text)
                .await
                .map_err(|e| format!("write output: {}", e))?,
            Frame::Stderr { text } => {
                stderr
                    .write_all(text.as_bytes())
                    .await
                    .map_err(|e| format!("write stderr: {}", e))?;
            }
            Frame::Exit {
                code,
                assistant: a,
                provider_continuation: continuation,
            } => {
                exit_code = code;
                assistant = a;
                provider_continuation = continuation;
                break;
            }
            Frame::Status { .. } => {
                // Status frames are only expected from --status pings.
            }
        }
    }

    sink.finish()
        .await
        .map_err(|e| format!("flush output: {}", e))?;

    if let (Some(p), Some(text)) = (&args.stateful, assistant) {
        push_assistant_turn(&mut conversation, text, provider_continuation.as_ref())?;
        save_stateful(p, &conversation).await?;
    }

    Ok(exit_code)
}

async fn connect(path: &Path) -> Result<Stream, String> {
    let name = path
        .to_fs_name::<GenericFilePath>()
        .map_err(|e| format!("invalid socket path: {}", e))?;
    Stream::connect(name).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound => {
            format!(
                "connection {} unreachable (is `mii-text --serve` running?)",
                path.display()
            )
        }
        _ => format!("connect {}: {}", path.display(), e),
    })
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[tokio::test]
    async fn connect_reports_unreachable_socket_with_hint() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mii-text-missing-socket-{}-{unique}.sock",
            std::process::id()
        ));

        let err = connect(&path).await.unwrap_err();

        assert!(err.contains(&path.display().to_string()));
        assert!(err.contains("unreachable"));
        assert!(err.contains("mii-text --serve"));
    }
}

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::sink::Sink;
use crate::tools;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Prospect {
    pub reasoning: Option<String>,
    pub content: String,
    pub tool_calls: Vec<Value>,
}

pub struct OutputWriter {
    simple: bool,
    stream: bool,
    emit_reasoning: bool,
    prospect: Prospect,
    think_open: bool,
    think_closed: bool,
}

impl OutputWriter {
    pub fn with_reasoning(simple: bool, stream: bool, emit_reasoning: bool) -> Self {
        Self {
            simple,
            stream,
            emit_reasoning,
            prospect: Prospect::default(),
            think_open: false,
            think_closed: false,
        }
    }

    pub async fn push_reasoning(
        &mut self,
        sink: &mut Sink,
        mirror: &mut String,
        text: &str,
    ) -> Result<bool, String> {
        if text.is_empty() {
            return Ok(false);
        }
        self.prospect
            .reasoning
            .get_or_insert_with(String::new)
            .push_str(text);

        if !self.emit_reasoning {
            return Ok(true);
        }

        if self.simple && self.stream {
            let opened = if !self.think_open && !self.think_closed {
                self.think_open = true;
                write_raw(sink, mirror, "<think>").await?;
                true
            } else {
                false
            };
            write_raw(sink, mirror, text).await?;
            return Ok(opened);
        }

        if self.stream {
            write_jsonl(
                sink,
                mirror,
                json!({ "type": "reasoning_delta", "delta": text }),
            )
            .await?;
        }
        Ok(true)
    }

    pub async fn push_content(
        &mut self,
        sink: &mut Sink,
        mirror: &mut String,
        text: &str,
    ) -> Result<bool, String> {
        if text.is_empty() {
            return Ok(false);
        }
        self.prospect.content.push_str(text);

        if self.simple && self.stream {
            self.close_think_if_open(sink, mirror).await?;
            write_raw(sink, mirror, text).await?;
            return Ok(true);
        }

        if self.stream {
            write_jsonl(
                sink,
                mirror,
                json!({ "type": "content_delta", "delta": text }),
            )
            .await?;
        }
        Ok(true)
    }

    pub fn set_tool_calls(&mut self, tool_calls: Vec<Value>) {
        self.prospect.tool_calls = tool_calls;
    }

    pub async fn finish(&mut self, sink: &mut Sink, mirror: &mut String) -> Result<(), String> {
        let prospect = self.visible_prospect();
        if self.simple {
            if self.stream {
                self.close_think_if_open(sink, mirror).await?;
            } else {
                let text = simple_text(&prospect)?;
                if !text.is_empty() {
                    write_raw(sink, mirror, &text).await?;
                }
                return Ok(());
            }
            if prospect.content.trim().is_empty() && !prospect.tool_calls.is_empty() {
                let text = tools::format_tool_calls(&prospect.tool_calls)?;
                write_raw(sink, mirror, &text).await?;
            }
            return Ok(());
        }

        if self.stream {
            if !prospect.tool_calls.is_empty() {
                write_jsonl(
                    sink,
                    mirror,
                    json!({
                        "type": "tool_calls",
                        "tool_calls": prospect.tool_calls,
                    }),
                )
                .await?;
            }
            write_jsonl(
                sink,
                mirror,
                json!({
                    "type": "done",
                    "reasoning": prospect.reasoning,
                    "content": prospect.content,
                    "tool_calls": prospect.tool_calls,
                }),
            )
            .await?;
            return Ok(());
        }

        write_raw(sink, mirror, &render_structured(&prospect)?).await
    }

    pub fn prospect(&self) -> &Prospect {
        &self.prospect
    }

    fn visible_prospect(&self) -> Prospect {
        visible_prospect(&self.prospect, self.emit_reasoning)
    }
}

pub fn render_cached(
    prospect: &Prospect,
    simple: bool,
    stream: bool,
    emit_reasoning: bool,
) -> Result<String, String> {
    let prospect = visible_prospect(prospect, emit_reasoning);
    if simple {
        return simple_text(&prospect);
    }
    if stream {
        return render_jsonl(&prospect);
    }
    render_structured(&prospect)
}

fn visible_prospect(prospect: &Prospect, emit_reasoning: bool) -> Prospect {
    let mut out = prospect.clone();
    if !emit_reasoning {
        out.reasoning = None;
    }
    out
}

fn simple_text(prospect: &Prospect) -> Result<String, String> {
    let mut text = String::new();
    if let Some(reasoning) = &prospect.reasoning {
        text.push_str("<think>");
        text.push_str(reasoning);
        text.push_str("</think>\n");
    }
    if prospect.content.trim().is_empty() && !prospect.tool_calls.is_empty() {
        text.push_str(&tools::format_tool_calls(&prospect.tool_calls)?);
    } else {
        text.push_str(&prospect.content);
    }
    Ok(text)
}

fn render_jsonl(prospect: &Prospect) -> Result<String, String> {
    let mut text = String::new();
    if let Some(reasoning) = &prospect.reasoning
        && !reasoning.is_empty()
    {
        push_jsonl(
            &mut text,
            json!({ "type": "reasoning_delta", "delta": reasoning }),
        )?;
    }
    if !prospect.content.is_empty() {
        push_jsonl(
            &mut text,
            json!({ "type": "content_delta", "delta": prospect.content }),
        )?;
    }
    if !prospect.tool_calls.is_empty() {
        push_jsonl(
            &mut text,
            json!({ "type": "tool_calls", "tool_calls": prospect.tool_calls }),
        )?;
    }
    push_jsonl(
        &mut text,
        json!({
            "type": "done",
            "reasoning": prospect.reasoning,
            "content": prospect.content,
            "tool_calls": prospect.tool_calls,
        }),
    )?;
    Ok(text)
}

fn render_structured(prospect: &Prospect) -> Result<String, String> {
    let mut text =
        serde_json::to_string_pretty(prospect).map_err(|e| format!("serialize output: {}", e))?;
    text.push('\n');
    Ok(text)
}

fn push_jsonl(out: &mut String, value: Value) -> Result<(), String> {
    let line = serde_json::to_string(&value).map_err(|e| format!("serialize output: {}", e))?;
    out.push_str(&line);
    out.push('\n');
    Ok(())
}

async fn write_raw(sink: &mut Sink, mirror: &mut String, text: &str) -> Result<(), String> {
    sink.write_str(text)
        .await
        .map_err(|e| format!("write output: {}", e))?;
    mirror.push_str(text);
    Ok(())
}

async fn write_jsonl(sink: &mut Sink, mirror: &mut String, value: Value) -> Result<(), String> {
    let mut text = serde_json::to_string(&value).map_err(|e| format!("serialize output: {}", e))?;
    text.push('\n');
    write_raw(sink, mirror, &text).await
}

impl OutputWriter {
    async fn close_think_if_open(
        &mut self,
        sink: &mut Sink,
        mirror: &mut String,
    ) -> Result<(), String> {
        if self.think_open && !self.think_closed {
            write_raw(sink, mirror, "</think>\n").await?;
            self.think_closed = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::mpsc::{self, error::TryRecvError};

    use super::*;

    #[test]
    fn structured_prospect_serializes_with_expected_fields() {
        let prospect = Prospect {
            reasoning: Some("because".to_string()),
            content: "hello".to_string(),
            tool_calls: vec![json!({ "name": "echo" })],
        };
        let value = serde_json::to_value(prospect).unwrap();

        assert_eq!(value["reasoning"], "because");
        assert_eq!(value["content"], "hello");
        assert_eq!(value["tool_calls"][0]["name"], "echo");
    }

    #[tokio::test]
    async fn writes_structured_json_object_when_not_streaming() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(false, false, true);

        writer
            .push_content(&mut sink, &mut mirror, "hello")
            .await
            .unwrap();
        writer.finish(&mut sink, &mut mirror).await.unwrap();

        let text = rx.try_recv().unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["reasoning"], Value::Null);
        assert_eq!(value["content"], "hello");
        assert_eq!(value["tool_calls"], json!([]));
        assert_eq!(mirror, text);
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test]
    async fn writes_structured_jsonl_when_streaming() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(false, true, true);

        writer
            .push_content(&mut sink, &mut mirror, "hel")
            .await
            .unwrap();
        writer
            .push_content(&mut sink, &mut mirror, "lo")
            .await
            .unwrap();
        writer.finish(&mut sink, &mut mirror).await.unwrap();

        let first: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        let second: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(first["type"], "content_delta");
        assert_eq!(first["delta"], "hel");
        assert_eq!(second["type"], "content_delta");
        assert_eq!(second["delta"], "lo");
        let done: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(done["type"], "done");
        assert_eq!(done["content"], "hello");
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test]
    async fn simple_mode_preserves_plain_text_output() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(true, true, true);

        writer
            .push_reasoning(&mut sink, &mut mirror, "thinking")
            .await
            .unwrap();
        writer
            .push_content(&mut sink, &mut mirror, "answer")
            .await
            .unwrap();
        writer.finish(&mut sink, &mut mirror).await.unwrap();

        assert_eq!(rx.try_recv().unwrap(), "<think>");
        assert_eq!(rx.try_recv().unwrap(), "thinking");
        assert_eq!(rx.try_recv().unwrap(), "</think>\n");
        assert_eq!(rx.try_recv().unwrap(), "answer");
        assert_eq!(mirror, "<think>thinking</think>\nanswer");
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test]
    async fn simple_mode_opens_think_block_only_once() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(true, true, true);

        assert!(
            writer
                .push_reasoning(&mut sink, &mut mirror, "first")
                .await
                .unwrap()
        );
        assert!(
            !writer
                .push_reasoning(&mut sink, &mut mirror, " second")
                .await
                .unwrap()
        );
        writer
            .push_content(&mut sink, &mut mirror, "answer")
            .await
            .unwrap();

        assert_eq!(rx.try_recv().unwrap(), "<think>");
        assert_eq!(rx.try_recv().unwrap(), "first");
        assert_eq!(rx.try_recv().unwrap(), " second");
        assert_eq!(rx.try_recv().unwrap(), "</think>\n");
        assert_eq!(rx.try_recv().unwrap(), "answer");
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(mirror, "<think>first second</think>\nanswer");
    }

    #[tokio::test]
    async fn simple_mode_writes_tool_calls_when_content_is_empty() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(true, false, true);

        writer.set_tool_calls(vec![json!({
            "type": "function_call",
            "name": "echo",
            "arguments": "{\"message\":\"hello\"}"
        })]);
        writer.finish(&mut sink, &mut mirror).await.unwrap();

        let text = rx.try_recv().unwrap();
        let calls: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(calls[0]["name"], "echo");
        assert_eq!(mirror, text);
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test]
    async fn simple_mode_does_not_append_tool_calls_after_text_content() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(true, false, true);

        writer
            .push_content(&mut sink, &mut mirror, "answer")
            .await
            .unwrap();
        writer.set_tool_calls(vec![json!({ "call_id": "call_1" })]);
        writer.finish(&mut sink, &mut mirror).await.unwrap();

        assert_eq!(rx.try_recv().unwrap(), "answer");
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(mirror, "answer");
    }

    #[tokio::test]
    async fn simple_non_streaming_defers_reasoning_until_finish() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(true, false, true);

        writer
            .push_reasoning(&mut sink, &mut mirror, "thinking")
            .await
            .unwrap();
        writer
            .push_content(&mut sink, &mut mirror, "answer")
            .await
            .unwrap();
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));

        writer.finish(&mut sink, &mut mirror).await.unwrap();

        assert_eq!(rx.try_recv().unwrap(), "<think>thinking</think>\nanswer");
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(mirror, "<think>thinking</think>\nanswer");
    }

    #[tokio::test]
    async fn structured_stream_writes_tool_calls_before_done() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(false, true, true);

        writer
            .push_reasoning(&mut sink, &mut mirror, "because")
            .await
            .unwrap();
        writer.set_tool_calls(vec![json!({ "call_id": "call_1" })]);
        writer.finish(&mut sink, &mut mirror).await.unwrap();

        let reasoning: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        let calls: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        let done: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(reasoning["type"], "reasoning_delta");
        assert_eq!(calls["type"], "tool_calls");
        assert_eq!(calls["tool_calls"][0]["call_id"], "call_1");
        assert_eq!(done["type"], "done");
        assert_eq!(done["reasoning"], "because");
        assert_eq!(done["tool_calls"][0]["call_id"], "call_1");
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test]
    async fn prospect_tracks_reasoning_content_and_tool_calls_before_finish() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(false, false, true);

        writer
            .push_reasoning(&mut sink, &mut mirror, "because")
            .await
            .unwrap();
        writer
            .push_content(&mut sink, &mut mirror, "answer")
            .await
            .unwrap();
        writer.set_tool_calls(vec![json!({ "call_id": "call_1" })]);

        let prospect = writer.prospect();
        assert_eq!(prospect.reasoning.as_deref(), Some("because"));
        assert_eq!(prospect.content, "answer");
        assert_eq!(prospect.tool_calls[0]["call_id"], "call_1");
        assert!(mirror.is_empty());
    }

    #[test]
    fn cached_rendering_reuses_one_prospect_for_multiple_output_modes() {
        let prospect = Prospect {
            reasoning: Some("because".to_string()),
            content: "answer".to_string(),
            tool_calls: vec![json!({ "call_id": "call_1" })],
        };

        let structured: Value =
            serde_json::from_str(&render_cached(&prospect, false, false, true).unwrap()).unwrap();
        assert_eq!(structured["reasoning"], "because");
        assert_eq!(structured["content"], "answer");

        let hidden: Value =
            serde_json::from_str(&render_cached(&prospect, false, false, false).unwrap()).unwrap();
        assert_eq!(hidden["reasoning"], Value::Null);

        let simple = render_cached(&prospect, true, false, true).unwrap();
        assert_eq!(simple, "<think>because</think>\nanswer");

        let jsonl = render_cached(&prospect, false, true, true).unwrap();
        let lines = jsonl
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(lines[0]["type"], "reasoning_delta");
        assert_eq!(lines[1]["type"], "content_delta");
        assert_eq!(lines[2]["type"], "tool_calls");
        assert_eq!(lines[3]["type"], "done");
    }
}

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::sink::Sink;
use crate::tools;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Prospect {
    pub reasoning: Option<String>,
    pub content: String,
    pub tool_calls: Vec<Value>,
    pub provider_continuation: Option<ProviderContinuation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constrained: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderContinuation {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default)]
    pub reasoning_items: Vec<Value>,
}

impl ProviderContinuation {
    pub fn stream_event(&self) -> Value {
        let mut value =
            serde_json::to_value(self).expect("provider continuation serializes as json object");
        let object = value
            .as_object_mut()
            .expect("provider continuation serializes as json object");
        object.insert(
            "type".to_string(),
            Value::String("provider_continuation".to_string()),
        );
        value
    }
}

pub struct OutputWriter {
    simple: bool,
    stream: bool,
    emit_reasoning: bool,
    prospect: Prospect,
    events: Vec<Value>,
    final_events_recorded: bool,
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
            events: Vec::new(),
            final_events_recorded: false,
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
        self.events
            .push(json!({ "type": "reasoning_delta", "delta": text }));

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
        self.events
            .push(json!({ "type": "content_delta", "delta": text }));

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

    pub fn set_provider_continuation(&mut self, continuation: ProviderContinuation) {
        self.prospect.provider_continuation = Some(continuation);
    }

    pub async fn finish(&mut self, sink: &mut Sink, mirror: &mut String) -> Result<(), String> {
        self.record_final_events();
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
            if let Some(continuation) = &prospect.provider_continuation {
                write_jsonl(sink, mirror, continuation.stream_event()).await?;
            }
            write_jsonl(sink, mirror, done_event(&prospect)).await?;
            return Ok(());
        }

        write_raw(sink, mirror, &render_structured(&prospect)?).await
    }

    pub fn prospect(&self) -> &Prospect {
        &self.prospect
    }

    pub fn events(&self) -> &[Value] {
        &self.events
    }

    fn visible_prospect(&self) -> Prospect {
        visible_prospect(&self.prospect, self.emit_reasoning)
    }

    fn record_final_events(&mut self) {
        if self.final_events_recorded {
            return;
        }
        self.final_events_recorded = true;
        if !self.prospect.tool_calls.is_empty() {
            self.events.push(json!({
                "type": "tool_calls",
                "tool_calls": self.prospect.tool_calls.clone(),
            }));
        }
        if let Some(continuation) = &self.prospect.provider_continuation {
            self.events.push(continuation.stream_event());
        }
    }
}

pub fn render_cached(
    prospect: &Prospect,
    events: Option<&[Value]>,
    simple: bool,
    stream: bool,
    emit_reasoning: bool,
) -> Result<String, String> {
    let prospect = visible_prospect(prospect, emit_reasoning);
    if simple {
        return simple_text(&prospect);
    }
    if stream {
        if let Some(events) = events
            && !events.is_empty()
        {
            return render_jsonl_events(events, &prospect, emit_reasoning);
        }
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
    if let Some(constrained) = &prospect.constrained {
        let mut text = serde_json::to_string_pretty(constrained)
            .map_err(|e| format!("serialize constrained output: {}", e))?;
        text.push('\n');
        return Ok(text);
    }

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
    push_jsonl(&mut text, done_event(prospect))?;
    Ok(text)
}

fn render_jsonl_events(
    events: &[Value],
    prospect: &Prospect,
    emit_reasoning: bool,
) -> Result<String, String> {
    let mut text = String::new();
    for event in events {
        if event.get("type").and_then(Value::as_str) == Some("reasoning_delta") && !emit_reasoning {
            continue;
        }
        push_jsonl(&mut text, event.clone())?;
    }
    push_jsonl(&mut text, done_event(prospect))?;
    Ok(text)
}

fn render_structured(prospect: &Prospect) -> Result<String, String> {
    let mut text =
        serde_json::to_string_pretty(prospect).map_err(|e| format!("serialize output: {}", e))?;
    text.push('\n');
    Ok(text)
}

fn done_event(prospect: &Prospect) -> Value {
    let mut event = json!({
        "type": "done",
        "reasoning": prospect.reasoning,
        "content": prospect.content,
        "tool_calls": prospect.tool_calls,
        "provider_continuation": prospect.provider_continuation,
    });
    if let Some(constrained) = &prospect.constrained {
        event["constrained"] = constrained.clone();
    }
    event
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
            provider_continuation: None,
            constrained: None,
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
    async fn simple_stream_does_not_append_tool_calls_after_text_content() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(true, true, true);

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
    async fn simple_stream_without_content_or_tool_calls_writes_nothing() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(true, true, true);

        writer.finish(&mut sink, &mut mirror).await.unwrap();

        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        assert!(mirror.is_empty());
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
        assert_eq!(writer.events().len(), 2);
        assert_eq!(writer.events()[0]["type"], "reasoning_delta");
        assert_eq!(writer.events()[1]["type"], "tool_calls");
        assert_eq!(writer.events()[1]["tool_calls"][0]["call_id"], "call_1");
    }

    #[tokio::test]
    async fn structured_stream_writes_provider_continuation_before_done() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(false, true, true);

        writer.set_provider_continuation(ProviderContinuation {
            provider: "openai".to_string(),
            response_id: Some("resp_1".to_string()),
            reasoning_items: vec![json!({
                "type": "reasoning",
                "encrypted_content": "opaque"
            })],
        });
        writer.finish(&mut sink, &mut mirror).await.unwrap();

        let continuation: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        let done: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(continuation["type"], "provider_continuation");
        assert_eq!(continuation["provider"], "openai");
        assert_eq!(
            continuation["reasoning_items"][0]["encrypted_content"],
            "opaque"
        );
        assert_eq!(done["type"], "done");
        assert_eq!(done["provider_continuation"]["response_id"], "resp_1");
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(writer.events().len(), 1);
        assert_eq!(writer.events()[0]["type"], "provider_continuation");
    }

    #[tokio::test]
    async fn final_events_are_recorded_only_once() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut sink = Sink::channel(tx);
        let mut mirror = String::new();
        let mut writer = OutputWriter::with_reasoning(false, true, true);

        writer.set_tool_calls(vec![json!({ "call_id": "call_1" })]);
        writer.finish(&mut sink, &mut mirror).await.unwrap();
        writer.finish(&mut sink, &mut mirror).await.unwrap();

        let tool_events = writer
            .events()
            .iter()
            .filter(|event| event.get("type").and_then(Value::as_str) == Some("tool_calls"))
            .count();
        assert_eq!(tool_events, 1);
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
            provider_continuation: None,
            constrained: None,
        };

        let structured: Value =
            serde_json::from_str(&render_cached(&prospect, None, false, false, true).unwrap())
                .unwrap();
        assert_eq!(structured["reasoning"], "because");
        assert_eq!(structured["content"], "answer");

        let hidden: Value =
            serde_json::from_str(&render_cached(&prospect, None, false, false, false).unwrap())
                .unwrap();
        assert_eq!(hidden["reasoning"], Value::Null);

        let simple = render_cached(&prospect, None, true, false, true).unwrap();
        assert_eq!(simple, "<think>because</think>\nanswer");

        let jsonl = render_cached(&prospect, None, false, true, true).unwrap();
        let lines = jsonl
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(lines[0]["type"], "reasoning_delta");
        assert_eq!(lines[1]["type"], "content_delta");
        assert_eq!(lines[2]["type"], "tool_calls");
        assert_eq!(lines[3]["type"], "done");
    }

    #[test]
    fn cached_stream_rendering_replays_event_chunks_losslessly() {
        let prospect = Prospect {
            reasoning: Some("because".to_string()),
            content: "answer".to_string(),
            tool_calls: Vec::new(),
            provider_continuation: None,
            constrained: None,
        };
        let events = vec![
            json!({ "type": "reasoning_delta", "delta": "because" }),
            json!({ "type": "content_delta", "delta": "ans" }),
            json!({ "type": "content_delta", "delta": "wer" }),
        ];

        let jsonl = render_cached(&prospect, Some(&events), false, true, false).unwrap();
        let lines = jsonl
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], json!({ "type": "content_delta", "delta": "ans" }));
        assert_eq!(lines[1], json!({ "type": "content_delta", "delta": "wer" }));
        assert_eq!(lines[2]["type"], "done");
        assert_eq!(lines[2]["reasoning"], Value::Null);
    }

    #[test]
    fn simple_rendering_prefers_constrained_output_when_present() {
        let prospect = Prospect {
            reasoning: Some("because".to_string()),
            content: "draft".to_string(),
            tool_calls: Vec::new(),
            provider_continuation: None,
            constrained: Some(json!({ "title": "Brazil" })),
        };

        let simple = render_cached(&prospect, None, true, false, true).unwrap();

        assert_eq!(simple, "{\n  \"title\": \"Brazil\"\n}\n");
    }
}

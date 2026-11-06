//! The Messages API's server-sent events (`stream: true`): a parser that folds the event stream
//! into the same message object a non-streaming reply carries, and the loop that reads a body
//! under an IDLE timeout.
//!
//! Why the API leg streams. A fixed total timeout (600 seconds) around one non-streaming request
//! is wrong for the replies this product asks for: a reply that really uses a large `max_tokens`
//! takes minutes, and a timeout is then a transport failure that is retried, a second payment for
//! the same answer. A stream shows progress as it goes, so the only thing that can fail a healthy
//! call is silence: the idle timeout (no bytes at all for the window) governs, never the total
//! time. A stream that dies late has already delivered text, and that text is salvaged (the CLI
//! leg does the same) instead of being thrown away with the connection.
//!
//! Everything here is pure or runs against a [`ChunkSource`], so the whole path is tested without
//! a network: a canned body, a source that stalls, a source that breaks.

use std::time::Duration;

use serde_json::{json, Value};

/// Folds Messages-API stream events into one message.
///
/// Events handled: `message_start` (model and the input-side usage), `content_block_delta` (a
/// `text_delta` is reply text; thinking, signature and tool-input deltas are not text and are
/// ignored), `message_delta` (the stop reason and the final output token count), `message_stop`
/// and `error`. Everything else (`ping`, block start and stop) carries nothing the reply needs.
#[derive(Debug, Default)]
pub(crate) struct SseAccumulator {
    /// Bytes of a line not yet terminated.
    pending: Vec<u8>,
    text: String,
    model: Option<String>,
    input_tokens: Option<u64>,
    cache_read: u64,
    cache_creation: u64,
    cache_creation_1h: Option<u64>,
    /// `message_start` reports a small running figure; `message_delta` the final one.
    start_output_tokens: Option<u64>,
    final_output_tokens: Option<u64>,
    stop_reason: Option<String>,
    stopped: bool,
    error: Option<(String, String)>,
}

impl SseAccumulator {
    /// Feed the next chunk of the body. Returns the reply text that arrived in it, in order, so
    /// the caller can forward each piece as it lands.
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(chunk);
        let mut deltas = Vec::new();
        while let Some(newline) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            // Only `data:` lines carry an event; `event:` names the same type the JSON repeats.
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<Value>(payload.trim_start()) else {
                continue;
            };
            if let Some(delta) = self.event(&event) {
                deltas.push(delta);
            }
        }
        deltas
    }

    /// Apply one decoded event; returns the reply text it carried, if any.
    fn event(&mut self, event: &Value) -> Option<String> {
        match event["type"].as_str()? {
            "message_start" => {
                let m = &event["message"];
                self.model = m["model"].as_str().map(str::to_string);
                let u = &m["usage"];
                self.input_tokens = u["input_tokens"].as_u64();
                self.cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                self.cache_creation = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                self.cache_creation_1h = u["cache_creation"]["ephemeral_1h_input_tokens"].as_u64();
                self.start_output_tokens = u["output_tokens"].as_u64();
                None
            }
            "content_block_delta" => {
                let d = &event["delta"];
                if d["type"].as_str() == Some("text_delta") {
                    let t = d["text"].as_str()?;
                    self.text.push_str(t);
                    return Some(t.to_string());
                }
                None
            }
            "message_delta" => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.to_string());
                }
                let u = &event["usage"];
                if let Some(o) = u["output_tokens"].as_u64() {
                    self.final_output_tokens = Some(o);
                }
                // The final usage may restate the input side; the latest figure wins.
                if let Some(i) = u["input_tokens"].as_u64() {
                    self.input_tokens = Some(i);
                }
                if let Some(c) = u["cache_read_input_tokens"].as_u64() {
                    self.cache_read = c;
                }
                if let Some(c) = u["cache_creation_input_tokens"].as_u64() {
                    self.cache_creation = c;
                }
                None
            }
            "message_stop" => {
                self.stopped = true;
                None
            }
            "error" => {
                let e = &event["error"];
                self.error = Some((
                    e["type"].as_str().unwrap_or("error").to_string(),
                    e["message"].as_str().unwrap_or("").to_string(),
                ));
                None
            }
            _ => None,
        }
    }

    /// True once the model finished its reply: it reported a stop reason or the stream said stop.
    pub(crate) fn is_finished(&self) -> bool {
        self.stopped || self.stop_reason.is_some()
    }

    /// True when any reply text has arrived.
    pub(crate) fn has_text(&self) -> bool {
        !self.text.is_empty()
    }

    /// The provider's `error` event, if the stream carried one: `(type, message)`.
    pub(crate) fn provider_error(&self) -> Option<&(String, String)> {
        self.error.as_ref()
    }

    /// The folded message, shaped like a non-streaming reply (`content`, `usage`, `stop_reason`,
    /// `model`), so one parser reads both ([`crate::llm`]'s `messages_response`).
    ///
    /// `salvage_output_tokens` is for a stream that died before the final usage arrived: the
    /// output the call was billed is then unknown, and the figure given (the request's cap, the
    /// most it could have generated) is used so a spend limit never under-counts. A stream that
    /// delivered its final usage uses that.
    pub(crate) fn message(&self, salvage_output_tokens: Option<u64>) -> Value {
        let finished = self.is_finished();
        let output = self.final_output_tokens.or(if finished {
            self.start_output_tokens
        } else {
            salvage_output_tokens.or(self.start_output_tokens)
        });
        let mut usage = json!({
            "cache_read_input_tokens": self.cache_read,
            "cache_creation_input_tokens": self.cache_creation,
        });
        if let Some(i) = self.input_tokens {
            usage["input_tokens"] = json!(i);
        }
        if let Some(o) = output {
            usage["output_tokens"] = json!(o);
        }
        if let Some(h) = self.cache_creation_1h {
            usage["cache_creation"] = json!({ "ephemeral_1h_input_tokens": h });
        }
        // A stream that stopped without a reason was cut: report what a reply cut off at the
        // limit reports, so the stages' own cut-off handling (salvage the complete entries,
        // ask for the rest) applies to it unchanged.
        let stop_reason = self
            .stop_reason
            .clone()
            .unwrap_or_else(|| "max_tokens".to_string());
        let mut message = json!({
            "content": [{ "type": "text", "text": self.text }],
            "usage": usage,
            "stop_reason": stop_reason,
        });
        if let Some(m) = &self.model {
            message["model"] = json!(m);
        }
        message
    }
}

/// A body the stream is read from, one chunk at a time: a `reqwest::Response` in production, a
/// scripted source in a test.
#[async_trait::async_trait]
pub(crate) trait ChunkSource: Send {
    /// The next chunk of the body; `Ok(None)` at a clean end of body.
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String>;
}

/// How reading the stream ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StreamEnd {
    /// `message_stop` arrived, or the body closed after the model reported a stop reason.
    Finished,
    /// No bytes at all for the idle window.
    Stalled(Duration),
    /// The body broke (a connection error) or closed before the model finished, or the provider
    /// sent an `error` event; the string says which.
    Broken(String),
}

/// Read `src` into `acc` until the message stops, the body ends or breaks, or `idle` passes
/// with no bytes. Each text delta is passed to `on_delta` as it arrives. The idle window restarts
/// on every chunk, whatever it holds (a `ping` or a thinking delta is progress), so total time is
/// never limited, only silence.
pub(crate) async fn read_stream(
    src: &mut dyn ChunkSource,
    idle: Duration,
    acc: &mut SseAccumulator,
    on_delta: &mut (dyn FnMut(&str) + Send),
) -> StreamEnd {
    loop {
        match tokio::time::timeout(idle, src.next_chunk()).await {
            Err(_) => return StreamEnd::Stalled(idle),
            Ok(Err(e)) => return StreamEnd::Broken(e),
            Ok(Ok(None)) => {
                if acc.is_finished() {
                    return StreamEnd::Finished;
                }
                return StreamEnd::Broken(match acc.provider_error() {
                    Some((t, m)) => format!("the provider sent an error mid-stream ({t}): {m}"),
                    None => "the stream ended before the reply was finished".to_string(),
                });
            }
            Ok(Ok(Some(chunk))) => {
                for delta in acc.feed(&chunk) {
                    on_delta(&delta);
                }
                if let Some((t, m)) = acc.provider_error() {
                    return StreamEnd::Broken(format!(
                        "the provider sent an error mid-stream ({t}): {m}"
                    ));
                }
                if acc.stopped {
                    return StreamEnd::Finished;
                }
            }
        }
    }
}

/// Scripted bodies and event builders, shared with the transport's tests.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use std::collections::VecDeque;

    /// A scripted body: the chunks it yields, then either a clean end, a break, or silence.
    pub(crate) enum Ending {
        Clean,
        Break(&'static str),
        Silence,
    }

    pub(crate) struct Scripted {
        chunks: VecDeque<Vec<u8>>,
        ending: Ending,
    }

    impl Scripted {
        pub(crate) fn new(chunks: Vec<String>, ending: Ending) -> Self {
            Self {
                chunks: chunks.into_iter().map(String::into_bytes).collect(),
                ending,
            }
        }
    }

    #[async_trait::async_trait]
    impl ChunkSource for Scripted {
        async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
            if let Some(c) = self.chunks.pop_front() {
                return Ok(Some(c));
            }
            match self.ending {
                Ending::Clean => Ok(None),
                Ending::Break(m) => Err(m.to_string()),
                Ending::Silence => std::future::pending().await,
            }
        }
    }

    pub(crate) fn event(kind: &str, data: Value) -> String {
        format!("event: {kind}\ndata: {data}\n\n")
    }

    pub(crate) fn start(input: u64) -> String {
        event(
            "message_start",
            json!({"type":"message_start","message":{"model":"claude-sonnet-5-5","usage":{
                "input_tokens": input, "cache_read_input_tokens": 7,
                "cache_creation_input_tokens": 3, "output_tokens": 1}}}),
        )
    }

    pub(crate) fn text(t: &str) -> String {
        event(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":t}}),
        )
    }

    pub(crate) fn thinking(t: &str) -> String {
        event(
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":t}}),
        )
    }

    pub(crate) fn finish(reason: &str, output: u64) -> String {
        let mut s = event(
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"output_tokens":output}}),
        );
        s.push_str(&event("message_stop", json!({"type":"message_stop"})));
        s
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn the_events_fold_into_the_message_a_non_streaming_reply_would_carry() {
        let mut acc = SseAccumulator::default();
        let body = format!(
            "{}{}{}{}{}{}",
            start(100),
            event("ping", json!({"type":"ping"})),
            thinking("hmm, let me think"),
            text("{\"findings\":"),
            text("[]}"),
            finish("end_turn", 480),
        );
        // Fed in awkward pieces: a chunk boundary can fall anywhere, even inside a line.
        let bytes = body.as_bytes();
        let mut deltas = Vec::new();
        for piece in bytes.chunks(17) {
            deltas.extend(acc.feed(piece));
        }
        assert_eq!(
            deltas,
            vec!["{\"findings\":".to_string(), "[]}".to_string()]
        );
        assert!(acc.is_finished() && acc.has_text());
        let m = acc.message(None);
        assert_eq!(m["content"][0]["text"], "{\"findings\":[]}");
        assert_eq!(m["stop_reason"], "end_turn");
        assert_eq!(m["model"], "claude-sonnet-5-5");
        assert_eq!(m["usage"]["input_tokens"], 100);
        assert_eq!(
            m["usage"]["output_tokens"], 480,
            "thinking is billed output"
        );
        assert_eq!(m["usage"]["cache_read_input_tokens"], 7);
        assert_eq!(m["usage"]["cache_creation_input_tokens"], 3);
    }

    #[test]
    fn a_stream_with_no_stop_reason_reads_as_a_reply_cut_off_and_bills_the_cap() {
        let mut acc = SseAccumulator::default();
        acc.feed(format!("{}{}", start(50), text("{\"findings\":[{\"a\":1},")).as_bytes());
        assert!(!acc.is_finished());
        let m = acc.message(Some(9_600));
        assert_eq!(m["stop_reason"], "max_tokens");
        assert_eq!(
            m["usage"]["output_tokens"], 9_600,
            "the most it could have generated"
        );
        assert_eq!(m["content"][0]["text"], "{\"findings\":[{\"a\":1},");
    }

    #[test]
    fn a_provider_error_event_is_kept() {
        let mut acc = SseAccumulator::default();
        acc.feed(
            event(
                "error",
                json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
            )
            .as_bytes(),
        );
        assert_eq!(
            acc.provider_error(),
            Some(&("overloaded_error".to_string(), "Overloaded".to_string()))
        );
    }

    #[tokio::test]
    async fn reading_stops_at_message_stop_and_forwards_each_delta() {
        let mut src = Scripted::new(
            vec![start(10), text("ab"), text("cd"), finish("end_turn", 5)],
            Ending::Silence,
        );
        let mut acc = SseAccumulator::default();
        let mut seen = String::new();
        let end = read_stream(&mut src, Duration::from_secs(120), &mut acc, &mut |t| {
            seen.push_str(t)
        })
        .await;
        assert_eq!(end, StreamEnd::Finished);
        assert_eq!(seen, "abcd");
    }

    /// The idle timeout governs: silence for the window ends the read, however much time the
    /// stream had been alive before. Text already delivered stays in the accumulator.
    #[tokio::test(start_paused = true)]
    async fn silence_for_the_idle_window_is_a_stall_whatever_the_total_time() {
        let mut src = Scripted::new(vec![start(10), text("partial")], Ending::Silence);
        let mut acc = SseAccumulator::default();
        let end = read_stream(&mut src, Duration::from_secs(90), &mut acc, &mut |_| {}).await;
        assert_eq!(end, StreamEnd::Stalled(Duration::from_secs(90)));
        assert!(acc.has_text(), "what arrived before the silence is kept");
    }

    /// A stream may run far past what a fixed total timeout would have allowed so long as bytes
    /// keep coming: 100 chunks one minute apart is well over an hour and is not a stall.
    #[tokio::test(start_paused = true)]
    async fn a_slow_but_live_stream_is_never_cut_by_total_time() {
        struct Slow(u32);
        #[async_trait::async_trait]
        impl ChunkSource for Slow {
            async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
                tokio::time::sleep(Duration::from_secs(60)).await;
                self.0 += 1;
                Ok(Some(match self.0 {
                    1 => start(1).into_bytes(),
                    101 => finish("end_turn", 100).into_bytes(),
                    _ => text("x").into_bytes(),
                }))
            }
        }
        let mut acc = SseAccumulator::default();
        let end = read_stream(
            &mut Slow(0),
            Duration::from_secs(120),
            &mut acc,
            &mut |_| {},
        )
        .await;
        assert_eq!(end, StreamEnd::Finished);
        assert_eq!(
            acc.message(None)["content"][0]["text"]
                .as_str()
                .unwrap()
                .len(),
            99
        );
    }

    #[tokio::test]
    async fn a_body_that_closes_early_or_breaks_is_broken_with_the_reason() {
        let mut src = Scripted::new(vec![start(1), text("a")], Ending::Clean);
        let mut acc = SseAccumulator::default();
        let end = read_stream(&mut src, Duration::from_secs(5), &mut acc, &mut |_| {}).await;
        assert!(matches!(end, StreamEnd::Broken(m) if m.contains("before the reply was finished")));

        let mut src = Scripted::new(vec![start(1), text("a")], Ending::Break("connection reset"));
        let mut acc = SseAccumulator::default();
        let end = read_stream(&mut src, Duration::from_secs(5), &mut acc, &mut |_| {}).await;
        assert_eq!(end, StreamEnd::Broken("connection reset".to_string()));

        let err = event(
            "error",
            json!({"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}),
        );
        let mut src = Scripted::new(vec![start(1), err], Ending::Clean);
        let mut acc = SseAccumulator::default();
        let end = read_stream(&mut src, Duration::from_secs(5), &mut acc, &mut |_| {}).await;
        assert!(matches!(end, StreamEnd::Broken(m) if m.contains("overloaded_error")));
    }
}

//! Minimal Server-Sent Events parser.
//!
//! Track B emits events shaped per the HTML SSE spec:
//!   event: <name>\n
//!   data: <json>\n
//!   \n
//!
//! Streams are framed by blank lines. We iterate the response body and yield
//! one [`Event`] per frame. Multi-line `data:` is concatenated with `\n`.

use std::pin::Pin;

use bytes::Bytes;
use futures_util::stream::Stream;
use reqwest::Response;
use serde::Deserialize;

use crate::errors::CliError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub name: String,
    pub data: String,
}

impl Event {
    /// Decode `data` as JSON of type `T`.
    pub fn parse<T: for<'de> Deserialize<'de>>(&self) -> Result<T, CliError> {
        serde_json::from_str(&self.data).map_err(CliError::from)
    }
}

/// Iterator-shaped wrapper over a streaming response body.
pub struct EventStream {
    inner: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin>>,
    buf: String,
    pending_event: Option<String>,
    pending_data: String,
    finished: bool,
}

impl EventStream {
    pub fn from_response(resp: Response) -> Self {
        let inner = Box::pin(resp.bytes_stream());
        Self {
            inner,
            buf: String::new(),
            pending_event: None,
            pending_data: String::new(),
            finished: false,
        }
    }

    pub async fn next(&mut self) -> Result<Option<Event>, CliError> {
        use futures_util::StreamExt;

        if self.finished {
            return Ok(None);
        }

        loop {
            // Pull complete events out of the buffer first.
            if let Some(event) = self.drain_one() {
                return Ok(Some(event));
            }

            // Need more bytes.
            match self.inner.next().await {
                None => {
                    self.finished = true;
                    return Ok(self.drain_one());
                }
                Some(Ok(chunk)) => {
                    self.buf.push_str(&String::from_utf8_lossy(&chunk));
                }
                Some(Err(e)) => return Err(CliError::Network(e.to_string())),
            }
        }
    }

    /// Pull at most one event out of `buf`. Returns None if no full event is
    /// buffered yet.
    fn drain_one(&mut self) -> Option<Event> {
        loop {
            let line_end = self.buf.find('\n')?;
            let line: String = self.buf.drain(..=line_end).collect();
            // Strip the trailing \n (and any \r before it).
            let line = line.trim_end_matches('\n').trim_end_matches('\r');

            if line.is_empty() {
                // End of frame — emit if we have anything.
                if self.pending_event.is_some() || !self.pending_data.is_empty() {
                    let name = self.pending_event.take().unwrap_or_else(|| "message".into());
                    let data = std::mem::take(&mut self.pending_data);
                    return Some(Event { name, data });
                }
                continue;
            }

            if let Some(rest) = line.strip_prefix(':') {
                // SSE comment / keep-alive — ignore.
                let _ = rest;
                continue;
            }

            if let Some(rest) = line.strip_prefix("event:") {
                self.pending_event = Some(rest.trim_start().to_string());
            } else if let Some(rest) = line.strip_prefix("event: ") {
                self.pending_event = Some(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                let payload = rest.strip_prefix(' ').unwrap_or(rest);
                if !self.pending_data.is_empty() {
                    self.pending_data.push('\n');
                }
                self.pending_data.push_str(payload);
            }
            // Other field types (id:, retry:) are accepted-but-ignored.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain_all(text: &str) -> Vec<Event> {
        // Test the drain logic in isolation by feeding one big chunk.
        let mut s = EventStream {
            inner: Box::pin(futures_util::stream::iter(std::iter::empty())),
            buf: text.to_string(),
            pending_event: None,
            pending_data: String::new(),
            finished: false,
        };
        let mut out = Vec::new();
        while let Some(e) = s.drain_one() {
            out.push(e);
        }
        // Simulate end of stream — flush any trailing event.
        if s.pending_event.is_some() || !s.pending_data.is_empty() {
            // Force a blank-line flush.
        }
        out
    }

    #[test]
    fn parses_one_event() {
        let events = drain_all("event: question\ndata: hi\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "question");
        assert_eq!(events[0].data, "hi");
    }

    #[test]
    fn concatenates_multiline_data() {
        let events = drain_all("event: log\ndata: line1\ndata: line2\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "line1\nline2");
    }

    #[test]
    fn ignores_comments() {
        let events = drain_all(": ping\nevent: x\ndata: 1\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "x");
    }

    #[test]
    fn defaults_event_to_message_when_omitted() {
        let events = drain_all("data: hello\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "message");
        assert_eq!(events[0].data, "hello");
    }

    #[test]
    fn handles_crlf() {
        let events = drain_all("event: x\r\ndata: y\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "x");
        assert_eq!(events[0].data, "y");
    }

    #[test]
    fn parses_event_data_pairs() {
        let events = drain_all(
            "event: a\ndata: 1\n\nevent: b\ndata: 2\n\n",
        );
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].name, "a");
        assert_eq!(events[1].name, "b");
    }

    #[test]
    fn parse_decodes_json_payload() {
        let ev = Event {
            name: "question".into(),
            data: "{\"text\":\"hi\"}".into(),
        };
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Q {
            text: String,
        }
        let q: Q = ev.parse().unwrap();
        assert_eq!(q, Q { text: "hi".into() });
    }
}

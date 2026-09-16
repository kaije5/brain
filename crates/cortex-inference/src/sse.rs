//! Incremental Server-Sent Events decoding for OpenAI-compatible streams
//! (SCRUM-80).
//!
//! The decoder consumes raw network chunks and yields logical SSE events.
//! It correctly reassembles frames split across arbitrary chunk boundaries
//! (including multi-byte UTF-8 characters split mid-chunk), accepts CRLF or
//! LF line endings, skips SSE comments and keep-alives, recognizes the
//! `data: [DONE]` terminator, and enforces a per-frame byte cap.

use crate::error::{ProviderError, ProviderFailureCategory};

/// A decoded SSE event from the wire.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SseEvent {
    /// A `data:` payload (the `data:` prefix is stripped).
    Data(String),
    /// The `data: [DONE]` terminator.
    Done,
}

/// Incremental SSE decoder over raw bytes.
#[derive(Debug)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    max_frame_bytes: usize,
    done: bool,
}

impl SseDecoder {
    #[must_use]
    pub fn new(max_frame_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_frame_bytes,
            done: false,
        }
    }

    /// Appends raw bytes. Oversized input fails with a typed over-budget
    /// error rather than growing unbounded.
    ///
    /// # Errors
    /// Returns a classified malformed/over-budget error when the buffered
    /// frame exceeds the configured cap.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), ProviderError> {
        if self.done {
            return Ok(());
        }
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > self.max_frame_bytes {
            return Err(crate::error::response_too_large());
        }
        Ok(())
    }

    /// Pops the next complete event, if any. Frames split across chunks are
    /// reassembled; a trailing multi-byte UTF-8 sequence split across chunks
    /// is held back until its remainder arrives. Invalid UTF-8 fails as a
    /// typed malformed-response error.
    ///
    /// # Errors
    /// Returns a classified malformed-response error on invalid UTF-8 or an
    /// over-cap frame.
    pub fn next_event(&mut self) -> Result<Option<SseEvent>, ProviderError> {
        if self.done {
            return Ok(None);
        }
        let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') else {
            if self.buffer.len() > self.max_frame_bytes {
                return Err(crate::error::response_too_large());
            }
            return Ok(None);
        };
        let mut line_bytes: Vec<u8> = self.buffer.drain(..=newline).collect();
        // Strip the trailing LF and an immediately preceding CR (CRLF).
        line_bytes.pop();
        if line_bytes.last() == Some(&b'\r') {
            line_bytes.pop();
        }

        let line = match std::str::from_utf8(&line_bytes) {
            Ok(line) => line.to_owned(),
            // The line ends mid-character: incomplete UTF-8, wait for the
            // remainder to arrive in a later chunk.
            Err(error) if error.error_len().is_none() => {
                self.buffer = line_bytes;
                if self.buffer.len() > self.max_frame_bytes {
                    return Err(crate::error::response_too_large());
                }
                return Ok(None);
            }
            Err(_) => {
                return Err(ProviderError::from_category(
                    ProviderFailureCategory::MalformedResponse,
                ));
            }
        };

        // SSE comments and keep-alives start with `:` and are ignored.
        if line.starts_with(':') || line.is_empty() {
            return self.next_event();
        }
        let Some((field, value)) = line.split_once(':') else {
            return self.next_event();
        };
        let value = value.strip_prefix(' ').unwrap_or(value);
        if field == "data" {
            if value == "[DONE]" {
                self.done = true;
                return Ok(Some(SseEvent::Done));
            }
            if !value.is_empty() {
                return Ok(Some(SseEvent::Data(value.to_owned())));
            }
        }
        // Other fields (event:, id:, retry:) are not used by the provider
        // contract and are ignored.
        self.next_event()
    }

    /// Signals end-of-stream. Any incomplete buffered frame means the stream
    /// was truncated: surfaced as a typed malformed-response error.
    ///
    /// # Errors
    /// Returns a classified malformed-response error when buffered bytes
    /// remain (a truncated frame).
    #[allow(dead_code)] // reserved for transport-level EOF classification
    pub fn finish(&mut self) -> Result<(), ProviderError> {
        if !self
            .buffer
            .iter()
            .copied()
            .all(|byte| byte.is_ascii_whitespace())
        {
            return Err(ProviderError::from_category(
                ProviderFailureCategory::MalformedResponse,
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(chunks: &[&[u8]]) -> Result<Vec<SseEvent>, ProviderError> {
        let mut decoder = SseDecoder::new(64 * 1024);
        let mut events = Vec::new();
        for chunk in chunks {
            decoder.feed(chunk)?;
            while let Some(event) = decoder.next_event()? {
                events.push(event);
            }
        }
        decoder.finish()?;
        Ok(events)
    }

    #[test]
    fn normal_stream_with_done_terminator() {
        let events = events(&[b"data: {\"a\":1}\n\ndata: {\"b\":2}\n\ndata: [DONE]\n\n"])
            .expect("normal stream");
        assert_eq!(
            events,
            vec![
                SseEvent::Data("{\"a\":1}".to_owned()),
                SseEvent::Data("{\"b\":2}".to_owned()),
                SseEvent::Done,
            ]
        );
    }

    #[test]
    fn crlf_line_endings_are_accepted() {
        let events = events(&[b"data: one\r\n\r\ndata: [DONE]\r\n\r\n"]).expect("crlf");
        assert_eq!(
            events,
            vec![SseEvent::Data("one".to_owned()), SseEvent::Done]
        );
    }

    #[test]
    fn frames_split_byte_by_byte_are_reassembled() {
        let wire: Vec<u8> = b"data: hello\n\ndata: [DONE]\n\n".to_vec();
        let chunks: Vec<&[u8]> = wire.chunks(1).collect();
        let events = events(&chunks).expect("byte-by-byte");
        assert_eq!(
            events,
            vec![SseEvent::Data("hello".to_owned()), SseEvent::Done]
        );
    }

    #[test]
    fn multibyte_utf8_split_across_chunks_is_reassembled() {
        // "héllo": é is two bytes; split the chunk inside that character.
        let wire = b"data: h\xc3\xa9llo\n\n".to_vec();
        let split_at = "data: h".len() + 1;
        let events = events(&[&wire[..split_at], &wire[split_at..]]).expect("utf-8 split");
        assert_eq!(events, vec![SseEvent::Data("héllo".to_owned())]);
    }

    #[test]
    fn comments_and_keep_alives_are_ignored() {
        let events =
            events(&[b": keep-alive\n\ndata: real\n: another comment\n\ndata: [DONE]\n\n"])
                .expect("comments");
        assert_eq!(
            events,
            vec![SseEvent::Data("real".to_owned()), SseEvent::Done]
        );
    }

    #[test]
    fn multiple_data_frames_in_one_chunk_are_all_delivered() {
        let events = events(&[b"data: a\n\ndata: b\n\ndata: c\n\ndata: [DONE]\n\n"])
            .expect("batched frames");
        assert_eq!(
            events,
            vec![
                SseEvent::Data("a".to_owned()),
                SseEvent::Data("b".to_owned()),
                SseEvent::Data("c".to_owned()),
                SseEvent::Done,
            ]
        );
    }

    #[test]
    fn after_done_no_further_events_are_emitted() {
        let mut decoder = SseDecoder::new(64 * 1024);
        decoder
            .feed(b"data: [DONE]\n\ndata: ignored\n\n")
            .expect("feed");
        let mut events = Vec::new();
        while let Some(event) = decoder.next_event().expect("event") {
            events.push(event);
        }
        assert_eq!(events, vec![SseEvent::Done]);
    }

    #[test]
    fn oversized_frames_fail_as_over_budget() {
        let mut decoder = SseDecoder::new(16);
        let error = decoder
            .feed(&[b'x'; 32])
            .expect_err("oversized input fails");
        assert_eq!(error.category(), ProviderFailureCategory::MalformedResponse);
    }

    #[test]
    fn truncated_final_frame_without_newline_fails_on_finish() {
        let mut decoder = SseDecoder::new(64 * 1024);
        decoder.feed(b"data: partial").expect("feed");
        let error = decoder.finish().expect_err("truncated stream");
        assert_eq!(error.category(), ProviderFailureCategory::MalformedResponse);
    }

    #[test]
    fn complete_stream_without_trailing_newline_still_yields_its_frames() {
        let mut decoder = SseDecoder::new(64 * 1024);
        decoder.feed(b"data: [DONE]\n").expect("feed");
        let event = decoder.next_event().expect("event");
        assert_eq!(event, Some(SseEvent::Done));
        decoder.finish().expect("clean finish");
    }
}

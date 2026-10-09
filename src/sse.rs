// Copyright 2026 Salesforce, Inc. All rights reserved.

//! Incremental `text/event-stream` splitter: feeds raw chunks, yields complete `data:` payloads.

#[derive(Debug, Default)]
pub struct SseBuffer {
    pending: Vec<u8>,
}

impl SseBuffer {
    /// Appends `chunk` and returns the joined `data:` payload of every event it completed.
    /// A partial trailing event stays buffered for the next chunk.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(chunk);
        let mut payloads = Vec::new();
        while let Some((end, boundary_len)) = find_event_boundary(&self.pending) {
            if let Some(payload) = self.pending.get(..end).and_then(data_payload) {
                payloads.push(payload);
            }
            self.pending.drain(..end + boundary_len);
        }
        payloads
    }

    /// Flushes a final event that was not terminated by a blank line.
    pub fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.pending);
        data_payload(&rest)
    }

    #[cfg(test)]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

/// Joins the `data:` lines of one event with `\n`. `None` when the event has no data.
fn data_payload(event: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(event);
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|payload| payload.strip_prefix(' ').unwrap_or(payload))
        .collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// First `\n\n`, `\r\n\r\n` or `\n\r\n` blank-line boundary as `(index, length)`.
fn find_event_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < buf.len() {
        if buf.get(i) == Some(&b'\n') {
            match (buf.get(i + 1), buf.get(i + 2)) {
                (Some(b'\n'), _) => return Some((i, 2)),
                (Some(b'\r'), Some(b'\n')) => return Some((i, 3)),
                _ => {}
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lf_boundary() {
        let mut sse = SseBuffer::default();
        assert_eq!(sse.push(b"data: one\n\ndata: two\n\n"), vec!["one", "two"]);
        assert_eq!(sse.pending_len(), 0);
    }

    #[test]
    fn test_crlf_boundary() {
        let mut sse = SseBuffer::default();
        assert_eq!(
            sse.push(b"data: one\r\n\r\ndata: two\r\n\r\n"),
            vec!["one", "two"]
        );
    }

    #[test]
    fn test_partial_event_is_kept_across_chunks() {
        let mut sse = SseBuffer::default();
        assert!(sse.push(b"data: {\"a\"").is_empty());
        assert_eq!(sse.push(b":1}\n\n"), vec![r#"{"a":1}"#]);
    }

    #[test]
    fn test_optional_space_event_field_and_multiline_data() {
        let mut sse = SseBuffer::default();
        assert_eq!(
            sse.push(b"event: delta\ndata:x\ndata: y\nid: 3\n\n"),
            vec!["x\ny"]
        );
    }

    #[test]
    fn test_comment_only_event_yields_nothing() {
        let mut sse = SseBuffer::default();
        assert!(sse.push(b": keep-alive\n\n").is_empty());
    }

    #[test]
    fn test_finish_flushes_unterminated_event() {
        let mut sse = SseBuffer::default();
        assert!(sse.push(b"data: [DONE]").is_empty());
        assert_eq!(sse.finish().as_deref(), Some("[DONE]"));
    }
}

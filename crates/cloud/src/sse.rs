//! Incremental parser for server-sent events
//! (<https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream>).
//!
//! Bytes go in as the network delivers them, split anywhere, including inside
//! a multi-byte character or between the `\r` and `\n` of a line ending.

use crate::error::ProviderError;

/// Longest line accepted; a server that sends more is not speaking SSE.
const MAX_LINE: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    Continue,
    /// Abandon the response: the caller has what it needs or was cancelled.
    Stop,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SseEvent {
    /// The `event:` field; empty when the server sends none.
    pub name: String,
    pub data: String,
}

#[derive(Debug, Default)]
pub(crate) struct SseParser {
    line: Vec<u8>,
    /// The last byte was `\r`, so an immediately following `\n` belongs to it.
    after_cr: bool,
    at_start: bool,
    name: String,
    data: String,
    has_data: bool,
}

impl SseParser {
    pub fn new() -> Self {
        SseParser {
            at_start: true,
            ..Default::default()
        }
    }

    /// Feeds bytes and calls `sink` for each complete event. Stops early when
    /// the sink says so.
    pub fn feed(
        &mut self,
        bytes: &[u8],
        sink: &mut dyn FnMut(SseEvent) -> Result<Flow, ProviderError>,
    ) -> Result<Flow, ProviderError> {
        for &b in bytes {
            match b {
                b'\r' => {
                    self.after_cr = true;
                    if self.end_line(sink)? == Flow::Stop {
                        return Ok(Flow::Stop);
                    }
                }
                b'\n' if self.after_cr => self.after_cr = false,
                b'\n' => {
                    if self.end_line(sink)? == Flow::Stop {
                        return Ok(Flow::Stop);
                    }
                }
                _ => {
                    self.after_cr = false;
                    if self.line.len() >= MAX_LINE {
                        return Err(ProviderError::Malformed(
                            "event stream line too long".into(),
                        ));
                    }
                    self.line.push(b);
                }
            }
        }
        Ok(Flow::Continue)
    }

    /// The stream ended. Servers often omit the blank line after the last
    /// event, so a pending event is delivered.
    pub fn finish(
        &mut self,
        sink: &mut dyn FnMut(SseEvent) -> Result<Flow, ProviderError>,
    ) -> Result<Flow, ProviderError> {
        if !self.line.is_empty() && self.end_line(sink)? == Flow::Stop {
            return Ok(Flow::Stop);
        }
        self.dispatch(sink)
    }

    fn end_line(
        &mut self,
        sink: &mut dyn FnMut(SseEvent) -> Result<Flow, ProviderError>,
    ) -> Result<Flow, ProviderError> {
        let line = std::mem::take(&mut self.line);
        let mut text = String::from_utf8_lossy(&line);
        if self.at_start {
            self.at_start = false;
            if let Some(rest) = text.strip_prefix('\u{feff}') {
                text = rest.to_owned().into();
            }
        }
        let flow = if text.is_empty() {
            self.dispatch(sink)?
        } else {
            self.field(&text);
            Flow::Continue
        };
        // Hand the allocation back so a long stream reuses one buffer.
        self.line = line;
        self.line.clear();
        Ok(flow)
    }

    fn field(&mut self, line: &str) {
        if line.starts_with(':') {
            return;
        }
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match name {
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "event" => {
                self.name.clear();
                self.name.push_str(value);
            }
            _ => {}
        }
    }

    fn dispatch(
        &mut self,
        sink: &mut dyn FnMut(SseEvent) -> Result<Flow, ProviderError>,
    ) -> Result<Flow, ProviderError> {
        if !self.has_data {
            self.name.clear();
            return Ok(Flow::Continue);
        }
        self.has_data = false;
        sink(SseEvent {
            name: std::mem::take(&mut self.name),
            data: std::mem::take(&mut self.data),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn parse_in_chunks(input: &[u8], sizes: &[usize]) -> Vec<SseEvent> {
        let mut parser = SseParser::new();
        let mut events = Vec::new();
        let mut sink = |e: SseEvent| {
            events.push(e);
            Ok(Flow::Continue)
        };
        let mut rest = input;
        let mut sizes = sizes.iter().copied().cycle();
        while !rest.is_empty() {
            let n = sizes.next().unwrap().clamp(1, rest.len());
            let (head, tail) = rest.split_at(n);
            parser.feed(head, &mut sink).unwrap();
            rest = tail;
        }
        parser.finish(&mut sink).unwrap();
        events
    }

    const STREAM: &str = "\u{feff}: comment\r\nevent: a\r\ndata: {\"x\":1}\r\n\r\ndata: one\ndata: two\n\nretry: 5\n\ndata:no-space\r\r\ndata\n\ndata: é☃\n\ndata: tail";

    fn expected() -> Vec<SseEvent> {
        let ev = |name: &str, data: &str| SseEvent {
            name: name.into(),
            data: data.into(),
        };
        vec![
            ev("a", "{\"x\":1}"),
            ev("", "one\ntwo"),
            ev("", "no-space"),
            ev("", ""),
            ev("", "é☃"),
            ev("", "tail"),
        ]
    }

    #[test]
    fn parses_comments_multiline_data_bom_and_all_line_endings() {
        assert_eq!(
            parse_in_chunks(STREAM.as_bytes(), &[usize::MAX]),
            expected()
        );
    }

    #[test]
    fn byte_at_a_time_matches_whole_buffer() {
        assert_eq!(parse_in_chunks(STREAM.as_bytes(), &[1]), expected());
    }

    #[test]
    fn crlf_split_between_chunks_is_one_line_ending() {
        let events = parse_in_chunks(b"data: a\r\n\r\ndata: b\r\n\r\n", &[8, 1, 2]);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn sink_can_stop_the_stream() {
        let mut parser = SseParser::new();
        let mut seen = 0;
        let flow = parser
            .feed(b"data: 1\n\ndata: 2\n\n", &mut |_| {
                seen += 1;
                Ok(Flow::Stop)
            })
            .unwrap();
        assert_eq!((flow, seen), (Flow::Stop, 1));
    }

    #[test]
    fn endless_line_is_rejected() {
        let mut parser = SseParser::new();
        let junk = vec![b'x'; MAX_LINE + 1];
        assert!(matches!(
            parser.feed(&junk, &mut |_| Ok(Flow::Continue)),
            Err(ProviderError::Malformed(_))
        ));
    }

    proptest! {
        #[test]
        fn chunking_never_changes_the_events(sizes in proptest::collection::vec(1usize..40, 1..8)) {
            prop_assert_eq!(parse_in_chunks(STREAM.as_bytes(), &sizes), expected());
        }
    }
}

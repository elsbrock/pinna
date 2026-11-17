//! Sans-IO server-sent events decoding for streaming replies (ADR 0017).

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SseError {
    LineTooLong { max: usize },
}

impl fmt::Display for SseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LineTooLong { max } => write!(f, "SSE line exceeds {max} bytes"),
        }
    }
}

impl std::error::Error for SseError {}

pub struct SseDecoder {
    max_line: usize,
    line: Vec<u8>,
    skip_lf: bool,
    event: Option<String>,
    data: Option<String>,
}

impl SseDecoder {
    pub fn new(max_line: usize) -> Self {
        Self {
            max_line,
            line: Vec::new(),
            skip_lf: false,
            event: None,
            data: None,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>, SseError> {
        let mut events = Vec::new();
        for &byte in chunk {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\n' => self.line_end(&mut events),
                b'\r' => {
                    self.line_end(&mut events);
                    self.skip_lf = true;
                }
                _ => {
                    if self.line.len() == self.max_line {
                        return Err(SseError::LineTooLong { max: self.max_line });
                    }
                    self.line.push(byte);
                }
            }
        }
        Ok(events)
    }

    pub fn finish(&mut self) -> Option<SseEvent> {
        self.skip_lf = false;
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            self.field(&String::from_utf8_lossy(&line));
        }
        self.dispatch()
    }

    fn line_end(&mut self, events: &mut Vec<SseEvent>) {
        let line = std::mem::take(&mut self.line);
        if line.is_empty() {
            if let Some(event) = self.dispatch() {
                events.push(event);
            }
        } else {
            self.field(&String::from_utf8_lossy(&line));
        }
    }

    fn field(&mut self, line: &str) {
        if line.starts_with(':') {
            return;
        }
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match name {
            "event" => self.event = Some(value.to_owned()),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            },
            _ => {}
        }
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = self.event.take();
        self.data.take().map(|data| SseEvent { event, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_split(input: &str, split: usize) -> Result<Vec<SseEvent>, SseError> {
        let mut decoder = SseDecoder::new(256 * 1024);
        let mut events = decoder.push(&input.as_bytes()[..split])?;
        events.extend(decoder.push(&input.as_bytes()[split..])?);
        events.extend(decoder.finish());
        Ok(events)
    }

    fn check_splits(input: &str, expected: &[SseEvent]) {
        for split in 0..=input.len() {
            assert_eq!(
                decode_split(input, split).unwrap(),
                expected,
                "split at byte {split}"
            );
        }
    }

    fn event(name: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            event: name.map(str::to_owned),
            data: data.to_owned(),
        }
    }

    #[test]
    fn lf_comments_and_multiline_data() {
        check_splits(
            ": a comment\n\nevent: message\ndata: one\ndata:  two\n\n",
            &[event(Some("message"), "one\n two")],
        );
    }

    #[test]
    fn cr_and_crlf_are_line_endings() {
        check_splits(
            "data: first\r\rdata: second\r\n\r\n",
            &[event(None, "first"), event(None, "second")],
        );
    }

    #[test]
    fn fields_finish_and_empty_data() {
        check_splits(
            "id: 42\nretry: 10\nevent: custom\nunknown: x\ndata: payload",
            &[event(Some("custom"), "payload")],
        );
        check_splits("event: ignored\n\n", &[]);
        check_splits("data:\n\n", &[event(None, "")]);
    }

    #[test]
    fn utf8_can_split_inside_a_character() {
        check_splits("data: héllo 👋\r\n\r\n", &[event(None, "héllo 👋")]);
    }

    #[test]
    fn line_limit_is_enforced_across_chunks() {
        let mut decoder = SseDecoder::new(5);
        decoder.push(b"data").unwrap();
        assert_eq!(
            decoder.push(b": x").unwrap_err(),
            SseError::LineTooLong { max: 5 }
        );

        let mut decoder = SseDecoder::new(5);
        decoder.push(b"data:").unwrap();
        assert_eq!(decoder.push(b"\n\n").unwrap(), vec![event(None, "")]);
    }
}

//! Borrowed terminals and owned pseudo-input (`language/input_adapters.py`).
use crate::core::characters::is_scalar;
use crate::core::errors::Result;
use std::io::BufRead;
pub struct TerminalInput<'a, R: BufRead + ?Sized> {
    pub stream: &'a mut R,
}
impl<R: BufRead + ?Sized> TerminalInput<'_, R> {
    pub fn readline(&mut self, limit: Option<usize>) -> Result<Vec<u8>> {
        let mut line = Vec::new();
        loop {
            if limit.is_some_and(|limit| line.len() >= limit) {
                break;
            }
            let buffer = self.stream.fill_buf()?;
            if buffer.is_empty() {
                break;
            }
            let available = limit.map_or(buffer.len(), |n| buffer.len().min(n - line.len()));
            let length = buffer[..available]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(available, |n| n + 1);
            let finished = buffer[length - 1] == b'\n';
            line.extend_from_slice(&buffer[..length]);
            self.stream.consume(length);
            if finished {
                break;
            }
        }
        Ok(line)
    }
    pub fn close(&mut self) {}
}
pub struct PseudoInput {
    lines: std::vec::IntoIter<String>,
    pub closed: bool,
}
impl PseudoInput {
    pub fn new(text: &str, newline: i64, unicode: bool) -> Self {
        let valid = is_scalar(newline) && (unicode || newline <= 255);
        let mut lines: Vec<_> = if valid {
            text.split(char::from_u32(newline as u32).expect("newline scalar"))
                .map(str::to_string)
                .collect()
        } else {
            vec![text.into()]
        };
        if text.is_empty()
            || (valid && text.ends_with(char::from_u32(newline as u32).expect("newline scalar")))
        {
            lines.pop();
        }
        Self {
            lines: lines.into_iter(),
            closed: false,
        }
    }
    pub fn readline(&mut self, limit: Option<usize>) -> Option<String> {
        self.lines.next().map(|line| match limit {
            Some(limit) => line.chars().take(limit).collect(),
            None => line,
        })
    }
    pub fn close(&mut self) {
        self.closed = true;
    }
}

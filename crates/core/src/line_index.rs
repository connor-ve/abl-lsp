//! Converts between byte offsets and LSP positions (line + UTF-16 column).

use lsp_types::{Position, Range};

#[derive(Clone, Debug)]
pub struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { starts }
    }

    /// Zero-based line containing `off`.
    pub fn line_of(&self, off: usize) -> usize {
        match self.starts.binary_search(&off) {
            Ok(l) => l,
            Err(l) => l - 1,
        }
    }

    pub fn position(&self, text: &str, off: usize) -> Position {
        let off = floor_boundary(text, off.min(text.len()));
        let line = self.line_of(off);
        let col = text[self.starts[line]..off].encode_utf16().count();
        Position::new(line as u32, col as u32)
    }

    pub fn offset(&self, text: &str, pos: Position) -> usize {
        let line = pos.line as usize;
        if line >= self.starts.len() {
            return text.len();
        }
        let start = self.starts[line];
        let end = self.starts.get(line + 1).copied().unwrap_or(text.len());
        let mut units = 0u32;
        for (i, ch) in text[start..end].char_indices() {
            if units >= pos.character || ch == '\n' {
                return start + i;
            }
            units += ch.len_utf16() as u32;
        }
        end
    }

    pub fn range(&self, text: &str, start: usize, end: usize) -> Range {
        Range::new(self.position(text, start), self.position(text, end))
    }
}

fn floor_boundary(text: &str, mut off: usize) -> usize {
    while off > 0 && !text.is_char_boundary(off) {
        off -= 1;
    }
    off
}

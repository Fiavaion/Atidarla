//! Source spans and diagnostics (FR-K3: errors shown inline with line and column).

use std::fmt;

/// A byte range in the kernel source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// The smallest span covering both.
    pub fn to(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }
}

/// Where a diagnostic was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The kernel compiler (lexer, parser, type checker).
    Kernel,
    /// naga rejected the generated WGSL; the position is mapped back to the kernel
    /// source where possible.
    Wgsl,
}

/// One compile error, positioned for an editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// 1-based line.
    pub line: u32,
    /// 1-based column, in characters.
    pub col: u32,
    /// Characters to underline (at least 1; never past the end of the line).
    pub len: u32,
    pub message: String,
    pub hint: Option<String>,
    /// The byte range in the source.
    pub span: Span,
    pub origin: Origin,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)?;
        if let Some(h) = &self.hint {
            write!(f, " (hint: {h})")?;
        }
        Ok(())
    }
}

impl Diagnostic {
    /// Renders the diagnostic with the offending line and a caret underline, as the CLI
    /// prints it.
    pub fn render(&self, file: &str, source: &str) -> String {
        let mut out = format!(
            "{file}:{}:{}: error: {}\n",
            self.line, self.col, self.message
        );
        if let Some(text) = source.split('\n').nth(self.line.saturating_sub(1) as usize) {
            let text = text.strip_suffix('\r').unwrap_or(text);
            out.push_str("  ");
            out.push_str(text);
            out.push_str("\n  ");
            out.push_str(&" ".repeat(self.col.saturating_sub(1) as usize));
            out.push_str(&"^".repeat(self.len.max(1) as usize));
            out.push('\n');
        }
        if let Some(h) = &self.hint {
            out.push_str(&format!("  hint: {h}\n"));
        }
        out
    }
}

/// An error before positions are resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Error {
    pub span: Span,
    pub message: String,
    pub hint: Option<String>,
}

impl Error {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
            hint: None,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// The most errors reported for one compile.
pub const MAX_ERRORS: usize = 25;

/// Maps byte offsets to lines and columns.
#[derive(Debug)]
pub(crate) struct LineIndex<'a> {
    source: &'a str,
    /// Byte offset of the start of each line.
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(source: &'a str) -> Self {
        let mut starts = vec![0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self { source, starts }
    }

    /// Clamps `offset` into the source, on a character boundary.
    fn clamp(&self, offset: usize) -> usize {
        let mut o = offset.min(self.source.len());
        while !self.source.is_char_boundary(o) {
            o -= 1;
        }
        o
    }

    /// (1-based line, 1-based column in characters) of `offset`.
    pub fn position(&self, offset: usize) -> (u32, u32) {
        let o = self.clamp(offset);
        let line = self.starts.partition_point(|&s| s <= o).max(1) - 1;
        let start = self.starts[line];
        let col = self.source[start..o].chars().count();
        (to_u32(line + 1), to_u32(col + 1))
    }

    /// Characters of `span` on its first line, at least 1.
    fn len(&self, span: Span) -> u32 {
        let s = self.clamp(span.start);
        let e = self.clamp(span.end.max(span.start));
        let text = &self.source[s..e];
        let text = text.split('\n').next().unwrap_or("");
        let text = text.strip_suffix('\r').unwrap_or(text);
        to_u32(text.chars().count().max(1))
    }

    pub fn diagnostic(&self, e: &Error, origin: Origin) -> Diagnostic {
        let (line, col) = self.position(e.span.start);
        Diagnostic {
            line,
            col,
            len: self.len(e.span),
            message: e.message.clone(),
            hint: e.hint.clone(),
            span: e.span,
            origin,
        }
    }
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_characters_and_lines() {
        let src = "ab\n\u{e9}cd\r\nx";
        let idx = LineIndex::new(src);
        assert_eq!(idx.position(0), (1, 1));
        assert_eq!(idx.position(1), (1, 2));
        assert_eq!(idx.position(3), (2, 1));
        // 'é' is two bytes: 'c' is the second character.
        assert_eq!(idx.position(5), (2, 2));
        assert_eq!(idx.position(src.len()), (3, 2));
        // Inside a multi-byte character: clamped back to its start.
        assert_eq!(idx.position(4), (2, 1));
        assert_eq!(idx.position(10_000), (3, 2));
        // A span over a line end is cut at the line end (CR excluded).
        assert_eq!(idx.len(Span::new(5, src.len())), 2);
        assert_eq!(idx.len(Span::new(2, 2)), 1);
    }
}

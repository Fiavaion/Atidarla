//! The small Markdown subset quest text uses: paragraphs, `- ` and `1. `
//! lists, `**bold**` and `` `code` ``.

use egui::{RichText, Ui};

/// The bullet is in the app's default fonts (a test checks); check marks and
/// arrows are not, so the panel draws those (`mod.rs`).
const BULLET: &str = "\u{2022}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span<'a> {
    pub text: &'a str,
    pub bold: bool,
    pub code: bool,
}

/// Splits a line into plain, bold and code spans. An unmatched marker is
/// plain text.
pub fn spans(line: &str) -> Vec<Span<'_>> {
    let mut out = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let bold = rest.find("**");
        let code = rest.find('`');
        let next = match (bold, code) {
            (Some(b), Some(c)) => Some(b.min(c)),
            (b, c) => b.or(c),
        };
        let Some(at) = next else {
            out.push(plain(rest));
            break;
        };
        let (marker, len) = if rest[at..].starts_with("**") {
            ("**", 2)
        } else {
            ("`", 1)
        };
        match rest[at + len..].find(marker) {
            Some(end) => {
                if at > 0 {
                    out.push(plain(&rest[..at]));
                }
                let inner = &rest[at + len..at + len + end];
                out.push(Span {
                    text: inner,
                    bold: marker == "**",
                    code: marker == "`",
                });
                rest = &rest[at + len + end + len..];
            }
            None => {
                out.push(plain(rest));
                break;
            }
        }
    }
    out
}

fn plain(text: &str) -> Span<'_> {
    Span {
        text,
        bold: false,
        code: false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block<'a> {
    Paragraph(&'a str),
    /// A fenced block: lines shown as they are.
    Code(Vec<&'a str>),
    Item {
        marker: String,
        text: &'a str,
    },
}

/// Blocks of `text`: a blank line ends a paragraph; list lines are items.
pub fn blocks(text: &str) -> Vec<Block<'_>> {
    let mut out = Vec::new();
    let mut fence: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.trim().starts_with("```") {
            match fence.take() {
                Some(code) => out.push(Block::Code(code)),
                None => fence = Some(Vec::new()),
            }
            continue;
        }
        if let Some(code) = &mut fence {
            code.push(line.trim_end());
            continue;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("- ") {
            out.push(Block::Item {
                marker: BULLET.to_owned(),
                text: rest,
            });
        } else if let Some((n, rest)) = line.split_once(". ")
            && !n.is_empty()
            && n.chars().all(|c| c.is_ascii_digit())
        {
            out.push(Block::Item {
                marker: format!("{n}."),
                text: rest,
            });
        } else {
            out.push(Block::Paragraph(line));
        }
    }
    // An unclosed fence still shows what was written.
    out.extend(fence.map(Block::Code));
    out
}

pub fn show(ui: &mut Ui, text: &str) {
    for block in blocks(text) {
        match block {
            Block::Paragraph(t) => line(ui, None, t),
            Block::Code(lines) => {
                ui.group(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for l in lines {
                        ui.label(RichText::new(if l.is_empty() { " " } else { l }).monospace());
                    }
                });
            }
            Block::Item { marker, text } => line(ui, Some(&marker), text),
        }
    }
}

fn line(ui: &mut Ui, marker: Option<&str>, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if let Some(m) = marker {
            ui.label(format!("{m} "));
        }
        for s in spans(text) {
            let mut t = RichText::new(s.text);
            if s.bold {
                t = t.strong();
            }
            if s.code {
                t = t.code();
            }
            ui.label(t);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(line: &str) -> Vec<(&str, bool, bool)> {
        spans(line)
            .into_iter()
            .map(|s| (s.text, s.bold, s.code))
            .collect()
    }

    #[test]
    fn spans_split_bold_and_code_and_keep_stray_markers() {
        assert_eq!(
            flat("Add **Noise** and `img.noise` now"),
            [
                ("Add ", false, false),
                ("Noise", true, false),
                (" and ", false, false),
                ("img.noise", false, true),
                (" now", false, false)
            ]
        );
        assert_eq!(flat("a ** b"), [("a ** b", false, false)]);
        assert_eq!(flat("tick ` only"), [("tick ` only", false, false)]);
        assert!(flat("").is_empty());
    }

    #[test]
    fn blocks_find_paragraphs_and_lists() {
        let b = blocks("Intro line\n\n- one\n- two\n1. first\nlast");
        assert_eq!(b.len(), 5);
        assert_eq!(b[0], Block::Paragraph("Intro line"));
        assert!(matches!(&b[1], Block::Item { text: "one", .. }));
        assert!(matches!(&b[3], Block::Item { marker, text: "first" } if marker == "1."));
        assert_eq!(b[4], Block::Paragraph("last"));
        let c = blocks("Before\n```\nlet a = 1;\n\n  indented\n```\nAfter");
        assert_eq!(
            c,
            [
                Block::Paragraph("Before"),
                Block::Code(vec!["let a = 1;", "", "  indented"]),
                Block::Paragraph("After")
            ]
        );
    }
}

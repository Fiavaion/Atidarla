//! Tokens with byte spans. The lexer never fails: bad input becomes an error plus a
//! best-effort token, so the parser can keep going and report more.

use crate::diag::{Error, Span};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    Ident(String),
    /// An integer literal; range-checked by the type checker (it may be negated).
    Int(u64),
    Float(f64),
    Str(String),
    /// `@name` with an optional type sigil (`v@P`).
    Attr {
        sigil: Option<char>,
        name: String,
    },
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Dot,
    Question,
    Colon,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Bang,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    Ne,
    AndAnd,
    OrOr,
    Shl,
    Shr,
    Assign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    PlusPlus,
    MinusMinus,
    Eof,
}

impl Tok {
    /// How the token reads in an error message.
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Int(n) => format!("`{n}`"),
            Tok::Float(_) => "a number".into(),
            Tok::Str(_) => "a string".into(),
            Tok::Attr { name, .. } => format!("`@{name}`"),
            Tok::Eof => "the end of the kernel".into(),
            other => format!("`{}`", other.symbol()),
        }
    }

    fn symbol(&self) -> &'static str {
        match self {
            Tok::LParen => "(",
            Tok::RParen => ")",
            Tok::LBrace => "{",
            Tok::RBrace => "}",
            Tok::LBracket => "[",
            Tok::RBracket => "]",
            Tok::Comma => ",",
            Tok::Semi => ";",
            Tok::Dot => ".",
            Tok::Question => "?",
            Tok::Colon => ":",
            Tok::Plus => "+",
            Tok::Minus => "-",
            Tok::Star => "*",
            Tok::Slash => "/",
            Tok::Percent => "%",
            Tok::Amp => "&",
            Tok::Pipe => "|",
            Tok::Caret => "^",
            Tok::Tilde => "~",
            Tok::Bang => "!",
            Tok::Lt => "<",
            Tok::Gt => ">",
            Tok::Le => "<=",
            Tok::Ge => ">=",
            Tok::EqEq => "==",
            Tok::Ne => "!=",
            Tok::AndAnd => "&&",
            Tok::OrOr => "||",
            Tok::Shl => "<<",
            Tok::Shr => ">>",
            Tok::Assign => "=",
            Tok::PlusEq => "+=",
            Tok::MinusEq => "-=",
            Tok::StarEq => "*=",
            Tok::SlashEq => "/=",
            Tok::PercentEq => "%=",
            Tok::PlusPlus => "++",
            Tok::MinusMinus => "--",
            _ => "?",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Token {
    pub tok: Tok,
    pub span: Span,
}

/// Sigils accepted before `@` (VEX's set); unsupported ones are rejected by the checker.
const SIGILS: &[char] = &['f', 'i', 'v', 'u', 'p', 's', '2', '3', '4'];

/// Splits `src` into tokens, ending with `Eof`.
pub(crate) fn lex(src: &str, errors: &mut Vec<Error>) -> Vec<Token> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let start = i;
        // Whitespace and comments.
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            let mut closed = false;
            while i < bytes.len() {
                if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    i += 2;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                errors.push(
                    Error::new(Span::new(start, start + 2), "unterminated block comment")
                        .hint("close it with */"),
                );
            }
            continue;
        }
        // Identifiers, keywords, and sigil-prefixed attributes.
        if c.is_ascii_alphabetic() || c == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &src[start..i];
            if bytes.get(i) == Some(&b'@') {
                let mut chars = word.chars();
                if let (Some(s), None) = (chars.next(), chars.next())
                    && SIGILS.contains(&s)
                {
                    let (name, end) = attr_name(src, i + 1);
                    if name.is_empty() {
                        errors.push(Error::new(
                            Span::new(start, i + 1),
                            "expected an attribute name after `@`",
                        ));
                    }
                    out.push(Token {
                        tok: Tok::Attr {
                            sigil: Some(s),
                            name,
                        },
                        span: Span::new(start, end),
                    });
                    i = end;
                    continue;
                }
            }
            out.push(Token {
                tok: Tok::Ident(word.to_owned()),
                span: Span::new(start, i),
            });
            continue;
        }
        if c == b'@' {
            let (name, end) = attr_name(src, i + 1);
            if name.is_empty() {
                errors.push(Error::new(
                    Span::new(start, start + 1),
                    "expected an attribute name after `@`",
                ));
            }
            out.push(Token {
                tok: Tok::Attr { sigil: None, name },
                span: Span::new(start, end),
            });
            i = end;
            continue;
        }
        // Numbers (and the matrix sigils `2@`, `3@`, `4@`).
        if c.is_ascii_digit() || (c == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            if matches!(c, b'2' | b'3' | b'4') && bytes.get(i + 1) == Some(&b'@') {
                let (name, end) = attr_name(src, i + 2);
                out.push(Token {
                    tok: Tok::Attr {
                        sigil: Some(c as char),
                        name,
                    },
                    span: Span::new(start, end),
                });
                i = end;
                continue;
            }
            let (tok, end) = number(src, i, errors);
            out.push(Token {
                tok,
                span: Span::new(start, end),
            });
            i = end;
            continue;
        }
        if c == b'"' {
            i += 1;
            let mut s = String::new();
            let mut closed = false;
            while i < bytes.len() {
                match bytes[i] {
                    b'"' => {
                        i += 1;
                        closed = true;
                        break;
                    }
                    b'\n' => break,
                    b'\\' if i + 1 < bytes.len() => {
                        // `\` is ASCII, so `i + 1` is a character boundary.
                        let e = src[i + 1..].chars().next().unwrap_or('?');
                        s.push(match e {
                            'n' => '\n',
                            't' => '\t',
                            '"' => '"',
                            '\\' => '\\',
                            _ => '?',
                        });
                        i += 1 + e.len_utf8();
                    }
                    _ => {
                        // Copy one whole UTF-8 character.
                        let ch = src[i..].chars().next().unwrap_or('?');
                        s.push(ch);
                        i += ch.len_utf8();
                    }
                }
            }
            if !closed {
                errors.push(Error::new(
                    Span::new(start, i.max(start + 1)),
                    "unterminated string",
                ));
            }
            out.push(Token {
                tok: Tok::Str(s),
                span: Span::new(start, i),
            });
            continue;
        }
        // Punctuation, longest match first.
        let two = bytes.get(i + 1).copied();
        let (tok, n) = match (c, two) {
            (b'<', Some(b'<')) => (Tok::Shl, 2),
            (b'>', Some(b'>')) => (Tok::Shr, 2),
            (b'<', Some(b'=')) => (Tok::Le, 2),
            (b'>', Some(b'=')) => (Tok::Ge, 2),
            (b'=', Some(b'=')) => (Tok::EqEq, 2),
            (b'!', Some(b'=')) => (Tok::Ne, 2),
            (b'&', Some(b'&')) => (Tok::AndAnd, 2),
            (b'|', Some(b'|')) => (Tok::OrOr, 2),
            (b'+', Some(b'=')) => (Tok::PlusEq, 2),
            (b'-', Some(b'=')) => (Tok::MinusEq, 2),
            (b'*', Some(b'=')) => (Tok::StarEq, 2),
            (b'/', Some(b'=')) => (Tok::SlashEq, 2),
            (b'%', Some(b'=')) => (Tok::PercentEq, 2),
            (b'+', Some(b'+')) => (Tok::PlusPlus, 2),
            (b'-', Some(b'-')) => (Tok::MinusMinus, 2),
            (b'(', _) => (Tok::LParen, 1),
            (b')', _) => (Tok::RParen, 1),
            (b'{', _) => (Tok::LBrace, 1),
            (b'}', _) => (Tok::RBrace, 1),
            (b'[', _) => (Tok::LBracket, 1),
            (b']', _) => (Tok::RBracket, 1),
            (b',', _) => (Tok::Comma, 1),
            (b';', _) => (Tok::Semi, 1),
            (b'.', _) => (Tok::Dot, 1),
            (b'?', _) => (Tok::Question, 1),
            (b':', _) => (Tok::Colon, 1),
            (b'+', _) => (Tok::Plus, 1),
            (b'-', _) => (Tok::Minus, 1),
            (b'*', _) => (Tok::Star, 1),
            (b'/', _) => (Tok::Slash, 1),
            (b'%', _) => (Tok::Percent, 1),
            (b'&', _) => (Tok::Amp, 1),
            (b'|', _) => (Tok::Pipe, 1),
            (b'^', _) => (Tok::Caret, 1),
            (b'~', _) => (Tok::Tilde, 1),
            (b'!', _) => (Tok::Bang, 1),
            (b'<', _) => (Tok::Lt, 1),
            (b'>', _) => (Tok::Gt, 1),
            (b'=', _) => (Tok::Assign, 1),
            _ => {
                let ch = src[i..].chars().next().unwrap_or('?');
                let len = ch.len_utf8();
                errors.push(Error::new(
                    Span::new(i, i + len),
                    format!("unexpected character `{}`", ch.escape_default()),
                ));
                i += len;
                continue;
            }
        };
        out.push(Token {
            tok,
            span: Span::new(i, i + n),
        });
        i += n;
    }
    out.push(Token {
        tok: Tok::Eof,
        span: Span::new(src.len(), src.len()),
    });
    out
}

/// Reads an identifier starting at `from`; returns it and the end offset.
fn attr_name(src: &str, from: usize) -> (String, usize) {
    let bytes = src.as_bytes();
    let mut end = from;
    if bytes
        .get(end)
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
    {
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
    }
    (src[from..end].to_owned(), end)
}

/// Reads a number at `start`.
fn number(src: &str, start: usize, errors: &mut Vec<Error>) -> (Tok, usize) {
    let bytes = src.as_bytes();
    let mut i = start;
    // Hexadecimal integer.
    if bytes[i] == b'0' && matches!(bytes.get(i + 1), Some(b'x' | b'X')) {
        i += 2;
        let digits = i;
        while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
            i += 1;
        }
        let value = u64::from_str_radix(&src[digits..i], 16);
        return match value {
            Ok(v) => (Tok::Int(v), end_of_number(src, i, start, errors)),
            Err(_) => {
                errors.push(Error::new(
                    Span::new(start, i),
                    "invalid hexadecimal number",
                ));
                (Tok::Int(0), end_of_number(src, i, start, errors))
            }
        };
    }
    let mut float = false;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if bytes.get(i) == Some(&b'.') {
        float = true;
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(bytes.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if bytes.get(j).is_some_and(u8::is_ascii_digit) {
            float = true;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    let text = &src[start..i];
    let end = end_of_number(src, i, start, errors);
    if float {
        match text.parse::<f64>() {
            Ok(v) if v.is_finite() && v <= f64::from(f32::MAX) => (Tok::Float(v), end),
            _ => {
                errors.push(Error::new(
                    Span::new(start, i),
                    "number is too large for a float",
                ));
                (Tok::Float(0.0), end)
            }
        }
    } else {
        match text.parse::<u64>() {
            Ok(v) => (Tok::Int(v), end),
            Err(_) => {
                errors.push(Error::new(
                    Span::new(start, i),
                    "integer is too large for an int",
                ));
                (Tok::Int(0), end)
            }
        }
    }
}

/// Rejects letters glued to a number (`1x`, `2.0f`) and returns where it ends.
fn end_of_number(src: &str, i: usize, start: usize, errors: &mut Vec<Error>) -> usize {
    let bytes = src.as_bytes();
    let mut end = i;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    if end > i {
        errors.push(
            Error::new(
                Span::new(start, end),
                format!("invalid number `{}`", &src[start..end]),
            )
            .hint("numbers take no suffix: write 2.0, not 2.0f"),
        );
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        let mut errors = Vec::new();
        let t = lex(src, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        t.into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn attributes_and_sigils() {
        assert_eq!(
            toks("v@P += @Cd; 3@m"),
            vec![
                Tok::Attr {
                    sigil: Some('v'),
                    name: "P".into()
                },
                Tok::PlusEq,
                Tok::Attr {
                    sigil: None,
                    name: "Cd".into()
                },
                Tok::Semi,
                Tok::Attr {
                    sigil: Some('3'),
                    name: "m".into()
                },
                Tok::Eof
            ]
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(
            toks("1 .5 2. 1e-3 0x1F 7E2"),
            vec![
                Tok::Int(1),
                Tok::Float(0.5),
                Tok::Float(2.0),
                Tok::Float(0.001),
                Tok::Int(31),
                Tok::Float(700.0),
                Tok::Eof
            ]
        );
        let mut errors = Vec::new();
        lex("2.0f 1e999 99999999999999999999999", &mut errors);
        assert_eq!(errors.len(), 3, "{errors:?}");
    }

    #[test]
    fn comments_and_punctuation() {
        assert_eq!(
            toks("a /* x */ <<= // y\n b"),
            vec![
                Tok::Ident("a".into()),
                Tok::Shl,
                Tok::Assign,
                Tok::Ident("b".into()),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn bad_input_reports_and_continues() {
        let mut errors = Vec::new();
        let t = lex("a $ \u{e9} \"open\n/* never", &mut errors);
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert_eq!(t.last().map(|t| &t.tok), Some(&Tok::Eof));
    }
}

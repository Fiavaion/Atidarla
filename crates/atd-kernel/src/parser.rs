//! Recursive-descent parser. Reports errors and recovers at the next statement; nesting
//! is limited so no input can exhaust the stack.

use crate::ast::*;
use crate::diag::{Error, MAX_ERRORS, Span};
use crate::lexer::{Tok, Token};
use crate::types::Type;

/// Deepest nesting of expressions and statements accepted.
const MAX_DEPTH: u32 = 96;

/// Signals that an error was reported and the current statement should be abandoned.
#[derive(Debug)]
struct Stop;

type PResult<T> = Result<T, Stop>;

pub(crate) fn parse(tokens: &[Token], errors: &mut Vec<Error>) -> Program {
    let mut p = Parser {
        toks: tokens,
        pos: 0,
        errors,
        depth: 0,
    };
    let mut program = Program::default();
    while !p.at_eof() && p.errors.len() < MAX_ERRORS {
        let before = p.pos;
        if p.at_function() {
            match p.function() {
                Ok(f) => program.items.push(Item::Func(f)),
                Err(Stop) => p.recover(),
            }
        } else {
            match p.statement() {
                Ok(s) => program.items.push(Item::Stmt(s)),
                Err(Stop) => p.recover(),
            }
        }
        if p.pos == before {
            // Always make progress.
            p.pos += 1;
        }
    }
    program
}

struct Parser<'a> {
    toks: &'a [Token],
    pos: usize,
    errors: &'a mut Vec<Error>,
    depth: u32,
}

impl Parser<'_> {
    fn peek(&self) -> &Tok {
        self.peek_at(0)
    }

    fn peek_at(&self, n: usize) -> &Tok {
        let last = self.toks.len().saturating_sub(1);
        self.toks
            .get((self.pos + n).min(last))
            .map_or(&Tok::Eof, |t| &t.tok)
    }

    fn span(&self) -> Span {
        let last = self.toks.len().saturating_sub(1);
        self.toks
            .get(self.pos.min(last))
            .map_or(Span::default(), |t| t.span)
    }

    /// The span of the previous token.
    fn prev_span(&self) -> Span {
        if self.pos == 0 {
            return self.span();
        }
        self.toks
            .get(self.pos - 1)
            .map_or(Span::default(), |t| t.span)
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), Tok::Eof)
    }

    fn bump(&mut self) -> Token {
        let t = self.toks.get(self.pos).cloned().unwrap_or(Token {
            tok: Tok::Eof,
            span: self.span(),
        });
        if !matches!(t.tok, Tok::Eof) {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == tok {
            self.bump();
            true
        } else {
            false
        }
    }

    fn is_word(&self, n: usize, word: &str) -> bool {
        matches!(self.peek_at(n), Tok::Ident(w) if w == word)
    }

    fn error<T>(&mut self, e: Error) -> PResult<T> {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(e);
        }
        Err(Stop)
    }

    fn expect(&mut self, tok: &Tok, what: &str) -> PResult<Span> {
        if self.peek() == tok {
            Ok(self.bump().span)
        } else {
            let found = self.peek().describe();
            let mut e = Error::new(self.span(), format!("expected {what}, found {found}"));
            if *tok == Tok::Semi {
                // Point just after the previous token: that is where `;` is missing.
                let p = self.prev_span();
                e = Error::new(
                    Span::new(p.end, p.end),
                    format!("expected `;`, found {found}"),
                )
                .hint("end the statement with `;`");
            }
            self.error(e)
        }
    }

    /// Expects the `;` that ends a statement. A missing one is reported and treated as
    /// present, so the next statement still parses.
    fn semi(&mut self) {
        if !self.eat(&Tok::Semi) {
            let _ = self.expect(&Tok::Semi, "`;`");
        }
    }

    fn ident(&mut self, what: &str) -> PResult<(String, Span)> {
        if let Tok::Ident(name) = self.peek() {
            let name = name.clone();
            let span = self.bump().span;
            if Type::from_keyword(&name).is_some() || is_keyword(&name) {
                return self.error(Error::new(
                    span,
                    format!("`{name}` is a keyword and cannot be used as a name"),
                ));
            }
            Ok((name, span))
        } else {
            let found = self.peek().describe();
            self.error(Error::new(
                self.span(),
                format!("expected {what}, found {found}"),
            ))
        }
    }

    fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            let span = self.span();
            self.depth = 0;
            // Give up on the rest of the input: nothing further would be reliable.
            self.pos = self.toks.len().saturating_sub(1);
            return self.error(Error::new(span, "the kernel is nested too deeply"));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Skips to the end of the broken statement.
    fn recover(&mut self) {
        let mut braces = 0usize;
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::Semi if braces == 0 => {
                    self.bump();
                    return;
                }
                Tok::LBrace => braces += 1,
                Tok::RBrace => {
                    if braces == 0 {
                        self.bump();
                        return;
                    }
                    braces -= 1;
                    if braces == 0 {
                        self.bump();
                        return;
                    }
                }
                _ => {}
            }
            self.bump();
        }
    }

    // ---- items ---------------------------------------------------------------------

    fn type_word(&self, n: usize) -> Option<Type> {
        match self.peek_at(n) {
            Tok::Ident(w) if w == "void" => Some(Type::Void),
            Tok::Ident(w) => Type::from_keyword(w),
            _ => None,
        }
    }

    fn at_function(&self) -> bool {
        let k = usize::from(self.is_word(0, "function"));
        self.type_word(k).is_some()
            && matches!(self.peek_at(k + 1), Tok::Ident(_))
            && matches!(self.peek_at(k + 2), Tok::LParen)
    }

    fn function(&mut self) -> PResult<FuncDef> {
        let start = self.span();
        self.is_word(0, "function").then(|| self.bump());
        let ret = self.type_word(0).unwrap_or(Type::Void);
        self.bump();
        let (name, name_span) = self.ident("a function name")?;
        self.expect(&Tok::LParen, "`(`")?;
        let mut params = Vec::new();
        if !self.eat(&Tok::RParen) {
            loop {
                let Some(ty) = self.type_word(0).filter(|t| *t != Type::Void) else {
                    let found = self.peek().describe();
                    return self.error(
                        Error::new(
                            self.span(),
                            format!("expected a parameter type, found {found}"),
                        )
                        .hint("write each parameter as TYPE name, e.g. float x"),
                    );
                };
                self.bump();
                let (pname, span) = self.ident("a parameter name")?;
                params.push(Param {
                    ty,
                    name: pname,
                    span,
                });
                // VEX separates parameters with `;` too.
                if self.eat(&Tok::Comma) || self.eat(&Tok::Semi) {
                    continue;
                }
                self.expect(&Tok::RParen, "`,` or `)`")?;
                break;
            }
        }
        if !matches!(self.peek(), Tok::LBrace) {
            let found = self.peek().describe();
            return self.error(Error::new(
                self.span(),
                format!("expected `{{` to start the body of `{name}`, found {found}"),
            ));
        }
        let body = self.block()?;
        Ok(FuncDef {
            name,
            name_span,
            ret,
            params,
            body,
            span: start.to(self.prev_span()),
        })
    }

    // ---- statements ----------------------------------------------------------------

    fn block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect(&Tok::LBrace, "`{`")?;
        let mut stmts = Vec::new();
        loop {
            match self.peek() {
                Tok::RBrace => {
                    self.bump();
                    return Ok(stmts);
                }
                Tok::Eof => {
                    return self.error(
                        Error::new(self.span(), "expected `}`, found the end of the kernel")
                            .hint("a `{` is not closed"),
                    );
                }
                _ => {}
            }
            if self.errors.len() >= MAX_ERRORS {
                return Err(Stop);
            }
            if self.at_function() {
                let span = self.span();
                return self.error(Error::new(
                    span,
                    "functions can only be defined at the top level",
                ));
            }
            let before = self.pos;
            match self.statement() {
                Ok(s) => stmts.push(s),
                Err(Stop) => {
                    if self.at_eof() {
                        return Err(Stop);
                    }
                    self.recover_in_block();
                }
            }
            if self.pos == before {
                self.pos += 1;
            }
        }
    }

    /// Like [`Self::recover`], but leaves a closing `}` of the enclosing block alone.
    fn recover_in_block(&mut self) {
        let mut braces = 0usize;
        loop {
            match self.peek() {
                Tok::Eof => return,
                Tok::Semi if braces == 0 => {
                    self.bump();
                    return;
                }
                Tok::LBrace => braces += 1,
                Tok::RBrace => {
                    if braces == 0 {
                        return;
                    }
                    braces -= 1;
                    if braces == 0 {
                        self.bump();
                        return;
                    }
                }
                _ => {}
            }
            self.bump();
        }
    }

    fn statement(&mut self) -> PResult<Stmt> {
        let saved = self.depth;
        self.enter()?;
        let r = self.statement_inner();
        self.restore(saved);
        r
    }

    /// Returns to the depth before a construct, whether it parsed or not.
    fn restore(&mut self, saved: u32) {
        // After an overflow the cursor is at the end and depth is reset; keep it so.
        if self.depth != 0 || saved == 0 {
            self.depth = saved;
        }
    }

    fn statement_inner(&mut self) -> PResult<Stmt> {
        let start = self.span();
        let kind = match self.peek() {
            Tok::LBrace => StmtKind::Block(self.block()?),
            Tok::Semi => {
                self.bump();
                StmtKind::Empty
            }
            Tok::Ident(w) => match w.as_str() {
                "if" => {
                    self.bump();
                    self.expect(&Tok::LParen, "`(` after `if`")?;
                    let cond = self.expr()?;
                    self.expect(&Tok::RParen, "`)`")?;
                    let then = Box::new(self.statement()?);
                    let els = if self.is_word(0, "else") {
                        self.bump();
                        Some(Box::new(self.statement()?))
                    } else {
                        None
                    };
                    StmtKind::If { cond, then, els }
                }
                "for" => self.for_loop()?,
                "foreach" => self.foreach()?,
                "while" | "do" => {
                    let w = w.clone();
                    return self.error(
                        Error::new(start, format!("`{w}` loops are not supported"))
                            .hint("use a counted loop: for (int i = 0; i < n; i++)"),
                    );
                }
                "break" => {
                    self.bump();
                    self.semi();
                    StmtKind::Break
                }
                "continue" => {
                    self.bump();
                    self.semi();
                    StmtKind::Continue
                }
                "return" => {
                    self.bump();
                    let value = if matches!(self.peek(), Tok::Semi) {
                        None
                    } else {
                        Some(self.expr()?)
                    };
                    self.semi();
                    StmtKind::Return(value)
                }
                _ => {
                    let k = self.simple()?;
                    self.semi();
                    k
                }
            },
            _ => {
                let k = self.simple()?;
                self.semi();
                k
            }
        };
        Ok(Stmt {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    /// A declaration, assignment, increment or expression, without the `;`.
    fn simple(&mut self) -> PResult<StmtKind> {
        // `let x = e`
        if self.is_word(0, "let") {
            self.bump();
            let (name, span) = self.ident("a variable name")?;
            self.expect(
                &Tok::Assign,
                "`=` (a `let` needs a value to infer its type)",
            )?;
            let init = self.expr()?;
            return Ok(StmtKind::Decl {
                ty: None,
                vars: vec![Declarator {
                    name,
                    span,
                    init: Some(init),
                }],
            });
        }
        // `TYPE a = e, b`
        if let Some(ty) = self.type_word(0)
            && matches!(self.peek_at(1), Tok::Ident(_))
        {
            let ty_span = self.bump().span;
            if ty == Type::Void {
                return self.error(Error::new(ty_span, "a variable cannot be `void`"));
            }
            let mut vars = Vec::new();
            loop {
                let (name, span) = self.ident("a variable name")?;
                let init = if self.eat(&Tok::Assign) {
                    Some(self.expr()?)
                } else {
                    None
                };
                vars.push(Declarator { name, span, init });
                if !self.eat(&Tok::Comma) {
                    break;
                }
            }
            return Ok(StmtKind::Decl { ty: Some(ty), vars });
        }
        // `++x`, `--x`
        if matches!(self.peek(), Tok::PlusPlus | Tok::MinusMinus) {
            let inc = matches!(self.bump().tok, Tok::PlusPlus);
            let target = self.unary()?;
            return Ok(StmtKind::IncDec { target, inc });
        }
        let target = self.expr()?;
        let op = match self.peek() {
            Tok::Assign => AssignOp::Set,
            Tok::PlusEq => AssignOp::Add,
            Tok::MinusEq => AssignOp::Sub,
            Tok::StarEq => AssignOp::Mul,
            Tok::SlashEq => AssignOp::Div,
            Tok::PercentEq => AssignOp::Rem,
            Tok::PlusPlus | Tok::MinusMinus => {
                let inc = matches!(self.bump().tok, Tok::PlusPlus);
                return Ok(StmtKind::IncDec { target, inc });
            }
            _ => return Ok(StmtKind::Expr(target)),
        };
        self.bump();
        let value = self.expr()?;
        Ok(StmtKind::Assign { target, op, value })
    }

    fn for_loop(&mut self) -> PResult<StmtKind> {
        self.bump();
        self.expect(&Tok::LParen, "`(` after `for`")?;
        let init = if matches!(self.peek(), Tok::Semi) {
            None
        } else {
            let s = self.span();
            let k = self.simple()?;
            Some(Box::new(Stmt {
                kind: k,
                span: s.to(self.prev_span()),
            }))
        };
        self.expect(&Tok::Semi, "`;`")?;
        let cond = if matches!(self.peek(), Tok::Semi) {
            None
        } else {
            Some(self.expr()?)
        };
        self.expect(&Tok::Semi, "`;`")?;
        let step = if matches!(self.peek(), Tok::RParen) {
            None
        } else {
            let s = self.span();
            let k = self.simple()?;
            Some(Box::new(Stmt {
                kind: k,
                span: s.to(self.prev_span()),
            }))
        };
        self.expect(&Tok::RParen, "`)`")?;
        let body = Box::new(self.statement()?);
        Ok(StmtKind::For {
            init,
            cond,
            step,
            body,
        })
    }

    fn foreach(&mut self) -> PResult<StmtKind> {
        self.bump();
        self.expect(&Tok::LParen, "`(` after `foreach`")?;
        let Some(ty) = self.type_word(0).filter(|t| *t != Type::Void) else {
            let found = self.peek().describe();
            return self.error(
                Error::new(
                    self.span(),
                    format!("expected the loop variable's type, found {found}"),
                )
                .hint("foreach (int j; nearpoints(0, @P, radius, maxpts))"),
            );
        };
        self.bump();
        let (var, var_span) = self.ident("a loop variable name")?;
        self.expect(&Tok::Semi, "`;`")?;
        let source = self.expr()?;
        self.expect(&Tok::RParen, "`)`")?;
        let body = Box::new(self.statement()?);
        Ok(StmtKind::Foreach {
            ty,
            var,
            var_span,
            source,
            body,
        })
    }

    // ---- expressions ---------------------------------------------------------------

    fn expr(&mut self) -> PResult<Expr> {
        let saved = self.depth;
        self.enter()?;
        let r = self.ternary();
        self.restore(saved);
        r
    }

    fn ternary(&mut self) -> PResult<Expr> {
        let cond = self.binary(0)?;
        if !self.eat(&Tok::Question) {
            return Ok(cond);
        }
        let a = self.expr()?;
        self.expect(&Tok::Colon, "`:` of `? :`")?;
        let b = self.expr()?;
        let span = cond.span.to(b.span);
        Ok(Expr {
            kind: ExprKind::Ternary(Box::new(cond), Box::new(a), Box::new(b)),
            span,
        })
    }

    /// The binary operator at the cursor and its precedence (higher binds tighter).
    fn binop(&self) -> Option<(BinOp, u8)> {
        Some(match self.peek() {
            Tok::OrOr => (BinOp::Or, 0),
            Tok::AndAnd => (BinOp::And, 1),
            Tok::Pipe => (BinOp::BitOr, 2),
            Tok::Caret => (BinOp::BitXor, 3),
            Tok::Amp => (BinOp::BitAnd, 4),
            Tok::EqEq => (BinOp::Eq, 5),
            Tok::Ne => (BinOp::Ne, 5),
            Tok::Lt => (BinOp::Lt, 6),
            Tok::Le => (BinOp::Le, 6),
            Tok::Gt => (BinOp::Gt, 6),
            Tok::Ge => (BinOp::Ge, 6),
            Tok::Shl => (BinOp::Shl, 7),
            Tok::Shr => (BinOp::Shr, 7),
            Tok::Plus => (BinOp::Add, 8),
            Tok::Minus => (BinOp::Sub, 8),
            Tok::Star => (BinOp::Mul, 9),
            Tok::Slash => (BinOp::Div, 9),
            Tok::Percent => (BinOp::Rem, 9),
            _ => return None,
        })
    }

    /// Precedence climbing (left-associative). Every operator in a chain counts toward
    /// the nesting limit, because `a + b + c ...` builds a tree as deep as the chain and
    /// every later pass walks it recursively.
    fn binary(&mut self, min: u8) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        while let Some((op, prec)) = self.binop() {
            if prec < min {
                break;
            }
            self.bump();
            self.enter()?;
            let rhs = self.binary(prec + 1)?;
            let span = lhs.span.to(rhs.span);
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span,
            };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let op = match self.peek() {
            Tok::Minus => UnOp::Neg,
            Tok::Plus => UnOp::Plus,
            Tok::Bang => UnOp::Not,
            Tok::Tilde => UnOp::BitNot,
            Tok::PlusPlus | Tok::MinusMinus => {
                return self.error(
                    Error::new(self.span(), "`++` and `--` are statements, not expressions")
                        .hint("write `x++;` on its own line, or use x + 1"),
                );
            }
            _ => return self.postfix(),
        };
        let start = self.bump().span;
        self.enter()?;
        let inner = self.unary();
        self.leave();
        let inner = inner?;
        let span = start.to(inner.span);
        Ok(Expr {
            kind: ExprKind::Unary(op, Box::new(inner)),
            span,
        })
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            if matches!(self.peek(), Tok::Dot | Tok::LBracket) {
                // Each one nests the tree a level deeper.
                self.enter()?;
            }
            match self.peek() {
                Tok::Dot => {
                    self.bump();
                    let Tok::Ident(name) = self.peek().clone() else {
                        let found = self.peek().describe();
                        return self.error(
                            Error::new(
                                self.span(),
                                format!("expected a component name after `.`, found {found}"),
                            )
                            .hint("use .x .y .z .w or .r .g .b .a"),
                        );
                    };
                    let span = self.bump().span;
                    let full = e.span.to(span);
                    e = Expr {
                        kind: ExprKind::Member(Box::new(e), name, span),
                        span: full,
                    };
                }
                Tok::LBracket => {
                    self.bump();
                    let index = self.expr()?;
                    let end = self.expect(&Tok::RBracket, "`]`")?;
                    let full = e.span.to(end);
                    e = Expr {
                        kind: ExprKind::Index(Box::new(e), Box::new(index)),
                        span: full,
                    };
                }
                _ => return Ok(e),
            }
        }
    }

    fn primary(&mut self) -> PResult<Expr> {
        let t = self.peek().clone();
        let span = self.span();
        let kind = match t {
            Tok::Int(n) => {
                self.bump();
                ExprKind::Int(n)
            }
            Tok::Float(v) => {
                self.bump();
                ExprKind::Float(v)
            }
            Tok::Str(s) => {
                self.bump();
                ExprKind::Str(s)
            }
            Tok::Attr { sigil, name } => {
                self.bump();
                ExprKind::Attr { sigil, name }
            }
            Tok::Ident(name) => {
                self.bump();
                match name.as_str() {
                    "true" => ExprKind::Bool(true),
                    "false" => ExprKind::Bool(false),
                    _ if is_keyword(&name) => {
                        return self.error(Error::new(
                            span,
                            format!("expected an expression, found `{name}`"),
                        ));
                    }
                    _ if matches!(self.peek(), Tok::LParen) => {
                        self.bump();
                        let args = self.args(&Tok::RParen, "`)`")?;
                        return Ok(Expr {
                            kind: ExprKind::Call {
                                name,
                                name_span: span,
                                args,
                            },
                            span: span.to(self.prev_span()),
                        });
                    }
                    _ => ExprKind::Ident(name),
                }
            }
            Tok::LParen => {
                self.bump();
                let inner = self.expr()?;
                let end = self.expect(&Tok::RParen, "`)`")?;
                return Ok(Expr {
                    kind: inner.kind,
                    span: span.to(end),
                });
            }
            Tok::LBrace => {
                self.bump();
                let items = self.args(&Tok::RBrace, "`}`")?;
                return Ok(Expr {
                    kind: ExprKind::Braces(items),
                    span: span.to(self.prev_span()),
                });
            }
            other => {
                let found = other.describe();
                return self.error(Error::new(
                    span,
                    format!("expected an expression, found {found}"),
                ));
            }
        };
        Ok(Expr { kind, span })
    }

    /// Comma-separated expressions up to `close` (consumed).
    fn args(&mut self, close: &Tok, what: &str) -> PResult<Vec<Expr>> {
        let mut args = Vec::new();
        if self.eat(close) {
            return Ok(args);
        }
        loop {
            args.push(self.expr()?);
            if self.eat(&Tok::Comma) {
                continue;
            }
            self.expect(close, &format!("`,` or {what}"))?;
            return Ok(args);
        }
    }
}

/// Words that cannot name a variable or function.
fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "if" | "else"
            | "for"
            | "foreach"
            | "while"
            | "do"
            | "break"
            | "continue"
            | "return"
            | "let"
            | "function"
            | "void"
            | "true"
            | "false"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn parse_src(src: &str) -> (Program, Vec<Error>) {
        let mut errors = Vec::new();
        let toks = lex(src, &mut errors);
        let p = parse(&toks, &mut errors);
        (p, errors)
    }

    #[test]
    fn parses_statements_and_functions() {
        let (p, errors) = parse_src(
            "float f(float a, vec3 b) { return a * b.x; }\n\
             float x = 1, y;\nlet z = x ? y : -x;\n@P.y += f(x, {1,2,3});\n\
             for (int i = 0; i < 4; i++) { if (i == 2) continue; else break; }\n\
             foreach (int j; nearpoints(0, @P, 1.0, 8)) x += j;\n++x;\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(p.items.len(), 7);
        assert!(matches!(p.items[0], Item::Func(_)));
    }

    #[test]
    fn missing_semicolon_points_after_previous_token() {
        let (p, errors) = parse_src("float x = 1\nfloat y = 2;");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].span, Span::new(11, 11));
        assert!(errors[0].message.contains("expected `;`"));
        // The `;` is treated as present, so the next statement still parses.
        assert_eq!(p.items.len(), 2);
    }

    #[test]
    fn recovers_and_reports_several_errors() {
        let (p, errors) = parse_src("x = ;\ny = 1;\nz = );\nw = 2;\n");
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(p.items.len(), 2);
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_crash() {
        let src = format!("x = {}1{};", "(".repeat(5000), ")".repeat(5000));
        let (_, errors) = parse_src(&src);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("nested too deeply"))
        );
        let src = format!("{}x = 1;{}", "{".repeat(5000), "}".repeat(5000));
        let (_, errors) = parse_src(&src);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("nested too deeply"))
        );
        let src = format!("x = {}1;", "- ".repeat(5000));
        let (_, errors) = parse_src(&src);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("nested too deeply"))
        );
        // Long chains build deep trees too.
        let src = format!("x = 1{};", " + 1".repeat(5000));
        let (_, errors) = parse_src(&src);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("nested too deeply"))
        );
        let src = format!("x = v{};", ".x".repeat(5000));
        let (_, errors) = parse_src(&src);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("nested too deeply"))
        );
        // Moderate nesting is fine, and the depth is not leaked across statements.
        let ok = format!("x = {}1{};\n", "(".repeat(40), ")".repeat(40)).repeat(50);
        let (_, errors) = parse_src(&ok);
        assert!(errors.is_empty(), "{errors:?}");
    }
}

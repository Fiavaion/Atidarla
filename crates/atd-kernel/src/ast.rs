//! The syntax tree, as parsed, before types are known.

use crate::diag::Span;
use crate::types::Type;

#[derive(Debug, Default)]
pub(crate) struct Program {
    pub items: Vec<Item>,
}

#[derive(Debug)]
pub(crate) enum Item {
    Func(FuncDef),
    Stmt(Stmt),
}

#[derive(Debug)]
pub(crate) struct FuncDef {
    pub name: String,
    pub name_span: Span,
    /// `Type::Void` for `void`.
    pub ret: Type,
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug)]
pub(crate) struct Param {
    pub ty: Type,
    pub name: String,
    pub span: Span,
}

#[derive(Debug)]
pub(crate) struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AssignOp {
    Set,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Debug)]
pub(crate) struct Declarator {
    pub name: String,
    pub span: Span,
    pub init: Option<Expr>,
}

#[derive(Debug)]
pub(crate) enum StmtKind {
    /// `TYPE a = x, b;` or, with `ty: None`, `let a = x;`.
    Decl {
        ty: Option<Type>,
        vars: Vec<Declarator>,
    },
    Assign {
        target: Expr,
        op: AssignOp,
        value: Expr,
    },
    IncDec {
        target: Expr,
        inc: bool,
    },
    Expr(Expr),
    If {
        cond: Expr,
        then: Box<Stmt>,
        els: Option<Box<Stmt>>,
    },
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Box<Stmt>>,
        body: Box<Stmt>,
    },
    Foreach {
        ty: Type,
        var: String,
        var_span: Span,
        source: Expr,
        body: Box<Stmt>,
    },
    Break,
    Continue,
    Return(Option<Expr>),
    Block(Vec<Stmt>),
    Empty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnOp {
    Neg,
    Plus,
    Not,
    BitNot,
}

/// A binary operator (shared by the syntax tree and the IR).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
        }
    }
}

#[derive(Debug)]
pub(crate) struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug)]
pub(crate) enum ExprKind {
    Int(u64),
    /// A hexadecimal bit pattern above `0x7FFFFFFF`.
    IntBits(u32),
    Float(f64),
    Bool(bool),
    Str(String),
    Ident(String),
    Attr {
        sigil: Option<char>,
        name: String,
    },
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Call {
        name: String,
        name_span: Span,
        args: Vec<Expr>,
    },
    Member(Box<Expr>, String, Span),
    Index(Box<Expr>, Box<Expr>),
    /// `{a, b, c}`
    Braces(Vec<Expr>),
}

//! Parameter expressions (FR-P2, PRD A22).
//!
//! Two tiers. An expression in the **native subset** is parsed once and
//! evaluated in Rust on the cook path every frame, allocating nothing. Any
//! other expression is **Python**, evaluated on the Python worker thread;
//! its result arrives one frame late. The render thread never waits on
//! Python (CLAUDE.md rule 1).
//!
//! The native subset is a subset of Python with Python's results, so an
//! expression means the same in both tiers:
//!
//! - numbers, `True`, `False`, `pi`, `e`, `tau`;
//! - `+ - * / // % **`, unary `-` and `+`, parentheses;
//! - comparisons (chained, as in Python), `and`, `or`, `not`, and
//!   `a if condition else b`;
//! - functions: `abs min max round int float bool pow sqrt exp log log2
//!   log10 sin cos tan asin acos atan atan2 hypot degrees radians floor ceil
//!   trunc`, plus `clamp(x, low, high)` and `lerp(a, b, t)`;
//! - `time` (seconds) and `frame`, which make the node cook every frame;
//! - `me.digits`: the number at the end of the node's name (`noise3` → 3);
//! - `op('name').par.param` (`[i]` for one component of a vector or colour)
//!   and `op('name')['channel']`, another node's parameter or SIG channel;
//! - `op('name')[row, col]`, a DAT cell read as a number: each of `row` and
//!   `col` is a number (an expression) or a name in quotes, the row whose
//!   first cell is it or the column whose header (first-row cell) is it.
//!   Referenced nodes are dependencies: they cook first.
//!
//! As in Python, dividing by zero is an error, as is a maths domain error
//! (`sqrt(-1)`, `log(0)`). An error is shown on the node, and the parameter
//! keeps its stored value.

use std::fmt;
use std::sync::Arc;

use crate::dat::Dat;

/// A parsed native expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    root: Node,
    /// Node names referenced with `op('…')`, in order of first use. The
    /// evaluator's [`Env`] receives indices into this list.
    refs: Vec<String>,
    /// Each `op('…')[row, col]`, by slot: its node and the names it uses,
    /// for messages.
    cells: Vec<CellSpec>,
    uses_time: bool,
    uses_channels: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct CellSpec {
    node: usize,
    row: Option<String>,
    col: Option<String>,
}

/// A row or column of a DAT cell in the expression.
#[derive(Debug, Clone, PartialEq)]
enum At {
    Index(Box<Node>),
    Name(String),
}

impl At {
    /// As [`Env::cell`] takes it, with `index` the evaluated number.
    fn at(&self, index: u32) -> CellAt<'_> {
        match self {
            At::Index(_) => CellAt::Index(index),
            At::Name(n) => CellAt::Name(n),
        }
    }
}

/// A row or column of a DAT cell, as [`Env::cell`] receives it: an index,
/// or a name (the row whose first cell is it, the column whose header is
/// it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellAt<'a> {
    Index(u32),
    Name(&'a str),
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Num(f64),
    Time,
    Frame,
    MeDigits,
    Neg(Box<Node>),
    Not(Box<Node>),
    Bin(Bin, Box<Node>, Box<Node>),
    /// `a < b <= c …`: operands and the comparison before each.
    Compare(Box<Node>, Vec<(Cmp, Node)>),
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    If {
        cond: Box<Node>,
        then: Box<Node>,
        other: Box<Node>,
    },
    Call(Func, Vec<Node>),
    Par {
        node: usize,
        param: String,
        index: Option<usize>,
    },
    Chan {
        node: usize,
        channel: String,
    },
    Cell {
        node: usize,
        /// Index into [`Expr::cells`]; the engine caches by it.
        slot: usize,
        row: At,
        col: At,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Bin {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Func {
    Abs,
    Min,
    Max,
    Round,
    Int,
    Float,
    Bool,
    Pow,
    Sqrt,
    Exp,
    Log,
    Log2,
    Log10,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Hypot,
    Degrees,
    Radians,
    Floor,
    Ceil,
    Trunc,
    Clamp,
    Lerp,
}

impl Func {
    fn from_name(name: &str) -> Option<(Self, usize, usize)> {
        use Func::*;
        // (function, fewest arguments, most arguments)
        Some(match name {
            "abs" => (Abs, 1, 1),
            "min" => (Min, 2, 64),
            "max" => (Max, 2, 64),
            "round" => (Round, 1, 2),
            "int" => (Int, 1, 1),
            "float" => (Float, 1, 1),
            "bool" => (Bool, 1, 1),
            "pow" => (Pow, 2, 2),
            "sqrt" => (Sqrt, 1, 1),
            "exp" => (Exp, 1, 1),
            "log" => (Log, 1, 2),
            "log2" => (Log2, 1, 1),
            "log10" => (Log10, 1, 1),
            "sin" => (Sin, 1, 1),
            "cos" => (Cos, 1, 1),
            "tan" => (Tan, 1, 1),
            "asin" => (Asin, 1, 1),
            "acos" => (Acos, 1, 1),
            "atan" => (Atan, 1, 1),
            "atan2" => (Atan2, 2, 2),
            "hypot" => (Hypot, 2, 2),
            "degrees" => (Degrees, 1, 1),
            "radians" => (Radians, 1, 1),
            "floor" => (Floor, 1, 1),
            "ceil" => (Ceil, 1, 1),
            "trunc" => (Trunc, 1, 1),
            "clamp" => (Clamp, 3, 3),
            "lerp" => (Lerp, 3, 3),
            _ => return None,
        })
    }
}

/// Why an expression is not in the native subset (it then runs as Python).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotNative(pub String);

impl fmt::Display for NotNative {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An evaluation error. Small and `Copy`, so a failing expression costs
/// nothing per frame; [`EvalError::message`] formats it for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalError {
    DivisionByZero,
    /// A maths function outside its domain (or a complex result).
    Domain(&'static str),
    /// The node's name has no number at the end.
    NoDigits,
    /// `op(name)`: no such node (index into [`Expr::refs`]).
    NoNode(usize),
    /// The node has no such parameter, or it is not a number.
    NoParam(usize),
    /// The node has no such channel, or no channels.
    NoChannel(usize),
    /// A vector component out of range.
    Index(usize),
    /// Too many nested references (a parameter referring to itself).
    TooDeep,
    /// `op(name)[row, col]` on a node with no DAT value.
    NotDat(usize),
    /// A cell's row or column is not a whole number from 0 (by slot).
    CellIndex(usize),
    /// No such cell: the row or column is outside the table, or no row or
    /// header has that name. By slot, with the indices asked for.
    NoCell {
        slot: usize,
        row: u32,
        col: u32,
    },
    /// The cell's text is not a number.
    NotNumber {
        slot: usize,
        row: u32,
        col: u32,
    },
}

impl EvalError {
    pub fn message(&self, expr: &Expr) -> String {
        let name = |i: &usize| expr.refs.get(*i).map_or("?", String::as_str);
        match self {
            Self::DivisionByZero => "division by zero".into(),
            Self::Domain(f) => format!("{f}: math domain error"),
            Self::NoDigits => "me.digits: the node's name does not end in a number".into(),
            Self::NoNode(i) => format!("op('{}'): no such node", name(i)),
            Self::NoParam(i) => format!("op('{}').par: no such number parameter", name(i)),
            Self::NoChannel(i) => format!("op('{}'): no such channel", name(i)),
            Self::Index(i) => format!("op('{}').par: index out of range", name(i)),
            Self::TooDeep => "parameters refer to each other in a loop".into(),
            Self::NotDat(i) => format!("op('{}'): not a DAT, so it has no cells", name(i)),
            Self::CellIndex(slot) => format!(
                "{}: a row or column must be a whole number, 0 or more",
                expr.cell_label(*slot, None)
            ),
            Self::NoCell { slot, row, col } => format!(
                "{}: no such cell",
                expr.cell_label(*slot, Some((*row, *col)))
            ),
            Self::NotNumber { slot, row, col } => format!(
                "{}: the cell is not a number",
                expr.cell_label(*slot, Some((*row, *col)))
            ),
        }
    }
}

/// What an expression reads, supplied by the engine.
pub trait Env {
    fn time(&self) -> f64;
    fn frame(&self) -> f64;
    /// The number at the end of the evaluating node's name.
    fn me_digits(&self) -> Option<f64>;
    /// Parameter `param` of referenced node `node` (an index into
    /// [`Expr::refs`]), component `index` of a vector.
    fn param(&self, node: usize, param: &str, index: Option<usize>) -> Result<f64, EvalError>;
    /// The current value of channel `channel` of referenced node `node`.
    fn channel(&self, node: usize, channel: &str) -> Result<f64, EvalError>;
    /// A cell of referenced node `node`'s DAT as a number
    /// ([`cell_number`]); `slot` identifies the reference
    /// ([`Expr::cell_count`]) for caching. Without DATs, none has cells.
    fn cell(
        &self,
        node: usize,
        slot: usize,
        row: CellAt<'_>,
        col: CellAt<'_>,
    ) -> Result<f64, EvalError> {
        let _ = (slot, row, col);
        Err(EvalError::NotDat(node))
    }
}

/// What `op('t')[row, col]` gives: the cell of `dat` as a number (spaces
/// around it ignored). A name finds the first row whose first cell is it,
/// or the first column whose header (first-row cell) is it. Allocates
/// nothing. `slot` is reported in errors.
pub fn cell_number(
    dat: &Dat,
    slot: usize,
    row: CellAt<'_>,
    col: CellAt<'_>,
) -> Result<f64, EvalError> {
    let index = |at: CellAt<'_>| match at {
        CellAt::Index(i) => i,
        CellAt::Name(_) => u32::MAX,
    };
    let missing = EvalError::NoCell {
        slot,
        row: index(row),
        col: index(col),
    };
    let r = match row {
        CellAt::Index(i) => i as usize,
        CellAt::Name(n) => (0..dat.rows())
            .find(|&r| dat.cell(r, 0) == Some(n))
            .ok_or(missing)?,
    };
    let c = match col {
        CellAt::Index(i) => i as usize,
        CellAt::Name(n) => (0..dat.cols())
            .find(|&c| dat.cell(0, c) == Some(n))
            .ok_or(missing)?,
    };
    let text = dat.cell(r, c).ok_or(missing)?;
    text.trim().parse().map_err(|_| EvalError::NotNumber {
        slot,
        row: r as u32,
        col: c as u32,
    })
}

impl Expr {
    /// Parses `source` as a native expression.
    pub fn parse(source: &str) -> Result<Self, NotNative> {
        let tokens = lex(source)?;
        let mut p = Parser {
            tokens,
            pos: 0,
            refs: Vec::new(),
            cells: Vec::new(),
            uses_time: false,
            uses_channels: false,
        };
        let root = p.ternary()?;
        if p.pos != p.tokens.len() {
            return Err(NotNative(format!("unexpected {}", p.tokens[p.pos])));
        }
        Ok(Self {
            root,
            refs: p.refs,
            cells: p.cells,
            uses_time: p.uses_time,
            uses_channels: p.uses_channels,
        })
    }

    /// How many `op('…')[row, col]` cell references there are (slots
    /// `0..cell_count()`).
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// `op('t')[3, 'speed']` for slot `slot`; without indices, `…` stands
    /// for the computed ones.
    fn cell_label(&self, slot: usize, at: Option<(u32, u32)>) -> String {
        let Some(c) = self.cells.get(slot) else {
            return "op(…)[…]".into();
        };
        let part = |name: &Option<String>, i: Option<u32>| match (name, i) {
            (Some(n), _) => format!("'{n}'"),
            (None, Some(i)) => i.to_string(),
            (None, None) => "…".into(),
        };
        format!(
            "op('{}')[{}, {}]",
            self.refs.get(c.node).map_or("?", String::as_str),
            part(&c.row, at.map(|a| a.0)),
            part(&c.col, at.map(|a| a.1)),
        )
    }

    /// Node names referenced with `op('…')`.
    pub fn refs(&self) -> &[String] {
        &self.refs
    }

    /// Reads `time` or `frame`: the node must cook every frame.
    pub fn uses_time(&self) -> bool {
        self.uses_time
    }

    /// Reads a SIG channel.
    pub fn uses_channels(&self) -> bool {
        self.uses_channels
    }

    /// Evaluates the expression. Allocates nothing.
    pub fn eval(&self, env: &dyn Env) -> Result<f64, EvalError> {
        eval(&self.root, env)
    }
}

fn truthy(v: f64) -> bool {
    v != 0.0 && !v.is_nan()
}

fn domain(f: &'static str, v: f64) -> Result<f64, EvalError> {
    if v.is_nan() {
        Err(EvalError::Domain(f))
    } else {
        Ok(v)
    }
}

fn eval(n: &Node, env: &dyn Env) -> Result<f64, EvalError> {
    Ok(match n {
        Node::Num(v) => *v,
        Node::Time => env.time(),
        Node::Frame => env.frame(),
        Node::MeDigits => env.me_digits().ok_or(EvalError::NoDigits)?,
        Node::Neg(a) => -eval(a, env)?,
        Node::Not(a) => f64::from(u8::from(!truthy(eval(a, env)?))),
        Node::Bin(op, a, b) => {
            let (a, b) = (eval(a, env)?, eval(b, env)?);
            match op {
                Bin::Add => a + b,
                Bin::Sub => a - b,
                Bin::Mul => a * b,
                Bin::Div => {
                    if b == 0.0 {
                        return Err(EvalError::DivisionByZero);
                    }
                    a / b
                }
                Bin::FloorDiv => {
                    if b == 0.0 {
                        return Err(EvalError::DivisionByZero);
                    }
                    (a / b).floor()
                }
                Bin::Mod => {
                    if b == 0.0 {
                        return Err(EvalError::DivisionByZero);
                    }
                    // Python's sign: that of the divisor.
                    let r = a % b;
                    if r != 0.0 && (r < 0.0) != (b < 0.0) {
                        r + b
                    } else {
                        r
                    }
                }
                Bin::Pow => pow(a, b)?,
            }
        }
        Node::Compare(first, rest) => {
            let mut left = eval(first, env)?;
            for (cmp, right) in rest {
                let right = eval(right, env)?;
                let ok = match cmp {
                    Cmp::Lt => left < right,
                    Cmp::Le => left <= right,
                    Cmp::Gt => left > right,
                    Cmp::Ge => left >= right,
                    Cmp::Eq => left == right,
                    Cmp::Ne => left != right,
                };
                if !ok {
                    return Ok(0.0);
                }
                left = right;
            }
            1.0
        }
        // Python's `and`/`or` give back an operand, not a bool.
        Node::And(a, b) => {
            let a = eval(a, env)?;
            if truthy(a) { eval(b, env)? } else { a }
        }
        Node::Or(a, b) => {
            let a = eval(a, env)?;
            if truthy(a) { a } else { eval(b, env)? }
        }
        Node::If { cond, then, other } => {
            if truthy(eval(cond, env)?) {
                eval(then, env)?
            } else {
                eval(other, env)?
            }
        }
        Node::Call(f, args) => call(*f, args, env)?,
        Node::Par { node, param, index } => env.param(*node, param, *index)?,
        Node::Chan { node, channel } => env.channel(*node, channel)?,
        Node::Cell {
            node,
            slot,
            row,
            col,
        } => {
            let index = |a: &At| -> Result<u32, EvalError> {
                let At::Index(i) = a else { return Ok(0) };
                let v = eval(i, env)?;
                if v.fract() != 0.0 || !(0.0..f64::from(u32::MAX)).contains(&v) {
                    return Err(EvalError::CellIndex(*slot));
                }
                Ok(v as u32)
            };
            let (r, c) = (index(row)?, index(col)?);
            env.cell(*node, *slot, row.at(r), col.at(c))?
        }
    })
}

/// Python's `round(x, digits)`: the float's exact decimal value rounded
/// half to even (so `round(2.675, 2)` is 2.67, because 2.675 is stored as
/// 2.67499…). Formats into a stack buffer: nothing is allocated.
fn round_to(x: f64, digits: f64) -> f64 {
    use std::fmt::Write;
    struct Buf {
        bytes: [u8; 400],
        len: usize,
    }
    impl Write for Buf {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let end = self.len + s.len();
            if end > self.bytes.len() {
                return Err(fmt::Error);
            }
            self.bytes[self.len..end].copy_from_slice(s.as_bytes());
            self.len = end;
            Ok(())
        }
    }
    if !x.is_finite() {
        return x;
    }
    let digits = digits.trunc();
    if digits < 0.0 {
        let scale = 10f64.powf(-digits);
        return if scale.is_finite() {
            (x / scale).round_ties_even() * scale
        } else {
            0.0 * x
        };
    }
    // Past 17 significant decimals a double is already exact.
    if digits > 20.0 {
        return x;
    }
    let mut buf = Buf {
        bytes: [0; 400],
        len: 0,
    };
    if write!(buf, "{x:.*}", digits as usize).is_err() {
        return x;
    }
    std::str::from_utf8(&buf.bytes[..buf.len])
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(x)
}

fn pow(a: f64, b: f64) -> Result<f64, EvalError> {
    if a == 0.0 && b < 0.0 {
        return Err(EvalError::DivisionByZero);
    }
    // A negative base with a fractional exponent is complex in Python.
    if a < 0.0 && b.fract() != 0.0 {
        return Err(EvalError::Domain("pow"));
    }
    Ok(a.powf(b))
}

fn call(f: Func, args: &[Node], env: &dyn Env) -> Result<f64, EvalError> {
    let arg = |i: usize| eval(&args[i], env);
    let x = arg(0)?;
    Ok(match f {
        Func::Abs => x.abs(),
        Func::Min | Func::Max => {
            let mut best = x;
            for a in &args[1..] {
                let v = eval(a, env)?;
                // Python keeps the first of equal values.
                if (f == Func::Min && v < best) || (f == Func::Max && v > best) {
                    best = v;
                }
            }
            best
        }
        Func::Round => {
            if args.len() == 2 {
                round_to(x, arg(1)?)
            } else {
                // Python rounds halves to even.
                x.round_ties_even()
            }
        }
        Func::Int | Func::Trunc => x.trunc(),
        Func::Float => x,
        Func::Bool => f64::from(u8::from(truthy(x))),
        Func::Pow => pow(x, arg(1)?)?,
        Func::Sqrt => domain("sqrt", x.sqrt())?,
        Func::Exp => x.exp(),
        Func::Log => {
            if x <= 0.0 {
                return Err(EvalError::Domain("log"));
            }
            if args.len() == 2 {
                let base = arg(1)?;
                if base <= 0.0 || base == 1.0 {
                    return Err(EvalError::Domain("log"));
                }
                x.ln() / base.ln()
            } else {
                x.ln()
            }
        }
        Func::Log2 | Func::Log10 => {
            if x <= 0.0 {
                return Err(EvalError::Domain(if f == Func::Log2 {
                    "log2"
                } else {
                    "log10"
                }));
            }
            if f == Func::Log2 { x.log2() } else { x.log10() }
        }
        Func::Sin => x.sin(),
        Func::Cos => x.cos(),
        Func::Tan => x.tan(),
        Func::Asin => domain("asin", x.asin())?,
        Func::Acos => domain("acos", x.acos())?,
        Func::Atan => x.atan(),
        Func::Atan2 => x.atan2(arg(1)?),
        Func::Hypot => x.hypot(arg(1)?),
        Func::Degrees => x.to_degrees(),
        Func::Radians => x.to_radians(),
        Func::Floor => x.floor(),
        Func::Ceil => x.ceil(),
        Func::Clamp => x.max(arg(1)?).min(arg(2)?),
        Func::Lerp => {
            let b = arg(1)?;
            x + (b - x) * arg(2)?
        }
    })
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Name(String),
    Str(String),
    Op(&'static str),
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Num(n) => write!(f, "{n}"),
            Tok::Name(n) => write!(f, "`{n}`"),
            Tok::Str(s) => write!(f, "'{s}'"),
            Tok::Op(o) => write!(f, "`{o}`"),
        }
    }
}

const OPS: [&str; 22] = [
    "**", "//", "<=", ">=", "==", "!=", "+", "-", "*", "/", "%", "<", ">", "(", ")", "[", "]", ",",
    ".", "=", "!", "@",
];

fn lex(src: &str) -> Result<Vec<Tok>, NotNative> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b' ' || c == b'\t' {
            i += 1;
        } else if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            let start = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.' || b[i] == b'_') {
                i += 1;
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                i += 1;
                if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                    i += 1;
                }
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let text = src[start..i].replace('_', "");
            let v: f64 = text
                .parse()
                .map_err(|_| NotNative(format!("bad number `{}`", &src[start..i])))?;
            if i < b.len() && (b[i].is_ascii_alphabetic() || b[i] == b'_') {
                return Err(NotNative("a number followed by a name".into()));
            }
            out.push(Tok::Num(v));
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Tok::Name(src[start..i].to_owned()));
        } else if c == b'\'' || c == b'"' {
            let start = i + 1;
            let end = src[start..]
                .find(c as char)
                .map(|e| start + e)
                .ok_or_else(|| NotNative("unterminated string".into()))?;
            let s = &src[start..end];
            if s.contains('\\') {
                return Err(NotNative("escapes in strings".into()));
            }
            out.push(Tok::Str(s.to_owned()));
            i = end + 1;
        } else if let Some(op) = OPS.iter().find(|o| src[i..].starts_with(**o)) {
            out.push(Tok::Op(op));
            i += op.len();
        } else {
            return Err(NotNative(format!(
                "`{}`",
                src[i..].chars().next().unwrap_or(' ')
            )));
        }
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
    refs: Vec<String>,
    cells: Vec<CellSpec>,
    uses_time: bool,
    uses_channels: bool,
}

const KEYWORDS: [&str; 14] = [
    "lambda", "for", "in", "is", "import", "await", "yield", "del", "def", "class", "while",
    "return", "None", "with",
];

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if *o == op)
    }

    fn is_name(&self, name: &str) -> bool {
        matches!(self.peek(), Some(Tok::Name(n)) if n == name)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        let yes = self.is_op(op);
        if yes {
            self.pos += 1;
        }
        yes
    }

    fn eat_name(&mut self, name: &str) -> bool {
        let yes = self.is_name(name);
        if yes {
            self.pos += 1;
        }
        yes
    }

    fn expect_op(&mut self, op: &str) -> Result<(), NotNative> {
        if self.eat_op(op) {
            Ok(())
        } else {
            Err(NotNative(match self.peek() {
                Some(t) => format!("expected `{op}`, found {t}"),
                None => format!("expected `{op}`"),
            }))
        }
    }

    fn ternary(&mut self) -> Result<Node, NotNative> {
        let then = self.or()?;
        if self.eat_name("if") {
            let cond = self.or()?;
            if !self.eat_name("else") {
                return Err(NotNative("`if` without `else`".into()));
            }
            let other = self.ternary()?;
            return Ok(Node::If {
                cond: Box::new(cond),
                then: Box::new(then),
                other: Box::new(other),
            });
        }
        Ok(then)
    }

    fn or(&mut self) -> Result<Node, NotNative> {
        let mut a = self.and()?;
        while self.eat_name("or") {
            a = Node::Or(Box::new(a), Box::new(self.and()?));
        }
        Ok(a)
    }

    fn and(&mut self) -> Result<Node, NotNative> {
        let mut a = self.not()?;
        while self.eat_name("and") {
            a = Node::And(Box::new(a), Box::new(self.not()?));
        }
        Ok(a)
    }

    fn not(&mut self) -> Result<Node, NotNative> {
        if self.eat_name("not") {
            return Ok(Node::Not(Box::new(self.not()?)));
        }
        self.compare()
    }

    fn compare(&mut self) -> Result<Node, NotNative> {
        let first = self.arith()?;
        let mut rest = Vec::new();
        loop {
            let cmp = match self.peek() {
                Some(Tok::Op("<")) => Cmp::Lt,
                Some(Tok::Op("<=")) => Cmp::Le,
                Some(Tok::Op(">")) => Cmp::Gt,
                Some(Tok::Op(">=")) => Cmp::Ge,
                Some(Tok::Op("==")) => Cmp::Eq,
                Some(Tok::Op("!=")) => Cmp::Ne,
                _ => break,
            };
            self.pos += 1;
            rest.push((cmp, self.arith()?));
        }
        Ok(if rest.is_empty() {
            first
        } else {
            Node::Compare(Box::new(first), rest)
        })
    }

    fn arith(&mut self) -> Result<Node, NotNative> {
        let mut a = self.term()?;
        loop {
            let op = if self.eat_op("+") {
                Bin::Add
            } else if self.eat_op("-") {
                Bin::Sub
            } else {
                return Ok(a);
            };
            a = Node::Bin(op, Box::new(a), Box::new(self.term()?));
        }
    }

    fn term(&mut self) -> Result<Node, NotNative> {
        let mut a = self.unary()?;
        loop {
            let op = if self.eat_op("*") {
                Bin::Mul
            } else if self.eat_op("//") {
                Bin::FloorDiv
            } else if self.eat_op("/") {
                Bin::Div
            } else if self.eat_op("%") {
                Bin::Mod
            } else {
                return Ok(a);
            };
            a = Node::Bin(op, Box::new(a), Box::new(self.unary()?));
        }
    }

    fn unary(&mut self) -> Result<Node, NotNative> {
        if self.eat_op("-") {
            return Ok(Node::Neg(Box::new(self.unary()?)));
        }
        if self.eat_op("+") {
            return self.unary();
        }
        self.power()
    }

    fn power(&mut self) -> Result<Node, NotNative> {
        let base = self.primary()?;
        if self.eat_op("**") {
            // Right-associative, and binds tighter than a unary minus on
            // its left: -2 ** 2 == -4.
            return Ok(Node::Bin(Bin::Pow, Box::new(base), Box::new(self.unary()?)));
        }
        Ok(base)
    }

    fn reference(&mut self, name: String) -> usize {
        match self.refs.iter().position(|r| *r == name) {
            Some(i) => i,
            None => {
                self.refs.push(name);
                self.refs.len() - 1
            }
        }
    }

    fn primary(&mut self) -> Result<Node, NotNative> {
        let tok = self
            .tokens
            .get(self.pos)
            .cloned()
            .ok_or_else(|| NotNative("unexpected end".into()))?;
        self.pos += 1;
        match tok {
            Tok::Num(v) => Ok(Node::Num(v)),
            Tok::Op("(") => {
                let e = self.ternary()?;
                self.expect_op(")")?;
                Ok(e)
            }
            Tok::Name(name) => self.name(name),
            other => Err(NotNative(format!("unexpected {other}"))),
        }
    }

    fn name(&mut self, name: String) -> Result<Node, NotNative> {
        match name.as_str() {
            "True" => return Ok(Node::Num(1.0)),
            "False" => return Ok(Node::Num(0.0)),
            "pi" => return Ok(Node::Num(std::f64::consts::PI)),
            "e" => return Ok(Node::Num(std::f64::consts::E)),
            "tau" => return Ok(Node::Num(std::f64::consts::TAU)),
            "time" => {
                self.uses_time = true;
                return Ok(Node::Time);
            }
            "frame" => {
                self.uses_time = true;
                return Ok(Node::Frame);
            }
            "me" => {
                self.expect_op(".")?;
                if self.eat_name("digits") {
                    return Ok(Node::MeDigits);
                }
                return Err(NotNative("only `me.digits` is native".into()));
            }
            "op" => return self.op_ref(),
            k if KEYWORDS.contains(&k) => return Err(NotNative(format!("`{k}`"))),
            _ => {}
        }
        let Some((f, min, max)) = Func::from_name(&name) else {
            return Err(NotNative(format!("unknown name `{name}`")));
        };
        self.expect_op("(")?;
        let mut args = Vec::new();
        if !self.is_op(")") {
            loop {
                args.push(self.ternary()?);
                if !self.eat_op(",") || self.is_op(")") {
                    break;
                }
            }
        }
        self.expect_op(")")?;
        if args.len() < min || args.len() > max {
            return Err(NotNative(format!("`{name}` with {} arguments", args.len())));
        }
        Ok(Node::Call(f, args))
    }

    /// A cell's row or column: a name in quotes, or an expression.
    fn cell_at(&mut self) -> Result<At, NotNative> {
        if let Some(Tok::Str(name)) = self.peek().cloned() {
            self.pos += 1;
            return Ok(At::Name(name));
        }
        Ok(At::Index(Box::new(self.ternary()?)))
    }

    /// `op('name').par.param[i]`, `op('name')['channel']` or
    /// `op('name')[row, col]`.
    fn op_ref(&mut self) -> Result<Node, NotNative> {
        self.expect_op("(")?;
        let Some(Tok::Str(name)) = self.peek().cloned() else {
            return Err(NotNative("op() needs a name in quotes".into()));
        };
        self.pos += 1;
        self.expect_op(")")?;
        let node = self.reference(name);
        if self.eat_op("[") {
            let row = self.cell_at()?;
            if self.eat_op(",") {
                let col = self.cell_at()?;
                self.expect_op("]")?;
                let name = |a: &At| match a {
                    At::Name(n) => Some(n.clone()),
                    At::Index(_) => None,
                };
                let slot = self.cells.len();
                self.cells.push(CellSpec {
                    node,
                    row: name(&row),
                    col: name(&col),
                });
                return Ok(Node::Cell {
                    node,
                    slot,
                    row,
                    col,
                });
            }
            let At::Name(channel) = row else {
                return Err(NotNative("op(…)[…] needs a channel name in quotes".into()));
            };
            self.expect_op("]")?;
            self.uses_channels = true;
            return Ok(Node::Chan { node, channel });
        }
        self.expect_op(".")?;
        if !self.eat_name("par") {
            return Err(NotNative("only op(…).par and op(…)[…] are native".into()));
        }
        self.expect_op(".")?;
        let Some(Tok::Name(param)) = self.peek().cloned() else {
            return Err(NotNative("op(…).par. needs a parameter name".into()));
        };
        self.pos += 1;
        let index = if self.eat_op("[") {
            let Some(Tok::Num(i)) = self.peek().cloned() else {
                return Err(NotNative("a component index must be a number".into()));
            };
            self.pos += 1;
            self.expect_op("]")?;
            if i.fract() != 0.0 || !(0.0..=255.0).contains(&i) {
                return Err(NotNative("a component index must be 0 to 255".into()));
            }
            Some(i as usize)
        } else {
            None
        };
        Ok(Node::Par { node, param, index })
    }
}

/// What an expression gives: one number (both tiers), or, from Python
/// only, a tuple or list of up to four numbers that drives a vector or
/// colour parameter component by component (PRD A66). Copy, so a result is
/// kept and read without allocating.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExprValue {
    Number(f64),
    Tuple { len: u8, values: [f64; 4] },
}

impl ExprValue {
    /// A tuple result; `None` when it is empty or longer than four.
    pub fn tuple(values: &[f64]) -> Option<Self> {
        if values.is_empty() || values.len() > 4 {
            return None;
        }
        let mut out = [0.0; 4];
        out[..values.len()].copy_from_slice(values);
        Some(Self::Tuple {
            len: values.len() as u8,
            values: out,
        })
    }

    /// The single number, if this is one.
    pub fn number(self) -> Option<f64> {
        match self {
            Self::Number(v) => Some(v),
            Self::Tuple { .. } => None,
        }
    }

    /// Component `i` for a vector or colour: a number drives every
    /// component, a tuple its component `i` (if it has one).
    pub fn component(self, i: usize) -> Option<f64> {
        match self {
            Self::Number(v) => Some(v),
            Self::Tuple { len, values } => (i < usize::from(len)).then(|| values[i]),
        }
    }

    /// How many numbers the result has (one for a number).
    pub fn count(self) -> usize {
        match self {
            Self::Number(_) => 1,
            Self::Tuple { len, .. } => usize::from(len),
        }
    }

    pub fn is_finite(self) -> bool {
        match self {
            Self::Number(v) => v.is_finite(),
            Self::Tuple { len, values } => values[..usize::from(len)].iter().all(|v| v.is_finite()),
        }
    }
}

impl fmt::Display for ExprValue {
    /// `0.500`, or `(1.000, 0.250)` for a tuple.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(v) => write!(f, "{v:.3}"),
            Self::Tuple { len, values } => {
                f.write_str("(")?;
                for (i, v) in values[..usize::from(*len)].iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v:.3}")?;
                }
                f.write_str(")")
            }
        }
    }
}

/// A Python expression for the Python worker, refreshed every frame.
#[derive(Debug, Clone, PartialEq)]
pub struct PyJob {
    /// Identifies the parameter; its result comes back under this key.
    pub key: u64,
    /// The node and parameter it drives (the worker checks the allow-list
    /// by them and `source` before evaluating).
    pub node: crate::graph::NodeId,
    pub param: Arc<str>,
    pub source: Arc<str>,
    pub time: f64,
    pub frame: f64,
    /// The node's name (`me.name`; `me.digits` comes from it).
    pub me: Arc<str>,
    /// Nodes the source names as `op('…')`, with their values.
    pub refs: Vec<PyRef>,
}

impl Default for PyJob {
    fn default() -> Self {
        Self {
            key: 0,
            node: crate::graph::NodeId(0),
            param: Arc::from(""),
            source: Arc::from(""),
            time: 0.0,
            frame: 0.0,
            me: Arc::from(""),
            refs: Vec::new(),
        }
    }
}

/// A node an expression refers to, as seen this frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PyRef {
    pub name: Arc<str>,
    pub exists: bool,
    /// Number, toggle and vector parameters (one value per component).
    pub params: Vec<(Arc<str>, Vec<f64>)>,
    /// SIG channels: the latest value of each.
    pub channels: Vec<(Arc<str>, f64)>,
    /// Its DAT value, for `op('t')[row, col]` (shared, not copied).
    pub dat: Option<Dat>,
}

/// Why a Python-tier expression gives no value: this expression, as it is
/// written, has not been allowed (`docs/python-ops.md`). Its parameter
/// keeps its stored value.
pub const PYTHON_NOT_ALLOWED: &str = "Python expressions in this project are not allowed to run";

/// The Python worker (`atd-py`), as the engine sees it.
pub trait PythonHost: Send {
    /// Changes whenever what [`Self::allows`] answers may have changed.
    /// Read once per frame: it must be cheap.
    fn generation(&self) -> u64 {
        0
    }
    /// Whether the expression `source` on `node`'s `param` may run. The
    /// engine asks when its expressions or [`Self::generation`] change, never
    /// per frame; one that may not is offered to no worker and reports
    /// [`PYTHON_NOT_ALLOWED`]. No default: every host decides.
    fn allows(&self, node: crate::graph::NodeId, param: &str, source: &str) -> bool;
    /// Offers this frame's jobs. Never blocks: when the worker is busy they
    /// are skipped, and results arrive a frame later.
    fn submit(&mut self, jobs: &[PyJob]);
    /// Offline render only (FR-H1): offers this frame's jobs, waiting for a
    /// busy worker, and returns once they are evaluated, so the next
    /// [`Self::collect`] has every result (still one frame late, but always
    /// exactly one). The default suits hosts that evaluate in `submit`.
    fn submit_wait(&mut self, jobs: &[PyJob]) {
        self.submit(jobs);
    }
    /// Moves results that arrived since the last call into `out` (cleared
    /// first): `(key, value or error message)`.
    fn collect(&mut self, out: &mut Vec<(u64, Result<ExprValue, String>)>);
}

/// Node names written as `op('…')` or `op("…")` in Python source (a plain
/// scan, so the engine can send those nodes' values with the job).
pub fn python_refs(source: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = source;
    while let Some(i) = rest.find("op(") {
        let before = &source[..source.len() - rest.len() + i];
        rest = &rest[i + 3..];
        // `op(` must not be the end of a longer name (`loop(`).
        if before
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let trimmed = rest.trim_start();
        let Some(q) = trimmed.chars().next().filter(|c| *c == '\'' || *c == '"') else {
            continue;
        };
        if let Some(end) = trimmed[1..].find(q) {
            let name = &trimmed[1..1 + end];
            if !name.is_empty() && !out.iter().any(|n| n == name) {
                out.push(name.to_owned());
            }
        }
    }
    out
}

/// The number at the end of a node name (`noise12` → 12), as `me.digits`.
pub fn name_digits(name: &str) -> Option<f64> {
    let digits = name.len() - name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    (digits > 0).then(|| name[name.len() - digits..].parse::<f64>().ok())?
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestEnv;
    impl Env for TestEnv {
        fn time(&self) -> f64 {
            2.5
        }
        fn frame(&self) -> f64 {
            150.0
        }
        fn me_digits(&self) -> Option<f64> {
            Some(3.0)
        }
        fn param(&self, node: usize, param: &str, index: Option<usize>) -> Result<f64, EvalError> {
            match (node, param, index) {
                (0, "scale", None) => Ok(4.0),
                (0, "size", Some(1)) => Ok(0.25),
                (0, "size", Some(_)) => Err(EvalError::Index(0)),
                _ => Err(EvalError::NoParam(node)),
            }
        }
        fn channel(&self, node: usize, channel: &str) -> Result<f64, EvalError> {
            if channel == "lfo" {
                Ok(0.5)
            } else {
                Err(EvalError::NoChannel(node))
            }
        }
    }

    fn ev(src: &str) -> Result<f64, EvalError> {
        Expr::parse(src)
            .unwrap_or_else(|e| panic!("{src}: {e}"))
            .eval(&TestEnv)
    }

    #[test]
    fn python_arithmetic() {
        for (src, want) in [
            ("1 + 2 * 3", 7.0),
            ("(1 + 2) * 3", 9.0),
            ("7 / 2", 3.5),
            ("7 // 2", 3.0),
            ("-7 // 2", -4.0),
            ("-7 % 3", 2.0),
            ("7 % -3", -2.0),
            ("7.5 % 2", 1.5),
            ("2 ** 3 ** 2", 512.0),
            ("-2 ** 2", -4.0),
            ("2 ** -1", 0.5),
            ("1_000 + 1e3 + .5", 2000.5),
            ("round(2.5) + round(3.5) + round(-0.5)", 6.0),
            ("round(1.2345, 2)", 1.23),
            ("round(2.675, 2) + round(0.125, 2)", 2.79),
            ("round(1234.5, -2)", 1200.0),
            ("int(-2.7) + int(2.7)", 0.0),
            ("abs(-3) + min(4, 2, 9) + max(1, 5)", 10.0),
            ("clamp(5, 0, 1) + lerp(10, 20, 0.25)", 13.5),
            ("1 < 2 < 3", 1.0),
            ("1 < 3 < 2", 0.0),
            ("2 == 2.0", 1.0),
            ("0 or 5", 5.0),
            ("3 and 4", 4.0),
            ("0 and 4", 0.0),
            ("not 0", 1.0),
            ("10 if 1 > 2 else 20", 20.0),
            ("1 if 0 else 2 if 0 else 3", 3.0),
            ("True + True", 2.0),
            ("degrees(pi) + radians(180) / pi", 181.0),
            ("log(8, 2) + log2(8) + log10(1000)", 9.0),
            ("hypot(3, 4) + atan2(0, 1) + sqrt(16)", 9.0),
            ("floor(-1.5) + ceil(-1.5) + trunc(-1.5)", -4.0),
        ] {
            let got = ev(src).unwrap();
            assert!((got - want).abs() < 1e-12, "{src} = {got}, want {want}");
        }
    }

    #[test]
    fn context_and_references() {
        assert_eq!(ev("time * 2 + frame").unwrap(), 155.0);
        assert_eq!(ev("me.digits * 10").unwrap(), 30.0);
        assert_eq!(ev("op('noise1').par.scale / 2").unwrap(), 2.0);
        assert_eq!(ev("op(\"noise1\").par.size[1]").unwrap(), 0.25);
        assert_eq!(ev("op('lfo1')['lfo'] + 1").unwrap(), 1.5);
        let e = Expr::parse("op('a').par.x + op('b')['c'] + op('a').par.y").unwrap();
        assert_eq!(e.refs(), ["a", "b"]);
        assert!(e.uses_channels() && !e.uses_time());
        assert!(Expr::parse("frame % 2").unwrap().uses_time());
        assert!(!Expr::parse("1 + 1").unwrap().uses_time());
    }

    #[test]
    fn errors_like_python() {
        assert_eq!(ev("1 / 0"), Err(EvalError::DivisionByZero));
        assert_eq!(ev("1 // 0"), Err(EvalError::DivisionByZero));
        assert_eq!(ev("1 % 0"), Err(EvalError::DivisionByZero));
        assert_eq!(ev("0 ** -1"), Err(EvalError::DivisionByZero));
        assert_eq!(ev("(-8) ** 0.5"), Err(EvalError::Domain("pow")));
        assert_eq!(ev("sqrt(-1)"), Err(EvalError::Domain("sqrt")));
        assert_eq!(ev("log(0)"), Err(EvalError::Domain("log")));
        assert_eq!(ev("asin(2)"), Err(EvalError::Domain("asin")));
        assert_eq!(ev("op('x').par.nope"), Err(EvalError::NoParam(0)));
        assert_eq!(ev("op('x')['nope']"), Err(EvalError::NoChannel(0)));
        // Short-circuiting skips the error, as in Python.
        assert_eq!(ev("0 and 1 / 0").unwrap(), 0.0);
        assert_eq!(ev("1 if 1 else 1 / 0").unwrap(), 1.0);
        let e = Expr::parse("op('a').par.b").unwrap();
        assert_eq!(EvalError::NoNode(0).message(&e), "op('a'): no such node");
    }

    /// `op('t')` is a table; `op('note')` is text; `op('x')` has no DAT.
    struct DatEnv {
        table: Dat,
        text: Dat,
    }

    impl Env for DatEnv {
        fn time(&self) -> f64 {
            2.5
        }
        fn frame(&self) -> f64 {
            0.0
        }
        fn me_digits(&self) -> Option<f64> {
            None
        }
        fn param(&self, node: usize, _: &str, _: Option<usize>) -> Result<f64, EvalError> {
            Err(EvalError::NoParam(node))
        }
        fn channel(&self, node: usize, _: &str) -> Result<f64, EvalError> {
            Err(EvalError::NoChannel(node))
        }
        fn cell(
            &self,
            node: usize,
            slot: usize,
            row: CellAt<'_>,
            col: CellAt<'_>,
        ) -> Result<f64, EvalError> {
            let dat = match node {
                0 => &self.table,
                1 => &self.text,
                _ => return Err(EvalError::NotDat(node)),
            };
            cell_number(dat, slot, row, col)
        }
    }

    fn dat_env() -> DatEnv {
        DatEnv {
            table: Dat::table(crate::dat::Table::from_rows([
                ["name", "speed", "label"],
                ["slow", " 0.25 ", "calm"],
                ["fast", "4", "wild"],
            ])),
            text: Dat::text("10\n20\n"),
        }
    }

    fn cell_ev(src: &str) -> Result<f64, EvalError> {
        // `t`, `note` and `x` are refs 0, 1 and 2 when named in that order.
        let full = format!(
            "0 * op('t')[0, 0] if 0 else 0 * op('note')[0, 0] if 0 else 0 * op('x')[0, 0] if 0 else {src}"
        );
        Expr::parse(&full)
            .unwrap_or_else(|e| panic!("{src}: {e}"))
            .eval(&dat_env())
    }

    #[test]
    fn dat_cells_by_index_and_name() {
        for (src, want) in [
            ("op('t')[1, 1]", 0.25),
            ("op('t')[2, 'speed']", 4.0),
            ("op('t')['fast', 1]", 4.0),
            ("op('t')['slow', 'speed'] * 2", 0.5),
            ("op('t')[1 + int(time) - 2, 'speed']", 0.25),
            ("op('t')[2.0, 1]", 4.0),
            ("op('note')[1, 0] + op('note')['10', 0]", 30.0),
        ] {
            assert_eq!(cell_ev(src).unwrap(), want, "{src}");
        }
        let e = Expr::parse("op('t')[1, 'speed'] + op('t')['fast', 1] + op('u')['c']").unwrap();
        assert_eq!(e.refs(), ["t", "u"]);
        assert_eq!(e.cell_count(), 2);
        assert!(e.uses_channels() && !e.uses_time());
    }

    #[test]
    fn missing_cells_fail_clearly() {
        let msg = |src: &str| {
            let full = format!(
                "0 * op('t')[0, 0] if 0 else 0 * op('note')[0, 0] if 0 else 0 * op('x')[0, 0] if 0 else {src}"
            );
            let e = Expr::parse(&full).unwrap();
            e.eval(&dat_env()).unwrap_err().message(&e)
        };
        assert_eq!(msg("op('t')[3, 1]"), "op('t')[3, 1]: no such cell");
        assert_eq!(
            msg("op('t')[1, 'colour']"),
            "op('t')[1, 'colour']: no such cell"
        );
        assert_eq!(
            msg("op('t')['medium', 2]"),
            "op('t')['medium', 2]: no such cell"
        );
        assert_eq!(msg("op('note')[0, 1]"), "op('note')[0, 1]: no such cell");
        assert_eq!(
            msg("op('t')[1, 'label']"),
            "op('t')[1, 'label']: the cell is not a number"
        );
        assert_eq!(
            msg("op('t')[time, 0]"),
            "op('t')[…, …]: a row or column must be a whole number, 0 or more"
        );
        assert_eq!(
            msg("op('t')[-1, 0]"),
            "op('t')[…, …]: a row or column must be a whole number, 0 or more"
        );
        assert_eq!(
            msg("op('x')[0, 0]"),
            "op('x'): not a DAT, so it has no cells"
        );
        // Short-circuiting skips a missing cell, as in Python.
        assert_eq!(cell_ev("1 if 1 else op('t')[9, 9]").unwrap(), 1.0);
        // An environment without DATs has none.
        assert_eq!(ev("op('t')[0, 0]"), Err(EvalError::NotDat(0)));
    }

    #[test]
    fn anything_else_is_python() {
        for src in [
            "[x for x in range(3)][1]",
            "math.sin(1)",
            "me.name",
            "len('abc')",
            "lambda: 1",
            "x = 1",
            "1 +",
            "(1",
            "op(name).par.x",
            "op('a').inputs[0]",
            "'a' 'b'",
            "1if 1 else 2",
            "a.b",
            "min(1)",
            "None",
            "3 @ 4",
            "1 is 1",
            "sin()",
            "print('hi')",
        ] {
            assert!(Expr::parse(src).is_err(), "{src} should not be native");
        }
    }

    #[test]
    fn python_refs_finds_op_names() {
        assert_eq!(
            python_refs("op('a').par.x + op( \"b\")['c'] + loop('d') + op('a').par.y"),
            ["a", "b"]
        );
        assert!(python_refs("op(name)").is_empty());
    }

    #[test]
    fn name_digits_like_touchdesigner() {
        assert_eq!(name_digits("noise12"), Some(12.0));
        assert_eq!(name_digits("level"), None);
        assert_eq!(name_digits("a1b2"), Some(2.0));
        assert_eq!(name_digits("x007"), Some(7.0));
    }
}

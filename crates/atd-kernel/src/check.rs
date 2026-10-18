//! Type checking: resolves names, decides attribute types, inserts every implicit
//! conversion and produces the typed IR.

use std::collections::{BTreeSet, HashMap};

use crate::AttrSchema;
use crate::Target;
use crate::ast::{self, AssignOp, BinOp, ExprKind as AK, StmtKind as SK};
use crate::bindings::{MAX_PARAMS, ParamDecl, ParamType};
use crate::diag::{Error, MAX_ERRORS, Span};
use crate::ir::*;
use crate::types::{AttrType, Type, standard_attribute};

type CResult<T> = Result<T, Error>;

pub(crate) fn check(
    program: &ast::Program,
    target: Target,
    schema: Option<&[AttrSchema]>,
    errors: &mut Vec<Error>,
) -> Option<Program> {
    let mut c = Checker {
        target,
        schema,
        errors: Vec::new(),
        attrs: Vec::new(),
        attr_index: HashMap::new(),
        sigils: HashMap::new(),
        params: Vec::new(),
        images: BTreeSet::new(),
        sigs: Vec::new(),
        fn_index: HashMap::new(),
        calls: Vec::new(),
        cx: FnCx::default(),
    };
    c.collect_sigils(program);
    c.collect_functions(program);

    let mut functions = Vec::new();
    for item in &program.items {
        if let ast::Item::Func(f) = item {
            functions.push(c.function(f));
        }
    }
    let body = c.body(program);
    c.check_recursion();

    errors.append(&mut c.errors);
    if !errors.is_empty() {
        return None;
    }
    Some(Program {
        target,
        attrs: c.attrs,
        params: c.params,
        images: c.images.into_iter().collect(),
        functions,
        body,
    })
}

#[derive(Clone, Debug)]
struct FnSig {
    name: String,
    params: Vec<Type>,
    ret: Type,
    span: Span,
}

/// State while checking one function (or the kernel body).
#[derive(Debug, Default)]
struct FnCx {
    locals: Vec<Local>,
    readonly: Vec<bool>,
    scopes: Vec<Vec<(String, LocalId)>>,
    ret: Type,
    /// The kernel body (may use `@`); otherwise a user function.
    is_body: bool,
    loops: u32,
    /// Index into `calls`: the function's id, or `sigs.len()` for the body.
    id: usize,
}

#[derive(Debug)]
struct Checker<'a> {
    target: Target,
    schema: Option<&'a [AttrSchema]>,
    errors: Vec<Error>,
    attrs: Vec<Attr>,
    attr_index: HashMap<String, AttrId>,
    /// The type a sigil gives each attribute, and where.
    sigils: HashMap<String, (Type, Span)>,
    params: Vec<ParamDecl>,
    images: BTreeSet<u32>,
    sigs: Vec<FnSig>,
    fn_index: HashMap<String, FnId>,
    /// Functions each function calls (the body last).
    calls: Vec<BTreeSet<FnId>>,
    cx: FnCx,
}

/// Names a user function may not take.
const RESERVED_FNS: &[&str] = &[
    "ch",
    "chf",
    "chi",
    "chu",
    "chv",
    "chp",
    "point",
    "npoints",
    "nearpoint",
    "nearpoints",
    "texture",
    "set",
    "float",
    "int",
    "bool",
    "vec2",
    "vec3",
    "vec4",
    "mat3",
    "mat4",
    "ident",
    "ident3",
    "fit01",
    "smooth",
    "lerp",
    "frac",
];

fn sigil_type(s: char) -> Option<Type> {
    Some(match s {
        'f' => Type::Float,
        'i' => Type::Int,
        'u' => Type::Vec(2),
        'v' => Type::Vec(3),
        'p' => Type::Vec(4),
        _ => return None,
    })
}

fn int_lit(e: &ast::Expr) -> Option<i64> {
    match &e.kind {
        AK::Int(n) => i64::try_from(*n).ok(),
        AK::IntBits(n) => Some(i64::from(*n as i32)),
        AK::Unary(ast::UnOp::Neg, inner) => int_lit(inner).map(|n| -n),
        AK::Unary(ast::UnOp::Plus, inner) => int_lit(inner),
        _ => None,
    }
}

/// Evaluates a constant `ch()` argument: numbers, `-`, `{}` and vector constructors.
fn const_value(e: &ast::Expr) -> Option<Vec<f64>> {
    match &e.kind {
        AK::Int(n) => Some(vec![*n as f64]),
        AK::IntBits(n) => Some(vec![f64::from(*n as i32)]),
        AK::Float(v) => Some(vec![*v]),
        AK::Bool(b) => Some(vec![f64::from(u8::from(*b))]),
        AK::Unary(ast::UnOp::Neg, inner) => {
            const_value(inner).map(|v| v.into_iter().map(|x| -x).collect())
        }
        AK::Unary(ast::UnOp::Plus, inner) => const_value(inner),
        AK::Braces(items) => flatten(items),
        AK::Call { name, args, .. }
            if matches!(
                name.as_str(),
                "set" | "vec2" | "vec3" | "vec4" | "float" | "int"
            ) =>
        {
            flatten(args)
        }
        _ => None,
    }
}

fn flatten(items: &[ast::Expr]) -> Option<Vec<f64>> {
    let mut out = Vec::new();
    for i in items {
        out.extend(const_value(i)?);
    }
    Some(out)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Every builtin name, for suggestions.
const KNOWN_FNS: &[&str] = &[
    "abs",
    "sign",
    "floor",
    "ceil",
    "frac",
    "fract",
    "trunc",
    "round",
    "sqrt",
    "exp",
    "log",
    "log2",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "radians",
    "degrees",
    "pow",
    "min",
    "max",
    "step",
    "clamp",
    "lerp",
    "mix",
    "smoothstep",
    "smooth",
    "fit",
    "fit01",
    "length",
    "length2",
    "distance",
    "distance2",
    "normalize",
    "dot",
    "cross",
    "transpose",
    "determinant",
    "invert",
    "ident",
    "ident3",
    "quaternion",
    "qmultiply",
    "qrotate",
    "qinvert",
    "qconvert",
    "slerp",
    "perlin",
    "simplex",
    "worley",
    "curlnoise",
    "rand",
    "ch",
    "chf",
    "chi",
    "chu",
    "chv",
    "chp",
    "point",
    "npoints",
    "nearpoint",
    "nearpoints",
    "texture",
    "set",
    "float",
    "int",
    "bool",
    "vec2",
    "vec3",
    "vec4",
    "mat3",
    "mat4",
];

impl Checker<'_> {
    fn push(&mut self, e: Error) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(e);
        }
    }

    // ---- pre-passes ------------------------------------------------------------------

    /// Records the sigil types of `@name` uses in the kernel body, so a sigil on one use
    /// types them all.
    fn collect_sigils(&mut self, program: &ast::Program) {
        let mut found: Vec<(String, Type, Span)> = Vec::new();
        for item in &program.items {
            if let ast::Item::Stmt(s) = item {
                walk_stmt(s, &mut |e| {
                    if let AK::Attr {
                        sigil: Some(s),
                        name,
                    } = &e.kind
                        && let Some(t) = sigil_type(*s)
                    {
                        found.push((name.clone(), t, e.span));
                    }
                });
            }
        }
        for (name, ty, span) in found {
            match self.sigils.get(&name) {
                Some((prev, _)) if *prev != ty => {
                    let prev = *prev;
                    self.push(
                        Error::new(
                            span,
                            format!("`@{name}` is used as `{prev}` elsewhere and `{ty}` here"),
                        )
                        .hint("use one type sigil per attribute"),
                    );
                }
                Some(_) => {}
                None => {
                    self.sigils.insert(name, (ty, span));
                }
            }
        }
    }

    fn collect_functions(&mut self, program: &ast::Program) {
        for item in &program.items {
            let ast::Item::Func(f) = item else { continue };
            if RESERVED_FNS.contains(&f.name.as_str()) || KNOWN_FNS.contains(&f.name.as_str()) {
                self.push(Error::new(
                    f.name_span,
                    format!(
                        "`{}` is a built-in function and cannot be redefined",
                        f.name
                    ),
                ));
            }
            if self.fn_index.contains_key(&f.name) {
                self.push(Error::new(
                    f.name_span,
                    format!("function `{}` is defined twice", f.name),
                ));
                continue;
            }
            let id = self.sigs.len() as FnId;
            self.fn_index.insert(f.name.clone(), id);
            self.sigs.push(FnSig {
                name: f.name.clone(),
                params: f.params.iter().map(|p| p.ty).collect(),
                ret: f.ret,
                span: f.name_span,
            });
        }
        self.calls = vec![BTreeSet::new(); self.sigs.len() + 1];
    }

    fn check_recursion(&mut self) {
        // A function is recursive if it can reach itself.
        let n = self.sigs.len();
        for start in 0..n {
            let mut seen = vec![false; n];
            let mut stack: Vec<FnId> = self.calls[start].iter().copied().collect();
            while let Some(f) = stack.pop() {
                let f = f as usize;
                if f == start {
                    let name = self.sigs[start].name.clone();
                    let span = self.sigs[start].span;
                    self.push(
                        Error::new(span, format!("function `{name}` calls itself"))
                            .hint("recursion is not supported on the GPU; use a loop"),
                    );
                    break;
                }
                if !seen[f] {
                    seen[f] = true;
                    stack.extend(self.calls[f].iter().copied());
                }
            }
        }
    }

    // ---- functions and scopes --------------------------------------------------------

    fn function(&mut self, f: &ast::FuncDef) -> Function {
        let id = self
            .fn_index
            .get(&f.name)
            .copied()
            .map_or(usize::MAX, |i| i as usize);
        self.cx = FnCx {
            ret: f.ret,
            is_body: false,
            id,
            scopes: vec![Vec::new()],
            ..FnCx::default()
        };
        let mut params = Vec::new();
        for p in &f.params {
            if self.cx.scopes[0].iter().any(|(n, _)| *n == p.name) {
                self.push(Error::new(
                    p.span,
                    format!("parameter `{}` is declared twice", p.name),
                ));
            }
            params.push(self.declare(&p.name, p.ty));
        }
        let errors_before = self.errors.len();
        let body = self.block(&f.body);
        // A statement that failed to check is missing from `body`; do not also report
        // the missing return it causes.
        if f.ret != Type::Void && self.errors.len() == errors_before && !always_returns(&body) {
            self.push(
                Error::new(
                    f.name_span,
                    format!(
                        "function `{}` can end without returning a `{}`",
                        f.name, f.ret
                    ),
                )
                .hint("add a return statement at the end"),
            );
        }
        let cx = std::mem::take(&mut self.cx);
        Function {
            name: f.name.clone(),
            ret: f.ret,
            params,
            locals: cx.locals,
            body,
            span: f.span,
        }
    }

    fn body(&mut self, program: &ast::Program) -> Function {
        self.cx = FnCx {
            ret: Type::Void,
            is_body: true,
            id: self.sigs.len(),
            scopes: vec![Vec::new()],
            ..FnCx::default()
        };
        let mut body = Vec::new();
        for item in &program.items {
            if let ast::Item::Stmt(s) = item {
                self.stmt(s, &mut body);
            }
        }
        let cx = std::mem::take(&mut self.cx);
        Function {
            name: "kernel".into(),
            ret: Type::Void,
            params: Vec::new(),
            locals: cx.locals,
            body,
            span: Span::default(),
        }
    }

    fn declare(&mut self, name: &str, ty: Type) -> LocalId {
        let id = self.cx.locals.len() as LocalId;
        self.cx.locals.push(Local {
            name: name.to_owned(),
            ty,
        });
        self.cx.readonly.push(false);
        if let Some(scope) = self.cx.scopes.last_mut() {
            scope.push((name.to_owned(), id));
        }
        id
    }

    fn lookup(&self, name: &str) -> Option<LocalId> {
        self.cx
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.iter().rev().find(|(n, _)| n == name).map(|(_, id)| *id))
    }

    fn declared_here(&self, name: &str) -> bool {
        self.cx
            .scopes
            .last()
            .is_some_and(|s| s.iter().any(|(n, _)| n == name))
    }

    fn block(&mut self, stmts: &[ast::Stmt]) -> Vec<Stmt> {
        self.cx.scopes.push(Vec::new());
        let mut out = Vec::new();
        for s in stmts {
            self.stmt(s, &mut out);
        }
        self.cx.scopes.pop();
        out
    }

    /// A statement as a block of its own (the branches of `if`, loop bodies).
    fn scoped(&mut self, s: &ast::Stmt) -> Vec<Stmt> {
        match &s.kind {
            SK::Block(stmts) => self.block(stmts),
            _ => self.block(std::slice::from_ref(s)),
        }
    }

    // ---- statements ------------------------------------------------------------------

    /// Checks `s` and appends the result to `out` (declarations stay in the enclosing
    /// scope, so `int a = 1, b = a;` appends two statements).
    fn stmt(&mut self, s: &ast::Stmt, out: &mut Vec<Stmt>) {
        if let SK::Decl { ty, vars } = &s.kind {
            self.decl(*ty, vars, s.span, out);
            return;
        }
        match self.stmt_inner(s) {
            Ok(Some(kind)) => out.push(Stmt { kind, span: s.span }),
            Ok(None) => {}
            Err(e) => self.push(e),
        }
    }

    fn decl(
        &mut self,
        ty: Option<Type>,
        vars: &[ast::Declarator],
        span: Span,
        out: &mut Vec<Stmt>,
    ) {
        for d in vars {
            let init = match &d.init {
                Some(e) => match self.expr(e) {
                    Ok(v) => Some(v),
                    Err(err) => {
                        self.push(err);
                        None
                    }
                },
                None => None,
            };
            let (decl_ty, init) = match (ty, init) {
                (Some(t), Some(v)) => match self.coerce(v, t) {
                    Ok(v) => (t, Some(v)),
                    Err(err) => {
                        self.push(err);
                        (t, None)
                    }
                },
                (Some(t), None) => (t, None),
                (None, Some(v)) => {
                    if v.ty == Type::Void {
                        self.push(Error::new(v.span, "this call returns nothing"));
                        continue;
                    }
                    (v.ty, Some(v))
                }
                // A `let` whose value failed: already reported.
                (None, None) => continue,
            };
            if self.declared_here(&d.name) {
                self.push(
                    Error::new(d.span, format!("`{}` is already declared here", d.name))
                        .hint("assign to it, or pick another name"),
                );
            }
            let id = self.declare(&d.name, decl_ty);
            out.push(Stmt {
                kind: StmtKind::Decl(id, init),
                span: d.span.to(span),
            });
        }
    }

    fn stmt_inner(&mut self, s: &ast::Stmt) -> CResult<Option<StmtKind>> {
        Ok(Some(match &s.kind {
            SK::Empty | SK::Decl { .. } => return Ok(None),
            SK::Block(stmts) => StmtKind::Block(self.block(stmts)),
            SK::Assign { target, op, value } => {
                let place = self.place(target, *op != AssignOp::Set)?;
                let v = self.expr(value)?;
                let v = match assign_binop(*op) {
                    None => v,
                    Some(bop) => {
                        let cur = self.place_value(&place, target.span);
                        self.binary(bop, cur, v, s.span)?
                    }
                };
                let v = self.coerce(v, place.ty)?;
                StmtKind::Assign(place, v)
            }
            SK::IncDec { target, inc } => {
                let place = self.place(target, true)?;
                if !place.ty.is_scalar_number() {
                    return Err(Error::new(
                        target.span,
                        format!("`++`/`--` need an `int` or a `float`, found `{}`", place.ty),
                    ));
                }
                let cur = self.place_value(&place, target.span);
                let one = if place.ty == Type::Int {
                    Expr::new(ExprKind::Int(1), Type::Int, s.span)
                } else {
                    Expr::new(ExprKind::Float(1.0), Type::Float, s.span)
                };
                let op = if *inc { BinOp::Add } else { BinOp::Sub };
                let v = self.binary(op, cur, one, s.span)?;
                StmtKind::Assign(place, v)
            }
            SK::Expr(e) => {
                let v = self.expr(e)?;
                if !matches!(v.kind, ExprKind::Call(..)) {
                    return Err(Error::new(e.span, "this statement has no effect")
                        .hint("assign the value to a variable or an attribute"));
                }
                StmtKind::Expr(v)
            }
            SK::If { cond, then, els } => {
                let c = self.expr(cond)?;
                let c = self.condition(c)?;
                let t = self.scoped(then);
                let e = els.as_ref().map(|e| self.scoped(e)).unwrap_or_default();
                StmtKind::If(c, t, e)
            }
            SK::For {
                init,
                cond,
                step,
                body,
            } => return self.for_loop(s, init.as_deref(), cond.as_ref(), step.as_deref(), body),
            SK::Foreach {
                ty,
                var,
                var_span,
                source,
                body,
            } => return self.foreach(*ty, var, *var_span, source, body),
            SK::Break | SK::Continue => {
                if self.cx.loops == 0 {
                    return Err(Error::new(
                        s.span,
                        "`break` and `continue` only work inside a loop",
                    ));
                }
                if matches!(s.kind, SK::Break) {
                    StmtKind::Break
                } else {
                    StmtKind::Continue
                }
            }
            SK::Return(value) => {
                let ret = self.cx.ret;
                match (value, ret) {
                    (None, Type::Void) => StmtKind::Return(None),
                    (None, t) => {
                        return Err(Error::new(
                            s.span,
                            format!("this function must return a `{t}`"),
                        ));
                    }
                    (Some(v), Type::Void) => {
                        let what = if self.cx.is_body {
                            "the kernel body returns nothing"
                        } else {
                            "this function returns nothing (`void`)"
                        };
                        return Err(Error::new(v.span, what).hint("write `return;`"));
                    }
                    (Some(v), t) => {
                        let v = self.expr(v)?;
                        StmtKind::Return(Some(self.coerce(v, t)?))
                    }
                }
            }
        }))
    }

    fn for_loop(
        &mut self,
        s: &ast::Stmt,
        init: Option<&ast::Stmt>,
        cond: Option<&ast::Expr>,
        step: Option<&ast::Stmt>,
        body: &ast::Stmt,
    ) -> CResult<Option<StmtKind>> {
        let form = "loops must count: for (int i = START; i < END; i++)";
        let bad = |span: Span, msg: &str| Error::new(span, msg.to_owned()).hint(form);
        // init: `int i = START`
        let Some(init) = init else {
            return Err(bad(s.span, "a `for` loop needs a counter"));
        };
        let SK::Decl {
            ty: Some(Type::Int),
            vars,
        } = &init.kind
        else {
            return Err(bad(init.span, "the loop must declare an `int` counter"));
        };
        let [
            ast::Declarator {
                name,
                span: var_span,
                init: Some(start),
            },
        ] = vars.as_slice()
        else {
            return Err(bad(
                init.span,
                "the loop must declare one counter with a start value",
            ));
        };
        // cond: `i CMP END`
        let Some(cond) = cond else {
            return Err(bad(s.span, "the loop needs a condition"));
        };
        let AK::Binary(op, lhs, end) = &cond.kind else {
            return Err(bad(cond.span, "the condition must compare the counter"));
        };
        let cmp = match op {
            BinOp::Lt => Cmp::Lt,
            BinOp::Le => Cmp::Le,
            BinOp::Gt => Cmp::Gt,
            BinOp::Ge => Cmp::Ge,
            _ => return Err(bad(cond.span, "the condition must be <, <=, > or >=")),
        };
        if !matches!(&lhs.kind, AK::Ident(n) if n == name) {
            return Err(bad(
                lhs.span,
                &format!("the condition must start with the counter `{name}`"),
            ));
        }
        // step: `i++`, `i--`, `i += K`, `i -= K`
        let Some(step) = step else {
            return Err(bad(s.span, "the loop needs a step"));
        };
        let is_counter = |e: &ast::Expr| matches!(&e.kind, AK::Ident(n) if n == name);
        let delta: i64 = match &step.kind {
            SK::IncDec { target, inc } if is_counter(target) => {
                if *inc {
                    1
                } else {
                    -1
                }
            }
            SK::Assign {
                target,
                op: op @ (AssignOp::Add | AssignOp::Sub),
                value,
            } if is_counter(target) => {
                let Some(k) = int_lit(value).filter(|k| *k > 0 && *k <= i64::from(i32::MAX)) else {
                    return Err(bad(
                        value.span,
                        "the step must be a positive integer literal",
                    ));
                };
                if *op == AssignOp::Add { k } else { -k }
            }
            _ => {
                return Err(bad(
                    step.span,
                    "the step must change the counter by a constant",
                ));
            }
        };
        let up = matches!(cmp, Cmp::Lt | Cmp::Le);
        if up != (delta > 0) {
            return Err(Error::new(
                step.span,
                "the step counts away from the end, so the loop would never finish",
            )
            .hint(if up {
                "count up: i++"
            } else {
                "count down: i--"
            }));
        }
        let start = self.expr(start)?;
        let start = self.coerce(start, Type::Int)?;
        let end = self.expr(end)?;
        let end = self.coerce_int_bound(end)?;
        self.cx.scopes.push(Vec::new());
        let var = self.declare(name, Type::Int);
        self.cx.readonly[var as usize] = true;
        self.cx.loops += 1;
        let body = self.scoped(body);
        self.cx.loops -= 1;
        self.cx.scopes.pop();
        let _ = var_span;
        Ok(Some(StmtKind::For {
            var,
            start,
            end,
            cmp,
            step: delta as i32,
            body,
        }))
    }

    fn coerce_int_bound(&mut self, end: Expr) -> CResult<Expr> {
        if end.ty == Type::Float {
            return Err(Error::new(end.span, "the loop end must be an `int`")
                .hint("convert it with int(...)"));
        }
        self.coerce(end, Type::Int)
    }

    fn foreach(
        &mut self,
        ty: Type,
        var: &str,
        var_span: Span,
        source: &ast::Expr,
        body: &ast::Stmt,
    ) -> CResult<Option<StmtKind>> {
        let AK::Call { name, args, .. } = &source.kind else {
            return Err(
                Error::new(source.span, "foreach can only loop over nearpoints(...)")
                    .hint("foreach (int j; nearpoints(0, @P, radius, maxpts))"),
            );
        };
        if name != "nearpoints" {
            return Err(
                Error::new(source.span, "foreach can only loop over nearpoints(...)")
                    .hint("foreach (int j; nearpoints(0, @P, radius, maxpts))"),
            );
        }
        if ty != Type::Int {
            return Err(Error::new(
                var_span,
                "the foreach variable must be an `int` point number",
            ));
        }
        self.points_only("nearpoints", source.span)?;
        if args.len() != 4 {
            return Err(Error::new(
                source.span,
                format!(
                    "`nearpoints` takes 4 arguments (geometry, position, radius, maximum), found {}",
                    args.len()
                ),
            ));
        }
        self.geo_arg(&args[0])?;
        let pos = self.expr(&args[1])?;
        let pos = self.coerce(pos, Type::Vec(3))?;
        let radius = self.expr(&args[2])?;
        let radius = self.coerce(radius, Type::Float)?;
        let max = self.expr(&args[3])?;
        let max = self.coerce(max, Type::Int)?;
        self.position_attr(source.span)?;
        self.cx.scopes.push(Vec::new());
        let id = self.declare(var, Type::Int);
        self.cx.readonly[id as usize] = true;
        self.cx.loops += 1;
        let body = self.scoped(body);
        self.cx.loops -= 1;
        self.cx.scopes.pop();
        Ok(Some(StmtKind::NearPoints {
            var: id,
            pos,
            radius,
            max,
            body,
        }))
    }

    // ---- places ----------------------------------------------------------------------

    /// The target of an assignment. `reads` marks a read-modify-write.
    fn place(&mut self, e: &ast::Expr, reads: bool) -> CResult<Place> {
        match &e.kind {
            AK::Ident(name) => {
                let Some(id) = self.lookup(name) else {
                    return Err(self.unknown_var(name, e.span));
                };
                if self.cx.readonly[id as usize] {
                    return Err(Error::new(
                        e.span,
                        format!("the loop counter `{name}` cannot be assigned in the loop"),
                    ));
                }
                Ok(Place {
                    base: PlaceBase::Local(id),
                    comp: None,
                    ty: self.cx.locals[id as usize].ty,
                })
            }
            AK::Attr { sigil, name } => {
                let r = self.attr_ref(*sigil, name, e.span)?;
                let AttrRef::Attr(id) = r else {
                    return Err(Error::new(e.span, format!("`@{name}` is read-only")));
                };
                let a = &mut self.attrs[id as usize];
                a.written = true;
                a.read |= reads;
                Ok(Place {
                    base: PlaceBase::Attr(id),
                    comp: None,
                    ty: a.ty,
                })
            }
            AK::Member(base, comp, comp_span) => {
                let mut p = self.place(base, true)?;
                if p.comp.is_some() {
                    return Err(Error::new(e.span, "only one component can be assigned"));
                }
                let Type::Vec(n) = p.ty else {
                    return Err(Error::new(
                        *comp_span,
                        format!("`.{comp}` needs a vector, found `{}`", p.ty),
                    ));
                };
                let idx = swizzle(comp, n, *comp_span)?;
                if idx.len() != 1 {
                    return Err(Error::new(
                        *comp_span,
                        "only one component can be assigned at a time",
                    )
                    .hint("assign a whole vector, e.g. v = set(a, b, v.z)"));
                }
                p.comp = Some(Comp::Const(idx[0]));
                p.ty = Type::Float;
                Ok(p)
            }
            AK::Index(base, index) => {
                let mut p = self.place(base, true)?;
                if p.comp.is_some() {
                    return Err(Error::new(e.span, "only one component can be assigned"));
                }
                let (n, ty) = match p.ty {
                    Type::Vec(n) => (n, Type::Float),
                    // A matrix column.
                    Type::Mat(n) => (n, Type::Vec(n)),
                    t => {
                        return Err(Error::new(
                            base.span,
                            format!("only vectors and matrices can be indexed, found `{t}`"),
                        ));
                    }
                };
                p.comp = Some(self.component_index(index, n)?);
                p.ty = ty;
                Ok(p)
            }
            _ => Err(Error::new(e.span, "this cannot be assigned to")
                .hint("assign to a variable, an attribute or one component")),
        }
    }

    fn component_index(&mut self, index: &ast::Expr, n: u8) -> CResult<Comp> {
        if let Some(k) = int_lit(index) {
            if k < 0 || k >= i64::from(n) {
                return Err(Error::new(
                    index.span,
                    format!("index {k} is out of range for a {n}-component vector"),
                ));
            }
            return Ok(Comp::Const(k as u8));
        }
        let i = self.expr(index)?;
        if i.ty != Type::Int {
            return Err(Error::new(
                i.span,
                format!("an index must be an `int`, found `{}`", i.ty),
            )
            .hint("convert it with int(...)"));
        }
        Ok(Comp::Dynamic(Box::new(i)))
    }

    /// The current value of a place (for `+=` and `++`).
    fn place_value(&self, p: &Place, span: Span) -> Expr {
        let base = match p.base {
            PlaceBase::Local(id) => {
                Expr::new(ExprKind::Local(id), self.cx.locals[id as usize].ty, span)
            }
            PlaceBase::Attr(id) => Expr::new(ExprKind::Attr(id), self.attrs[id as usize].ty, span),
        };
        match &p.comp {
            None => base,
            Some(Comp::Const(k)) if matches!(base.ty, Type::Vec(_)) => {
                Expr::new(ExprKind::Swizzle(Box::new(base), vec![*k]), p.ty, span)
            }
            Some(Comp::Const(k)) => {
                let i = Expr::new(ExprKind::Int(i32::from(*k)), Type::Int, span);
                Expr::new(ExprKind::Index(Box::new(base), Box::new(i)), p.ty, span)
            }
            Some(Comp::Dynamic(i)) => {
                Expr::new(ExprKind::Index(Box::new(base), i.clone()), p.ty, span)
            }
        }
    }

    // ---- attributes ------------------------------------------------------------------

    fn attr_ref(&mut self, sigil: Option<char>, name: &str, span: Span) -> CResult<AttrRef> {
        if !self.cx.is_body {
            return Err(
                Error::new(span, format!("`@{name}` cannot be used inside a function"))
                    .hint("pass the attribute in as an argument"),
            );
        }
        if name.is_empty() {
            return Err(Error::new(span, "expected an attribute name after `@`"));
        }
        if !is_identifier(name) {
            return Err(
                Error::new(span, format!("`{name}` is not a valid attribute name"))
                    .hint("use letters, digits and _, starting with a letter"),
            );
        }
        let sig_ty = match sigil {
            None => None,
            Some('s') => {
                return Err(Error::new(
                    span,
                    "string attributes (`s@`) are not supported",
                ));
            }
            Some('2' | '3' | '4') => {
                return Err(Error::new(
                    span,
                    "matrix attributes are not supported (the attribute model has 1 to 4 components)",
                ));
            }
            Some(c) => sigil_type(c),
        };
        if let Some(g) = self.global(name) {
            if let Some(t) = sig_ty
                && t != g.ty()
            {
                return Err(Error::new(
                    span,
                    format!("`@{name}` is a `{}`, not a `{t}`", g.ty()),
                ));
            }
            return Ok(AttrRef::Global(g));
        }
        if self.target == Target::Pixels && name != "Cd" {
            return Err(
                Error::new(span, format!("pixel kernels have no attribute `@{name}`"))
                    .hint("use @Cd, @P, @ix, @iy, @res, @Time, @TimeInc or @Frame"),
            );
        }
        if let Some(&id) = self.attr_index.get(name) {
            return Ok(AttrRef::Attr(id));
        }
        let (ty, storage) = self.attr_type(name, span)?;
        let id = self.attrs.len() as AttrId;
        self.attrs.push(Attr {
            name: name.to_owned(),
            ty,
            storage,
            read: false,
            written: false,
            read_other: false,
            span,
        });
        self.attr_index.insert(name.to_owned(), id);
        Ok(AttrRef::Attr(id))
    }

    fn global(&self, name: &str) -> Option<Global> {
        Some(match (self.target, name) {
            (_, "Time") => Global::Time,
            (_, "TimeInc") => Global::TimeInc,
            (_, "Frame") => Global::Frame,
            (Target::Points, "ptnum") => Global::PtNum,
            (Target::Points, "numpt") => Global::NumPt,
            (Target::Pixels, "ix") => Global::Ix,
            (Target::Pixels, "iy") => Global::Iy,
            (Target::Pixels, "P") => Global::PixelP,
            (Target::Pixels, "res") => Global::Res,
            _ => return None,
        })
    }

    /// Decides an attribute's kernel type and storage (docs/kernel-dsl.md, "Attributes").
    fn attr_type(&self, name: &str, span: Span) -> CResult<(Type, AttrType)> {
        if self.target == Target::Pixels {
            // Only `@Cd`, the output colour.
            if let Some((t, s)) = self.sigils.get(name)
                && *t != Type::Vec(4)
            {
                return Err(Error::new(*s, "`@Cd` is a `vec4` in pixel kernels"));
            }
            return Ok((Type::Vec(4), AttrType::VEC4));
        }
        let sig = self.sigils.get(name).map(|(t, _)| *t);
        if let Some(schema) = self.schema
            && let Some(a) = schema.iter().find(|a| a.name == name)
        {
            let Some(kt) = a.ty.kernel_type() else {
                return Err(Error::new(
                    span,
                    format!(
                        "attribute `{name}` is stored as `{}`, which kernels cannot use yet",
                        a.ty
                    ),
                ));
            };
            if let Some(t) = sig
                && t != kt
            {
                let at = self.sigils.get(name).map_or(span, |(_, s)| *s);
                return Err(Error::new(
                    at,
                    format!("the input's `{name}` is a `{kt}`, but the sigil says `{t}`"),
                )
                .hint(format!("drop the sigil or use the input's type, `{kt}`")));
            }
            return Ok((kt, a.ty));
        }
        let std = standard_attribute(name);
        let ty = sig
            .or_else(|| std.and_then(AttrType::kernel_type))
            .unwrap_or(Type::Float);
        let storage = match std {
            Some(s) if s.kernel_type() == Some(ty) => s,
            _ => AttrType::for_kernel_type(ty).unwrap_or(AttrType::F32),
        };
        Ok((ty, storage))
    }

    /// `point()`/`nearpoint(s)` read attribute `name` at other points.
    fn other_attr(&mut self, name: &str, span: Span) -> CResult<AttrId> {
        let saved = self.cx.is_body;
        // Allowed in functions too: it reads only the input.
        self.cx.is_body = true;
        let r = self.attr_ref(None, name, span);
        self.cx.is_body = saved;
        match r? {
            AttrRef::Attr(id) => {
                self.attrs[id as usize].read_other = true;
                Ok(id)
            }
            AttrRef::Global(_) => Err(Error::new(
                span,
                format!("`{name}` is not a point attribute"),
            )),
        }
    }

    fn position_attr(&mut self, span: Span) -> CResult<AttrId> {
        let id = self.other_attr("P", span)?;
        if self.attrs[id as usize].ty != Type::Vec(3) {
            return Err(Error::new(span, "nearpoint(s) need `P` to be a `vec3`"));
        }
        Ok(id)
    }

    fn points_only(&self, what: &str, span: Span) -> CResult<()> {
        if self.target != Target::Points {
            return Err(Error::new(
                span,
                format!("`{what}` only works in point kernels"),
            ));
        }
        Ok(())
    }

    fn geo_arg(&self, e: &ast::Expr) -> CResult<()> {
        if int_lit(e) != Some(0) {
            return Err(Error::new(
                e.span,
                "only input 0 (the kernel's own geometry) is supported",
            )
            .hint("write 0"));
        }
        Ok(())
    }

    fn unknown_var(&self, name: &str, span: Span) -> Error {
        let e = Error::new(span, format!("unknown variable `{name}`"));
        if standard_attribute(name).is_some() || self.attr_index.contains_key(name) {
            e.hint(format!("attributes are written with @: @{name}"))
        } else if let Some(t) = Type::from_keyword(name) {
            e.hint(format!(
                "`{name}` is a type; declare a variable as {t} x = ..."
            ))
        } else {
            e
        }
    }

    // ---- expressions -----------------------------------------------------------------

    fn expr(&mut self, e: &ast::Expr) -> CResult<Expr> {
        let span = e.span;
        Ok(match &e.kind {
            AK::Int(n) => {
                let Ok(v) = i32::try_from(*n) else {
                    return Err(Error::new(span, "integer is too large for an int")
                        .hint("ints hold -2147483648 to 2147483647; use a float for more"));
                };
                Expr::new(ExprKind::Int(v), Type::Int, span)
            }
            AK::IntBits(v) => Expr::new(ExprKind::Int(*v as i32), Type::Int, span),
            AK::Float(v) => Expr::new(ExprKind::Float(*v as f32), Type::Float, span),
            AK::Bool(b) => Expr::new(ExprKind::Bool(*b), Type::Bool, span),
            AK::Str(_) => {
                return Err(Error::new(
                    span,
                    "strings can only be ch() and point() names",
                ));
            }
            AK::Ident(name) => match self.lookup(name) {
                Some(id) => Expr::new(ExprKind::Local(id), self.cx.locals[id as usize].ty, span),
                None => match name.as_str() {
                    "M_PI" => Expr::new(ExprKind::Float(std::f32::consts::PI), Type::Float, span),
                    "M_TWO_PI" => {
                        Expr::new(ExprKind::Float(std::f32::consts::TAU), Type::Float, span)
                    }
                    "M_E" => Expr::new(ExprKind::Float(std::f32::consts::E), Type::Float, span),
                    _ => return Err(self.unknown_var(name, span)),
                },
            },
            AK::Attr { sigil, name } => match self.attr_ref(*sigil, name, span)? {
                AttrRef::Global(g) => Expr::new(ExprKind::Global(g), g.ty(), span),
                AttrRef::Attr(id) => {
                    let a = &mut self.attrs[id as usize];
                    a.read = true;
                    Expr::new(ExprKind::Attr(id), a.ty, span)
                }
            },
            AK::Unary(op, inner) => {
                // `-2147483648` is a literal, not a negated overflow.
                if *op == ast::UnOp::Neg
                    && let AK::Int(n) = inner.kind
                    && n == 1 << 31
                {
                    return Ok(Expr::new(ExprKind::Int(i32::MIN), Type::Int, span));
                }
                let v = self.expr(inner)?;
                self.unary(*op, v, span)?
            }
            AK::Binary(op, l, r) => {
                let l = self.expr(l)?;
                let r = self.expr(r)?;
                self.binary(*op, l, r, span)?
            }
            AK::Ternary(c, a, b) => {
                let c = self.expr(c)?;
                let c = self.condition(c)?;
                let a = self.expr(a)?;
                let b = self.expr(b)?;
                let Some(t) = unify(a.ty, b.ty) else {
                    return Err(Error::new(
                        span,
                        format!("the two sides of `? :` differ: `{}` and `{}`", a.ty, b.ty),
                    ));
                };
                if t == Type::Void {
                    return Err(Error::new(span, "`? :` needs values"));
                }
                let a = self.coerce(a, t)?;
                let b = self.coerce(b, t)?;
                Expr::new(
                    ExprKind::Select(Box::new(c), Box::new(a), Box::new(b)),
                    t,
                    span,
                )
            }
            AK::Member(base, comp, comp_span) => {
                let b = self.expr(base)?;
                let Type::Vec(n) = b.ty else {
                    return Err(Error::new(
                        *comp_span,
                        format!("`.{comp}` needs a vector, found `{}`", b.ty),
                    ));
                };
                let idx = swizzle(comp, n, *comp_span)?;
                let ty = if idx.len() == 1 {
                    Type::Float
                } else {
                    Type::Vec(idx.len() as u8)
                };
                Expr::new(ExprKind::Swizzle(Box::new(b), idx), ty, span)
            }
            AK::Index(base, index) => {
                let b = self.expr(base)?;
                let (n, ty) = match b.ty {
                    Type::Vec(n) => (n, Type::Float),
                    Type::Mat(n) => (n, Type::Vec(n)),
                    t => {
                        return Err(Error::new(
                            base.span,
                            format!("only vectors and matrices can be indexed, found `{t}`"),
                        ));
                    }
                };
                match self.component_index(index, n)? {
                    Comp::Const(k) if ty == Type::Float => {
                        Expr::new(ExprKind::Swizzle(Box::new(b), vec![k]), ty, span)
                    }
                    Comp::Const(k) => Expr::new(
                        ExprKind::Index(
                            Box::new(b),
                            Box::new(Expr::new(
                                ExprKind::Int(i32::from(k)),
                                Type::Int,
                                index.span,
                            )),
                        ),
                        ty,
                        span,
                    ),
                    Comp::Dynamic(i) => Expr::new(ExprKind::Index(Box::new(b), i), ty, span),
                }
            }
            AK::Braces(items) => {
                let args = items
                    .iter()
                    .map(|a| self.expr(a))
                    .collect::<CResult<Vec<_>>>()?;
                let n: usize = args.iter().map(|a| usize::from(a.ty.components())).sum();
                if !(2..=4).contains(&n) || args.iter().any(|a| a.ty.components() == 0) {
                    return Err(Error::new(
                        span,
                        "`{...}` builds a vector from 2 to 4 numbers",
                    ));
                }
                self.construct_vec(n as u8, args, span)?
            }
            AK::Call {
                name,
                name_span,
                args,
            } => self.call(name, *name_span, args, span)?,
        })
    }

    /// A condition: `bool`, or a number compared with zero.
    fn condition(&mut self, e: Expr) -> CResult<Expr> {
        let span = e.span;
        match e.ty {
            Type::Bool => Ok(e),
            Type::Int => {
                let zero = Expr::new(ExprKind::Int(0), Type::Int, span);
                Ok(Expr::new(
                    ExprKind::Binary(BinOp::Ne, Box::new(e), Box::new(zero)),
                    Type::Bool,
                    span,
                ))
            }
            Type::Float => {
                let zero = Expr::new(ExprKind::Float(0.0), Type::Float, span);
                Ok(Expr::new(
                    ExprKind::Binary(BinOp::Ne, Box::new(e), Box::new(zero)),
                    Type::Bool,
                    span,
                ))
            }
            t => Err(Error::new(
                span,
                format!("a condition must be a `bool`, `int` or `float`, found `{t}`"),
            )),
        }
    }

    /// Converts `e` to `to` where the language converts implicitly.
    fn coerce(&mut self, e: Expr, to: Type) -> CResult<Expr> {
        let span = e.span;
        let from = e.ty;
        if from == to {
            return Ok(e);
        }
        match (from, to) {
            // A literal converts at compile time (the same rounding as at run time).
            (Type::Int, Type::Float) if matches!(e.kind, ExprKind::Int(_)) => {
                let ExprKind::Int(v) = e.kind else {
                    return Ok(e);
                };
                Ok(Expr::new(ExprKind::Float(v as f32), Type::Float, span))
            }
            (Type::Int, Type::Float) | (Type::Bool, Type::Int) => {
                Ok(Expr::new(ExprKind::Convert(Box::new(e)), to, span))
            }
            (Type::Int | Type::Float, Type::Vec(_)) => {
                let f = self.coerce(e, Type::Float)?;
                Ok(Expr::new(ExprKind::Splat(Box::new(f)), to, span))
            }
            (Type::Void, _) => Err(Error::new(span, "this call returns nothing")),
            _ => {
                let hint = match (from, to) {
                    (Type::Float, Type::Int) => Some("convert explicitly with int(...)".to_owned()),
                    (Type::Bool, Type::Float) => {
                        Some("convert explicitly with float(...)".to_owned())
                    }
                    (Type::Vec(a), Type::Vec(b)) if a > b => {
                        Some(format!("take components: .{}", &"xyzw"[..usize::from(b)]))
                    }
                    (Type::Vec(_), Type::Vec(4)) => Some("add components: vec4(v, 1)".to_owned()),
                    (Type::Vec(_), Type::Float) => {
                        Some("take one component, e.g. .x, or length(...)".to_owned())
                    }
                    _ => None,
                };
                let e = Error::new(span, format!("expected `{to}`, found `{from}`"));
                Err(match hint {
                    Some(h) => e.hint(h),
                    None => e,
                })
            }
        }
    }

    fn unary(&mut self, op: ast::UnOp, v: Expr, span: Span) -> CResult<Expr> {
        match op {
            ast::UnOp::Plus => {
                if !(v.ty.is_scalar_number() || matches!(v.ty, Type::Vec(_))) {
                    return Err(Error::new(
                        span,
                        format!("`+` needs a number, found `{}`", v.ty),
                    ));
                }
                Ok(v)
            }
            ast::UnOp::Neg => {
                if !(v.ty.is_scalar_number() || matches!(v.ty, Type::Vec(_))) {
                    return Err(Error::new(
                        span,
                        format!("`-` needs a number or vector, found `{}`", v.ty),
                    ));
                }
                let ty = v.ty;
                Ok(Expr::new(ExprKind::Unary(UnOp::Neg, Box::new(v)), ty, span))
            }
            ast::UnOp::Not => {
                let c = self.condition(v)?;
                Ok(Expr::new(
                    ExprKind::Unary(UnOp::Not, Box::new(c)),
                    Type::Bool,
                    span,
                ))
            }
            ast::UnOp::BitNot => {
                if v.ty != Type::Int {
                    return Err(Error::new(
                        span,
                        format!("`~` needs an `int`, found `{}`", v.ty),
                    ));
                }
                Ok(Expr::new(
                    ExprKind::Unary(UnOp::BitNot, Box::new(v)),
                    Type::Int,
                    span,
                ))
            }
        }
    }

    fn binary(&mut self, op: BinOp, l: Expr, r: Expr, span: Span) -> CResult<Expr> {
        let mk = |op, l: Expr, r: Expr, ty| {
            Expr::new(ExprKind::Binary(op, Box::new(l), Box::new(r)), ty, span)
        };
        let mismatch = |l: Type, r: Type| {
            Error::new(
                span,
                format!("`{}` cannot combine `{l}` and `{r}`", op.symbol()),
            )
        };
        match op {
            BinOp::And | BinOp::Or => {
                let l = self.condition(l)?;
                let r = self.condition(r)?;
                Ok(mk(op, l, r, Type::Bool))
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                let (l, r) = (self.coerce_bool_int(l), self.coerce_bool_int(r));
                if l.ty != Type::Int || r.ty != Type::Int {
                    return Err(mismatch(l.ty, r.ty).hint("bit operators work on `int`s"));
                }
                Ok(mk(op, l, r, Type::Int))
            }
            BinOp::Eq | BinOp::Ne => {
                let (lt, rt) = (l.ty, r.ty);
                let t = match (lt, rt) {
                    (Type::Vec(a), Type::Vec(b)) if a == b => lt,
                    (Type::Bool, Type::Bool) => Type::Bool,
                    _ => match unify(lt, rt) {
                        Some(t @ (Type::Int | Type::Float)) => t,
                        _ => return Err(mismatch(lt, rt)),
                    },
                };
                let l = self.coerce(l, t)?;
                let r = self.coerce(r, t)?;
                Ok(mk(op, l, r, Type::Bool))
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let (lt, rt) = (l.ty, r.ty);
                let t = match unify(lt, rt) {
                    Some(t @ (Type::Int | Type::Float))
                        if lt.is_scalar_number() && rt.is_scalar_number() =>
                    {
                        t
                    }
                    _ => {
                        let e = mismatch(lt, rt);
                        return Err(
                            if matches!(lt, Type::Vec(_)) || matches!(rt, Type::Vec(_)) {
                                e.hint("compare components (v.x < 1) or lengths")
                            } else {
                                e
                            },
                        );
                    }
                };
                let l = self.coerce(l, t)?;
                let r = self.coerce(r, t)?;
                Ok(mk(op, l, r, Type::Bool))
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                let (lt, rt) = (l.ty, r.ty);
                if lt == Type::Bool || rt == Type::Bool {
                    return Err(mismatch(lt, rt).hint("convert the bool with int(...)"));
                }
                // Matrices.
                match (op, lt, rt) {
                    (BinOp::Add | BinOp::Sub, Type::Mat(a), Type::Mat(b)) if a == b => {
                        return Ok(mk(op, l, r, lt));
                    }
                    (BinOp::Mul, Type::Mat(a), Type::Mat(b)) if a == b => {
                        return Ok(mk(op, l, r, lt));
                    }
                    (BinOp::Mul, Type::Mat(a), Type::Vec(b)) if a == b => {
                        return Ok(mk(op, l, r, rt));
                    }
                    (BinOp::Mul, Type::Vec(a), Type::Mat(b)) if a == b => {
                        return Ok(mk(op, l, r, lt));
                    }
                    (BinOp::Mul, Type::Mat(_), Type::Int | Type::Float) => {
                        let r = self.coerce(r, Type::Float)?;
                        return Ok(mk(op, l, r, lt));
                    }
                    (BinOp::Mul, Type::Int | Type::Float, Type::Mat(_)) => {
                        let l = self.coerce(l, Type::Float)?;
                        return Ok(mk(op, l, r, rt));
                    }
                    (_, Type::Mat(_), _) | (_, _, Type::Mat(_)) => return Err(mismatch(lt, rt)),
                    _ => {}
                }
                let Some(t) =
                    unify(lt, rt).filter(|t| t.is_scalar_number() || matches!(t, Type::Vec(_)))
                else {
                    return Err(mismatch(lt, rt));
                };
                let l = self.coerce(l, t)?;
                let r = self.coerce(r, t)?;
                Ok(mk(op, l, r, t))
            }
        }
    }

    fn coerce_bool_int(&mut self, e: Expr) -> Expr {
        if e.ty == Type::Bool {
            let span = e.span;
            Expr::new(ExprKind::Convert(Box::new(e)), Type::Int, span)
        } else {
            e
        }
    }

    // ---- calls -----------------------------------------------------------------------

    fn args(&mut self, args: &[ast::Expr]) -> CResult<Vec<Expr>> {
        args.iter().map(|a| self.expr(a)).collect()
    }

    fn arity(name: &str, args: &[ast::Expr], n: &[usize], span: Span) -> CResult<()> {
        if n.contains(&args.len()) {
            return Ok(());
        }
        let want = match n {
            [a] => format!("{a}"),
            [a, b] => format!("{a} or {b}"),
            _ => format!("{n:?}"),
        };
        let plural = if n == [1] { "argument" } else { "arguments" };
        Err(Error::new(
            span,
            format!("`{name}` takes {want} {plural}, found {}", args.len()),
        ))
    }

    fn call(
        &mut self,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> CResult<Expr> {
        // User functions first: they cannot shadow built-ins (checked at definition).
        if let Some(&id) = self.fn_index.get(name) {
            let sig = self.sigs[id as usize].clone();
            if args.len() != sig.params.len() {
                return Err(Error::new(
                    span,
                    format!(
                        "`{name}` takes {} arguments, found {}",
                        sig.params.len(),
                        args.len()
                    ),
                ));
            }
            let mut out = Vec::new();
            for (a, t) in args.iter().zip(&sig.params) {
                let v = self.expr(a)?;
                out.push(self.coerce(v, *t)?);
            }
            if let Some(c) = self.calls.get_mut(self.cx.id) {
                c.insert(id);
            }
            return Ok(Expr::new(ExprKind::Call(id, out), sig.ret, span));
        }
        let b = |kind: Builtin, args: Vec<Expr>, ty: Type| {
            Expr::new(ExprKind::Builtin(kind, args), ty, span)
        };
        match name {
            // Constructors and conversions.
            "float" | "int" | "bool" => {
                Self::arity(name, args, &[1], span)?;
                let v = self.expr(&args[0])?;
                let to = Type::from_keyword(name).unwrap_or(Type::Float);
                if !matches!(v.ty, Type::Bool | Type::Int | Type::Float) {
                    return Err(Error::new(
                        v.span,
                        format!("`{name}()` converts a scalar, found `{}`", v.ty),
                    )
                    .hint("take one component first, e.g. .x"));
                }
                if v.ty == to {
                    return Ok(v);
                }
                Ok(Expr::new(ExprKind::Convert(Box::new(v)), to, span))
            }
            "vec2" | "vec3" | "vec4" | "vector2" | "vector" | "vector4" | "quat" => {
                let Some(Type::Vec(n)) = Type::from_keyword(name) else {
                    return Err(Error::new(name_span, "unknown vector type"));
                };
                let v = self.args(args)?;
                self.construct_vec(n, v, span)
            }
            "set" => {
                let v = self.args(args)?;
                let n: usize = v.iter().map(|a| usize::from(a.ty.components())).sum();
                if !(2..=4).contains(&n) || v.iter().any(|a| a.ty.components() == 0) {
                    return Err(Error::new(
                        span,
                        "`set` builds a vector from 2 to 4 numbers",
                    ));
                }
                self.construct_vec(n as u8, v, span)
            }
            "mat3" | "mat4" | "matrix3" | "matrix" => {
                let Some(Type::Mat(n)) = Type::from_keyword(name) else {
                    return Err(Error::new(name_span, "unknown matrix type"));
                };
                let v = self.args(args)?;
                let n2 = usize::from(n) * usize::from(n);
                if v.is_empty() {
                    return Ok(Expr::new(ExprKind::Construct(v), Type::Mat(n), span));
                }
                if v.len() == usize::from(n) && v.iter().all(|a| a.ty == Type::Vec(n)) {
                    return Ok(Expr::new(ExprKind::Construct(v), Type::Mat(n), span));
                }
                if v.len() == n2 && v.iter().all(|a| a.ty.is_scalar_number()) {
                    let v = v
                        .into_iter()
                        .map(|a| self.coerce(a, Type::Float))
                        .collect::<CResult<Vec<_>>>()?;
                    return Ok(Expr::new(ExprKind::Construct(v), Type::Mat(n), span));
                }
                Err(Error::new(
                    span,
                    format!("`{name}` takes {n} column vectors (`vec{n}`) or {n2} numbers"),
                ))
            }
            "ident" | "ident3" => {
                Self::arity(name, args, &[0], span)?;
                let n = if name == "ident" { 4 } else { 3 };
                Ok(b(Builtin::Ident, Vec::new(), Type::Mat(n)))
            }
            // Parameters.
            "ch" | "chf" | "chi" | "chu" | "chv" | "chp" => self.ch(name, args, span),
            // Geometry.
            "point" => {
                self.points_only(name, span)?;
                Self::arity(name, args, &[3], span)?;
                self.geo_arg(&args[0])?;
                let AK::Str(attr) = &args[1].kind else {
                    return Err(Error::new(
                        args[1].span,
                        "the attribute name must be a string literal",
                    )
                    .hint("point(0, \"P\", i)"));
                };
                let id = self.other_attr(attr, args[1].span)?;
                let i = self.expr(&args[2])?;
                let i = self.coerce(i, Type::Int)?;
                let ty = self.attrs[id as usize].ty;
                Ok(Expr::new(ExprKind::PointRead(id, Box::new(i)), ty, span))
            }
            "npoints" => {
                self.points_only(name, span)?;
                Self::arity(name, args, &[1], span)?;
                self.geo_arg(&args[0])?;
                Ok(Expr::new(ExprKind::NumPoints, Type::Int, span))
            }
            "nearpoint" => {
                self.points_only(name, span)?;
                Self::arity(name, args, &[2, 3], span)?;
                self.geo_arg(&args[0])?;
                let pos = self.expr(&args[1])?;
                let pos = self.coerce(pos, Type::Vec(3))?;
                let maxd = match args.get(2) {
                    Some(a) => {
                        let v = self.expr(a)?;
                        Some(Box::new(self.coerce(v, Type::Float)?))
                    }
                    None => None,
                };
                self.position_attr(span)?;
                Ok(Expr::new(
                    ExprKind::NearPoint(Box::new(pos), maxd),
                    Type::Int,
                    span,
                ))
            }
            "nearpoints" => Err(Error::new(
                span,
                "`nearpoints` can only be the source of a foreach",
            )
            .hint("foreach (int j; nearpoints(0, @P, radius, maxpts)) { ... }")),
            "texture" => {
                Self::arity(name, args, &[1, 2], span)?;
                let (input, uv) = if args.len() == 2 {
                    let Some(k) = int_lit(&args[0]).filter(|k| (0..4).contains(k)) else {
                        return Err(Error::new(
                            args[0].span,
                            "the image input must be 0, 1, 2 or 3 (a literal)",
                        ));
                    };
                    (k as u32, &args[1])
                } else {
                    (0, &args[0])
                };
                let uv = self.expr(uv)?;
                let uv = self.coerce(uv, Type::Vec(2))?;
                self.images.insert(input);
                Ok(Expr::new(
                    ExprKind::Texture(input, Box::new(uv)),
                    Type::Vec(4),
                    span,
                ))
            }
            _ => self.math(name, name_span, args, span),
        }
    }

    fn construct_vec(&mut self, n: u8, args: Vec<Expr>, span: Span) -> CResult<Expr> {
        let ty = Type::Vec(n);
        if args.is_empty() {
            return Ok(Expr::new(ExprKind::Construct(args), ty, span));
        }
        if args.len() == 1 {
            let a = args
                .into_iter()
                .next()
                .unwrap_or_else(|| Expr::new(ExprKind::Float(0.0), Type::Float, span));
            return match a.ty {
                Type::Int | Type::Float | Type::Bool => {
                    let a = if a.ty == Type::Bool {
                        Expr::new(ExprKind::Convert(Box::new(a)), Type::Float, span)
                    } else {
                        self.coerce(a, Type::Float)?
                    };
                    Ok(Expr::new(ExprKind::Splat(Box::new(a)), ty, span))
                }
                Type::Vec(m) if m == n => Ok(a),
                Type::Vec(m) if m > n => Ok(Expr::new(
                    ExprKind::Swizzle(Box::new(a), (0..n).collect()),
                    ty,
                    span,
                )),
                t => Err(Error::new(
                    span,
                    format!("`vec{n}` needs {n} components, found one `{t}`"),
                )
                .hint(if matches!(t, Type::Vec(_)) {
                    "add components, e.g. vec4(v, 1)"
                } else {
                    "pass numbers or vectors"
                })),
            };
        }
        let mut total = 0usize;
        let mut out = Vec::new();
        for a in args {
            let c = usize::from(a.ty.components());
            if c == 0 || a.ty == Type::Bool {
                return Err(Error::new(
                    a.span,
                    format!("a vector component must be a number, found `{}`", a.ty),
                ));
            }
            total += c;
            out.push(if c == 1 {
                self.coerce(a, Type::Float)?
            } else {
                a
            });
        }
        if total != usize::from(n) {
            return Err(Error::new(
                span,
                format!("`vec{n}` needs {n} components, found {total}"),
            ));
        }
        Ok(Expr::new(ExprKind::Construct(out), ty, span))
    }

    fn ch(&mut self, name: &str, args: &[ast::Expr], span: Span) -> CResult<Expr> {
        let ty = match name {
            "chi" => ParamType::Int,
            "chu" => ParamType::Vec2,
            "chv" => ParamType::Vec3,
            "chp" => ParamType::Vec4,
            _ => ParamType::Float,
        };
        Self::arity(name, args, &[1, 2, 3, 4], span)?;
        let AK::Str(pname) = &args[0].kind else {
            return Err(
                Error::new(args[0].span, "the parameter name must be a string literal")
                    .hint(format!("{name}(\"amount\", 1.0)")),
            );
        };
        if !is_identifier(pname) {
            return Err(Error::new(
                args[0].span,
                format!("`{pname}` is not a valid parameter name"),
            )
            .hint("use letters, digits and _, starting with a letter"));
        }
        let comps = ty.components();
        let mut default = [0.0f32; 4];
        if let Some(d) = args.get(1) {
            let Some(v) = const_value(d) else {
                return Err(Error::new(d.span, "a default must be a constant")
                    .hint("e.g. 1.0 or {0, 1, 0}"));
            };
            let v: Vec<f64> = if v.len() == 1 { vec![v[0]; comps] } else { v };
            if v.len() != comps {
                return Err(Error::new(
                    d.span,
                    format!(
                        "`{name}` needs a default with {comps} components, found {}",
                        v.len()
                    ),
                ));
            }
            for (i, x) in v.iter().enumerate() {
                default[i] = if ty == ParamType::Int {
                    x.trunc() as f32
                } else {
                    *x as f32
                };
            }
        }
        let bound = |i: usize| -> CResult<Option<f32>> {
            match args.get(i) {
                None => Ok(None),
                Some(a) => match const_value(a).as_deref() {
                    Some([x]) => Ok(Some(*x as f32)),
                    _ => Err(Error::new(
                        a.span,
                        "a minimum or maximum must be a constant number",
                    )),
                },
            }
        };
        let (min, max) = (bound(2)?, bound(3)?);
        let slot = match self.params.iter().find(|p| p.name == *pname) {
            Some(p) => {
                if p.ty != ty {
                    return Err(Error::new(
                        args[0].span,
                        format!("parameter `{pname}` is used with two types"),
                    ));
                }
                if args.len() > 1 && (p.default != default || p.min != min || p.max != max) {
                    return Err(Error::new(
                        args[0].span,
                        format!("parameter `{pname}` is declared twice with different settings"),
                    )
                    .hint("give the default, minimum and maximum once; later uses can be ch(\"name\")"));
                }
                p.slot
            }
            None => {
                if self.params.len() >= MAX_PARAMS {
                    return Err(Error::new(
                        span,
                        format!("a kernel can have at most {MAX_PARAMS} parameters"),
                    ));
                }
                let slot = self.params.len() as u32;
                self.params.push(ParamDecl {
                    name: pname.clone(),
                    ty,
                    default,
                    min,
                    max,
                    slot,
                    span,
                });
                slot
            }
        };
        Ok(Expr::new(ExprKind::Param(slot), ty.kernel_type(), span))
    }

    /// Maths, vector, matrix, quaternion and noise helpers.
    fn math(
        &mut self,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> CResult<Expr> {
        use Builtin as B;
        let b = |kind: Builtin, args: Vec<Expr>, ty: Type| {
            Expr::new(ExprKind::Builtin(kind, args), ty, span)
        };
        let unary_float = match name {
            "floor" => Some(B::Floor),
            "ceil" => Some(B::Ceil),
            "frac" | "fract" => Some(B::Fract),
            "trunc" => Some(B::Trunc),
            "round" => Some(B::Round),
            "sqrt" => Some(B::Sqrt),
            "exp" => Some(B::Exp),
            "log" => Some(B::Log),
            "log2" => Some(B::Log2),
            "sin" => Some(B::Sin),
            "cos" => Some(B::Cos),
            "tan" => Some(B::Tan),
            "asin" => Some(B::Asin),
            "acos" => Some(B::Acos),
            "atan" => Some(B::Atan),
            "radians" => Some(B::Radians),
            "degrees" => Some(B::Degrees),
            _ => None,
        };
        if let Some(k) = unary_float {
            Self::arity(name, args, &[1], span)?;
            let (v, t) = self.unify_float(name, args)?;
            return Ok(b(k, v, t));
        }
        let float_n = match name {
            "atan2" => Some((B::Atan2, 2)),
            "pow" => Some((B::Pow, 2)),
            "step" => Some((B::Step, 2)),
            "lerp" | "mix" => Some((B::Mix, 3)),
            "smoothstep" | "smooth" => Some((B::Smoothstep, 3)),
            "fit" => Some((B::Fit, 5)),
            _ => None,
        };
        if let Some((k, n)) = float_n {
            Self::arity(name, args, &[n], span)?;
            let (v, t) = self.unify_float(name, args)?;
            return Ok(b(k, v, t));
        }
        let num_n = match name {
            "abs" => Some((B::Abs, 1)),
            "sign" => Some((B::Sign, 1)),
            "min" => Some((B::Min, 2)),
            "max" => Some((B::Max, 2)),
            "clamp" => Some((B::Clamp, 3)),
            _ => None,
        };
        if let Some((k, n)) = num_n {
            Self::arity(name, args, &[n], span)?;
            let v = self.args(args)?;
            if v.iter().all(|a| a.ty == Type::Int) {
                return Ok(b(k, v, Type::Int));
            }
            let (v, t) = self.unify_float(name, args)?;
            return Ok(b(k, v, t));
        }
        match name {
            "fit01" => {
                Self::arity(name, args, &[3], span)?;
                let (mut v, t) = self.unify_float(name, args)?;
                let lit = |x: f32| {
                    let f = Expr::new(ExprKind::Float(x), Type::Float, span);
                    if t == Type::Float {
                        f
                    } else {
                        Expr::new(ExprKind::Splat(Box::new(f)), t, span)
                    }
                };
                v.insert(1, lit(0.0));
                v.insert(2, lit(1.0));
                Ok(b(B::Fit, v, t))
            }
            "length" | "length2" | "normalize" => {
                Self::arity(name, args, &[1], span)?;
                let v = self.expr(&args[0])?;
                let Type::Vec(_) = v.ty else {
                    return Err(Error::new(
                        v.span,
                        format!("`{name}` needs a vector, found `{}`", v.ty),
                    ));
                };
                let (k, ty) = match name {
                    "length" => (B::Length, Type::Float),
                    "length2" => (B::Length2, Type::Float),
                    _ => (B::Normalize, v.ty),
                };
                Ok(b(k, vec![v], ty))
            }
            "distance" | "distance2" | "dot" => {
                Self::arity(name, args, &[2], span)?;
                let (v, t) = self.unify_float(name, args)?;
                if !matches!(t, Type::Vec(_)) {
                    return Err(Error::new(span, format!("`{name}` needs vectors")));
                }
                let k = match name {
                    "distance" => B::Distance,
                    "distance2" => B::Distance2,
                    _ => B::Dot,
                };
                Ok(b(k, v, Type::Float))
            }
            "cross" => {
                Self::arity(name, args, &[2], span)?;
                let v = self.typed_args(args, &[Type::Vec(3), Type::Vec(3)])?;
                Ok(b(B::Cross, v, Type::Vec(3)))
            }
            "transpose" | "determinant" | "invert" => {
                Self::arity(name, args, &[1], span)?;
                let v = self.expr(&args[0])?;
                let Type::Mat(_) = v.ty else {
                    return Err(Error::new(
                        v.span,
                        format!("`{name}` needs a matrix, found `{}`", v.ty),
                    ));
                };
                let (k, ty) = match name {
                    "transpose" => (B::Transpose, v.ty),
                    "determinant" => (B::Determinant, Type::Float),
                    _ => (B::Invert, v.ty),
                };
                Ok(b(k, vec![v], ty))
            }
            "quaternion" => {
                Self::arity(name, args, &[2], span)?;
                let v = self.typed_args(args, &[Type::Float, Type::Vec(3)])?;
                Ok(b(B::Quaternion, v, Type::Vec(4)))
            }
            "qmultiply" => {
                Self::arity(name, args, &[2], span)?;
                let v = self.typed_args(args, &[Type::Vec(4), Type::Vec(4)])?;
                Ok(b(B::QMultiply, v, Type::Vec(4)))
            }
            "qrotate" => {
                Self::arity(name, args, &[2], span)?;
                let v = self.typed_args(args, &[Type::Vec(4), Type::Vec(3)])?;
                Ok(b(B::QRotate, v, Type::Vec(3)))
            }
            "qinvert" => {
                Self::arity(name, args, &[1], span)?;
                let v = self.typed_args(args, &[Type::Vec(4)])?;
                Ok(b(B::QInvert, v, Type::Vec(4)))
            }
            "qconvert" => {
                Self::arity(name, args, &[1], span)?;
                let v = self.typed_args(args, &[Type::Vec(4)])?;
                Ok(b(B::QConvert, v, Type::Mat(3)))
            }
            "slerp" => {
                Self::arity(name, args, &[3], span)?;
                let v = self.typed_args(args, &[Type::Vec(4), Type::Vec(4), Type::Float])?;
                Ok(b(B::Slerp, v, Type::Vec(4)))
            }
            "perlin" | "simplex" | "worley" => {
                Self::arity(name, args, &[1], span)?;
                let v = self.expr(&args[0])?;
                if !matches!(v.ty, Type::Vec(2 | 3)) {
                    return Err(Error::new(
                        v.span,
                        format!("`{name}` takes a `vec2` or a `vec3`, found `{}`", v.ty),
                    )
                    .hint("for one dimension, use set(x, 0)"));
                }
                let k = match name {
                    "perlin" => B::Perlin,
                    "simplex" => B::Simplex,
                    _ => B::Worley,
                };
                Ok(b(k, vec![v], Type::Float))
            }
            "curlnoise" | "curl" => {
                Self::arity(name, args, &[1], span)?;
                let v = self.typed_args(args, &[Type::Vec(3)])?;
                Ok(b(B::CurlNoise, v, Type::Vec(3)))
            }
            "rand" => {
                Self::arity(name, args, &[1], span)?;
                let v = self.expr(&args[0])?;
                if !matches!(v.ty, Type::Int | Type::Float | Type::Vec(2 | 3)) {
                    return Err(Error::new(
                        v.span,
                        format!(
                            "`rand` takes an `int`, `float`, `vec2` or `vec3`, found `{}`",
                            v.ty
                        ),
                    ));
                }
                Ok(b(B::Rand, vec![v], Type::Float))
            }
            _ => {
                let best = KNOWN_FNS
                    .iter()
                    .copied()
                    .chain(self.sigs.iter().map(|s| s.name.as_str()))
                    .map(|k| (edit_distance(name, k), k))
                    .filter(|(d, _)| *d <= 2)
                    .min();
                let e = Error::new(name_span, format!("unknown function `{name}`"));
                Err(match best {
                    Some((_, k)) => e.hint(format!("did you mean `{k}`?")),
                    None => e,
                })
            }
        }
    }

    fn typed_args(&mut self, args: &[ast::Expr], types: &[Type]) -> CResult<Vec<Expr>> {
        let mut out = Vec::new();
        for (a, t) in args.iter().zip(types) {
            let v = self.expr(a)?;
            out.push(self.coerce(v, *t)?);
        }
        Ok(out)
    }

    /// Converts arguments to one `float` or vector type (scalars widen to the vector).
    fn unify_float(&mut self, name: &str, args: &[ast::Expr]) -> CResult<(Vec<Expr>, Type)> {
        let v = self.args(args)?;
        let mut t = Type::Float;
        for a in &v {
            match a.ty {
                Type::Int | Type::Float => {}
                Type::Vec(n) => match t {
                    Type::Vec(m) if m != n => {
                        return Err(Error::new(
                            a.span,
                            format!("`{name}` mixes `vec{m}` and `vec{n}`"),
                        ));
                    }
                    _ => t = Type::Vec(n),
                },
                other => {
                    return Err(Error::new(
                        a.span,
                        format!("`{name}` needs numbers or vectors, found `{other}`"),
                    ));
                }
            }
        }
        let v = v
            .into_iter()
            .map(|a| self.coerce(a, t))
            .collect::<CResult<Vec<_>>>()?;
        Ok((v, t))
    }
}

#[derive(Clone, Copy, Debug)]
enum AttrRef {
    Global(Global),
    Attr(AttrId),
}

/// The operator of a compound assignment (`+=` → `+`); `None` for `=`.
fn assign_binop(op: AssignOp) -> Option<BinOp> {
    Some(match op {
        AssignOp::Set => return None,
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Rem => BinOp::Rem,
    })
}

/// ASCII letters, digits and `_`, not starting with a digit (and not `__`, which WGSL
/// reserves).
fn is_identifier(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.starts_with("__")
}

/// The common type of two operands under the implicit conversions.
fn unify(a: Type, b: Type) -> Option<Type> {
    if a == b {
        return Some(a);
    }
    Some(match (a, b) {
        (Type::Int, Type::Float) | (Type::Float, Type::Int) => Type::Float,
        (Type::Bool, Type::Int) | (Type::Int, Type::Bool) => Type::Int,
        (Type::Int | Type::Float, Type::Vec(n)) | (Type::Vec(n), Type::Int | Type::Float) => {
            Type::Vec(n)
        }
        _ => return None,
    })
}

/// Component indices of a swizzle (`xyzw` or `rgba`, 1 to 4 letters).
fn swizzle(comp: &str, n: u8, span: Span) -> CResult<Vec<u8>> {
    let xyzw = comp.chars().all(|c| "xyzw".contains(c));
    let rgba = comp.chars().all(|c| "rgba".contains(c));
    if comp.is_empty() || comp.len() > 4 || !(xyzw || rgba) {
        return Err(Error::new(span, format!("`.{comp}` is not a component"))
            .hint("use up to four of x y z w (or r g b a)"));
    }
    let set = if xyzw { "xyzw" } else { "rgba" };
    let mut out = Vec::new();
    for c in comp.chars() {
        let i = set.find(c).unwrap_or(0) as u8;
        if i >= n {
            return Err(Error::new(
                span,
                format!("`.{c}` is out of range for a `vec{n}`"),
            ));
        }
        out.push(i);
    }
    Ok(out)
}

/// True if every path through `stmts` returns.
fn always_returns(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match &s.kind {
        StmtKind::Return(_) => true,
        StmtKind::Block(b) => always_returns(b),
        StmtKind::If(_, t, e) => always_returns(t) && always_returns(e),
        _ => false,
    })
}

/// Visits every expression of a statement (pre-order).
fn walk_stmt(s: &ast::Stmt, f: &mut dyn FnMut(&ast::Expr)) {
    match &s.kind {
        SK::Decl { vars, .. } => vars
            .iter()
            .filter_map(|d| d.init.as_ref())
            .for_each(|e| walk_expr(e, f)),
        SK::Assign { target, value, .. } => {
            walk_expr(target, f);
            walk_expr(value, f);
        }
        SK::IncDec { target, .. } => walk_expr(target, f),
        SK::Expr(e) => walk_expr(e, f),
        SK::If { cond, then, els } => {
            walk_expr(cond, f);
            walk_stmt(then, f);
            if let Some(e) = els {
                walk_stmt(e, f);
            }
        }
        SK::For {
            init,
            cond,
            step,
            body,
        } => {
            if let Some(i) = init {
                walk_stmt(i, f);
            }
            if let Some(c) = cond {
                walk_expr(c, f);
            }
            if let Some(s) = step {
                walk_stmt(s, f);
            }
            walk_stmt(body, f);
        }
        SK::Foreach { source, body, .. } => {
            walk_expr(source, f);
            walk_stmt(body, f);
        }
        SK::Return(Some(e)) => walk_expr(e, f),
        SK::Block(b) => b.iter().for_each(|s| walk_stmt(s, f)),
        SK::Return(None) | SK::Break | SK::Continue | SK::Empty => {}
    }
}

fn walk_expr(e: &ast::Expr, f: &mut dyn FnMut(&ast::Expr)) {
    f(e);
    match &e.kind {
        AK::Unary(_, a) | AK::Member(a, _, _) => walk_expr(a, f),
        AK::Binary(_, a, b) | AK::Index(a, b) => {
            walk_expr(a, f);
            walk_expr(b, f);
        }
        AK::Ternary(a, b, c) => {
            walk_expr(a, f);
            walk_expr(b, f);
            walk_expr(c, f);
        }
        AK::Call { args, .. } | AK::Braces(args) => args.iter().for_each(|a| walk_expr(a, f)),
        _ => {}
    }
}

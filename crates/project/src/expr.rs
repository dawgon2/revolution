//! Comp parameter expressions (fork C4, ADR 0009 §1): a small formula language, Fusion / After
//! Effects-like, for comp node parameters ("rotate with time", "follow that node's size",
//! "wiggle").
//!
//! Formulas come from project files and agents, so everything here treats the source as hostile:
//! the source is at most [`MAX_SOURCE`] bytes, a parse makes at most [`MAX_NODES`] AST nodes,
//! nests at most [`MAX_DEPTH`] levels (an explicit counter, not the stack) and builds trees at
//! most [`MAX_HEIGHT`] tall, so evaluation (one walk of the AST, the language has no loops) can't
//! overflow the stack; a step counter caps every evaluation as a belt-and-braces bound. Every
//! function guards its domain and a non-finite result is an error, never a NaN in a render.
//!
//! [`parse`] turns a source into an [`Expr`]; [`eval`] evaluates it in a [`Context`] (time, frame,
//! size, this parameter's keyframed `value`) with a [`Resolver`] that reads other parameters
//! (`self.<param>`, `node("<label or id>").<param>`). [`to_param`] / [`from_param`] convert
//! between expression [`Value`]s and [`ParamValue`]s (ADR 0009 §1.5).
//!
//! Grammar (lowest precedence first): `c ? a : b` (right-assoc), `||`, `&&`, `== !=`,
//! `< <= > >=`, `+ -`, `* / %`, unary `- ! +`, `^` (right-assoc, binds tighter than a unary minus
//! on its left: `-2^2` is −4), postfix `v[i]`, `.x .y .z .w` / `.r .g .b .a`, primaries (numbers,
//! `[a, b, …]` vectors of up to 4, names, calls, `(…)`).

use std::fmt;

use filmcraft_geom::Vec2;
use filmcraft_time::{FrameRate, Tick};

use crate::keyframe::ParamValue;

/// Longest expression source in bytes.
pub const MAX_SOURCE: usize = 4_096;
/// Most AST nodes one expression parses into.
pub const MAX_NODES: usize = 1_024;
/// Deepest parser nesting (parentheses, brackets, arguments, conditionals, unary chains).
pub const MAX_DEPTH: u32 = 64;
/// Tallest AST (a long `1 + 1 + …` chain is as tall as it is long; this bounds the evaluator's
/// recursion).
pub const MAX_HEIGHT: u32 = 256;
/// Most evaluation steps of one expression (each AST node is one step; no loops, so this is a
/// belt-and-braces cap).
pub const MAX_STEPS: u32 = 4 * MAX_NODES as u32;
/// Tallest a whole reference chain may be: the heights of the expressions being evaluated, one
/// inside another through references, added up (each AST level is one evaluator frame on the
/// stack). A resolver that evaluates references checks it (see `Comp::param_at`).
pub const MAX_CHAIN_HEIGHT: u32 = 768;
/// Deepest chain of parameter references (`node()` / `self.`) an evaluation follows.
pub const MAX_REF_DEPTH: u32 = 8;
/// Most arguments of a call (`min` / `max` take several).
pub const MAX_ARGS: usize = 8;
/// Most components of a vector.
pub const MAX_COMPONENTS: usize = 4;

// ---------------------------------------------------------------------------------------------
// errors

/// Where in the source something is (byte offsets, `start..end`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end: end.max(start) }
    }
    fn join(self, o: Span) -> Span {
        Span::new(self.start.min(o.start), self.end.max(o.end))
    }
}

/// A parse or evaluation error: a plain-language message and where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExprError {
    pub span: Span,
    pub message: String,
    /// 1-based character column of `span.start` (0: unknown; [`ExprError::at`] fills it in).
    pub column: usize,
}

impl ExprError {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self { span, message: message.into(), column: 0 }
    }

    /// An error not tied to a place in a source (a missing node, a cycle).
    pub fn general(message: impl Into<String>) -> Self {
        Self::new(Span::default(), message)
    }

    /// This error with its column computed from the source it came from.
    pub fn at(mut self, src: &str) -> Self {
        let start = self.span.start.min(src.len());
        // (a span always starts on a char boundary; fall back to counting the whole prefix)
        self.column = src.get(..start).map_or(start, |s| s.chars().count()).saturating_add(1);
        self
    }
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.column > 0 { write!(f, "{} at column {}", self.message, self.column) } else { f.write_str(&self.message) }
    }
}

impl std::error::Error for ExprError {}

type Result<T> = std::result::Result<T, ExprError>;

// ---------------------------------------------------------------------------------------------
// AST

/// A parsed expression (one AST node and its children).
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
    height: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Num(f64),
    Name(Name),
    /// `[a, b]` … `[a, b, c, d]`.
    Vector(Vec<Expr>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `cond ? a : b`.
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `v[i]`.
    Index(Box<Expr>, Box<Expr>),
    /// `.x` (0) … `.w` (3), `.r` … `.a`.
    Component(Box<Expr>, u8),
    Call(Func, Vec<Expr>),
    /// `self.<key>` (`node` None) or `node("<label or id>").<key>`.
    Param {
        node: Option<String>,
        key: String,
        /// Where the `node(…)` argument token is in the source (quotes included), so a tool can
        /// rewrite the reference (comp templates renumber ids). Default for `self.`.
        node_span: Span,
    },
}

/// The built-in names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Name {
    Time,
    Frame,
    Fps,
    Width,
    Height,
    Value,
    Pi,
    E,
}

impl Name {
    fn from_name(s: &str) -> Option<Name> {
        Some(match s {
            "time" => Name::Time,
            "frame" => Name::Frame,
            "fps" => Name::Fps,
            "width" => Name::Width,
            "height" => Name::Height,
            "value" => Name::Value,
            "pi" => Name::Pi,
            "e" => Name::E,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

impl BinOp {
    fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Pow => "^",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::And => "&&",
            BinOp::Or => "||",
        }
    }
}

/// The built-in functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Func {
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Sqrt,
    Abs,
    Sign,
    Min,
    Max,
    Clamp,
    Floor,
    Ceil,
    Round,
    Fract,
    Exp,
    Ln,
    Log10,
    Pow,
    Lerp,
    Smoothstep,
    Step,
    Noise,
    Random,
    Wiggle,
}

impl Func {
    pub const ALL: [Func; 27] = [
        Func::Sin,
        Func::Cos,
        Func::Tan,
        Func::Asin,
        Func::Acos,
        Func::Atan,
        Func::Atan2,
        Func::Sqrt,
        Func::Abs,
        Func::Sign,
        Func::Min,
        Func::Max,
        Func::Clamp,
        Func::Floor,
        Func::Ceil,
        Func::Round,
        Func::Fract,
        Func::Exp,
        Func::Ln,
        Func::Log10,
        Func::Pow,
        Func::Lerp,
        Func::Smoothstep,
        Func::Step,
        Func::Noise,
        Func::Random,
        Func::Wiggle,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Func::Sin => "sin",
            Func::Cos => "cos",
            Func::Tan => "tan",
            Func::Asin => "asin",
            Func::Acos => "acos",
            Func::Atan => "atan",
            Func::Atan2 => "atan2",
            Func::Sqrt => "sqrt",
            Func::Abs => "abs",
            Func::Sign => "sign",
            Func::Min => "min",
            Func::Max => "max",
            Func::Clamp => "clamp",
            Func::Floor => "floor",
            Func::Ceil => "ceil",
            Func::Round => "round",
            Func::Fract => "fract",
            Func::Exp => "exp",
            Func::Ln => "ln",
            Func::Log10 => "log10",
            Func::Pow => "pow",
            Func::Lerp => "lerp",
            Func::Smoothstep => "smoothstep",
            Func::Step => "step",
            Func::Noise => "noise",
            Func::Random => "random",
            Func::Wiggle => "wiggle",
        }
    }

    pub fn from_name(s: &str) -> Option<Func> {
        Self::ALL.into_iter().find(|f| f.name() == s)
    }

    /// (fewest, most) arguments.
    pub fn arity(self) -> (usize, usize) {
        match self {
            Func::Atan2 | Func::Pow | Func::Step => (2, 2),
            Func::Min | Func::Max => (2, MAX_ARGS),
            Func::Clamp | Func::Lerp | Func::Smoothstep => (3, 3),
            Func::Noise => (1, 2),
            Func::Wiggle => (2, 3),
            _ => (1, 1),
        }
    }
}

impl Expr {
    /// Whether `f` holds for this node or any node below it (pre-order, stops at the first).
    pub fn any(&self, f: &mut dyn FnMut(&Expr) -> bool) -> bool {
        if f(self) {
            return true;
        }
        match &self.kind {
            ExprKind::Num(_) | ExprKind::Name(_) | ExprKind::Param { .. } => false,
            ExprKind::Vector(v) | ExprKind::Call(_, v) => v.iter().any(|e| e.any(f)),
            ExprKind::Unary(_, a) | ExprKind::Component(a, _) => a.any(f),
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => a.any(f) || b.any(f),
            ExprKind::Cond(c, a, b) => c.any(f) || a.any(f) || b.any(f),
        }
    }

    /// Height of the tree (a leaf is 1).
    pub fn height(&self) -> u32 {
        self.height
    }
}

// ---------------------------------------------------------------------------------------------
// lexer

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Str(String),
    Op(&'static str),
    End,
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    span: Span,
}

const OPS2: [&str; 6] = ["<=", ">=", "==", "!=", "&&", "||"];
const OPS1: [&str; 17] = ["+", "-", "*", "/", "%", "^", "!", "(", ")", "[", "]", ",", ".", "?", ":", "<", ">"];

fn lex(src: &str) -> Result<Vec<Token>> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let Some(&b) = bytes.get(i) else { break };
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let next = bytes.get(i + 1).copied();
        if b.is_ascii_digit() || (b == b'.' && next.is_some_and(|n| n.is_ascii_digit())) {
            while bytes.get(i).is_some_and(|c| c.is_ascii_digit()) {
                i += 1;
            }
            if bytes.get(i) == Some(&b'.') {
                i += 1;
                while bytes.get(i).is_some_and(|c| c.is_ascii_digit()) {
                    i += 1;
                }
            }
            if matches!(bytes.get(i), Some(b'e' | b'E')) {
                let mut j = i + 1;
                if matches!(bytes.get(j), Some(b'+' | b'-')) {
                    j += 1;
                }
                if bytes.get(j).is_some_and(|c| c.is_ascii_digit()) {
                    i = j;
                    while bytes.get(i).is_some_and(|c| c.is_ascii_digit()) {
                        i += 1;
                    }
                }
            }
            let span = Span::new(start, i);
            let text = src.get(start..i).unwrap_or("");
            let v: f64 = text.parse().map_err(|_| ExprError::new(span, format!("`{text}` is not a number")))?;
            if !v.is_finite() {
                return Err(ExprError::new(span, format!("the number `{text}` is too large")));
            }
            out.push(Token { tok: Tok::Num(v), span });
            continue;
        }
        if b.is_ascii_alphabetic() || b == b'_' {
            while bytes.get(i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                i += 1;
            }
            out.push(Token { tok: Tok::Ident(src.get(start..i).unwrap_or("").to_string()), span: Span::new(start, i) });
            continue;
        }
        if b == b'"' || b == b'\'' {
            i += 1;
            while bytes.get(i).is_some_and(|c| *c != b) {
                i += 1;
            }
            if i >= bytes.len() {
                return Err(ExprError::new(Span::new(start, i), "this text has no closing quote"));
            }
            // (quotes are ASCII, so both ends are char boundaries)
            let text = src.get(start + 1..i).unwrap_or("").to_string();
            i += 1;
            out.push(Token { tok: Tok::Str(text), span: Span::new(start, i) });
            continue;
        }
        if let Some(op) = src.get(i..i + 2).and_then(|two| OPS2.into_iter().find(|o| *o == two)) {
            i += 2;
            out.push(Token { tok: Tok::Op(op), span: Span::new(start, i) });
            continue;
        }
        if let Some(op) = src.get(i..i + 1).and_then(|one| OPS1.into_iter().find(|o| *o == one)) {
            i += 1;
            out.push(Token { tok: Tok::Op(op), span: Span::new(start, i) });
            continue;
        }
        // an unexpected character (maybe not ASCII: report the whole char)
        let ch = src.get(start..).and_then(|s| s.chars().next()).unwrap_or('?');
        let end = start.saturating_add(ch.len_utf8());
        let shown = if ch == '=' { "`=` (use `==` to compare; expressions don't assign)".to_string() } else { format!("`{}`", ch.escape_debug()) };
        return Err(ExprError::new(Span::new(start, end), format!("unexpected character {shown}")));
    }
    out.push(Token { tok: Tok::End, span: Span::new(bytes.len(), bytes.len()) });
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// parser

/// Parses an expression (see the module docs for the grammar and the caps).
pub fn parse(src: &str) -> Result<Expr> {
    if src.len() > MAX_SOURCE {
        return Err(ExprError::general(format!("the expression is longer than {MAX_SOURCE} bytes")));
    }
    if src.trim().is_empty() {
        return Err(ExprError::general("the expression is empty"));
    }
    let toks = lex(src)?;
    let mut p = Parser { src, toks: &toks, pos: 0, depth: 0, nodes: 0 };
    let e = p.cond()?;
    match p.peek() {
        Tok::End => Ok(e),
        _ => Err(ExprError::new(p.span(), format!("unexpected {} after the expression", p.describe()))),
    }
}

/// Most parsed expressions [`parse_cached`] keeps (then it starts again).
pub const PARSE_CACHE_ENTRIES: usize = 512;

/// [`parse`], remembering successful parses by source (at most [`PARSE_CACHE_ENTRIES`], so a
/// render re-parses nothing frame to frame). Errors are not kept.
pub fn parse_cached(src: &str) -> Result<std::sync::Arc<Expr>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Expr>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(e) = cache.lock().unwrap_or_else(PoisonError::into_inner).get(src) {
        return Ok(Arc::clone(e));
    }
    let e = Arc::new(parse(src)?);
    let mut map = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if map.len() >= PARSE_CACHE_ENTRIES {
        map.clear();
    }
    map.insert(src.to_string(), Arc::clone(&e));
    Ok(e)
}

struct Parser<'a> {
    src: &'a str,
    toks: &'a [Token],
    pos: usize,
    depth: u32,
    nodes: usize,
}

impl Parser<'_> {
    fn peek(&self) -> &Tok {
        self.toks.get(self.pos).map_or(&Tok::End, |t| &t.tok)
    }
    fn span(&self) -> Span {
        self.toks.get(self.pos).map_or(Span::new(self.src.len(), self.src.len()), |t| t.span)
    }
    fn bump(&mut self) -> Token {
        let t = self.toks.get(self.pos).cloned().unwrap_or(Token { tok: Tok::End, span: Span::new(self.src.len(), self.src.len()) });
        if self.pos < self.toks.len() {
            self.pos += 1;
        }
        t
    }
    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Tok::Op(o) if *o == op)
    }
    fn describe(&self) -> String {
        match self.peek() {
            Tok::End => "end of the expression".into(),
            Tok::Num(_) | Tok::Ident(_) | Tok::Str(_) | Tok::Op(_) => {
                let s = self.span();
                format!("`{}`", self.src.get(s.start..s.end).unwrap_or("?"))
            }
        }
    }
    /// The source from `from` to the current token, shortened, for "after `…`" messages.
    fn snippet(&self, from: usize) -> String {
        let to = self.span().start;
        let s = self.src.get(from..to).unwrap_or("").trim();
        if s.chars().count() > 40 { format!("{}…", s.chars().take(40).collect::<String>()) } else { s.to_string() }
    }
    fn expect(&mut self, op: &str, from: usize) -> Result<Span> {
        if self.is_op(op) {
            return Ok(self.bump().span);
        }
        Err(ExprError::new(self.span(), format!("expected `{op}` after `{}`", self.snippet(from))))
    }
    fn enter(&mut self) -> Result<()> {
        self.depth = self.depth.saturating_add(1);
        // (the whole expression is level 0: its own `cond` doesn't count as nesting)
        if self.depth > MAX_DEPTH.saturating_add(1) {
            return Err(ExprError::new(self.span(), format!("the expression is nested more than {MAX_DEPTH} levels deep")));
        }
        Ok(())
    }
    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
    fn node(&mut self, kind: ExprKind, span: Span) -> Result<Expr> {
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > MAX_NODES {
            return Err(ExprError::new(span, format!("the expression has more than {MAX_NODES} parts")));
        }
        let child = |e: &Expr| e.height;
        let below = match &kind {
            ExprKind::Num(_) | ExprKind::Name(_) | ExprKind::Param { .. } => 0,
            ExprKind::Vector(v) | ExprKind::Call(_, v) => v.iter().map(child).max().unwrap_or(0),
            ExprKind::Unary(_, a) | ExprKind::Component(a, _) => a.height,
            ExprKind::Binary(_, a, b) | ExprKind::Index(a, b) => a.height.max(b.height),
            ExprKind::Cond(c, a, b) => c.height.max(a.height).max(b.height),
        };
        let height = below.saturating_add(1);
        if height > MAX_HEIGHT {
            return Err(ExprError::new(span, format!("the expression is too long a chain (more than {MAX_HEIGHT} steps deep)")));
        }
        Ok(Expr { kind, span, height })
    }

    fn cond(&mut self) -> Result<Expr> {
        self.enter()?;
        let r = self.cond_inner();
        self.leave();
        r
    }
    fn cond_inner(&mut self) -> Result<Expr> {
        let c = self.binary(0)?;
        if !self.is_op("?") {
            return Ok(c);
        }
        self.bump();
        let a = self.cond()?;
        self.expect(":", c.span.start)?;
        let b = self.cond()?;
        let span = c.span.join(b.span);
        self.node(ExprKind::Cond(Box::new(c), Box::new(a), Box::new(b)), span)
    }

    fn level_op(&self, level: u8) -> Option<BinOp> {
        let Tok::Op(o) = self.peek() else { return None };
        let op = match (level, *o) {
            (0, "||") => BinOp::Or,
            (1, "&&") => BinOp::And,
            (2, "==") => BinOp::Eq,
            (2, "!=") => BinOp::Ne,
            (3, "<") => BinOp::Lt,
            (3, "<=") => BinOp::Le,
            (3, ">") => BinOp::Gt,
            (3, ">=") => BinOp::Ge,
            (4, "+") => BinOp::Add,
            (4, "-") => BinOp::Sub,
            (5, "*") => BinOp::Mul,
            (5, "/") => BinOp::Div,
            (5, "%") => BinOp::Rem,
            _ => return None,
        };
        Some(op)
    }

    /// Left-associative binary levels 0 (`||`) … 5 (`* / %`); level 6 is unary.
    fn binary(&mut self, level: u8) -> Result<Expr> {
        if level >= 6 {
            return self.unary();
        }
        let mut lhs = self.binary(level + 1)?;
        while let Some(op) = self.level_op(level) {
            let op_span = self.bump().span;
            if matches!(self.peek(), Tok::End) {
                return Err(ExprError::new(op_span, format!("`{}` needs something on its right", op.symbol())));
            }
            let rhs = self.binary(level + 1)?;
            let span = lhs.span.join(rhs.span);
            lhs = self.node(ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), span)?;
        }
        Ok(lhs)
    }

    /// A unary operand nested one level deeper (prefix operators and `^` recurse through this).
    fn nested_unary(&mut self) -> Result<Expr> {
        self.enter()?;
        let r = self.unary();
        self.leave();
        r
    }
    fn unary(&mut self) -> Result<Expr> {
        let start = self.span();
        let op = if self.is_op("-") {
            Some(UnOp::Neg)
        } else if self.is_op("!") {
            Some(UnOp::Not)
        } else if self.is_op("+") {
            self.bump();
            return self.nested_unary();
        } else {
            None
        };
        if let Some(op) = op {
            self.bump();
            let a = self.nested_unary()?;
            let span = start.join(a.span);
            return self.node(ExprKind::Unary(op, Box::new(a)), span);
        }
        let base = self.postfix()?;
        if !self.is_op("^") {
            return Ok(base);
        }
        self.bump();
        let exp = self.nested_unary()?;
        let span = base.span.join(exp.span);
        self.node(ExprKind::Binary(BinOp::Pow, Box::new(base), Box::new(exp)), span)
    }

    fn postfix(&mut self) -> Result<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.is_op("[") {
                self.bump();
                let i = self.cond()?;
                let close = self.expect("]", e.span.start)?;
                let span = e.span.join(close);
                e = self.node(ExprKind::Index(Box::new(e), Box::new(i)), span)?;
            } else if self.is_op(".") {
                self.bump();
                let t = self.bump();
                let Tok::Ident(name) = &t.tok else {
                    return Err(ExprError::new(t.span, "expected `.x`, `.y`, `.z`, `.w` (or `.r`, `.g`, `.b`, `.a`) after `.`"));
                };
                let c = match name.as_str() {
                    "x" | "r" => 0,
                    "y" | "g" => 1,
                    "z" | "b" => 2,
                    "w" | "a" => 3,
                    _ => return Err(ExprError::new(t.span, format!("unknown part `.{name}` (use .x .y .z .w or .r .g .b .a)"))),
                };
                let span = e.span.join(t.span);
                e = self.node(ExprKind::Component(Box::new(e), c), span)?;
            } else {
                return Ok(e);
            }
        }
    }

    /// `.<param key>` after `self` / `node(…)`.
    fn param_key(&mut self, from: usize) -> Result<(String, Span)> {
        self.expect(".", from)?;
        let t = self.bump();
        match t.tok {
            Tok::Ident(k) => Ok((k, t.span)),
            _ => Err(ExprError::new(t.span, "expected a parameter name after `.`")),
        }
    }

    fn primary(&mut self) -> Result<Expr> {
        let t = self.bump();
        match t.tok {
            Tok::Num(v) => self.node(ExprKind::Num(v), t.span),
            Tok::Op("(") => {
                let e = self.cond()?;
                self.expect(")", t.span.start)?;
                Ok(e)
            }
            Tok::Op("[") => {
                let mut items = Vec::new();
                loop {
                    if items.len() >= MAX_COMPONENTS {
                        return Err(ExprError::new(self.span(), format!("a vector has at most {MAX_COMPONENTS} parts")));
                    }
                    items.push(self.cond()?);
                    if self.is_op(",") {
                        self.bump();
                        continue;
                    }
                    break;
                }
                let close = self.expect("]", t.span.start)?;
                if items.len() == 1 {
                    return Ok(items.pop().unwrap_or(Expr { kind: ExprKind::Num(0.0), span: t.span, height: 1 }));
                }
                self.node(ExprKind::Vector(items), t.span.join(close))
            }
            Tok::Ident(name) => self.ident(name, t.span),
            Tok::End => Err(ExprError::new(t.span, "the expression ends too early")),
            Tok::Str(_) => Err(ExprError::new(t.span, "text in quotes can only name a node: `node(\"Blur 1\").blurriness`")),
            Tok::Op(o) => Err(ExprError::new(t.span, format!("unexpected `{o}`: expected a number, a name or `(`"))),
        }
    }

    fn ident(&mut self, name: String, span: Span) -> Result<Expr> {
        match name.as_str() {
            "self" => {
                let (key, ks) = self.param_key(span.start)?;
                return self.node(ExprKind::Param { node: None, key, node_span: Span::default() }, span.join(ks));
            }
            "node" => {
                self.expect("(", span.start)?;
                let a = self.bump();
                let node_span = a.span;
                let r = match a.tok {
                    Tok::Str(s) => s,
                    Tok::Num(v) if v >= 0.0 && v.fract() == 0.0 && v < 9.0e15 => format!("{v:.0}"),
                    _ => return Err(ExprError::new(a.span, "node(…) takes a node's label in quotes or its number: `node(\"Blur 1\")`, `node(3)`")),
                };
                self.expect(")", span.start)?;
                let (key, ks) = self.param_key(span.start)?;
                return self.node(ExprKind::Param { node: Some(r), key, node_span }, span.join(ks));
            }
            _ => {}
        }
        if self.is_op("(") {
            let Some(f) = Func::from_name(&name) else {
                return Err(ExprError::new(span, format!("unknown function `{name}`")));
            };
            self.bump();
            let mut args = Vec::new();
            if !self.is_op(")") {
                loop {
                    if args.len() >= MAX_ARGS {
                        return Err(ExprError::new(self.span(), format!("`{name}` takes at most {MAX_ARGS} values")));
                    }
                    args.push(self.cond()?);
                    if self.is_op(",") {
                        self.bump();
                        continue;
                    }
                    break;
                }
            }
            let close = self.expect(")", span.start)?;
            let (lo, hi) = f.arity();
            if args.len() < lo || args.len() > hi {
                let want = if lo == hi {
                    format!("{lo}")
                } else if hi == MAX_ARGS {
                    format!("{lo} or more")
                } else {
                    format!("{lo} or {hi}")
                };
                let s = if lo == 1 && hi == 1 { "" } else { "s" };
                return Err(ExprError::new(span.join(close), format!("`{name}` takes {want} value{s}, not {}", args.len())));
            }
            return self.node(ExprKind::Call(f, args), span.join(close));
        }
        match Name::from_name(&name) {
            Some(n) => self.node(ExprKind::Name(n), span),
            None if Func::from_name(&name).is_some() => Err(ExprError::new(span, format!("`{name}` is a function: write `{name}(…)`"))),
            None => Err(ExprError::new(span, format!("unknown name `{name}`"))),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// values

/// An expression value: a number or a vector of 2 to 4 numbers (booleans are 0 / 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Scalar(f64),
    Vector { len: u8, v: [f64; MAX_COMPONENTS] },
}

impl Value {
    /// A value from components: one is a scalar, 2–4 a vector (more are dropped; none is 0).
    pub fn from_slice(c: &[f64]) -> Value {
        match c {
            [] => Value::Scalar(0.0),
            [x] => Value::Scalar(*x),
            _ => {
                let mut v = [0.0; MAX_COMPONENTS];
                let n = c.len().min(MAX_COMPONENTS);
                for (d, s) in v.iter_mut().zip(c.iter()) {
                    *d = *s;
                }
                Value::Vector { len: n as u8, v }
            }
        }
    }

    /// The components (one for a scalar).
    pub fn components(&self) -> &[f64] {
        match self {
            Value::Scalar(x) => std::slice::from_ref(x),
            Value::Vector { len, v } => v.get(..usize::from(*len)).unwrap_or(v),
        }
    }

    pub fn len(&self) -> usize {
        self.components().len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// The first component (a scalar's value).
    pub fn first(&self) -> f64 {
        self.components().first().copied().unwrap_or(0.0)
    }

    pub fn is_finite(&self) -> bool {
        self.components().iter().all(|c| c.is_finite())
    }
}

/// What an expression sees of the world (ADR 0009 §1.2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Context {
    /// Comp time in seconds (for formulas only; edit maths stays in ticks).
    pub time: f64,
    /// The frame containing the time, exactly from the tick and the comp rate.
    pub frame: i64,
    pub fps: f64,
    pub width: f64,
    pub height: f64,
    /// This parameter's keyframed (or static) value at this time.
    pub value: Value,
    /// How many references deep this evaluation is (passed on to the [`Resolver`]).
    pub depth: u32,
}

impl Context {
    pub fn new(t: Tick, rate: FrameRate, width: u32, height: u32, value: Value) -> Self {
        let rate = rate.sane();
        Context { time: t.seconds(), frame: rate.frame_at(t), fps: rate.as_f64(), width: f64::from(width), height: f64::from(height), value, depth: 0 }
    }
}

/// Reads other parameters for `self.<key>` (`node` None) and `node("<ref>").<key>`, at the same
/// time. `depth` is the reference depth of the expression asking (0 at the top); a resolver
/// refuses to go past [`MAX_REF_DEPTH`] and reports cycles as errors.
pub trait Resolver {
    fn param(&self, node: Option<&str>, key: &str, depth: u32) -> Result<Value>;
}

/// A resolver with no parameters (every reference is an error).
pub struct NoParams;

impl Resolver for NoParams {
    fn param(&self, _: Option<&str>, key: &str, _: u32) -> Result<Value> {
        Err(ExprError::general(format!("no parameter `{key}` to read here")))
    }
}

/// Evaluates `e`. The result is always finite (else an error).
pub fn eval(e: &Expr, cx: &Context, r: &dyn Resolver) -> Result<Value> {
    let mut ev = Eval { cx, r, steps: 0 };
    let v = ev.eval(e)?;
    if !v.is_finite() {
        return Err(ExprError::new(e.span, "the result is not a finite number"));
    }
    Ok(v)
}

/// Parses and evaluates `src` (errors carry their column).
pub fn eval_str(src: &str, cx: &Context, r: &dyn Resolver) -> Result<Value> {
    parse_cached(src).and_then(|e| eval(&e, cx, r)).map_err(|e| e.at(src))
}

struct Eval<'a> {
    cx: &'a Context,
    r: &'a dyn Resolver,
    steps: u32,
}

/// Applies `f` component-wise to `args`, scalars broadcasting over vectors (which must all be
/// the same length). Every component of the result must be finite.
fn broadcast(args: &[Value], span: Span, what: &str, f: impl Fn(&[f64]) -> std::result::Result<f64, &'static str>) -> Result<Value> {
    let mut n = 1usize;
    for a in args {
        let l = a.len();
        if l > 1 {
            if n > 1 && n != l {
                return Err(ExprError::new(span, format!("`{what}` mixes vectors of {n} and {l} parts")));
            }
            n = l;
        }
    }
    let mut out = [0.0; MAX_COMPONENTS];
    let mut buf = [0.0; MAX_ARGS];
    for (i, o) in out.iter_mut().enumerate().take(n) {
        let k = args.len().min(MAX_ARGS);
        for (b, a) in buf.iter_mut().zip(args.iter()) {
            let c = a.components();
            *b = if c.len() == 1 { c.first().copied().unwrap_or(0.0) } else { c.get(i).copied().unwrap_or(0.0) };
        }
        let x = f(buf.get(..k).unwrap_or(&[])).map_err(|m| ExprError::new(span, m))?;
        if !x.is_finite() {
            return Err(ExprError::new(span, format!("`{what}` gives a number that is too large or not a number")));
        }
        *o = x;
    }
    Ok(Value::from_slice(out.get(..n).unwrap_or(&out)))
}

fn arg(a: &[f64], i: usize) -> f64 {
    a.get(i).copied().unwrap_or(0.0)
}

fn truth(v: Value, span: Span, what: &str) -> Result<bool> {
    match v {
        Value::Scalar(x) => Ok(x != 0.0),
        Value::Vector { .. } => Err(ExprError::new(span, format!("{what} must be a single number, not a vector"))),
    }
}

fn bool_num(b: bool) -> f64 {
    if b { 1.0 } else { 0.0 }
}

// The evaluator recurses once per AST level, so `eval` keeps its own frame small: it evaluates
// the children and hands their values to non-inlined helpers that do the maths and build the
// messages (their locals are not on the stack while the children are evaluated).
impl Eval<'_> {
    fn eval(&mut self, e: &Expr) -> Result<Value> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > MAX_STEPS {
            return Err(too_many_steps(e.span));
        }
        let span = e.span;
        let v = match &e.kind {
            ExprKind::Num(x) => Value::Scalar(*x),
            ExprKind::Name(n) => name_value(self.cx, *n),
            ExprKind::Vector(items) => {
                let mut c = [0.0; MAX_COMPONENTS];
                for (d, it) in c.iter_mut().zip(items.iter()) {
                    *d = scalar_part(self.eval(it)?, it.span)?;
                }
                Value::from_slice(c.get(..items.len().min(MAX_COMPONENTS)).unwrap_or(&c))
            }
            ExprKind::Unary(op, a) => {
                let a = self.eval(a)?;
                unary(*op, a, span)?
            }
            ExprKind::Binary(BinOp::And, a, b) => {
                let l = truth(self.eval(a)?, a.span, "each side of `&&`")?;
                Value::Scalar(bool_num(l && truth(self.eval(b)?, b.span, "each side of `&&`")?))
            }
            ExprKind::Binary(BinOp::Or, a, b) => {
                let l = truth(self.eval(a)?, a.span, "each side of `||`")?;
                Value::Scalar(bool_num(l || truth(self.eval(b)?, b.span, "each side of `||`")?))
            }
            ExprKind::Binary(op, a, b) => {
                let a = self.eval(a)?;
                let b = self.eval(b)?;
                binary(*op, a, b, span)?
            }
            ExprKind::Cond(c, a, b) => {
                if truth(self.eval(c)?, c.span, "the condition before `?`")? {
                    self.eval(a)?
                } else {
                    self.eval(b)?
                }
            }
            ExprKind::Index(v, i) => {
                let v = self.eval(v)?;
                let i = self.eval(i)?;
                index(v, i, span)?
            }
            ExprKind::Component(v, k) => {
                let v = self.eval(v)?;
                component(v, *k, span)?
            }
            ExprKind::Param { node, key, .. } => self.r.param(node.as_deref(), key, self.cx.depth).map_err(|err| ExprError::new(span, err.message))?,
            ExprKind::Call(f, args) => {
                let mut vals = Vec::with_capacity(args.len().min(MAX_ARGS));
                for a in args.iter().take(MAX_ARGS) {
                    vals.push(self.eval(a)?);
                }
                apply(self.cx, *f, &vals, span)?
            }
        };
        if !v.is_finite() {
            return Err(not_finite(span));
        }
        Ok(v)
    }
}

#[inline(never)]
fn too_many_steps(span: Span) -> ExprError {
    ExprError::new(span, "the expression takes too many steps")
}

#[inline(never)]
fn not_finite(span: Span) -> ExprError {
    ExprError::new(span, "this gives a number that is too large or not a number")
}

#[inline(never)]
fn name_value(cx: &Context, n: Name) -> Value {
    match n {
        Name::Time => Value::Scalar(cx.time),
        Name::Frame => Value::Scalar(cx.frame as f64),
        Name::Fps => Value::Scalar(cx.fps),
        Name::Width => Value::Scalar(cx.width),
        Name::Height => Value::Scalar(cx.height),
        Name::Value => cx.value,
        Name::Pi => Value::Scalar(std::f64::consts::PI),
        Name::E => Value::Scalar(std::f64::consts::E),
    }
}

#[inline(never)]
fn scalar_part(v: Value, span: Span) -> Result<f64> {
    match v {
        Value::Scalar(x) => Ok(x),
        Value::Vector { .. } => Err(ExprError::new(span, "a vector's parts must be single numbers")),
    }
}

#[inline(never)]
fn unary(op: UnOp, a: Value, span: Span) -> Result<Value> {
    match op {
        UnOp::Neg => broadcast(&[a], span, "-", |x| Ok(-arg(x, 0))),
        UnOp::Not => Ok(Value::Scalar(bool_num(!truth(a, span, "the value after `!`")?))),
    }
}

#[inline(never)]
fn binary(op: BinOp, a: Value, b: Value, span: Span) -> Result<Value> {
    broadcast(&[a, b], span, op.symbol(), move |x| {
        let (l, r) = (arg(x, 0), arg(x, 1));
        Ok(match op {
            BinOp::Add => l + r,
            BinOp::Sub => l - r,
            BinOp::Mul => l * r,
            BinOp::Div if r == 0.0 => return Err("division by zero"),
            BinOp::Div => l / r,
            BinOp::Rem if r == 0.0 => return Err("remainder of a division by zero"),
            BinOp::Rem => l % r,
            BinOp::Pow => pow(l, r)?,
            BinOp::Lt => bool_num(l < r),
            BinOp::Le => bool_num(l <= r),
            BinOp::Gt => bool_num(l > r),
            BinOp::Ge => bool_num(l >= r),
            BinOp::Eq => bool_num(l == r),
            BinOp::Ne => bool_num(l != r),
            BinOp::And | BinOp::Or => 0.0,
        })
    })
}

#[inline(never)]
fn index(v: Value, i: Value, span: Span) -> Result<Value> {
    let Value::Scalar(i) = i else {
        return Err(ExprError::new(span, "an index must be a single number"));
    };
    let idx = i.floor();
    let c = v.components();
    if !(0.0..c.len() as f64).contains(&idx) {
        return Err(ExprError::new(span, format!("index {idx} is out of range (the value has {} part{})", c.len(), if c.len() == 1 { "" } else { "s" })));
    }
    Ok(Value::Scalar(c.get(idx as usize).copied().unwrap_or(0.0)))
}

#[inline(never)]
fn component(v: Value, k: u8, span: Span) -> Result<Value> {
    match v.components().get(usize::from(k)) {
        Some(x) => Ok(Value::Scalar(*x)),
        None => {
            let n = v.len();
            Err(ExprError::new(span, format!("the value has {n} part{}, so it has no part {}", if n == 1 { "" } else { "s" }, k + 1)))
        }
    }
}

/// A function applied to its evaluated arguments.
#[inline(never)]
fn apply(cx: &Context, f: Func, vals: &[Value], span: Span) -> Result<Value> {
    let name = f.name();
    let one = |g: fn(f64) -> f64| broadcast(vals, span, name, move |x| Ok(g(arg(x, 0))));
    match f {
        Func::Sin => one(f64::sin),
        Func::Cos => one(f64::cos),
        Func::Tan => one(f64::tan),
        Func::Atan => one(f64::atan),
        Func::Abs => one(f64::abs),
        Func::Floor => one(f64::floor),
        Func::Ceil => one(f64::ceil),
        Func::Round => one(f64::round),
        Func::Fract => one(|x| x - x.floor()),
        Func::Exp => one(f64::exp),
        Func::Sign => one(|x| {
            if x > 0.0 {
                1.0
            } else if x < 0.0 {
                -1.0
            } else {
                0.0
            }
        }),
        Func::Asin | Func::Acos => broadcast(vals, span, name, move |x| {
            let v = arg(x, 0);
            if !(-1.0..=1.0).contains(&v) {
                return Err(if f == Func::Asin { "asin needs a value from -1 to 1" } else { "acos needs a value from -1 to 1" });
            }
            Ok(if f == Func::Asin { v.asin() } else { v.acos() })
        }),
        Func::Sqrt => broadcast(vals, span, name, |x| {
            let v = arg(x, 0);
            if v < 0.0 { Err("square root of a negative number") } else { Ok(v.sqrt()) }
        }),
        Func::Ln | Func::Log10 => broadcast(vals, span, name, move |x| {
            let v = arg(x, 0);
            if v <= 0.0 {
                return Err("logarithm of zero or a negative number");
            }
            Ok(if f == Func::Ln { v.ln() } else { v.log10() })
        }),
        Func::Atan2 => broadcast(vals, span, name, |x| Ok(arg(x, 0).atan2(arg(x, 1)))),
        Func::Pow => broadcast(vals, span, name, |x| pow(arg(x, 0), arg(x, 1))),
        Func::Min => broadcast(vals, span, name, |x| Ok(x.iter().copied().fold(f64::INFINITY, f64::min))),
        Func::Max => broadcast(vals, span, name, |x| Ok(x.iter().copied().fold(f64::NEG_INFINITY, f64::max))),
        Func::Clamp => broadcast(vals, span, name, |x| {
            let (v, a, b) = (arg(x, 0), arg(x, 1), arg(x, 2));
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            Ok(v.max(lo).min(hi))
        }),
        Func::Lerp => broadcast(vals, span, name, |x| {
            let (a, b, t) = (arg(x, 0), arg(x, 1), arg(x, 2));
            Ok(a + (b - a) * t)
        }),
        Func::Smoothstep => broadcast(vals, span, name, |x| {
            let (e0, e1, v) = (arg(x, 0), arg(x, 1), arg(x, 2));
            if e0 == e1 {
                return Ok(bool_num(v >= e0));
            }
            let t = ((v - e0) / (e1 - e0)).clamp(0.0, 1.0);
            Ok(t * t * (3.0 - 2.0 * t))
        }),
        Func::Step => broadcast(vals, span, name, |x| Ok(bool_num(arg(x, 1) >= arg(x, 0)))),
        Func::Noise => broadcast(vals, span, name, |x| Ok(noise(arg(x, 0), seed_bits(arg(x, 1))))),
        Func::Random => broadcast(vals, span, name, |x| Ok(random(arg(x, 0)))),
        Func::Wiggle => {
            let (freq, amp) = (vals.first().copied().unwrap_or(Value::Scalar(0.0)), vals.get(1).copied().unwrap_or(Value::Scalar(0.0)));
            let seed = vals.get(2).copied().unwrap_or(Value::Scalar(0.0));
            let Value::Scalar(freq) = freq else {
                return Err(ExprError::new(span, "wiggle's frequency must be a single number"));
            };
            let Value::Scalar(seed) = seed else {
                return Err(ExprError::new(span, "wiggle's seed must be a single number"));
            };
            let x = cx.time * freq;
            let base = seed_bits(seed);
            let value = cx.value;
            // component c wiggles on its own seed (component 0 on the seed itself)
            let comps = broadcast(&[value, amp], span, name, |_| Ok(0.0))?.len();
            let mut out = [0.0; MAX_COMPONENTS];
            for (c, o) in out.iter_mut().enumerate().take(comps) {
                let s = if c == 0 { base } else { hash64(base ^ hash64(c as u64)) };
                let v = value.components();
                let a = amp.components();
                let pick = |s: &[f64]| if s.len() == 1 { s.first().copied().unwrap_or(0.0) } else { s.get(c).copied().unwrap_or(0.0) };
                *o = pick(v) + pick(a) * noise(x, s);
            }
            let r = Value::from_slice(out.get(..comps).unwrap_or(&out));
            if !r.is_finite() {
                return Err(ExprError::new(span, "`wiggle` gives a number that is too large or not a number"));
            }
            Ok(r)
        }
    }
}

/// `a ^ b`, refusing results that are not real numbers.
fn pow(a: f64, b: f64) -> std::result::Result<f64, &'static str> {
    if a < 0.0 && b.fract() != 0.0 {
        return Err("a negative number to a fractional power is not a real number");
    }
    if a == 0.0 && b < 0.0 {
        return Err("zero to a negative power (a division by zero)");
    }
    let r = a.powf(b);
    if r.is_finite() { Ok(r) } else { Err("the power is too large") }
}

// ---------------------------------------------------------------------------------------------
// deterministic noise (our own: a 64-bit mix and 1-D gradient noise)

/// A 64-bit mix (SplitMix64's finaliser).
fn hash64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// A seed's bits (−0 and 0 are the same seed).
fn seed_bits(seed: f64) -> u64 {
    if seed == 0.0 { 0 } else { seed.to_bits() }
}

/// 0..1 from 53 bits of `h`.
fn unit(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// The gradient (−1..1) at lattice point `i`.
fn gradient(i: u64, seed: u64) -> f64 {
    unit(hash64(i ^ hash64(seed))) * 2.0 - 1.0
}

/// Deterministic 1-D gradient noise in −1..1 (0 at every integer).
pub fn noise(x: f64, seed: u64) -> f64 {
    if !x.is_finite() {
        return 0.0;
    }
    let xf = x.floor();
    let f = x - xf;
    // the lattice index wrapped into 48 bits (beyond 2^53 every x is an integer: noise 0)
    let i = xf.rem_euclid(281_474_976_710_656.0) as u64;
    let (g0, g1) = (gradient(i, seed), gradient(i.wrapping_add(1), seed));
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let (n0, n1) = (g0 * f, g1 * (f - 1.0));
    let n = n0 + (n1 - n0) * u;
    (n * 2.0).clamp(-1.0, 1.0)
}

/// Deterministic 0..1 from a seed (no state).
pub fn random(seed: f64) -> f64 {
    unit(hash64(seed_bits(seed) ^ 0xA076_1D64_78BD_642F))
}

// ---------------------------------------------------------------------------------------------
// coercion (ADR 0009 §1.5)

/// Whether a parameter of this value's type can take an expression (Text / Curve / Path can't).
pub fn kind_allowed(v: &ParamValue) -> bool {
    matches!(v, ParamValue::Float(_) | ParamValue::Vec2(_) | ParamValue::Color(_) | ParamValue::Bool(_) | ParamValue::Choice(_))
}

/// A parameter value as an expression value (None for Text / Curve / Path). A point's "auto"
/// (NaN) coordinates stay NaN: callers substitute what auto means for them.
pub fn from_param(v: &ParamValue) -> Option<Value> {
    Some(match v {
        ParamValue::Float(x) => Value::Scalar(*x),
        ParamValue::Choice(c) => Value::Scalar(f64::from(*c)),
        ParamValue::Bool(b) => Value::Scalar(bool_num(*b)),
        ParamValue::Vec2(p) => Value::from_slice(&[p.x, p.y]),
        ParamValue::Color(c) => Value::from_slice(&c.map(f64::from)),
        ParamValue::Text(_) | ParamValue::Curve(_) | ParamValue::Path(_) => return None,
    })
}

/// An expression result as a value of `like`'s type: Float ← a scalar (or `v[0]`), Vec2 ← a
/// vector (a scalar broadcasts), Color ← a 4-vector (a 3-vector or a scalar keeps `like`'s
/// alpha), Bool ← ≠ 0, Choice ← rounded and clamped to `choices` entries.
pub fn to_param(v: Value, like: &ParamValue, choices: Option<usize>) -> Result<ParamValue> {
    if !v.is_finite() {
        return Err(ExprError::general("the result is not a finite number"));
    }
    let c = v.components();
    let at = |i: usize| c.get(i).copied().unwrap_or_else(|| v.first());
    Ok(match like {
        ParamValue::Float(_) => ParamValue::Float(v.first()),
        ParamValue::Bool(_) => ParamValue::Bool(v.first() != 0.0),
        ParamValue::Choice(_) => {
            let hi = choices.map_or(f64::from(u32::MAX), |n| n.saturating_sub(1).min(u32::MAX as usize) as f64);
            ParamValue::Choice(v.first().round().max(0.0).min(hi) as u32)
        }
        ParamValue::Vec2(_) => {
            if c.len() > 2 {
                return Err(ExprError::general(format!("a point needs 2 parts ([x, y]), not {}", c.len())));
            }
            ParamValue::Vec2(Vec2::new(at(0), at(1)))
        }
        ParamValue::Color(old) => {
            if c.len() == 2 {
                return Err(ExprError::general("a colour needs 3 or 4 parts ([r, g, b] or [r, g, b, a]), not 2"));
            }
            let alpha = if c.len() == 4 { at(3) } else { f64::from(old[3]) };
            let rgba = [at(0), at(1), at(2), alpha].map(|x| x as f32);
            if !rgba.iter().all(|x| x.is_finite()) {
                return Err(ExprError::general("the colour is too large"));
            }
            ParamValue::Color(rgba)
        }
        ParamValue::Text(_) | ParamValue::Curve(_) | ParamValue::Path(_) => {
            return Err(ExprError::general("text, curve and path parameters can't take an expression"));
        }
    })
}

#[cfg(test)]
#[path = "expr_tests.rs"]
mod tests;

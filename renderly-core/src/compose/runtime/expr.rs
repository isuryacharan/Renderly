//! Shaping-expression language of effect pack format 2 (`expr.v1`).
//!
//! A pure, deterministic expression over numbers and booleans. Parsed and type-checked once
//! when an item loads, then evaluated per frame (never per pixel). Grammar, lowest precedence
//! first:
//!
//! ```text
//! expr    := or ( '?' expr ':' expr )?           (right-associative)
//! or      := and ( '||' and )*
//! and     := cmp ( '&&' cmp )*
//! cmp     := add ( ('==' | '!=' | '<' | '<=' | '>' | '>=') add )?   (non-associative)
//! add     := mul ( ('+' | '-') mul )*
//! mul     := unary ( ('*' | '/' | '%') unary )*
//! unary   := ('-' | '+' | '!') unary | primary
//! primary := number | 'true' | 'false' | name | name '(' args ')' | '(' expr ')'
//! ```
//!
//! Types are `num` and `bool`. A bool (a `true`/`false` literal, a comparison, a bool param)
//! coerces to 0/1 wherever a number is used. A number is never a boolean: `&&`, `||`, `!` and
//! the condition of `?:` need a bool. Division or remainder by zero, `smoothstep` with equal
//! edges, `remap` with equal input bounds, and any non-finite result are evaluation errors.

use thiserror::Error;

/// Maximum number of AST nodes in one expression (the design's limit).
pub const MAX_NODES: usize = 256;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ExprError {
    #[error("expression syntax error at byte {pos}: {message}")]
    Syntax { pos: usize, message: String },
    #[error("expression type error: {0}")]
    Type(String),
    #[error("unknown name '{0}' in expression")]
    UnknownName(String),
    #[error("unknown function '{0}' in expression")]
    UnknownFunction(String),
    #[error("function '{name}' takes {expected} argument(s), got {got}")]
    Arity {
        name: String,
        expected: usize,
        got: usize,
    },
    #[error("expression has {0} nodes (limit {MAX_NODES})")]
    TooLarge(usize),
    #[error("division by zero in expression")]
    DivideByZero,
    #[error("expression produced a non-finite value")]
    NonFinite,
}

/// Static type of a variable or expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Num,
    Bool,
}

/// The variables an expression may read, in slot order. [`Expr::eval`] takes a slice of
/// values indexed by these slots.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    names: Vec<(String, Ty)>,
}

impl Scope {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a variable and returns its slot. The most recent declaration of a name wins.
    pub fn push(&mut self, name: &str, ty: Ty) -> usize {
        self.names.push((name.to_string(), ty));
        self.names.len() - 1
    }

    fn lookup(&self, name: &str) -> Option<(usize, Ty)> {
        self.names
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (n, _))| n == name)
            .map(|(i, (_, ty))| (i, *ty))
    }
}

/// A parsed, type-checked expression ready to evaluate.
#[derive(Debug, Clone)]
pub struct Expr {
    node: Node,
    result: Ty,
    nodes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Func {
    Abs,
    Min,
    Max,
    Clamp,
    Mix,
    Step,
    Smoothstep,
    Pow,
    Sqrt,
    Exp,
    Log,
    Sin,
    Cos,
    Tan,
    Atan2,
    Floor,
    Ceil,
    Fract,
    Round,
    Logistic,
    Remap,
    Hash,
}

impl Func {
    fn parse(name: &str) -> Option<(Func, usize)> {
        Some(match name {
            "abs" => (Func::Abs, 1),
            "min" => (Func::Min, 2),
            "max" => (Func::Max, 2),
            "clamp" => (Func::Clamp, 3),
            "mix" => (Func::Mix, 3),
            "step" => (Func::Step, 2),
            "smoothstep" => (Func::Smoothstep, 3),
            "pow" => (Func::Pow, 2),
            "sqrt" => (Func::Sqrt, 1),
            "exp" => (Func::Exp, 1),
            "log" => (Func::Log, 1),
            "sin" => (Func::Sin, 1),
            "cos" => (Func::Cos, 1),
            "tan" => (Func::Tan, 1),
            "atan2" => (Func::Atan2, 2),
            "floor" => (Func::Floor, 1),
            "ceil" => (Func::Ceil, 1),
            "fract" => (Func::Fract, 1),
            "round" => (Func::Round, 1),
            "logistic" => (Func::Logistic, 3),
            "remap" => (Func::Remap, 5),
            "hash" => (Func::Hash, 1),
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone)]
enum Node {
    Num(f64),
    Bool(bool),
    Var(usize),
    Unary(UnOp, Box<Node>),
    Binary(BinOp, Box<Node>, Box<Node>),
    /// Short-circuit `&&` (`true`) or `||` (`false`).
    Logic(bool, Box<Node>, Box<Node>),
    Cond(Box<Node>, Box<Node>, Box<Node>),
    Call(Func, Vec<Node>),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    Question,
    Colon,
    End,
}

const OPS: &[&str] = &[
    "==", "!=", "<=", ">=", "&&", "||", "+", "-", "*", "/", "%", "<", ">", "!",
];

fn lex(src: &str) -> Result<Vec<(usize, Tok)>, ExprError> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if c.is_ascii_digit() || (c == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'.' {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
                let mut j = i + 1;
                if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
                    j += 1;
                }
                if j < bytes.len() && bytes[j].is_ascii_digit() {
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                }
            }
            let text = &src[start..i];
            let value: f64 = text.parse().map_err(|_| ExprError::Syntax {
                pos: start,
                message: format!("bad number '{text}'"),
            })?;
            out.push((start, Tok::Num(value)));
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push((start, Tok::Ident(src[start..i].to_string())));
            continue;
        }
        let rest = &src[i..];
        if let Some(op) = OPS.iter().find(|op| rest.starts_with(**op)) {
            i += op.len();
            out.push((start, Tok::Op(op)));
            continue;
        }
        let tok = match c {
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b',' => Tok::Comma,
            b'?' => Tok::Question,
            b':' => Tok::Colon,
            _ => {
                return Err(ExprError::Syntax {
                    pos: start,
                    message: format!("unexpected character '{}'", c as char),
                })
            }
        };
        i += 1;
        out.push((start, tok));
    }
    out.push((src.len(), Tok::End));
    Ok(out)
}

#[derive(Debug, Clone)]
struct Typed {
    node: Node,
    ty: Ty,
}

struct Parser<'a> {
    toks: Vec<(usize, Tok)>,
    at: usize,
    scope: &'a Scope,
    /// Names that exist for the item but are not usable in this expression (for example `x`
    /// outside a `uniform` expression). Using one gives a message saying so.
    forbidden: &'a [&'a str],
}

impl Parser<'_> {
    fn peek(&self) -> &Tok {
        &self.toks[self.at].1
    }

    fn pos(&self) -> usize {
        self.toks[self.at].0
    }

    fn bump(&mut self) {
        if self.at + 1 < self.toks.len() {
            self.at += 1;
        }
    }

    fn syntax<T>(&self, message: impl Into<String>) -> Result<T, ExprError> {
        Err(ExprError::Syntax {
            pos: self.pos(),
            message: message.into(),
        })
    }

    fn expect(&mut self, tok: &Tok, what: &str) -> Result<(), ExprError> {
        if self.peek() == tok {
            self.bump();
            Ok(())
        } else {
            self.syntax(format!("expected {what}"))
        }
    }

    fn expr(&mut self) -> Result<Typed, ExprError> {
        let cond = self.or()?;
        if *self.peek() != Tok::Question {
            return Ok(cond);
        }
        self.bump();
        let then = self.expr()?;
        self.expect(&Tok::Colon, "':' in conditional")?;
        let els = self.expr()?;
        let cond = as_bool(cond, "condition of '?:'")?;
        // Both branches bool gives a bool; otherwise the result is a number (bools as 0/1).
        let ty = if then.ty == Ty::Bool && els.ty == Ty::Bool {
            Ty::Bool
        } else {
            Ty::Num
        };
        Ok(Typed {
            node: Node::Cond(Box::new(cond), Box::new(then.node), Box::new(els.node)),
            ty,
        })
    }

    fn or(&mut self) -> Result<Typed, ExprError> {
        let mut lhs = self.and()?;
        while *self.peek() == Tok::Op("||") {
            self.bump();
            let rhs = self.and()?;
            lhs = logic(false, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn and(&mut self) -> Result<Typed, ExprError> {
        let mut lhs = self.cmp()?;
        while *self.peek() == Tok::Op("&&") {
            self.bump();
            let rhs = self.cmp()?;
            lhs = logic(true, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn cmp(&mut self) -> Result<Typed, ExprError> {
        let lhs = self.add()?;
        let op = match self.peek() {
            Tok::Op("==") => BinOp::Eq,
            Tok::Op("!=") => BinOp::Ne,
            Tok::Op("<") => BinOp::Lt,
            Tok::Op("<=") => BinOp::Le,
            Tok::Op(">") => BinOp::Gt,
            Tok::Op(">=") => BinOp::Ge,
            _ => return Ok(lhs),
        };
        self.bump();
        let rhs = self.add()?;
        if matches!(self.peek(), Tok::Op(o) if is_cmp(o)) {
            return self.syntax("comparisons do not chain; combine them with '&&'");
        }
        Ok(Typed {
            node: Node::Binary(op, Box::new(lhs.node), Box::new(rhs.node)),
            ty: Ty::Bool,
        })
    }

    fn add(&mut self) -> Result<Typed, ExprError> {
        let mut lhs = self.mul()?;
        loop {
            let op = match self.peek() {
                Tok::Op("+") => BinOp::Add,
                Tok::Op("-") => BinOp::Sub,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.mul()?;
            lhs = arith(op, lhs, rhs);
        }
    }

    fn mul(&mut self) -> Result<Typed, ExprError> {
        let mut lhs = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Op("*") => BinOp::Mul,
                Tok::Op("/") => BinOp::Div,
                Tok::Op("%") => BinOp::Rem,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.unary()?;
            lhs = arith(op, lhs, rhs);
        }
    }

    fn unary(&mut self) -> Result<Typed, ExprError> {
        match self.peek() {
            Tok::Op("-") => {
                self.bump();
                let inner = self.unary()?;
                Ok(Typed {
                    node: Node::Unary(UnOp::Neg, Box::new(inner.node)),
                    ty: Ty::Num,
                })
            }
            Tok::Op("+") => {
                self.bump();
                let inner = self.unary()?;
                Ok(Typed {
                    node: inner.node,
                    ty: Ty::Num,
                })
            }
            Tok::Op("!") => {
                self.bump();
                let inner = self.unary()?;
                let inner = as_bool(inner, "operand of '!'")?;
                Ok(Typed {
                    node: Node::Unary(UnOp::Not, Box::new(inner)),
                    ty: Ty::Bool,
                })
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Result<Typed, ExprError> {
        match self.peek().clone() {
            Tok::Num(v) => {
                self.bump();
                Ok(Typed {
                    node: Node::Num(v),
                    ty: Ty::Num,
                })
            }
            Tok::LParen => {
                self.bump();
                let inner = self.expr()?;
                self.expect(&Tok::RParen, "')'")?;
                Ok(inner)
            }
            Tok::Ident(name) => {
                self.bump();
                if name == "true" || name == "false" {
                    return Ok(Typed {
                        node: Node::Bool(name == "true"),
                        ty: Ty::Bool,
                    });
                }
                if *self.peek() == Tok::LParen {
                    return self.call(&name);
                }
                if self.forbidden.contains(&name.as_str()) {
                    return Err(ExprError::Type(format!(
                        "'{name}' is not available in this expression"
                    )));
                }
                match self.scope.lookup(&name) {
                    Some((slot, ty)) => Ok(Typed {
                        node: Node::Var(slot),
                        ty,
                    }),
                    None => Err(ExprError::UnknownName(name)),
                }
            }
            Tok::End => self.syntax("unexpected end of expression"),
            _ => self.syntax("expected a number, name or '('"),
        }
    }

    fn call(&mut self, name: &str) -> Result<Typed, ExprError> {
        let Some((func, arity)) = Func::parse(name) else {
            return Err(ExprError::UnknownFunction(name.to_string()));
        };
        self.expect(&Tok::LParen, "'('")?;
        let mut args = Vec::new();
        if *self.peek() != Tok::RParen {
            loop {
                args.push(self.expr()?.node);
                if *self.peek() != Tok::Comma {
                    break;
                }
                self.bump();
            }
        }
        self.expect(&Tok::RParen, "')' after arguments")?;
        if args.len() != arity {
            return Err(ExprError::Arity {
                name: name.to_string(),
                expected: arity,
                got: args.len(),
            });
        }
        Ok(Typed {
            node: Node::Call(func, args),
            ty: Ty::Num,
        })
    }
}

fn is_cmp(op: &str) -> bool {
    matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=")
}

/// A bool used as a condition must be a bool: a number is not implicitly a boolean.
fn as_bool(t: Typed, what: &str) -> Result<Node, ExprError> {
    match t.ty {
        Ty::Bool => Ok(t.node),
        Ty::Num => Err(ExprError::Type(format!(
            "{what} must be a bool, found a number"
        ))),
    }
}

fn arith(op: BinOp, lhs: Typed, rhs: Typed) -> Typed {
    Typed {
        node: Node::Binary(op, Box::new(lhs.node), Box::new(rhs.node)),
        ty: Ty::Num,
    }
}

fn logic(is_and: bool, lhs: Typed, rhs: Typed) -> Result<Typed, ExprError> {
    let l = as_bool(lhs, "operand of '&&' or '||'")?;
    let r = as_bool(rhs, "operand of '&&' or '||'")?;
    Ok(Typed {
        node: Node::Logic(is_and, Box::new(l), Box::new(r)),
        ty: Ty::Bool,
    })
}

fn count(node: &Node) -> usize {
    match node {
        Node::Num(_) | Node::Bool(_) | Node::Var(_) => 1,
        Node::Unary(_, a) => 1 + count(a),
        Node::Binary(_, a, b) | Node::Logic(_, a, b) => 1 + count(a) + count(b),
        Node::Cond(c, a, b) => 1 + count(c) + count(a) + count(b),
        Node::Call(_, args) => 1 + args.iter().map(count).sum::<usize>(),
    }
}

impl Expr {
    /// Parses and type-checks `src` against `scope`. Names in `forbidden` are rejected with a
    /// message saying they are not available here (for example `x` outside `uniform`).
    pub fn compile(src: &str, scope: &Scope, forbidden: &[&str]) -> Result<Expr, ExprError> {
        let toks = lex(src)?;
        let mut parser = Parser {
            toks,
            at: 0,
            scope,
            forbidden,
        };
        let typed = parser.expr()?;
        if *parser.peek() != Tok::End {
            return parser.syntax("unexpected trailing input");
        }
        let nodes = count(&typed.node);
        if nodes > MAX_NODES {
            return Err(ExprError::TooLarge(nodes));
        }
        Ok(Expr {
            node: typed.node,
            result: typed.ty,
            nodes,
        })
    }

    /// Static type of the whole expression.
    pub fn result_ty(&self) -> Ty {
        self.result
    }

    /// AST node count (at most [`MAX_NODES`]).
    pub fn node_count(&self) -> usize {
        self.nodes
    }

    /// Evaluates against `slots`, indexed as the [`Scope`] the expression was compiled with.
    /// Bools are 1.0 / 0.0.
    pub fn eval(&self, slots: &[f64]) -> Result<f64, ExprError> {
        let v = eval_node(&self.node, slots)?;
        if v.is_finite() {
            Ok(v)
        } else {
            Err(ExprError::NonFinite)
        }
    }
}

fn eval_node(node: &Node, s: &[f64]) -> Result<f64, ExprError> {
    Ok(match node {
        Node::Num(v) => *v,
        Node::Bool(b) => bool_f(*b),
        Node::Var(slot) => s.get(*slot).copied().unwrap_or(0.0),
        Node::Unary(UnOp::Neg, a) => -eval_node(a, s)?,
        Node::Unary(UnOp::Not, a) => bool_f(eval_node(a, s)? == 0.0),
        Node::Logic(is_and, a, b) => {
            let left = eval_node(a, s)? != 0.0;
            // Short-circuit: the right side is only evaluated when the left does not decide.
            if *is_and && !left {
                0.0
            } else if !*is_and && left {
                1.0
            } else {
                bool_f(eval_node(b, s)? != 0.0)
            }
        }
        Node::Binary(op, a, b) => {
            let x = eval_node(a, s)?;
            let y = eval_node(b, s)?;
            match op {
                BinOp::Add => x + y,
                BinOp::Sub => x - y,
                BinOp::Mul => x * y,
                BinOp::Div => {
                    if y == 0.0 {
                        return Err(ExprError::DivideByZero);
                    }
                    x / y
                }
                BinOp::Rem => {
                    if y == 0.0 {
                        return Err(ExprError::DivideByZero);
                    }
                    x % y
                }
                BinOp::Eq => bool_f(x == y),
                BinOp::Ne => bool_f(x != y),
                BinOp::Lt => bool_f(x < y),
                BinOp::Le => bool_f(x <= y),
                BinOp::Gt => bool_f(x > y),
                BinOp::Ge => bool_f(x >= y),
            }
        }
        Node::Cond(c, a, b) => {
            if eval_node(c, s)? != 0.0 {
                eval_node(a, s)?
            } else {
                eval_node(b, s)?
            }
        }
        Node::Call(func, args) => {
            let mut v = Vec::with_capacity(args.len());
            for a in args {
                v.push(eval_node(a, s)?);
            }
            call(*func, &v)?
        }
    })
}

fn bool_f(b: bool) -> f64 {
    if b {
        1.0
    } else {
        0.0
    }
}

/// Applies a built-in. `a` has exactly the arity checked at parse time.
fn call(func: Func, a: &[f64]) -> Result<f64, ExprError> {
    Ok(match func {
        Func::Abs => a[0].abs(),
        Func::Min => a[0].min(a[1]),
        Func::Max => a[0].max(a[1]),
        // WGSL definition: min(max(x, lo), hi).
        Func::Clamp => a[0].max(a[1]).min(a[2]),
        // WGSL definition: a * (1 - t) + b * t.
        Func::Mix => a[0] * (1.0 - a[2]) + a[1] * a[2],
        // WGSL step(edge, x): 0 below the edge, 1 otherwise.
        Func::Step => {
            if a[1] < a[0] {
                0.0
            } else {
                1.0
            }
        }
        Func::Smoothstep => {
            if a[1] == a[0] {
                return Err(ExprError::DivideByZero);
            }
            let t = ((a[2] - a[0]) / (a[1] - a[0])).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        }
        Func::Pow => a[0].powf(a[1]),
        Func::Sqrt => a[0].sqrt(),
        Func::Exp => a[0].exp(),
        Func::Log => a[0].ln(),
        Func::Sin => a[0].sin(),
        Func::Cos => a[0].cos(),
        Func::Tan => a[0].tan(),
        Func::Atan2 => a[0].atan2(a[1]),
        Func::Floor => a[0].floor(),
        Func::Ceil => a[0].ceil(),
        Func::Fract => a[0] - a[0].floor(),
        Func::Round => a[0].round(),
        // logistic(x, k, x0) = 1 / (1 + e^(-k (x - x0)))
        Func::Logistic => 1.0 / (1.0 + (-a[1] * (a[0] - a[2])).exp()),
        // remap(x, a, b, c, d): linear map of [a, b] onto [c, d], not clamped.
        Func::Remap => {
            if a[2] == a[1] {
                return Err(ExprError::DivideByZero);
            }
            a[3] + (a[0] - a[1]) * (a[4] - a[3]) / (a[2] - a[1])
        }
        Func::Hash => hash_unit(a[0]),
    })
}

/// Deterministic hash of `n` into `[0, 1)`: the integer part of `n` (wrapped to u32) goes
/// through the PCG permutation the transition shaders use for blur-dissolve noise.
pub fn hash_unit(n: f64) -> f64 {
    let v = (n.floor() as i64) as u32;
    let state = v.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let word = ((state >> ((state >> 28).wrapping_add(4))) ^ state).wrapping_mul(277_803_737);
    let out = (word >> 22) ^ word;
    f64::from(out >> 8) / 16_777_216.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        let mut s = Scope::new();
        s.push("x", Ty::Num); // slot 0
        s.push("animated", Ty::Bool); // slot 1
        s.push("size", Ty::Num); // slot 2
        s
    }

    fn eval(src: &str, slots: &[f64]) -> f64 {
        Expr::compile(src, &scope(), &[])
            .and_then(|e| e.eval(slots))
            .unwrap_or_else(|e| panic!("{src}: {e}"))
    }

    fn err(src: &str) -> ExprError {
        match Expr::compile(src, &scope(), &[]) {
            Err(e) => e,
            Ok(e) => match e.eval(&[0.5, 1.0, 2.0]) {
                Err(e) => e,
                Ok(v) => panic!("{src} unexpectedly evaluated to {v}"),
            },
        }
    }

    #[test]
    fn precedence_and_associativity() {
        assert_eq!(eval("1 + 2 * 3", &[]), 7.0);
        assert_eq!(eval("(1 + 2) * 3", &[]), 9.0);
        assert_eq!(eval("10 - 4 - 3", &[]), 3.0);
        assert_eq!(eval("100 / 10 / 5", &[]), 2.0);
        assert_eq!(eval("7 % 4 * 2", &[]), 6.0);
        assert_eq!(eval("-2 * 3", &[]), -6.0);
        assert_eq!(eval("1 + 2 < 4", &[]), 1.0);
        assert_eq!(eval("true || false && false", &[]), 1.0);
        assert_eq!(eval("(true || false) && false", &[]), 0.0);
        assert_eq!(eval("false ? 1 : false ? 2 : 3", &[]), 3.0);
        assert_eq!(eval("true ? 2 : 3 + 100", &[]), 2.0);
    }

    #[test]
    fn literals_and_numbers() {
        assert_eq!(eval(".5", &[]), 0.5);
        assert_eq!(eval("1e-3", &[]), 0.001);
        assert_eq!(eval("2.5E2", &[]), 250.0);
        assert_eq!(eval("true", &[]), 1.0);
        assert_eq!(eval("!false", &[]), 1.0);
    }

    #[test]
    fn variables_and_bool_coercion() {
        assert_eq!(eval("x * 2", &[0.25, 0.0, 0.0]), 0.5);
        // a bool param is 0/1 in arithmetic
        assert_eq!(eval("animated * 24.0", &[0.0, 1.0, 0.0]), 24.0);
        assert_eq!(eval("animated * 24.0", &[0.0, 0.0, 0.0]), 0.0);
        assert_eq!(eval("animated ? size : 0", &[0.0, 1.0, 3.0]), 3.0);
        assert_eq!(eval("size == 3", &[0.0, 0.0, 3.0]), 1.0);
        // a comparison is a bool and coerces to 0/1 in arithmetic
        assert_eq!(eval("(x > 0) + 1", &[0.5, 0.0, 0.0]), 2.0);
        assert_eq!(eval("(x > 0) + 1", &[-0.5, 0.0, 0.0]), 1.0);
        assert_eq!(eval("animated == true", &[0.0, 1.0, 0.0]), 1.0);
    }

    #[test]
    fn type_errors() {
        // numbers are not booleans
        assert!(matches!(err("x && 1"), ExprError::Type(_)));
        assert!(matches!(err("!x"), ExprError::Type(_)));
        assert!(matches!(err("x ? 1 : 0"), ExprError::Type(_)));
        assert!(matches!(err("x && (1 > 0)"), ExprError::Type(_)));
        // chained comparison is a syntax error
        assert!(matches!(err("1 < x < 3"), ExprError::Syntax { .. }));
    }

    #[test]
    fn result_types() {
        let e = Expr::compile("x > 0.5", &scope(), &[]).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(e.result_ty(), Ty::Bool);
        let e =
            Expr::compile("animated && x > 0.5", &scope(), &[]).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(e.result_ty(), Ty::Bool);
        let e = Expr::compile("animated * 2", &scope(), &[]).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(e.result_ty(), Ty::Num);
    }

    #[test]
    fn name_and_call_errors() {
        assert!(matches!(err("nope + 1"), ExprError::UnknownName(n) if n == "nope"));
        assert!(matches!(
            err("frobnicate(1)"),
            ExprError::UnknownFunction(_)
        ));
        assert!(matches!(
            err("min(1)"),
            ExprError::Arity {
                expected: 2,
                got: 1,
                ..
            }
        ));
        assert!(matches!(err("1 +"), ExprError::Syntax { .. }));
        assert!(matches!(err("(1"), ExprError::Syntax { .. }));
        assert!(matches!(err("1 2"), ExprError::Syntax { .. }));
        assert!(matches!(err("1 # 2"), ExprError::Syntax { .. }));
        assert!(matches!(err("1 / 0"), ExprError::DivideByZero));
        assert!(matches!(err("1 % 0"), ExprError::DivideByZero));
        assert!(matches!(err("sqrt(-1)"), ExprError::NonFinite));
    }

    #[test]
    fn forbidden_names_are_explained() {
        let e = Expr::compile("x + 1", &Scope::new(), &["x"]).err();
        assert!(matches!(e, Some(ExprError::Type(_))));
    }

    #[test]
    fn node_limit() {
        let big = std::iter::repeat_n("1", 200).collect::<Vec<_>>().join("+");
        assert!(matches!(
            Expr::compile(&big, &Scope::new(), &[]),
            Err(ExprError::TooLarge(n)) if n > MAX_NODES
        ));
        let ok = std::iter::repeat_n("1", 100).collect::<Vec<_>>().join("+");
        assert!(Expr::compile(&ok, &Scope::new(), &[]).is_ok());
    }

    #[test]
    fn every_function() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-12;
        assert!(close(eval("abs(-2.5)", &[]), 2.5));
        assert!(close(eval("min(3, 2)", &[]), 2.0));
        assert!(close(eval("max(3, 2)", &[]), 3.0));
        assert!(close(eval("clamp(5, 0, 1)", &[]), 1.0));
        assert!(close(eval("clamp(-5, 0, 1)", &[]), 0.0));
        assert!(close(eval("mix(2, 4, 0.25)", &[]), 2.5));
        assert!(close(eval("step(0.5, 0.4)", &[]), 0.0));
        assert!(close(eval("step(0.5, 0.5)", &[]), 1.0));
        assert!(close(eval("smoothstep(0, 1, 0.5)", &[]), 0.5));
        assert!(close(eval("smoothstep(0, 1, 2)", &[]), 1.0));
        assert!(close(eval("pow(2, 10)", &[]), 1024.0));
        assert!(close(eval("sqrt(16)", &[]), 4.0));
        assert!(close(eval("exp(0)", &[]), 1.0));
        assert!(close(eval("log(1)", &[]), 0.0));
        assert!(close(eval("sin(0)", &[]), 0.0));
        assert!(close(eval("cos(0)", &[]), 1.0));
        assert!(close(eval("tan(0)", &[]), 0.0));
        assert!(close(eval("atan2(1, 1)", &[]), std::f64::consts::FRAC_PI_4));
        assert!(close(eval("floor(-1.5)", &[]), -2.0));
        assert!(close(eval("ceil(-1.5)", &[]), -1.0));
        assert!(close(eval("fract(2.75)", &[]), 0.75));
        assert!(close(eval("round(2.5)", &[]), 3.0));
        assert!(close(eval("round(-2.5)", &[]), -3.0));
        assert!(close(eval("logistic(0, 1, 0)", &[]), 0.5));
        assert!(close(eval("remap(0.5, 0, 1, 10, 20)", &[]), 15.0));
        assert!(close(eval("remap(2, 0, 1, 10, 20)", &[]), 30.0));
    }

    #[test]
    fn hash_is_deterministic_and_in_unit_range() {
        let a = eval("hash(7)", &[]);
        let b = eval("hash(7)", &[]);
        assert_eq!(a, b);
        assert!((0.0..1.0).contains(&a));
        assert_ne!(eval("hash(7)", &[]), eval("hash(8)", &[]));
        for n in 0..200 {
            let v = hash_unit(f64::from(n));
            assert!((0.0..1.0).contains(&v));
        }
    }

    #[test]
    fn remainder_follows_dividend_sign() {
        // remainder follows the dividend sign (fmod), like WGSL's %
        assert_eq!(eval("-7 % 3", &[]), -1.0);
        assert_eq!(eval("7 % -3", &[]), 1.0);
    }
}

//! The `when` condition language shared by keymaps, menus, views and extensions.
//!
//! ```text
//! expr    := and ('||' and)*
//! and     := unary ('&&' unary)*
//! unary   := '!' unary | primary
//! primary := '(' expr ')' | 'true' | 'false'
//!          | key ('==' | '!=') value          // a bare word value is a string
//!          | key ('<' | '<=' | '>' | '>=') int
//!          | key '=~' regex                   // /re/ or /re/i or a quoted string
//!          | operand ['not'] 'in' key         // list membership or substring
//!          | key                              // truthy
//! ```

use std::fmt;
use std::sync::Arc;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::context::{ContextLookup, CtxValue};

/// A `when` clause that failed to parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// Byte offset into the source.
    pub offset: usize,
    /// What went wrong.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at byte {}: {}", self.offset, self.message)
    }
}

impl std::error::Error for ParseError {}

/// A parsed `when` clause. Parse once (when bindings load), evaluate per key press.
#[derive(Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct When {
    src: Arc<str>,
    root: Node,
}

#[derive(Clone, Debug)]
struct Node {
    kind: Kind,
    start: usize,
    end: usize,
}

#[derive(Clone, Debug)]
enum Lit {
    Bool(bool),
    Int(i64),
    Str(Arc<str>),
}

#[derive(Clone, Debug)]
enum Operand {
    Key(Arc<str>),
    Lit(Lit),
}

#[derive(Clone, Copy, Debug)]
enum OrdOp {
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug)]
enum Kind {
    Const(bool),
    Truthy(Arc<str>),
    Not(Box<Node>),
    And(Vec<Node>),
    Or(Vec<Node>),
    Eq {
        key: Arc<str>,
        value: Lit,
        negate: bool,
    },
    Ord {
        key: Arc<str>,
        op: OrdOp,
        value: i64,
    },
    Match {
        key: Arc<str>,
        re: Regex,
    },
    In {
        needle: Operand,
        key: Arc<str>,
        negate: bool,
    },
}

impl When {
    /// Parses a clause.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] with the byte offset of the problem.
    pub fn parse(src: &str) -> Result<Self, ParseError> {
        let toks = lex(src)?;
        let mut p = Parser {
            toks,
            pos: 0,
            src,
            depth: 0,
        };
        let root = p.expr()?;
        if let Some(t) = p.toks.get(p.pos) {
            return Err(ParseError {
                offset: t.start,
                message: "unexpected token".into(),
            });
        }
        Ok(Self {
            src: src.into(),
            root,
        })
    }

    /// The source text.
    pub fn as_str(&self) -> &str {
        &self.src
    }

    /// Evaluates against `cx`.
    pub fn eval(&self, cx: &impl ContextLookup) -> bool {
        self.root.eval(cx)
    }

    /// The context keys the clause reads, in source order, without duplicates. Values
    /// are not keys: `ui.focus == sidebar` reads only `ui.focus`.
    pub fn keys(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.root.keys(&mut out);
        out
    }

    /// `None` if the clause holds; else the failing sub-clause and the key's actual
    /// value, for `describe-key` and keymap tracing.
    pub fn explain(&self, cx: &impl ContextLookup) -> Option<String> {
        self.root.explain(&self.src, cx)
    }
}

impl fmt::Debug for When {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "When({:?})", &*self.src)
    }
}

impl fmt::Display for When {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.src)
    }
}

impl TryFrom<String> for When {
    type Error = ParseError;
    fn try_from(s: String) -> Result<Self, ParseError> {
        Self::parse(&s)
    }
}

impl From<When> for String {
    fn from(w: When) -> String {
        w.src.to_string()
    }
}

fn lit_eq(v: &CtxValue, l: &Lit) -> bool {
    match (v, l) {
        (CtxValue::Bool(a), Lit::Bool(b)) => a == b,
        (CtxValue::Int(a), Lit::Int(b)) => a == b,
        (CtxValue::Str(a), Lit::Str(b)) => a == b,
        _ => false,
    }
}

fn contains(hay: Option<&CtxValue>, needle: &str) -> bool {
    match hay {
        Some(CtxValue::List(l)) => l.iter().any(|s| &**s == needle),
        Some(CtxValue::Str(s)) => s.contains(needle),
        _ => false,
    }
}

impl Node {
    fn keys<'a>(&'a self, out: &mut Vec<&'a str>) {
        let mut push = |k: &'a str| {
            if !out.contains(&k) {
                out.push(k);
            }
        };
        match &self.kind {
            Kind::Const(_) => {}
            Kind::Truthy(k)
            | Kind::Eq { key: k, .. }
            | Kind::Ord { key: k, .. }
            | Kind::Match { key: k, .. } => push(k),
            Kind::In { needle, key, .. } => {
                if let Operand::Key(n) = needle {
                    push(n);
                }
                push(key);
            }
            Kind::Not(n) => n.keys(out),
            Kind::And(ns) | Kind::Or(ns) => {
                for n in ns {
                    n.keys(out);
                }
            }
        }
    }

    fn eval(&self, cx: &impl ContextLookup) -> bool {
        match &self.kind {
            Kind::Const(b) => *b,
            Kind::Truthy(k) => cx.get(k).is_some_and(CtxValue::truthy),
            Kind::Not(n) => !n.eval(cx),
            Kind::And(v) => v.iter().all(|n| n.eval(cx)),
            Kind::Or(v) => v.iter().any(|n| n.eval(cx)),
            Kind::Eq { key, value, negate } => {
                cx.get(key).is_some_and(|v| lit_eq(v, value)) != *negate
            }
            Kind::Ord { key, op, value } => match cx.get(key) {
                Some(CtxValue::Int(i)) => match op {
                    OrdOp::Lt => i < value,
                    OrdOp::Le => i <= value,
                    OrdOp::Gt => i > value,
                    OrdOp::Ge => i >= value,
                },
                _ => false,
            },
            Kind::Match { key, re } => match cx.get(key) {
                Some(CtxValue::Str(s)) => re.is_match(s),
                _ => false,
            },
            Kind::In {
                needle,
                key,
                negate,
            } => {
                let hay = cx.get(key);
                let found = match needle {
                    Operand::Lit(Lit::Str(s)) => contains(hay, s),
                    Operand::Lit(Lit::Int(i)) => contains(hay, &i.to_string()),
                    Operand::Lit(Lit::Bool(b)) => contains(hay, &b.to_string()),
                    Operand::Key(k) => match cx.get(k) {
                        Some(CtxValue::Str(s)) => contains(hay, s),
                        Some(CtxValue::Int(i)) => contains(hay, &i.to_string()),
                        _ => false,
                    },
                };
                found != *negate
            }
        }
    }

    fn explain(&self, src: &str, cx: &impl ContextLookup) -> Option<String> {
        if self.eval(cx) {
            return None;
        }
        let text = &src[self.start..self.end];
        let key = match &self.kind {
            Kind::And(v) => return v.iter().find_map(|n| n.explain(src, cx)),
            Kind::Const(_) | Kind::Not(_) | Kind::Or(_) => return Some(text.to_owned()),
            Kind::Truthy(k)
            | Kind::Eq { key: k, .. }
            | Kind::Ord { key: k, .. }
            | Kind::Match { key: k, .. }
            | Kind::In { key: k, .. } => k,
        };
        let actual = cx.get(key).map_or_else(
            || "unset".to_owned(),
            |v| serde_json::to_string(v).unwrap_or_default(),
        );
        Some(format!("{text} ({key} is {actual})"))
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Int(i64),
    Regex(String, bool),
    EqEq,
    Ne,
    Match,
    And,
    Or,
    Not,
    Lt,
    Le,
    Gt,
    Ge,
    LParen,
    RParen,
}

struct Token {
    tok: Tok,
    start: usize,
    end: usize,
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
}

#[allow(clippy::too_many_lines)]
fn lex(src: &str) -> Result<Vec<Token>, ParseError> {
    let err = |offset, message: &str| ParseError {
        offset,
        message: message.into(),
    };
    let bytes = src.as_bytes();
    let mut out: Vec<Token> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let two = src.get(i..i + 2).unwrap_or("");
        let tok = match two {
            "==" => Some(Tok::EqEq),
            "!=" => Some(Tok::Ne),
            "=~" => Some(Tok::Match),
            "&&" => Some(Tok::And),
            "||" => Some(Tok::Or),
            "<=" => Some(Tok::Le),
            ">=" => Some(Tok::Ge),
            _ => None,
        };
        if let Some(tok) = tok {
            i += 2;
            let is_match = tok == Tok::Match;
            out.push(Token { tok, start, end: i });
            if is_match {
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if bytes.get(i) == Some(&b'/') {
                    let rstart = i;
                    i += 1;
                    let mut pat = String::new();
                    loop {
                        match bytes.get(i) {
                            None => return Err(err(rstart, "unterminated regex")),
                            Some(b'/') => break,
                            Some(b'\\') if bytes.get(i + 1) == Some(&b'/') => {
                                pat.push('/');
                                i += 2;
                            }
                            Some(_) => {
                                let ch = src[i..].chars().next().unwrap_or_default();
                                pat.push(ch);
                                i += ch.len_utf8();
                            }
                        }
                    }
                    i += 1;
                    let icase = bytes.get(i) == Some(&b'i');
                    if icase {
                        i += 1;
                    }
                    out.push(Token {
                        tok: Tok::Regex(pat, icase),
                        start: rstart,
                        end: i,
                    });
                }
            }
            continue;
        }
        let tok = match c {
            b'!' => Tok::Not,
            b'<' => Tok::Lt,
            b'>' => Tok::Gt,
            b'(' => Tok::LParen,
            b')' => Tok::RParen,
            b'\'' | b'"' => {
                let mut s = String::new();
                i += 1;
                loop {
                    match bytes.get(i) {
                        None => return Err(err(start, "unterminated string")),
                        Some(&q) if q == c => break,
                        Some(b'\\') if matches!(bytes.get(i + 1), Some(b'\\' | b'\'' | b'"')) => {
                            s.push(char::from(bytes[i + 1]));
                            i += 2;
                        }
                        Some(_) => {
                            let ch = src[i..].chars().next().unwrap_or_default();
                            s.push(ch);
                            i += ch.len_utf8();
                        }
                    }
                }
                i += 1;
                out.push(Token {
                    tok: Tok::Str(s),
                    start,
                    end: i,
                });
                continue;
            }
            _ if is_word(char::from(c)) => {
                while i < bytes.len() && is_word(char::from(bytes[i])) {
                    i += 1;
                }
                let w = &src[start..i];
                let tok = match w.parse::<i64>() {
                    Ok(n) => Tok::Int(n),
                    Err(_) => Tok::Word(w.to_owned()),
                };
                out.push(Token { tok, start, end: i });
                continue;
            }
            _ => return Err(err(start, "unexpected character")),
        };
        i += 1;
        out.push(Token { tok, start, end: i });
    }
    Ok(out)
}

/// Nesting limit for `(` and `!`, so hostile input cannot overflow the stack.
const MAX_NESTING: u32 = 64;

struct Parser<'s> {
    toks: Vec<Token>,
    pos: usize,
    src: &'s str,
    depth: u32,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.tok)
    }

    fn here(&self) -> usize {
        self.toks.get(self.pos).map_or(self.src.len(), |t| t.start)
    }

    fn last_end(&self) -> usize {
        self.pos
            .checked_sub(1)
            .and_then(|p| self.toks.get(p))
            .map_or(0, |t| t.end)
    }

    fn err<T>(&self, message: &str) -> Result<T, ParseError> {
        Err(ParseError {
            offset: self.here(),
            message: message.into(),
        })
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|t| t.tok.clone());
        self.pos += 1;
        t
    }

    fn list(
        &mut self,
        sep: &Tok,
        next: fn(&mut Self) -> Result<Node, ParseError>,
    ) -> Result<Vec<Node>, ParseError> {
        let mut v = vec![next(self)?];
        while self.peek() == Some(sep) {
            self.pos += 1;
            v.push(next(self)?);
        }
        Ok(v)
    }

    fn nested(&mut self, f: fn(&mut Self) -> Result<Node, ParseError>) -> Result<Node, ParseError> {
        if self.depth >= MAX_NESTING {
            return self.err("nested too deeply");
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        r
    }

    fn expr(&mut self) -> Result<Node, ParseError> {
        let mut v = self.list(&Tok::Or, Self::and)?;
        Ok(if v.len() == 1 {
            v.pop().unwrap_or_else(|| unreachable!())
        } else {
            let (start, end) = (v[0].start, v[v.len() - 1].end);
            Node {
                kind: Kind::Or(v),
                start,
                end,
            }
        })
    }

    fn and(&mut self) -> Result<Node, ParseError> {
        let mut v = self.list(&Tok::And, Self::unary)?;
        Ok(if v.len() == 1 {
            v.pop().unwrap_or_else(|| unreachable!())
        } else {
            let (start, end) = (v[0].start, v[v.len() - 1].end);
            Node {
                kind: Kind::And(v),
                start,
                end,
            }
        })
    }

    fn unary(&mut self) -> Result<Node, ParseError> {
        if self.peek() == Some(&Tok::Not) {
            let start = self.here();
            self.pos += 1;
            let inner = self.nested(Self::unary)?;
            let end = inner.end;
            return Ok(Node {
                kind: Kind::Not(Box::new(inner)),
                start,
                end,
            });
        }
        self.primary()
    }

    fn key(&mut self) -> Result<Arc<str>, ParseError> {
        match self.bump() {
            Some(Tok::Word(w)) if crate::name::check_name(&w, 1).is_ok() => Ok(w.into()),
            _ => {
                self.pos -= 1;
                self.err("expected a context key")
            }
        }
    }

    fn primary(&mut self) -> Result<Node, ParseError> {
        let start = self.here();
        let node = |kind, end| Node { kind, start, end };
        let first = self.bump();
        let needle = match first {
            Some(Tok::LParen) => {
                let mut inner = self.nested(Self::expr)?;
                if self.bump() != Some(Tok::RParen) {
                    self.pos -= 1;
                    return self.err("expected ')'");
                }
                inner.start = start;
                inner.end = self.last_end();
                return Ok(inner);
            }
            Some(Tok::Word(w)) if w == "true" || w == "false" => {
                return Ok(node(Kind::Const(w == "true"), self.last_end()));
            }
            Some(Tok::Str(s)) => Operand::Lit(Lit::Str(s.into())),
            Some(Tok::Int(n)) => Operand::Lit(Lit::Int(n)),
            Some(Tok::Word(_)) => {
                self.pos -= 1;
                Operand::Key(self.key()?)
            }
            None => return self.err("unexpected end of clause"),
            Some(_) => {
                self.pos -= 1;
                return self.err("expected a key, value or '('");
            }
        };
        let negate_in = matches!(self.peek(), Some(Tok::Word(w)) if w == "not");
        if negate_in || matches!(self.peek(), Some(Tok::Word(w)) if w == "in") {
            self.pos += 1;
            if negate_in && !matches!(self.bump(), Some(Tok::Word(w)) if w == "in") {
                self.pos -= 1;
                return self.err("expected 'in' after 'not'");
            }
            let key = self.key()?;
            return Ok(node(
                Kind::In {
                    needle,
                    key,
                    negate: negate_in,
                },
                self.last_end(),
            ));
        }
        let Operand::Key(key) = needle else {
            return self.err("a literal must be followed by 'in'");
        };
        let kind = match self.peek() {
            Some(Tok::EqEq | Tok::Ne) => {
                let negate = self.bump() == Some(Tok::Ne);
                let value = match self.bump() {
                    Some(Tok::Str(s)) => Lit::Str(s.into()),
                    Some(Tok::Int(n)) => Lit::Int(n),
                    Some(Tok::Word(w)) if w == "true" || w == "false" => Lit::Bool(w == "true"),
                    Some(Tok::Word(w)) => Lit::Str(w.into()),
                    _ => {
                        self.pos -= 1;
                        return self.err("expected a value");
                    }
                };
                Kind::Eq { key, value, negate }
            }
            Some(Tok::Lt | Tok::Le | Tok::Gt | Tok::Ge) => {
                let op = match self.bump() {
                    Some(Tok::Lt) => OrdOp::Lt,
                    Some(Tok::Le) => OrdOp::Le,
                    Some(Tok::Gt) => OrdOp::Gt,
                    _ => OrdOp::Ge,
                };
                let Some(Tok::Int(value)) = self.bump() else {
                    self.pos -= 1;
                    return self.err("expected an integer");
                };
                Kind::Ord { key, op, value }
            }
            Some(Tok::Match) => {
                self.pos += 1;
                let at = self.here();
                let pat = match self.bump() {
                    Some(Tok::Regex(p, true)) => format!("(?i){p}"),
                    Some(Tok::Regex(p, false) | Tok::Str(p)) => p,
                    _ => {
                        self.pos -= 1;
                        return self.err("expected /regex/ or a string");
                    }
                };
                let re = Regex::new(&pat).map_err(|e| ParseError {
                    offset: at,
                    message: format!("bad regex: {e}"),
                })?;
                Kind::Match { key, re }
            }
            _ => Kind::Truthy(key),
        };
        Ok(node(kind, self.last_end()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;

    fn cx() -> Context {
        let mut c = Context::new();
        c.set("editor.mode", "normal").unwrap();
        c.set("editor.focus", true).unwrap();
        c.set("editor.count", 3i64).unwrap();
        c.set("buffer.filetype", "rust").unwrap();
        c.set("buffer.path", "/src/main.rs").unwrap();
        c.set("editor.zero", 0i64).unwrap();
        c.set("editor.empty", "").unwrap();
        c.set("ext.enabled", vec!["git", "lsp"]).unwrap();
        c
    }

    fn t(src: &str) -> bool {
        When::parse(src)
            .unwrap_or_else(|e| panic!("{src}: {e}"))
            .eval(&cx())
    }

    #[test]
    fn keys_lists_what_a_clause_reads() {
        let w = When::parse(
            "ui.sidebar_open && (client.cols >= 100 || ui.focus == sidebar) && !ui.sidebar_open && x in ext.enabled && 'y' in ext.enabled && buffer.path =~ /rs$/",
        )
        .unwrap();
        assert_eq!(
            w.keys(),
            [
                "ui.sidebar_open",
                "client.cols",
                "ui.focus",
                "x",
                "ext.enabled",
                "buffer.path"
            ]
        );
        assert!(When::parse("true").unwrap().keys().is_empty());
    }

    #[test]
    fn operators() {
        let cases = [
            ("editor.focus", true),
            ("!editor.focus", false),
            ("editor.missing", false),
            ("editor.zero", false),
            ("editor.empty", false),
            ("ext.enabled", true),
            ("editor.mode == normal", true),
            ("editor.mode == 'normal'", true),
            ("editor.mode != insert", true),
            ("editor.missing != x", true),
            ("editor.missing == x", false),
            ("editor.count == 3", true),
            ("editor.count == '3'", false),
            ("editor.focus == true", true),
            ("editor.count > 2 && editor.count <= 3", true),
            ("editor.count < 3", false),
            ("editor.count >= 4", false),
            ("editor.mode > 1", false),
            ("buffer.path =~ /\\.rs$/", true),
            ("buffer.path =~ /MAIN/i", true),
            ("buffer.path =~ /MAIN/", false),
            ("buffer.path =~ '^/src/'", true),
            ("'git' in ext.enabled", true),
            ("'tree' in ext.enabled", false),
            ("'tree' not in ext.enabled", true),
            ("buffer.filetype in ext.enabled", false),
            ("'main' in buffer.path", true),
            ("true", true),
            ("false || editor.focus", true),
        ];
        for (src, want) in cases {
            assert_eq!(t(src), want, "{src}");
        }
    }

    #[test]
    fn precedence() {
        assert!(t("editor.focus || editor.missing && editor.missing"));
        assert!(!t("(editor.focus || editor.missing) && editor.missing"));
        assert!(!t("!editor.focus || editor.missing"));
        assert!(t("!(editor.missing && editor.focus)"));
    }

    #[test]
    fn explain() {
        let w = When::parse("editor.focus && editor.mode == insert").unwrap();
        assert_eq!(
            w.explain(&cx()).unwrap(),
            "editor.mode == insert (editor.mode is \"normal\")"
        );
        let w = When::parse("editor.a || editor.b").unwrap();
        assert_eq!(w.explain(&cx()).unwrap(), "editor.a || editor.b");
        assert!(
            When::parse("editor.focus")
                .unwrap()
                .explain(&cx())
                .is_none()
        );
        let w = When::parse("editor.focus && editor.nope").unwrap();
        assert_eq!(
            w.explain(&cx()).unwrap(),
            "editor.nope (editor.nope is unset)"
        );
    }

    #[test]
    fn parse_errors() {
        let cases = [
            ("", 0),
            ("editor.mode ==", 14),
            ("(editor.focus", 13),
            ("editor.mode == a b", 17),
            ("'x'", 3),
            ("editor.count > x", 15),
            ("buffer.path =~ /(/", 15),
            ("buffer.path =~ /abc", 15),
            ("a.b && 'oops", 7),
            ("a.b # c", 4),
            ("'x' not a.b", 8),
        ];
        for (src, offset) in cases {
            let e = When::parse(src)
                .err()
                .unwrap_or_else(|| panic!("{src} parsed"));
            assert_eq!(e.offset, offset, "{src}: {e}");
        }
    }

    #[test]
    fn serde_round_trip() {
        let w: When = serde_json::from_str("\"editor.mode == normal\"").unwrap();
        assert_eq!(
            serde_json::to_string(&w).unwrap(),
            "\"editor.mode == normal\""
        );
        assert!(serde_json::from_str::<When>("\"((\"").is_err());
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_crash() {
        for src in ["(".repeat(5000) + "a", "!".repeat(5000) + "a"] {
            let e = When::parse(&src).unwrap_err();
            assert!(e.message.contains("nested"), "{e:?}");
        }
        assert!(When::parse(&("(".repeat(20) + "a" + &")".repeat(20))).is_ok());
    }
}

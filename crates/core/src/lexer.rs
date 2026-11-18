//! Tokenizer and preprocessor for OpenEdge ABL.
//!
//! ABL rules that shape this lexer:
//! - Identifiers may contain `-`, `#`, `$`, `%`, `&` (so `a-b` is one word and
//!   subtraction needs spaces) and dots (`db.table.field`, `Progress.Lang.Object`).
//! - A `.` or `:` followed by whitespace ends a statement / block header; when it
//!   is followed by anything else it is part of an expression (`obj:Method()`).
//! - Strings may span lines, use `~` as escape, and may carry a `:U`-style suffix.
//! - `/* */` comments nest; `//` comments run to end of line.
//! - `{...}` is an include file or preprocessor reference; `&DIRECTIVE` lines
//!   are preprocessor statements.
//!
//! The preprocessor runs as the text is tokenized:
//! - `&GLOBAL-DEFINE` / `&SCOPED-DEFINE` / `&UNDEFINE` maintain the defined names.
//!   Include files are read (through [`PreEnv::resolve`]) so their global defines
//!   are known after the `{include.i}` reference.
//! - `&IF` / `&ELSEIF` conditions are evaluated; tokens in branches not taken are
//!   marked inactive. A condition that cannot be evaluated takes the first branch.
//! - `{&name}`, and `{1}` / `{*}` / `{&arg}` in an include analysed with its
//!   arguments, are replaced by the tokens of their value. The reference token
//!   stays in the list marked `expanded`; the value tokens all span the reference
//!   and carry their own text in `subst`.

use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokKind {
    Word,
    String,
    Number,
    Comment,
    Preproc,
    Include,
    /// `.` that terminates a statement.
    Period,
    /// `:` that terminates a block header.
    Colon,
    /// `:` used for member access (`obj:Method`).
    Member,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Operator,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokKind,
    pub start: usize,
    pub end: usize,
    /// Inside a preprocessor branch that is not taken.
    pub inactive: bool,
    /// A `{&name}` reference that was replaced by the tokens of its value.
    pub expanded: bool,
    /// Text of a token that came from a preprocessor value (start..end is the reference).
    pub subst: Option<Arc<str>>,
}

impl Token {
    fn new(kind: TokKind, start: usize, end: usize, inactive: bool) -> Self {
        Token {
            kind,
            start,
            end,
            inactive,
            expanded: false,
            subst: None,
        }
    }

    /// The token's text: its source span, or the expanded value text.
    pub fn text<'a>(&'a self, src: &'a str) -> &'a str {
        match &self.subst {
            Some(s) => s,
            None => &src[self.start..self.end],
        }
    }
}

#[derive(Clone, Debug)]
pub struct LexError {
    pub start: usize,
    pub end: usize,
    pub message: String,
}

pub fn is_word_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

pub fn is_word_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'#' | b'$' | b'%' | b'&') || c >= 0x80
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n' | 0x0c)
}

// ---- preprocessor names ----------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DefKind {
    Global,
    Scoped,
    /// Named include argument (`{inc.i &name=value}`).
    Arg,
}

/// Defined preprocessor names, keyed by upper-case name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Defines(BTreeMap<String, (DefKind, String)>);

impl Defines {
    pub fn get(&self, name: &str) -> Option<&(DefKind, String)> {
        self.0.get(&name.to_ascii_uppercase())
    }

    pub fn set(&mut self, name: &str, kind: DefKind, value: String) {
        self.0.insert(name.to_ascii_uppercase(), (kind, value));
    }

    pub fn remove(&mut self, name: &str) {
        self.0.remove(&name.to_ascii_uppercase());
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Named include arguments only.
    pub fn args(&self) -> Vec<(&String, &String)> {
        self.0
            .iter()
            .filter(|(_, (k, _))| *k == DefKind::Arg)
            .map(|(n, (_, v))| (n, v))
            .collect()
    }
}

/// How an include file is being read: the names defined where it is included,
/// plus its arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct IncludeCtx {
    pub defines: Defines,
    /// `{1}`, `{2}`... values.
    pub positional: Vec<String>,
    /// The arguments are known (the file is read for a particular include reference),
    /// so a missing `{3}` expands to nothing.
    pub has_args: bool,
}

/// Reads an include file by its reference (`inc/x.i`).
pub type Resolver<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Default)]
pub struct PreEnv<'a> {
    pub ctx: IncludeCtx,
    pub resolve: Option<Resolver<'a>>,
}

/// `{include.i args}` reference.
#[derive(Clone, Debug)]
pub struct IncludeSite {
    pub start: usize,
    pub end: usize,
    pub target: String,
    /// What the include sees: the names defined at this point plus its arguments.
    pub ctx: Arc<IncludeCtx>,
}

/// A preprocessor reference that was expanded.
#[derive(Clone, Debug)]
pub struct Expansion {
    pub start: usize,
    pub end: usize,
    /// `&NAME`, `1` or `*`.
    pub name: String,
    pub value: String,
}

pub struct LexOut {
    pub tokens: Vec<Token>,
    pub errors: Vec<LexError>,
    pub includes: Vec<IncludeSite>,
    pub expansions: Vec<Expansion>,
}

/// Names the compiler defines.
fn builtin_define(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_uppercase().as_str() {
        "OPSYS" => {
            if cfg!(windows) {
                "WIN32"
            } else {
                "UNIX"
            }
        }
        "WINDOW-SYSTEM" => {
            if cfg!(windows) {
                "MS-WINXP"
            } else {
                "TTY"
            }
        }
        "PROCESS-ARCHITECTURE" => "64",
        "BATCH-MODE" => "no",
        _ => return None,
    })
}

/// Splits an include reference body (`inc/x.i a "b c" &n=v`) into target, positional and named arguments.
fn include_args(body: &str) -> (String, Vec<String>, Vec<(String, String)>) {
    let b = body.as_bytes();
    let mut words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if is_ws(b[i]) {
            i += 1;
            continue;
        }
        let mut w = String::new();
        while i < b.len() && !is_ws(b[i]) {
            if b[i] == b'"' || b[i] == b'\'' {
                // Quoted text (the quotes are dropped), possibly after `&name=`.
                let q = b[i];
                i += 1;
                let s = i;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                w.push_str(&body[s..i.min(b.len())]);
                i += 1;
            } else {
                let s = i;
                while i < b.len() && !is_ws(b[i]) && b[i] != b'"' && b[i] != b'\'' {
                    i += 1;
                }
                w.push_str(&body[s..i]);
            }
        }
        words.push(w);
    }
    let mut it = words.into_iter();
    let target = it.next().unwrap_or_default();
    let mut positional = Vec::new();
    let mut named = Vec::new();
    for w in it {
        match w.strip_prefix('&').and_then(|r| r.split_once('=')) {
            Some((n, v)) => named.push((n.to_string(), v.to_string())),
            None => positional.push(w),
        }
    }
    (target, positional, named)
}

struct Cond {
    active: bool,
    /// A branch of this &IF has been taken.
    taken: bool,
    start: usize,
    end: usize,
}

struct Lx<'s, 'e> {
    src: &'s str,
    b: &'s [u8],
    n: usize,
    resolve: Option<Resolver<'e>>,
    defs: Defines,
    positional: Vec<String>,
    has_args: bool,
    depth: usize,
    /// An include file could not be read, so unknown names may be defined there.
    unresolved: bool,
    toks: Vec<Token>,
    errs: Vec<LexError>,
    conds: Vec<Cond>,
    includes: Vec<IncludeSite>,
    expansions: Vec<Expansion>,
}

pub fn lex(src: &str) -> (Vec<Token>, Vec<LexError>) {
    let out = lex_env(src, &PreEnv::default());
    (out.tokens, out.errors)
}

pub fn lex_env(src: &str, env: &PreEnv) -> LexOut {
    let mut lx = Lx::new(src, env.resolve, env.ctx.clone(), 0);
    lx.run();
    LexOut {
        tokens: lx.toks,
        errors: lx.errs,
        includes: lx.includes,
        expansions: lx.expansions,
    }
}

impl<'s, 'e> Lx<'s, 'e> {
    fn new(src: &'s str, resolve: Option<Resolver<'e>>, ctx: IncludeCtx, depth: usize) -> Self {
        Lx {
            src,
            b: src.as_bytes(),
            n: src.len(),
            resolve,
            defs: ctx.defines,
            positional: ctx.positional,
            has_args: ctx.has_args,
            depth,
            unresolved: false,
            toks: Vec::new(),
            errs: Vec::new(),
            conds: Vec::new(),
            includes: Vec::new(),
            expansions: Vec::new(),
        }
    }

    fn at(&self, i: usize) -> u8 {
        if i < self.n {
            self.b[i]
        } else {
            0
        }
    }

    fn ends_here(&self, i: usize) -> bool {
        i >= self.n
            || is_ws(self.b[i])
            || (self.b[i] == b'/' && matches!(self.at(i + 1), b'*' | b'/'))
    }

    fn err(&mut self, start: usize, end: usize, msg: &str) {
        self.errs.push(LexError {
            start,
            end,
            message: msg.into(),
        });
    }

    fn run(&mut self) {
        let (b, n) = (self.b, self.n);
        let mut i = 0;
        while i < n {
            let c = b[i];
            if is_ws(c) {
                i += 1;
                continue;
            }
            let start = i;
            let inactive = self.conds.iter().any(|c| !c.active);

            let kind = if c == b'/' && self.at(i + 1) == b'*' {
                let mut depth = 0usize;
                loop {
                    if i >= n {
                        self.err(start, start + 2, "Unterminated comment");
                        break;
                    }
                    if b[i] == b'/' && self.at(i + 1) == b'*' {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && self.at(i + 1) == b'/' {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                TokKind::Comment
            } else if c == b'/' && self.at(i + 1) == b'/' {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
                TokKind::Comment
            } else if c == b'"' || c == b'\'' {
                let q = c;
                i += 1;
                let mut closed = false;
                while i < n {
                    let ch = b[i];
                    if ch == b'~' {
                        i += 2;
                        continue;
                    }
                    if ch == q {
                        if self.at(i + 1) == q {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                i = i.min(n);
                if !closed {
                    self.err(start, start + 1, "Unterminated string");
                } else if self.at(i) == b':' && self.at(i + 1).is_ascii_alphabetic() {
                    // String attributes such as "text":U or "text":R20
                    i += 1;
                    while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'<' || b[i] == b'>') {
                        i += 1;
                    }
                }
                TokKind::String
            } else if c == b'{' {
                let mut depth = 0usize;
                let mut closed = false;
                while i < n {
                    match b[i] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                i += 1;
                                closed = true;
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                if !closed {
                    self.err(start, start + 1, "Unterminated '{' include reference");
                } else if !inactive {
                    self.brace(start, i);
                    continue;
                }
                TokKind::Include
            } else if c == b'&' && self.at(i + 1).is_ascii_alphabetic() {
                i = self.directive(start, inactive);
                TokKind::Preproc
            } else if is_word_start(c) {
                i += 1;
                loop {
                    let word_char = i < n && is_word_char(b[i]);
                    // A period followed by a name continues it (`table.field`, `pkg.Class`).
                    let qualifier = i + 1 < n
                        && b[i] == b'.'
                        && (is_word_start(b[i + 1]) || b[i + 1].is_ascii_digit());
                    if word_char || qualifier {
                        i += 1;
                    } else {
                        break;
                    }
                }
                TokKind::Word
            } else if c.is_ascii_digit() || (c == b'.' && self.at(i + 1).is_ascii_digit()) {
                i += 1;
                while i < n
                    && (b[i].is_ascii_alphanumeric()
                        || (b[i] == b'.' && self.at(i + 1).is_ascii_digit()))
                {
                    i += 1;
                }
                TokKind::Number
            } else if c == b'.' {
                i += 1;
                if self.ends_here(i) {
                    TokKind::Period
                } else {
                    TokKind::Operator
                }
            } else if c == b':' {
                i += 1;
                if self.ends_here(i) {
                    TokKind::Colon
                } else {
                    if self.at(i) == b':' {
                        i += 1;
                    }
                    TokKind::Member
                }
            } else {
                i += 1;
                match c {
                    b'(' => TokKind::LParen,
                    b')' => TokKind::RParen,
                    b'[' => TokKind::LBracket,
                    b']' => TokKind::RBracket,
                    b',' => TokKind::Comma,
                    b'<' => {
                        if matches!(self.at(i), b'=' | b'>') {
                            i += 1;
                        }
                        TokKind::Operator
                    }
                    b'>' => {
                        if self.at(i) == b'=' {
                            i += 1;
                        }
                        TokKind::Operator
                    }
                    _ => TokKind::Operator,
                }
            };

            let mut end = i.min(n);
            while matches!(kind, TokKind::Comment | TokKind::Preproc)
                && end > start
                && b[end - 1] == b'\r'
            {
                end -= 1;
            }
            self.toks.push(Token::new(kind, start, end, inactive));
        }

        for c in std::mem::take(&mut self.conds) {
            self.err(c.start, c.end, "&IF without matching &ENDIF");
        }
    }

    // ---- directives ----------------------------------------------------------

    /// Lexes `&DIRECTIVE ...` at `start`; returns where it ends.
    fn directive(&mut self, start: usize, inactive: bool) -> usize {
        let (b, n) = (self.b, self.n);
        let name_start = start + 1;
        let mut j = name_start;
        while j < n && (b[j].is_ascii_alphanumeric() || b[j] == b'-' || b[j] == b'_') {
            j += 1;
        }
        let name = self.src[name_start..j].to_ascii_uppercase();
        let mut i = j;
        match name.as_str() {
            "IF" | "ELSEIF" => {
                let mut cond_end = n;
                while i < n {
                    if b[i] == b'&'
                        && i + 5 <= n
                        && b[i + 1..i + 5].eq_ignore_ascii_case(b"THEN")
                        && !is_word_char(self.at(i + 5))
                    {
                        cond_end = i;
                        i += 5;
                        break;
                    }
                    i += 1;
                }
                let src = self.src;
                let cond = &src[j..cond_end.min(n)];
                let outer_active = !inactive;
                if name == "IF" {
                    let r = if outer_active {
                        self.eval_cond(cond)
                    } else {
                        Some(false)
                    };
                    let active = r.unwrap_or(true);
                    self.conds.push(Cond {
                        active,
                        taken: active,
                        start,
                        end: i,
                    });
                } else if self.conds.is_empty() {
                    self.err(start, j, "&ELSEIF without &IF");
                } else {
                    let taken = self.conds.last().unwrap().taken;
                    let enclosing = self.conds.len() == 1
                        || self.conds[..self.conds.len() - 1].iter().all(|c| c.active);
                    let active = !taken && enclosing && self.eval_cond(cond).unwrap_or(true);
                    let top = self.conds.last_mut().unwrap();
                    top.active = active;
                    top.taken |= active;
                }
            }
            "ELSE" => match self.conds.last_mut() {
                Some(top) => {
                    top.active = !top.taken;
                    top.taken = true;
                }
                None => self.err(start, j, "&ELSE without &IF"),
            },
            "ENDIF" => {
                if self.conds.pop().is_none() {
                    self.err(start, j, "&ENDIF without &IF");
                }
            }
            "THEN" => {}
            _ => {
                // &GLOBAL-DEFINE, &SCOPED-DEFINE, &ANALYZE-SUSPEND, &UNDEFINE, &MESSAGE...
                // run to end of line, honouring ~ line continuations.
                while i < n && b[i] != b'\n' {
                    if b[i] == b'~'
                        && (self.at(i + 1) == b'\n'
                            || (self.at(i + 1) == b'\r' && self.at(i + 2) == b'\n'))
                    {
                        i += if self.at(i + 1) == b'\r' { 3 } else { 2 };
                        continue;
                    }
                    i += 1;
                }
                if !inactive {
                    let src = self.src;
                    self.define(&name, &src[j..i]);
                }
            }
        }
        i
    }

    fn define(&mut self, dir: &str, rest: &str) {
        let is = |full: &str, min: usize| dir.len() >= min && full.starts_with(dir);
        let kind = if is("GLOBAL-DEFINE", 4) {
            DefKind::Global
        } else if is("SCOPED-DEFINE", 4) {
            DefKind::Scoped
        } else if is("UNDEFINE", 5) {
            if let Some(name) = rest.split_whitespace().next() {
                self.defs.remove(name);
            }
            return;
        } else {
            return;
        };
        let rest = rest.trim_start();
        let name_len = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        if name_len == 0 {
            return;
        }
        let value = rest[name_len..].replace("~\r\n", "\n").replace("~\n", "\n");
        self.defs
            .set(&rest[..name_len], kind, value.trim().to_string());
    }

    // ---- references ----------------------------------------------------------

    /// Value of the reference `{...}` (body without braces), when it is known.
    fn ref_value(&self, body: &str) -> Option<(String, String)> {
        let body = body.trim();
        if let Some(rest) = body.strip_prefix('&') {
            let name: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
            if let Some((_, v)) = self.defs.get(&name) {
                return Some((format!("&{name}"), v.clone()));
            }
            return builtin_define(&name).map(|v| (format!("&{name}"), v.to_string()));
        }
        if !self.has_args {
            return None;
        }
        if body == "*" {
            return Some(("*".into(), self.positional.join(" ")));
        }
        let n: usize = body.parse().ok()?;
        Some((
            n.to_string(),
            n.checked_sub(1)
                .and_then(|k| self.positional.get(k))
                .cloned()
                .unwrap_or_default(),
        ))
    }

    /// `{...}` at start..end in active code.
    fn brace(&mut self, start: usize, end: usize) {
        let src = self.src;
        let body = &src[start + 1..end - 1];
        let first = body.trim_start().chars().next().unwrap_or(' ');
        if first == '&' || first == '*' || first.is_ascii_digit() {
            let mut tok = Token::new(TokKind::Include, start, end, false);
            if let Some((name, value)) = self.ref_value(body) {
                tok.expanded = true;
                self.toks.push(tok);
                let mut out = Vec::new();
                self.expand_tokens(&value, 0, &mut out);
                for (kind, text) in out {
                    self.toks.push(Token {
                        kind,
                        start,
                        end,
                        inactive: false,
                        expanded: false,
                        subst: Some(text),
                    });
                }
                self.expansions.push(Expansion {
                    start,
                    end,
                    name,
                    value,
                });
            } else {
                self.toks.push(tok);
            }
            return;
        }
        self.toks
            .push(Token::new(TokKind::Include, start, end, false));
        let (expanded, _) = self.expand_text(body, 0);
        let (target, positional, named) = include_args(&expanded);
        if target.is_empty() {
            return;
        }
        let mut defines = self.defs.clone();
        for (k, v) in &named {
            defines.set(k, DefKind::Arg, v.clone());
        }
        let ctx = Arc::new(IncludeCtx {
            defines,
            positional,
            has_args: true,
        });
        self.includes.push(IncludeSite {
            start,
            end,
            target: target.clone(),
            ctx: ctx.clone(),
        });
        // Global names the include defines are visible from here on.
        let Some(resolve) = self.resolve else { return };
        if self.depth >= 8 {
            return;
        }
        match resolve(&target) {
            Some(text) => {
                let mut sub = Lx::new(&text, Some(resolve), (*ctx).clone(), self.depth + 1);
                sub.run();
                self.unresolved |= sub.unresolved;
                for (k, (kind, v)) in sub.defs.0 {
                    if kind == DefKind::Global {
                        self.defs.0.insert(k, (kind, v));
                    }
                }
            }
            None => self.unresolved = true,
        }
    }

    /// Tokens of a preprocessor value, with nested references expanded.
    fn expand_tokens(&self, value: &str, depth: usize, out: &mut Vec<(TokKind, Arc<str>)>) {
        let (toks, _) = lex(value);
        for t in toks {
            let txt = &value[t.start..t.end];
            match t.kind {
                TokKind::Comment | TokKind::Preproc => continue,
                TokKind::Include if depth < 16 => {
                    if let Some((_, v)) =
                        self.ref_value(&txt[1..txt.len().saturating_sub(1).max(1)])
                    {
                        self.expand_tokens(&v, depth + 1, out);
                        continue;
                    }
                }
                _ => {}
            }
            out.push((t.kind, Arc::from(txt)));
        }
    }

    /// `s` with `{&name}` / `{1}` references replaced by their values (unknown ones by
    /// nothing). The flag is set when an unknown name was replaced.
    fn expand_text(&self, s: &str, depth: usize) -> (String, bool) {
        let mut out = String::new();
        let mut unknown = false;
        let mut rest = s;
        while let Some(p) = rest.find('{') {
            out.push_str(&rest[..p]);
            let after = &rest[p..];
            let mut d = 0;
            let mut close = None;
            for (k, ch) in after.char_indices() {
                match ch {
                    '{' => d += 1,
                    '}' => {
                        d -= 1;
                        if d == 0 {
                            close = Some(k);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(close) = close else {
                out.push_str(after);
                return (out, unknown);
            };
            let body = &after[1..close];
            match self.ref_value(body) {
                Some((_, v)) if depth < 16 => {
                    let (e, u) = self.expand_text(&v, depth + 1);
                    out.push_str(&e);
                    unknown |= u;
                }
                _ if body.trim_start().starts_with('&') => unknown = true,
                _ => out.push_str(&after[..=close]),
            }
            rest = &after[close + 1..];
        }
        out.push_str(rest);
        (out, unknown)
    }

    // ---- &IF conditions ------------------------------------------------------

    /// `None` when the condition cannot be evaluated.
    fn eval_cond(&self, cond: &str) -> Option<bool> {
        let (text, unknown) = self.expand_text(cond, 0);
        if unknown && self.unresolved {
            return None;
        }
        let toks = cond_tokens(&text)?;
        let mut p = CondParser {
            toks: &toks,
            i: 0,
            lx: self,
        };
        let v = p.or()?;
        if p.i != toks.len() {
            return None;
        }
        v.truthy()
    }
}

// ---- &IF expression evaluator ------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum CTok {
    Str(String),
    Num(String),
    Word(String),
    Op(String),
}

fn cond_tokens(s: &str) -> Option<Vec<CTok>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if is_ws(c) {
            i += 1;
        } else if c == b'"' || c == b'\'' {
            let mut v = String::new();
            i += 1;
            loop {
                if i >= b.len() {
                    return None;
                }
                if b[i] == b'~' && i + 1 < b.len() {
                    v.push(b[i + 1] as char);
                    i += 2;
                } else if b[i] == c {
                    if b.get(i + 1) == Some(&c) {
                        v.push(c as char);
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    let ch = s[i..].chars().next()?;
                    v.push(ch);
                    i += ch.len_utf8();
                }
            }
            // "x":U
            if b.get(i) == Some(&b':') && b.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic()) {
                i += 1;
                while i < b.len() && b[i].is_ascii_alphanumeric() {
                    i += 1;
                }
            }
            out.push(CTok::Str(v));
        } else if c.is_ascii_digit() {
            let s0 = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            out.push(CTok::Num(s[s0..i].to_string()));
        } else if is_word_start(c) {
            let s0 = i;
            while i < b.len() && is_word_char(b[i]) {
                i += 1;
            }
            out.push(CTok::Word(s[s0..i].to_ascii_uppercase()));
        } else {
            let two = s.get(i..i + 2).unwrap_or("");
            let op = if matches!(two, "<>" | "<=" | ">=") {
                two
            } else {
                &s[i..i + 1]
            };
            i += op.len();
            out.push(CTok::Op(op.to_string()));
        }
    }
    Some(out)
}

#[derive(Clone, Debug)]
enum Val {
    S(String),
    N(f64),
    B(bool),
}

impl Val {
    fn truthy(&self) -> Option<bool> {
        match self {
            Val::B(b) => Some(*b),
            Val::N(n) => Some(*n != 0.0),
            Val::S(_) => None,
        }
    }

    fn num(&self) -> Option<f64> {
        match self {
            Val::N(n) => Some(*n),
            Val::S(s) => s.trim().parse().ok(),
            Val::B(_) => None,
        }
    }

    fn string(&self) -> String {
        match self {
            Val::S(s) => s.clone(),
            Val::N(n) if n.fract() == 0.0 => format!("{}", *n as i64),
            Val::N(n) => n.to_string(),
            Val::B(b) => {
                if *b {
                    "yes".into()
                } else {
                    "no".into()
                }
            }
        }
    }
}

struct CondParser<'t, 'l, 's, 'e> {
    toks: &'t [CTok],
    i: usize,
    lx: &'l Lx<'s, 'e>,
}

impl CondParser<'_, '_, '_, '_> {
    fn peek_word(&self) -> Option<&str> {
        match self.toks.get(self.i) {
            Some(CTok::Word(w)) => Some(w),
            _ => None,
        }
    }

    fn peek_op(&self) -> Option<&str> {
        match self.toks.get(self.i) {
            Some(CTok::Op(o)) => Some(o),
            _ => None,
        }
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if self.peek_op() == Some(op) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn or(&mut self) -> Option<Val> {
        let mut l = self.and()?;
        while self.peek_word() == Some("OR") {
            self.i += 1;
            let r = self.and()?;
            l = Val::B(l.truthy()? || r.truthy()?);
        }
        Some(l)
    }

    fn and(&mut self) -> Option<Val> {
        let mut l = self.not()?;
        while self.peek_word() == Some("AND") {
            self.i += 1;
            let r = self.not()?;
            l = Val::B(l.truthy()? && r.truthy()?);
        }
        Some(l)
    }

    fn not(&mut self) -> Option<Val> {
        if self.peek_word() == Some("NOT") {
            self.i += 1;
            return Some(Val::B(!self.not()?.truthy()?));
        }
        self.cmp()
    }

    fn cmp(&mut self) -> Option<Val> {
        let l = self.add()?;
        let op = match (self.peek_op(), self.peek_word()) {
            (Some(o @ ("=" | "<>" | "<" | ">" | "<=" | ">=")), _) => o.to_string(),
            (_, Some(w @ ("EQ" | "NE" | "LT" | "GT" | "LE" | "GE" | "BEGINS" | "MATCHES"))) => {
                w.to_string()
            }
            _ => return Some(l),
        };
        self.i += 1;
        let r = self.add()?;
        let ord = match (&l, &r) {
            (Val::S(a), Val::S(b)) => {
                let (a, b) = (
                    a.trim_end().to_ascii_lowercase(),
                    b.trim_end().to_ascii_lowercase(),
                );
                match op.as_str() {
                    "BEGINS" => return Some(Val::B(a.starts_with(&b))),
                    "MATCHES" => return Some(Val::B(matches_pattern(&a, &b))),
                    _ => a.cmp(&b),
                }
            }
            (Val::B(a), Val::B(b)) => a.cmp(b),
            _ => l.num()?.partial_cmp(&r.num()?)?,
        };
        use std::cmp::Ordering::*;
        Some(Val::B(match op.as_str() {
            "=" | "EQ" => ord == Equal,
            "<>" | "NE" => ord != Equal,
            "<" | "LT" => ord == Less,
            ">" | "GT" => ord == Greater,
            "<=" | "LE" => ord != Greater,
            ">=" | "GE" => ord != Less,
            _ => return None,
        }))
    }

    fn add(&mut self) -> Option<Val> {
        let mut l = self.mul()?;
        loop {
            if self.eat_op("+") {
                let r = self.mul()?;
                l = match (&l, &r) {
                    (Val::S(a), Val::S(b)) => Val::S(format!("{a}{b}")),
                    _ => Val::N(l.num()? + r.num()?),
                };
            } else if self.eat_op("-") {
                let r = self.mul()?;
                l = Val::N(l.num()? - r.num()?);
            } else {
                return Some(l);
            }
        }
    }

    fn mul(&mut self) -> Option<Val> {
        let mut l = self.unary()?;
        loop {
            if self.eat_op("*") {
                l = Val::N(l.num()? * self.unary()?.num()?);
            } else if self.eat_op("/") {
                l = Val::N(l.num()? / self.unary()?.num()?);
            } else {
                return Some(l);
            }
        }
    }

    fn unary(&mut self) -> Option<Val> {
        if self.eat_op("-") {
            return Some(Val::N(-self.unary()?.num()?));
        }
        self.primary()
    }

    fn args(&mut self) -> Option<Vec<Val>> {
        let mut out = Vec::new();
        if !self.eat_op("(") {
            return Some(out);
        }
        if self.eat_op(")") {
            return Some(out);
        }
        loop {
            out.push(self.or()?);
            if self.eat_op(")") {
                return Some(out);
            }
            if !self.eat_op(",") {
                return None;
            }
        }
    }

    fn primary(&mut self) -> Option<Val> {
        let t = self.toks.get(self.i)?.clone();
        self.i += 1;
        match t {
            CTok::Str(s) => Some(Val::S(s)),
            CTok::Num(n) => n.parse().ok().map(Val::N),
            CTok::Op(o) if o == "(" => {
                let v = self.or()?;
                self.eat_op(")").then_some(v)
            }
            CTok::Op(_) => None,
            CTok::Word(w) => match w.as_str() {
                "TRUE" | "YES" => Some(Val::B(true)),
                "FALSE" | "NO" => Some(Val::B(false)),
                "DEFINED" => {
                    if !self.eat_op("(") {
                        return None;
                    }
                    let name = match self.toks.get(self.i)? {
                        CTok::Word(n) => n.clone(),
                        _ => return None,
                    };
                    self.i += 1;
                    if !self.eat_op(")") {
                        return None;
                    }
                    let lx = self.lx;
                    Some(Val::N(match lx.defs.get(&name).map(|d| d.0) {
                        Some(DefKind::Global) => 1.0,
                        Some(DefKind::Scoped) => 2.0,
                        Some(DefKind::Arg) => 3.0,
                        None if lx.unresolved => return None,
                        None => 0.0,
                    }))
                }
                _ => {
                    let a = self.args()?;
                    let s = |k: usize| a.get(k).map(Val::string);
                    let n = |k: usize| a.get(k).and_then(Val::num);
                    Some(match w.as_str() {
                        "PROVERSION" => Val::S("12.8".into()),
                        "OPSYS" => Val::S(builtin_define("OPSYS")?.into()),
                        "PROCESS-ARCHITECTURE" => Val::N(64.0),
                        "INTEGER" | "INT" | "INT64" => Val::N(n(0)?.round()),
                        "DECIMAL" | "DEC" => Val::N(n(0)?),
                        "STRING" => Val::S(s(0)?),
                        "LENGTH" => Val::N(s(0)?.chars().count() as f64),
                        "TRIM" => Val::S(s(0)?.trim().to_string()),
                        "CAPS" => Val::S(s(0)?.to_ascii_uppercase()),
                        "LC" => Val::S(s(0)?.to_ascii_lowercase()),
                        "INDEX" => Val::N(
                            s(0)?
                                .to_ascii_lowercase()
                                .find(&s(1)?.to_ascii_lowercase())
                                .map_or(0.0, |p| (p + 1) as f64),
                        ),
                        "SUBSTRING" => {
                            let src = s(0)?;
                            let from = (n(1)? as usize).max(1) - 1;
                            let len = n(2).map(|l| l as usize).unwrap_or(usize::MAX);
                            Val::S(src.chars().skip(from).take(len).collect())
                        }
                        "NUM-ENTRIES" => {
                            Val::N(s(0)?.split(s(1).as_deref().unwrap_or(",")).count() as f64)
                        }
                        "ENTRY" => {
                            let k = n(0)? as usize;
                            Val::S(
                                s(1)?
                                    .split(s(2).as_deref().unwrap_or(","))
                                    .nth(k.checked_sub(1)?)?
                                    .to_string(),
                            )
                        }
                        "LOOKUP" => {
                            let needle = s(0)?.to_ascii_lowercase();
                            let list = s(1)?;
                            let p = list
                                .split(s(2).as_deref().unwrap_or(","))
                                .position(|e| e.to_ascii_lowercase() == needle);
                            Val::N(p.map_or(0.0, |p| (p + 1) as f64))
                        }
                        _ => return None,
                    })
                }
            },
        }
    }
}

/// ABL MATCHES: `*` any run of characters, `.` any one character.
fn matches_pattern(s: &str, p: &str) -> bool {
    let (s, p): (Vec<char>, Vec<char>) = (s.chars().collect(), p.chars().collect());
    fn go(s: &[char], p: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => (0..=s.len()).any(|k| go(&s[k..], &p[1..])),
            Some('.') => !s.is_empty() && go(&s[1..], &p[1..]),
            Some(c) => s.first() == Some(c) && go(&s[1..], &p[1..]),
        }
    }
    go(&s, &p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(TokKind, String)> {
        lex(src)
            .0
            .into_iter()
            .map(|t| (t.kind, src[t.start..t.end].to_string()))
            .collect()
    }

    #[test]
    fn periods_colons_and_members() {
        let k = kinds("DO:\n  obj:Method(cust.name).\nEND.");
        let expect = [
            (TokKind::Word, "DO"),
            (TokKind::Colon, ":"),
            (TokKind::Word, "obj"),
            (TokKind::Member, ":"),
            (TokKind::Word, "Method"),
            (TokKind::LParen, "("),
            (TokKind::Word, "cust.name"),
            (TokKind::RParen, ")"),
            (TokKind::Period, "."),
            (TokKind::Word, "END"),
            (TokKind::Period, "."),
        ];
        assert_eq!(k.len(), expect.len());
        for (got, (kind, text)) in k.iter().zip(expect) {
            assert_eq!(got.0, kind);
            assert_eq!(got.1, text);
        }
    }

    #[test]
    fn hyphenated_words_strings_and_nested_comments() {
        let k = kinds("x-y = \"a~\"b\":U /* outer /* inner */ still */ 1.5.");
        assert_eq!(k[0], (TokKind::Word, "x-y".into()));
        assert_eq!(k[2], (TokKind::String, "\"a~\"b\":U".into()));
        assert_eq!(k[3].0, TokKind::Comment);
        assert_eq!(k[4], (TokKind::Number, "1.5".into()));
        assert_eq!(k[5].0, TokKind::Period);
    }

    fn active_words(src: &str, env: &PreEnv) -> Vec<String> {
        let out = lex_env(src, env);
        out.tokens
            .iter()
            .filter(|t| t.kind == TokKind::Word && !t.inactive)
            .map(|t| t.text(src).to_string())
            .collect()
    }

    #[test]
    fn preprocessor_branches() {
        // DEFINED(X) is 0: the &ELSE branch is taken.
        let src = "&IF DEFINED(X) &THEN\na.\n&ELSE\nb.\n&ENDIF\nc.";
        let (_, errs) = lex(src);
        assert!(errs.is_empty());
        assert_eq!(active_words(src, &PreEnv::default()), vec!["b", "c"]);
        let src = "&GLOBAL-DEFINE X yes\n&IF DEFINED(X) &THEN\na.\n&ELSE\nb.\n&ENDIF\nc.";
        assert_eq!(active_words(src, &PreEnv::default()), vec!["a", "c"]);
    }

    #[test]
    fn conditions() {
        let env = PreEnv::default();
        let src = "&SCOPED-DEFINE LEVEL 3\n\
                   &IF {&LEVEL} > 5 &THEN big. &ELSEIF {&LEVEL} >= 3 AND \"{&OPSYS}\" <> \"\" &THEN mid. &ELSE small. &ENDIF\n\
                   &IF PROVERSION >= \"11\" AND NOT DEFINED(NOPE) = 1 &THEN new. &ENDIF\n\
                   &IF SOMETHING-ODD(1) &THEN first. &ELSE second. &ENDIF";
        assert_eq!(active_words(src, &env), vec!["mid", "new", "first"]);
        let src = "&IF DEFINED(G) = 0 &THEN\n&GLOBAL-DEFINE G 1\nguard.\n&ENDIF\n&IF DEFINED(G) = 0 &THEN twice. &ENDIF";
        assert_eq!(active_words(src, &env), vec!["guard"]);
        let src = "&GLOB TYPE \"char\"\n&IF {&TYPE} BEGINS \"CH\" AND \"abc\" MATCHES \"a*c\" &THEN ok. &ENDIF";
        assert_eq!(active_words(src, &env), vec!["ok"]);
    }

    #[test]
    fn references_expand_to_their_value() {
        let src = "&SCOPED-DEFINE T INTEGER\n&SCOPED-DEFINE DECL DEFINE VARIABLE {&NAME} AS {&T} NO-UNDO.\n&GLOBAL-DEFINE NAME iCount\n{&DECL}\n{&UNKNOWN}";
        let out = lex_env(src, &PreEnv::default());
        let words: Vec<&str> = out
            .tokens
            .iter()
            .filter(|t| t.kind == TokKind::Word)
            .map(|t| t.text(src))
            .collect();
        assert_eq!(
            words,
            vec!["DEFINE", "VARIABLE", "iCount", "AS", "INTEGER", "NO-UNDO"]
        );
        let decl = out.tokens.iter().find(|t| t.expanded).unwrap();
        assert_eq!(&src[decl.start..decl.end], "{&DECL}");
        assert!(out
            .tokens
            .iter()
            .any(|t| t.kind == TokKind::Include && !t.expanded && t.text(src) == "{&UNKNOWN}"));
        assert_eq!(out.expansions[0].name, "&DECL");
    }

    #[test]
    fn include_arguments() {
        let (t, p, n) = include_args("inc/tt.i ttOrder \"a b\" &prefix=o &label=\"Order No\"");
        assert_eq!(t, "inc/tt.i");
        assert_eq!(p, vec!["ttOrder", "a b"]);
        assert_eq!(
            n,
            vec![
                ("prefix".to_string(), "o".to_string()),
                ("label".to_string(), "Order No".to_string())
            ]
        );

        let resolve = |t: &str| {
            (t == "g.i")
                .then(|| "&GLOBAL-DEFINE FROM-INC yes\n&SCOPED-DEFINE LOCAL yes\n".to_string())
        };
        let env = PreEnv {
            ctx: IncludeCtx::default(),
            resolve: Some(&resolve),
        };
        let src =
            "{g.i x &k=v}\n&IF DEFINED(FROM-INC) = 1 AND DEFINED(LOCAL) = 0 &THEN seen. &ENDIF";
        let out = lex_env(src, &env);
        assert_eq!(out.includes[0].target, "g.i");
        assert_eq!(out.includes[0].ctx.positional, vec!["x"]);
        assert_eq!(
            out.includes[0].ctx.defines.get("k").map(|d| d.1.as_str()),
            Some("v")
        );
        assert_eq!(active_words(src, &env), vec!["seen"]);

        // Inside the include: {1} and {&k} come from the reference.
        let ctx = (*out.includes[0].ctx).clone();
        let env = PreEnv { ctx, resolve: None };
        assert_eq!(
            active_words("DEFINE TEMP-TABLE {1} FIELD {&k} AS INT. {2}", &env),
            vec!["DEFINE", "TEMP-TABLE", "x", "FIELD", "v", "AS", "INT"]
        );
    }
}

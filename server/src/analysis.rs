//! Statement-level parser and symbol table for OpenEdge ABL.
//!
//! ABL code is a sequence of statements ending in a period. Statements ending
//! in a colon (`DO:`, `PROCEDURE x:`, `FOR EACH ...:`) open a block that an
//! `END` statement closes. The parser does not build expression trees: it
//! finds statements, tracks block structure, and records the definitions it
//! recognises (DEFINE statements, procedures, functions, classes, methods...).
//! That is enough for outline, navigation, hover, completion and structural
//! diagnostics, and it stays tolerant of code it does not understand.

use crate::keywords::{keyword, type_name};
use std::sync::Arc;

use crate::lexer::{lex_env, Expansion, IncludeCtx, PreEnv, TokKind, Token};
use crate::line_index::LineIndex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SymKind {
    Procedure,
    Function,
    Method,
    Constructor,
    Destructor,
    Class,
    Interface,
    Enum,
    EnumMember,
    Property,
    Event,
    Variable,
    Parameter,
    Buffer,
    TempTable,
    Field,
    Index,
    Query,
    Dataset,
    DataSource,
    Frame,
    Stream,
    Widget,
    Trigger,
    Preprocessor,
    /// Database table from a .df schema file.
    Table,
    /// DATA-RELATION of a dataset.
    Relation,
}

impl SymKind {
    pub fn label(self) -> &'static str {
        use SymKind::*;
        match self {
            Procedure => "procedure",
            Function => "function",
            Method => "method",
            Constructor => "constructor",
            Destructor => "destructor",
            Class => "class",
            Interface => "interface",
            Enum => "enum",
            EnumMember => "enum member",
            Property => "property",
            Event => "event",
            Variable => "variable",
            Parameter => "parameter",
            Buffer => "buffer",
            TempTable => "temp-table",
            Field => "field",
            Index => "index",
            Query => "query",
            Dataset => "dataset",
            DataSource => "data-source",
            Frame => "frame",
            Stream => "stream",
            Widget => "widget",
            Trigger => "trigger",
            Preprocessor => "preprocessor name",
            Table => "database table",
            Relation => "data-relation",
        }
    }

    /// Symbols that are only reachable through their parent (`tt.field`, `Enum:Member`).
    pub fn is_nested(self) -> bool {
        matches!(
            self,
            SymKind::Field
                | SymKind::Index
                | SymKind::EnumMember
                | SymKind::Trigger
                | SymKind::Relation
        )
    }
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub name: String,
    pub kind: SymKind,
    /// Short description: type, `RETURNS x`, `FOR table`...
    pub detail: String,
    /// Class name for class-typed variables, table name for buffers, LIKE target for temp-tables.
    pub type_name: Option<String>,
    /// Declaration source text, shown on hover.
    pub decl: String,
    pub start: usize,
    pub end: usize,
    pub sel_start: usize,
    pub sel_end: usize,
    /// Scope in which the symbol is visible.
    pub scope: usize,
    /// Enclosing symbol, for the outline.
    pub parent: Option<usize>,
    pub access: Option<&'static str>,
    pub is_static: bool,
    /// FUNCTION ... FORWARD / IN prototypes.
    pub proto: bool,
    /// METHOD ... OVERRIDE: the signature is dictated by the superclass.
    pub is_override: bool,
}

/// A routine-level scope. Scope 0 is the file (main block or class body).
/// ABL scopes variables to procedures/functions/methods, not to DO blocks.
#[derive(Clone, Debug)]
pub struct Scope {
    pub start: usize,
    pub end: usize,
    pub parent: usize,
    /// False for routines without a body (prototypes, interface/abstract methods).
    pub body: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub start: usize,
    pub end: usize,
    pub message: String,
    pub severity: Severity,
}

/// `{include.i}` or `{&NAME}` reference.
#[derive(Clone, Debug)]
pub struct FileRef {
    pub start: usize,
    pub end: usize,
    pub target: String,
    /// For an include: the names defined where it is included, plus its arguments.
    pub ctx: Option<Arc<IncludeCtx>>,
}

#[derive(Clone, Debug)]
pub struct RunRef {
    pub start: usize,
    pub end: usize,
    pub target: String,
    /// `RUN x.p` (external file) rather than `RUN internalProc`.
    pub is_file: bool,
}

/// One statement: token indexes of its first and last token (terminator excluded).
/// Tokens in between may include comments; use [`Analysis::stmt_tokens`].
#[derive(Clone, Debug)]
pub struct Stmt {
    pub first: usize,
    pub last: usize,
    /// The terminating `.` or `:` token, if any.
    pub term: Option<usize>,
    /// Ends in `:` (block header).
    pub header: bool,
    /// Blocks open around the statement (an END counts at the depth of its header).
    pub depth: usize,
}

pub struct Analysis {
    pub text: String,
    pub lines: LineIndex,
    pub tokens: Vec<Token>,
    pub symbols: Vec<Symbol>,
    pub scopes: Vec<Scope>,
    pub diags: Vec<Diag>,
    pub includes: Vec<FileRef>,
    pub preproc_refs: Vec<FileRef>,
    pub runs: Vec<RunRef>,
    pub usings: Vec<String>,
    pub class_name: Option<String>,
    pub inherits: Option<String>,
    pub implements: Vec<String>,
    /// Byte ranges of every block (header start .. END), for folding.
    pub blocks: Vec<(usize, usize)>,
    pub statements: Vec<Stmt>,
    /// Preprocessor references that were replaced by their value.
    pub expansions: Vec<Expansion>,
    /// Resolved include files, filled on first use by `Workspace::includes_of`.
    pub include_cache: std::sync::OnceLock<Vec<(std::path::PathBuf, Arc<Analysis>)>>,
}

impl Analysis {
    /// Significant token indexes of a statement.
    pub fn stmt_tokens<'s>(&'s self, st: &Stmt) -> impl Iterator<Item = usize> + 's {
        (st.first..=st.last).filter(|&i| self.is_significant(i))
    }

    /// Full keyword of a word token, if it is one.
    pub fn kw(&self, i: usize) -> Option<&'static str> {
        (self.tokens[i].kind == TokKind::Word)
            .then(|| keyword(self.tok_text(i)))
            .flatten()
    }

    /// An analysis with no code, used for non-ABL sources such as .df schema files.
    pub fn empty(text: String) -> Self {
        let lines = LineIndex::new(&text);
        let len = text.len();
        Analysis {
            text,
            lines,
            tokens: Vec::new(),
            symbols: Vec::new(),
            scopes: vec![Scope {
                start: 0,
                end: len,
                parent: 0,
                body: true,
            }],
            diags: Vec::new(),
            includes: Vec::new(),
            preproc_refs: Vec::new(),
            runs: Vec::new(),
            usings: Vec::new(),
            class_name: None,
            inherits: None,
            implements: Vec::new(),
            blocks: Vec::new(),
            statements: Vec::new(),
            expansions: Vec::new(),
            include_cache: Default::default(),
        }
    }

    pub fn tok_text(&self, i: usize) -> &str {
        self.tokens[i].text(&self.text)
    }

    pub fn is_significant(&self, i: usize) -> bool {
        let t = &self.tokens[i];
        !t.inactive && !t.expanded && !matches!(t.kind, TokKind::Comment | TokKind::Preproc)
    }

    pub fn prev_sig(&self, i: usize) -> Option<usize> {
        (0..i).rev().find(|&j| self.is_significant(j))
    }

    /// Index of the active word token touching `off` (cursor inside or just after it).
    pub fn word_at(&self, off: usize) -> Option<usize> {
        let idx = self.tokens.partition_point(|t| t.start <= off);
        if idx == 0 {
            return None;
        }
        let i = idx - 1;
        let t = &self.tokens[i];
        (t.kind == TokKind::Word && !t.inactive && off <= t.end).then_some(i)
    }

    pub fn scope_at(&self, off: usize) -> usize {
        let mut best = 0;
        let mut best_len = usize::MAX;
        for (i, s) in self.scopes.iter().enumerate().skip(1) {
            if s.start <= off && off <= s.end && s.end - s.start < best_len {
                best = i;
                best_len = s.end - s.start;
            }
        }
        best
    }

    /// Innermost scope first, ending with the file scope 0.
    pub fn scope_chain(&self, off: usize) -> Vec<usize> {
        let mut chain = Vec::new();
        let mut s = self.scope_at(off);
        loop {
            chain.push(s);
            if s == 0 {
                break;
            }
            s = self.scopes[s].parent;
        }
        chain
    }

    /// Superclass and implemented interfaces.
    pub fn supertypes(&self) -> Vec<String> {
        self.inherits
            .iter()
            .cloned()
            .chain(self.implements.iter().cloned())
            .collect()
    }
}

pub fn analyze(text: String, is_include: bool) -> Analysis {
    analyze_env(text, is_include, &PreEnv::default())
}

/// Analysis with preprocessor names and include files (see [`PreEnv`]).
pub fn analyze_env(text: String, is_include: bool, env: &PreEnv) -> Analysis {
    let lines = LineIndex::new(&text);
    let lexed = lex_env(&text, env);
    let tokens = lexed.tokens;
    let lex_errors = lexed.errors;
    let mut p = Parser {
        src: &text,
        toks: &tokens,
        lines: &lines,
        is_include,
        symbols: Vec::new(),
        scopes: vec![Scope {
            start: 0,
            end: text.len(),
            parent: 0,
            body: true,
        }],
        diags: Vec::new(),
        runs: Vec::new(),
        usings: Vec::new(),
        class_name: None,
        inherits: None,
        implements: Vec::new(),
        stack: Vec::new(),
        blocks: Vec::new(),
        statements: Vec::new(),
    };
    for e in lex_errors {
        p.diag(e.start, e.end, e.message, Severity::Error);
    }
    p.preprocessor_defines();
    p.run();
    let preproc_refs = p.preproc_refs();
    let includes = lexed
        .includes
        .into_iter()
        .map(|s| FileRef {
            start: s.start,
            end: s.end,
            target: s.target,
            ctx: Some(s.ctx),
        })
        .collect();
    let Parser {
        symbols,
        scopes,
        diags,
        runs,
        usings,
        class_name,
        inherits,
        implements,
        blocks,
        statements,
        ..
    } = p;
    Analysis {
        text,
        lines,
        tokens,
        symbols,
        scopes,
        diags,
        includes,
        preproc_refs,
        runs,
        usings,
        class_name,
        inherits,
        implements,
        blocks,
        statements,
        expansions: lexed.expansions,
        include_cache: Default::default(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockKind {
    Procedure,
    Function,
    Method,
    Constructor,
    Destructor,
    Class,
    Interface,
    Enum,
    Case,
    Catch,
    Finally,
    Get,
    Set,
    Triggers,
    /// DO / FOR / REPEAT / ON ... DO / IF ... THEN DO and friends: closed by plain END.
    Do,
}

impl BlockKind {
    fn end_keyword(self) -> &'static str {
        use BlockKind::*;
        match self {
            Procedure => "PROCEDURE",
            Function => "FUNCTION",
            Method => "METHOD",
            Constructor => "CONSTRUCTOR",
            Destructor => "DESTRUCTOR",
            Class => "CLASS",
            Interface => "INTERFACE",
            Enum => "ENUM",
            Case => "CASE",
            Catch => "CATCH",
            Finally => "FINALLY",
            Get => "GET",
            Set => "SET",
            Triggers => "TRIGGERS",
            Do => "",
        }
    }
}

/// Keywords that may follow END and must match the block being closed.
const END_KEYWORDS: &[&str] = &[
    "PROCEDURE",
    "FUNCTION",
    "METHOD",
    "CONSTRUCTOR",
    "DESTRUCTOR",
    "CLASS",
    "INTERFACE",
    "ENUM",
    "CASE",
    "CATCH",
    "FINALLY",
    "GET",
    "SET",
    "TRIGGERS",
];

struct Open {
    kind: BlockKind,
    label: String,
    start: usize,
    hdr_end: usize,
    sym: Option<usize>,
    scope: Option<usize>,
}

struct TypeInfo {
    found: bool,
    detail: String,
    type_name: Option<String>,
}

struct Parser<'a> {
    src: &'a str,
    toks: &'a [Token],
    lines: &'a LineIndex,
    is_include: bool,
    symbols: Vec<Symbol>,
    scopes: Vec<Scope>,
    diags: Vec<Diag>,
    runs: Vec<RunRef>,
    usings: Vec<String>,
    class_name: Option<String>,
    inherits: Option<String>,
    implements: Vec<String>,
    stack: Vec<Open>,
    blocks: Vec<(usize, usize)>,
    statements: Vec<Stmt>,
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Names that share a namespace for duplicate-definition checks.
fn namespace(k: SymKind) -> Option<u8> {
    match k {
        SymKind::Variable | SymKind::Parameter | SymKind::Property => Some(1),
        SymKind::TempTable | SymKind::Buffer => Some(2),
        SymKind::Field => Some(3),
        _ => None,
    }
}

impl<'a> Parser<'a> {
    // ---- token helpers -------------------------------------------------

    fn text(&self, ti: usize) -> &'a str {
        self.toks[ti].text(self.src)
    }

    fn kind(&self, ti: usize) -> TokKind {
        self.toks[ti].kind
    }

    fn kw(&self, ti: usize) -> Option<&'static str> {
        if self.toks[ti].kind == TokKind::Word {
            keyword(self.text(ti))
        } else {
            None
        }
    }

    fn kw_at(&self, s: &[usize], i: usize) -> Option<&'static str> {
        s.get(i).and_then(|&t| self.kw(t))
    }

    fn is_word(&self, s: &[usize], i: usize) -> bool {
        s.get(i)
            .is_some_and(|&t| self.toks[t].kind == TokKind::Word)
    }

    fn line(&self, off: usize) -> usize {
        self.lines.line_of(off) + 1
    }

    fn diag(&mut self, start: usize, end: usize, message: impl Into<String>, severity: Severity) {
        self.diags.push(Diag {
            start,
            end: end.max(start),
            message: message.into(),
            severity,
        });
    }

    fn block_severity(&self) -> Severity {
        // Include files often contain partial blocks on purpose.
        if self.is_include {
            Severity::Warning
        } else {
            Severity::Error
        }
    }

    fn cur_scope(&self) -> usize {
        self.stack.iter().rev().find_map(|o| o.scope).unwrap_or(0)
    }

    fn cur_parent(&self) -> Option<usize> {
        self.stack.iter().rev().find_map(|o| o.sym)
    }

    /// Statement source with whitespace collapsed and comments dropped.
    fn stmt_text(&self, s: &[usize]) -> String {
        let mut out = String::new();
        let mut prev_end: Option<usize> = None;
        for &ti in s {
            let t = &self.toks[ti];
            if prev_end
                .is_some_and(|pe| t.start > pe || (t.subst.is_some() && !out.ends_with(['(', '['])))
            {
                out.push(' ');
            }
            out.push_str(&collapse_ws(self.text(ti)));
            prev_end = Some(t.end);
            if out.len() > 400 {
                out.push_str(" …");
                break;
            }
        }
        out
    }

    fn new_sym(&self, name_ti: usize, kind: SymKind, start: usize, end: usize) -> Symbol {
        let t = &self.toks[name_ti];
        Symbol {
            name: self.text(name_ti).to_string(),
            kind,
            detail: String::new(),
            type_name: None,
            decl: String::new(),
            start,
            end,
            sel_start: t.start,
            sel_end: t.end,
            scope: self.cur_scope(),
            parent: self.cur_parent(),
            access: None,
            is_static: false,
            proto: false,
            is_override: false,
        }
    }

    fn push_sym(&mut self, sym: Symbol) -> usize {
        if let Some(ns) = namespace(sym.kind) {
            let clash = self
                .symbols
                .iter()
                .find(|p| {
                    namespace(p.kind) == Some(ns)
                        && p.name.eq_ignore_ascii_case(&sym.name)
                        && if ns == 3 {
                            p.parent == sym.parent
                        } else {
                            p.scope == sym.scope
                        }
                })
                .map(|p| self.line(p.sel_start));
            if let Some(line) = clash {
                self.diag(
                    sym.sel_start,
                    sym.sel_end,
                    format!("'{}' is already defined on line {}", sym.name, line),
                    Severity::Error,
                );
            }
        }
        self.symbols.push(sym);
        self.symbols.len() - 1
    }

    // ---- driver --------------------------------------------------------

    fn run(&mut self) {
        let sig: Vec<usize> = (0..self.toks.len())
            .filter(|&i| {
                let t = &self.toks[i];
                !t.inactive && !t.expanded && !matches!(t.kind, TokKind::Comment | TokKind::Preproc)
            })
            .collect();

        let mut cur: Vec<usize> = Vec::new();
        for (k, &ti) in sig.iter().enumerate() {
            match self.kind(ti) {
                TokKind::Period => {
                    self.statement(&cur, Some(ti), false);
                    cur.clear();
                }
                TokKind::Colon => {
                    self.statement(&cur, Some(ti), true);
                    cur.clear();
                }
                // `{inc.i}` alone on its line is a complete statement with no period.
                TokKind::Include
                    if cur.is_empty() && self.ends_line(ti, sig.get(k + 1).copied()) => {}
                _ => cur.push(ti),
            }
        }

        if !cur.is_empty() {
            let only_includes = cur.iter().all(|&t| self.kind(t) == TokKind::Include);
            if !self.is_include && !only_includes {
                let last = self.toks[*cur.last().unwrap()].clone();
                self.diag(
                    last.start,
                    last.end,
                    "Statement is missing its terminating period",
                    Severity::Error,
                );
            }
            self.statement(&cur.clone(), None, false);
        }

        let sev = self.block_severity();
        while let Some(o) = self.stack.pop() {
            let line = self.line(o.start);
            self.diag(
                o.start,
                o.hdr_end,
                format!(
                    "{} block starting on line {} has no matching END",
                    o.label, line
                ),
                sev,
            );
            self.finish(&o, self.src.len());
        }
    }

    fn ends_line(&self, ti: usize, next: Option<usize>) -> bool {
        match next {
            None => true,
            Some(n) => self.src[self.toks[ti].end..self.toks[n].start].contains('\n'),
        }
    }

    fn statement(&mut self, s: &[usize], term: Option<usize>, is_header: bool) {
        if s.is_empty() {
            return;
        }
        self.statements.push(Stmt {
            first: s[0],
            last: *s.last().unwrap(),
            term,
            header: is_header,
            depth: self.stack.len(),
        });
        self.check_brackets(s);
        let start = self.toks[s[0]].start;
        let end = term
            .map(|t| self.toks[t].end)
            .unwrap_or(self.toks[*s.last().unwrap()].end);
        let first = self.kw(s[0]);

        if first == Some("END") && !is_header {
            self.close_block(s, start, end);
            let depth = self.stack.len();
            if let Some(st) = self.statements.last_mut() {
                st.depth = depth;
            }
            return;
        }
        if is_header {
            let single_word_blocks = [
                "DO",
                "REPEAT",
                "FINALLY",
                "GET",
                "SET",
                "TRIGGERS",
                "EDITING",
                "OTHERWISE",
            ];
            if s.len() == 1
                && self.kind(s[0]) == TokKind::Word
                && !first.is_some_and(|k| single_word_blocks.contains(&k))
            {
                return; // block label, e.g. `mainLoop:`
            }
            self.open_block(s, start, end);
        } else {
            self.simple_statement(s, start, end);
        }
    }

    fn finish(&mut self, o: &Open, end: usize) {
        self.blocks.push((o.start, end));
        if let Some(si) = o.sym {
            self.symbols[si].end = end;
        }
        if let Some(sc) = o.scope {
            self.scopes[sc].end = end;
        }
    }

    fn close_block(&mut self, s: &[usize], start: usize, end: usize) {
        let sev = self.block_severity();
        let end_kw = self.kw_at(s, 1).filter(|k| END_KEYWORDS.contains(k));
        if self.stack.is_empty() {
            self.diag(start, end, "END without a matching block", sev);
            return;
        }
        let Some(k) = end_kw else {
            let o = self.stack.pop().unwrap();
            self.finish(&o, end);
            return;
        };
        match self.stack.iter().rposition(|o| o.kind.end_keyword() == k) {
            Some(pos) => {
                // Blocks opened after the one this END names were never closed.
                while self.stack.len() > pos + 1 {
                    let o = self.stack.pop().unwrap();
                    let line = self.line(o.start);
                    self.diag(
                        o.start,
                        o.hdr_end,
                        format!("{} block starting on line {} has no matching END (END {} closes an outer block)", o.label, line, k),
                        sev,
                    );
                    self.finish(&o, start);
                }
                let o = self.stack.pop().unwrap();
                self.finish(&o, end);
            }
            None => {
                let o = self.stack.pop().unwrap();
                let line = self.line(o.start);
                self.diag(
                    start,
                    end,
                    format!(
                        "END {} does not match the {} block starting on line {}",
                        k, o.label, line
                    ),
                    sev,
                );
                self.finish(&o, end);
            }
        }
    }

    fn check_brackets(&mut self, s: &[usize]) {
        let mut open: Vec<usize> = Vec::new();
        for &t in s {
            match self.kind(t) {
                TokKind::LParen | TokKind::LBracket => open.push(t),
                TokKind::RParen | TokKind::RBracket => {
                    let want = if self.kind(t) == TokKind::RParen {
                        TokKind::LParen
                    } else {
                        TokKind::LBracket
                    };
                    match open.last() {
                        Some(&o) if self.kind(o) == want => {
                            open.pop();
                        }
                        _ => {
                            let tok = self.toks[t].clone();
                            self.diag(
                                tok.start,
                                tok.end,
                                format!("Unmatched '{}'", self.text(t)),
                                Severity::Error,
                            );
                            return;
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(&o) = open.last() {
            let tok = self.toks[o].clone();
            let close = if tok.kind == TokKind::LParen {
                ')'
            } else {
                ']'
            };
            self.diag(
                tok.start,
                tok.end,
                format!("'{}' is never closed with '{}'", self.text(o), close),
                Severity::Error,
            );
        }
    }

    // ---- block headers -------------------------------------------------

    fn open_block(&mut self, s: &[usize], start: usize, end: usize) {
        let first = self.kw(s[0]).unwrap_or("");
        let mut label = first.to_string();
        let (kind, sym, scope) = match first {
            "PROCEDURE" => {
                self.routine_block(s, SymKind::Procedure, BlockKind::Procedure, start, end)
            }
            "FUNCTION" => self.routine_block(s, SymKind::Function, BlockKind::Function, start, end),
            "METHOD" => self.routine_block(s, SymKind::Method, BlockKind::Method, start, end),
            "CONSTRUCTOR" => {
                self.routine_block(s, SymKind::Constructor, BlockKind::Constructor, start, end)
            }
            "DESTRUCTOR" => {
                self.routine_block(s, SymKind::Destructor, BlockKind::Destructor, start, end)
            }
            "CLASS" => (
                BlockKind::Class,
                self.type_header(s, SymKind::Class, start, end),
                None,
            ),
            "INTERFACE" => (
                BlockKind::Interface,
                self.type_header(s, SymKind::Interface, start, end),
                None,
            ),
            "ENUM" => (
                BlockKind::Enum,
                self.type_header(s, SymKind::Enum, start, end),
                None,
            ),
            "CASE" => (BlockKind::Case, None, None),
            "FINALLY" => (BlockKind::Finally, None, None),
            "TRIGGERS" => (BlockKind::Triggers, None, None),
            "CATCH" => {
                if self.is_word(s, 1) {
                    let ty = self.parse_type(s, 2, s.len());
                    let mut sym = self.new_sym(s[1], SymKind::Variable, start, end);
                    sym.detail = ty.detail;
                    sym.type_name = ty.type_name;
                    sym.decl = self.stmt_text(s);
                    self.push_sym(sym);
                }
                (BlockKind::Catch, None, None)
            }
            "DEFINE" => {
                // DEFINE PROPERTY x AS y GET(): ... END GET.
                let prop = self.define(s, start, end);
                let acc = s
                    .iter()
                    .rposition(|&t| matches!(self.kw(t), Some("GET" | "SET")));
                match acc {
                    Some(ai) => {
                        let kind = if self.kw(s[ai]) == Some("GET") {
                            BlockKind::Get
                        } else {
                            BlockKind::Set
                        };
                        label = kind.end_keyword().to_string();
                        let sc = self.new_scope(start, end);
                        if let Some(p) = prop {
                            self.params(&s[ai..], sc, p);
                        }
                        (kind, None, Some(sc))
                    }
                    None => (BlockKind::Do, None, None),
                }
            }
            "GET" | "SET" if s.len() == 1 || self.kind(s[1]) == TokKind::LParen => {
                let kind = if first == "GET" {
                    BlockKind::Get
                } else {
                    BlockKind::Set
                };
                let sc = self.new_scope(start, end);
                let owner = self
                    .symbols
                    .iter()
                    .rposition(|x| x.kind == SymKind::Property);
                if let Some(p) = owner {
                    self.params(s, sc, p);
                }
                (kind, None, Some(sc))
            }
            "ON" => {
                let stop = if self.kw(*s.last().unwrap()) == Some("DO") {
                    s.len() - 1
                } else {
                    s.len()
                };
                let mut sym = self.new_sym(s[0], SymKind::Trigger, start, end);
                let name = if stop > 1 {
                    self.stmt_text(&s[1..stop])
                } else {
                    "ON".to_string()
                };
                sym.name = name.chars().take(60).collect();
                sym.decl = self.stmt_text(s);
                let si = self.push_sym(sym);
                label = "ON".into();
                (BlockKind::Do, Some(si), None)
            }
            _ => {
                if self.kw(*s.last().unwrap()) == Some("DO") {
                    label = "DO".into();
                } else if label.is_empty() {
                    label = self.text(s[0]).to_ascii_uppercase();
                }
                (BlockKind::Do, None, None)
            }
        };
        self.stack.push(Open {
            kind,
            label,
            start,
            hdr_end: end,
            sym,
            scope,
        });
    }

    fn new_scope(&mut self, start: usize, end: usize) -> usize {
        let parent = self.cur_scope();
        self.scopes.push(Scope {
            start,
            end,
            parent,
            body: true,
        });
        self.scopes.len() - 1
    }

    fn routine_block(
        &mut self,
        s: &[usize],
        sk: SymKind,
        bk: BlockKind,
        start: usize,
        end: usize,
    ) -> (BlockKind, Option<usize>, Option<usize>) {
        match self.routine(s, sk, start, end) {
            Some((si, sc)) => (bk, Some(si), Some(sc)),
            None => (bk, None, None),
        }
    }

    /// PROCEDURE / FUNCTION / METHOD / CONSTRUCTOR / DESTRUCTOR header.
    fn routine(
        &mut self,
        s: &[usize],
        kind: SymKind,
        start: usize,
        end: usize,
    ) -> Option<(usize, usize)> {
        let mut access = None;
        let mut is_static = false;
        let mut is_override = false;
        let mut i = 1;
        if matches!(
            kind,
            SymKind::Method | SymKind::Constructor | SymKind::Destructor
        ) {
            while let Some(k) = self.kw_at(s, i) {
                match k {
                    "PUBLIC" | "PRIVATE" | "PROTECTED" | "PACKAGE-PRIVATE"
                    | "PACKAGE-PROTECTED" => access = Some(k),
                    "STATIC" => is_static = true,
                    "OVERRIDE" => is_override = true,
                    "ABSTRACT" | "FINAL" => {}
                    _ => break,
                }
                i += 1;
            }
        }
        let lparen = s.iter().position(|&t| self.kind(t) == TokKind::LParen);
        let (name_i, detail, tn) = match kind {
            SymKind::Procedure => {
                let flags: Vec<&str> = s[1..]
                    .iter()
                    .filter_map(|&t| self.kw(t))
                    .filter(|k| matches!(*k, "PRIVATE" | "EXTERNAL" | "PERSISTENT"))
                    .collect();
                (1, flags.join(" "), None)
            }
            SymKind::Function => {
                let mut j = 2;
                if self.kw_at(s, j) == Some("RETURNS") {
                    j += 1;
                }
                if self.kw_at(s, j) == Some("CLASS") {
                    j += 1;
                }
                let (ret, tn) = match s.get(j) {
                    Some(&t) if self.kind(t) == TokKind::Word => self.type_of(self.text(t)),
                    _ => (String::new(), None),
                };
                (
                    1,
                    if ret.is_empty() {
                        String::new()
                    } else {
                        format!("RETURNS {ret}")
                    },
                    tn,
                )
            }
            SymKind::Method => {
                let ni = match lparen {
                    Some(p) if p > i => p - 1,
                    _ => s.len() - 1,
                };
                let mut parts = Vec::new();
                let mut tn = None;
                for &t in &s[i.min(ni)..ni] {
                    if self.kw(t) == Some("CLASS") {
                        continue;
                    }
                    let (d, n) = self.type_of(self.text(t));
                    tn = tn.or(n);
                    parts.push(d);
                }
                let mut detail = parts.join(" ");
                if let Some(a) = access {
                    detail = format!("{a} {detail}");
                }
                if is_static {
                    detail = format!("STATIC {detail}");
                }
                (ni, detail.trim().to_string(), tn)
            }
            _ => {
                let ni = match lparen {
                    Some(p) if p > i => p - 1,
                    _ => i.min(s.len() - 1),
                };
                (ni, access.unwrap_or("").to_string(), None)
            }
        };
        if !self.is_word(s, name_i) {
            self.diag(
                start,
                end,
                format!("Expected a name after {}", kind.label().to_uppercase()),
                Severity::Error,
            );
            return None;
        }
        let mut sym = self.new_sym(s[name_i], kind, start, end);
        sym.detail = detail;
        sym.type_name = tn;
        sym.decl = self.stmt_text(s);
        sym.access = access;
        sym.is_static = is_static;
        sym.is_override = is_override;
        let si = self.push_sym(sym);
        let sc = self.new_scope(start, end);
        self.params(s, sc, si);
        Some((si, sc))
    }

    /// CLASS / INTERFACE / ENUM header.
    fn type_header(
        &mut self,
        s: &[usize],
        kind: SymKind,
        start: usize,
        end: usize,
    ) -> Option<usize> {
        if !self.is_word(s, 1) {
            self.diag(
                start,
                end,
                format!("Expected a name after {}", kind.label().to_uppercase()),
                Severity::Error,
            );
            return None;
        }
        let name = self.text(s[1]).to_string();
        let mut inherits = None;
        let mut implements = Vec::new();
        let mut j = 2;
        while j < s.len() {
            match self.kw(s[j]) {
                Some("INHERITS") => {
                    if self.is_word(s, j + 1) {
                        inherits = Some(self.text(s[j + 1]).to_string());
                    }
                    j += 2;
                    // interfaces may inherit several interfaces
                    while j + 1 < s.len()
                        && self.kind(s[j]) == TokKind::Comma
                        && self.is_word(s, j + 1)
                    {
                        implements.push(self.text(s[j + 1]).to_string());
                        j += 2;
                    }
                }
                Some("IMPLEMENTS") => {
                    j += 1;
                    while self.is_word(s, j) {
                        implements.push(self.text(s[j]).to_string());
                        j += 1;
                        if j < s.len() && self.kind(s[j]) == TokKind::Comma {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                }
                _ => j += 1,
            }
        }
        let mut detail = Vec::new();
        if let Some(i) = &inherits {
            detail.push(format!("INHERITS {i}"));
        }
        if !implements.is_empty() {
            detail.push(format!("IMPLEMENTS {}", implements.join(", ")));
        }
        if self.class_name.is_none() {
            self.class_name = Some(name);
            self.inherits = inherits;
            self.implements = implements;
        }
        let mut sym = self.new_sym(s[1], kind, start, end);
        sym.detail = detail.join(" ");
        sym.decl = self.stmt_text(s);
        Some(self.push_sym(sym))
    }

    // ---- plain statements ----------------------------------------------

    fn simple_statement(&mut self, s: &[usize], start: usize, end: usize) {
        match self.kw(s[0]) {
            Some("DEFINE") => {
                self.define(s, start, end);
            }
            Some("FUNCTION") => {
                // FUNCTION f RETURNS x (...) FORWARD. / IN SUPER. / IN hProc.
                if let Some((si, sc)) = self.routine(s, SymKind::Function, start, end) {
                    self.symbols[si].proto = true;
                    self.scopes[sc].body = false;
                }
            }
            Some("METHOD") => {
                // Interface or abstract method: no body.
                if let Some((_, sc)) = self.routine(s, SymKind::Method, start, end) {
                    self.scopes[sc].body = false;
                }
            }
            Some("USING") => {
                if let Some(t) = self.raw_target(s) {
                    self.usings.push(t.1);
                }
            }
            Some("RUN") => {
                if let Some((tstart, target)) = self.raw_target(s) {
                    let upper = target.to_ascii_uppercase();
                    if !upper.starts_with("VALUE") && !target.starts_with('{') {
                        let lower = target.to_ascii_lowercase();
                        let is_file = target.contains('/')
                            || target.contains('\\')
                            || [".p", ".w", ".r", ".i"].iter().any(|e| lower.ends_with(e));
                        self.runs.push(RunRef {
                            start: tstart,
                            end: tstart + target.len(),
                            target,
                            is_file,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    /// The raw source text after the first word, up to whitespace or `(`.
    /// Used for RUN and USING targets, which may contain `/`, `\` and `*`.
    fn raw_target(&self, s: &[usize]) -> Option<(usize, String)> {
        let &t1 = s.get(1)?;
        let start = self.toks[t1].start;
        let rest = &self.src[start..];
        let len = rest
            .find(|c: char| c.is_whitespace() || c == '(')
            .unwrap_or(rest.len());
        let target = rest[..len].trim_end_matches('.');
        (!target.is_empty()).then(|| (start, target.to_string()))
    }

    /// Display form of a type word plus the class name when it is not built in.
    fn type_of(&self, raw: &str) -> (String, Option<String>) {
        match type_name(raw) {
            Some(b) => (b.to_string(), None),
            None => match keyword(raw) {
                Some(k) => (k.to_string(), None),
                None => (raw.to_string(), Some(raw.to_string())),
            },
        }
    }

    fn parse_type(&self, s: &[usize], from: usize, to: usize) -> TypeInfo {
        let to = to.min(s.len());
        let mut i = from;
        while i < to {
            match self.kw(s[i]) {
                Some("AS") => {
                    let mut j = i + 1;
                    if j < to && self.kw(s[j]) == Some("CLASS") {
                        j += 1;
                    }
                    if j < to {
                        let (mut detail, tn) = self.type_of(self.text(s[j]));
                        if s[j + 1..to].iter().any(|&x| self.kw(x) == Some("EXTENT")) {
                            detail.push_str(" EXTENT");
                        }
                        return TypeInfo {
                            found: true,
                            detail,
                            type_name: tn,
                        };
                    }
                    return TypeInfo {
                        found: true,
                        detail: String::new(),
                        type_name: None,
                    };
                }
                Some("LIKE") => {
                    let detail = s
                        .get(i + 1)
                        .filter(|_| i + 1 < to)
                        .map(|&t| format!("LIKE {}", self.text(t)));
                    return TypeInfo {
                        found: true,
                        detail: detail.unwrap_or_default(),
                        type_name: None,
                    };
                }
                _ => {}
            }
            i += 1;
        }
        TypeInfo {
            found: false,
            detail: String::new(),
            type_name: None,
        }
    }

    /// Text of the token after keyword `kw` (skipping TEMP-TABLE), searching from `from`.
    fn after_kw(&self, s: &[usize], from: usize, kw: &str) -> Option<String> {
        let j = (from..s.len()).find(|&j| self.kw(s[j]) == Some(kw))?;
        let mut k = j + 1;
        if self.kw_at(s, k) == Some("TEMP-TABLE") {
            k += 1;
        }
        s.get(k).map(|&t| self.text(t).to_string())
    }

    fn define(&mut self, s: &[usize], start: usize, end: usize) -> Option<usize> {
        let mut i = 1;
        let mut access = None;
        let mut is_static = false;
        let mut dir = "";
        while let Some(k) = self.kw_at(s, i) {
            match k {
                "PRIVATE" | "PROTECTED" | "PUBLIC" | "PACKAGE-PRIVATE" | "PACKAGE-PROTECTED" => {
                    access = Some(k)
                }
                "STATIC" => is_static = true,
                "NEW" | "GLOBAL" | "SHARED" | "ABSTRACT" | "OVERRIDE" | "SERIALIZABLE"
                | "NON-SERIALIZABLE" | "FINAL" => {}
                "INPUT" | "OUTPUT" | "INPUT-OUTPUT" | "RETURN" => dir = k,
                _ => break,
            }
            i += 1;
        }
        let what = self.kw_at(s, i)?;
        i += 1;
        let kind = match what {
            "VARIABLE" => SymKind::Variable,
            "PARAMETER" => SymKind::Parameter,
            "TEMP-TABLE" | "WORK-TABLE" | "WORKFILE" => SymKind::TempTable,
            "BUFFER" => SymKind::Buffer,
            "QUERY" => SymKind::Query,
            "DATASET" => SymKind::Dataset,
            "DATA-SOURCE" => SymKind::DataSource,
            "FRAME" => SymKind::Frame,
            "STREAM" => SymKind::Stream,
            "EVENT" => SymKind::Event,
            "PROPERTY" => SymKind::Property,
            "BROWSE" | "BUTTON" | "IMAGE" | "MENU" | "SUB-MENU" | "RECTANGLE" => SymKind::Widget,
            "ENUM" => {
                self.enum_members(s, i);
                return None;
            }
            _ => return None,
        };

        let mut detail = String::new();
        let mut tn = None;
        let mut name_i = i;
        if kind == SymKind::Parameter {
            match self.kw_at(s, i) {
                Some("TABLE") | Some("DATASET") => return None, // refers to an existing table
                Some(k @ ("TABLE-HANDLE" | "DATASET-HANDLE")) => {
                    name_i = i + 1;
                    detail = k.to_string();
                }
                Some("BUFFER") => {
                    name_i = i + 1;
                    tn = self.after_kw(s, name_i + 1, "FOR");
                    detail = format!("BUFFER FOR {}", tn.clone().unwrap_or_default());
                }
                _ => {}
            }
        }
        if !self.is_word(s, name_i) {
            self.diag(
                start,
                end,
                format!("Expected a name after DEFINE {what}"),
                Severity::Error,
            );
            return None;
        }
        let name = self.text(s[name_i]);

        match kind {
            SymKind::Variable | SymKind::Parameter | SymKind::Property if detail.is_empty() => {
                let ty = self.parse_type(s, name_i + 1, s.len());
                if !ty.found {
                    self.diag(
                        start,
                        end,
                        format!("DEFINE {what} {name} needs an AS <type> or LIKE <field> clause"),
                        Severity::Error,
                    );
                }
                detail = ty.detail;
                tn = ty.type_name;
            }
            SymKind::Buffer => {
                tn = self.after_kw(s, name_i + 1, "FOR");
                detail = format!("FOR {}", tn.clone().unwrap_or_default());
            }
            SymKind::TempTable => {
                // Options before the first FIELD / INDEX: NO-UNDO, LIKE x, LIKE-SEQUENTIAL x, ...
                let opts_end = (name_i + 1..s.len())
                    .find(|&j| matches!(self.kw(s[j]), Some("FIELD" | "INDEX")))
                    .unwrap_or(s.len());
                let like = (name_i + 1..opts_end).find(|&j| {
                    matches!(
                        self.text(s[j]).to_ascii_uppercase().as_str(),
                        "LIKE" | "LIKE-SEQUENTIAL"
                    )
                });
                if let Some(&t) = like.and_then(|j| s.get(j + 1)) {
                    detail = format!("LIKE {}", self.text(t));
                    tn = Some(self.text(t).to_string());
                }
            }
            _ => {}
        }
        if !dir.is_empty() {
            detail = format!("{dir} {detail}").trim().to_string();
        }

        let mut sym = self.new_sym(s[name_i], kind, start, end);
        sym.detail = detail;
        sym.type_name = tn;
        sym.decl = self.stmt_text(s);
        sym.access = access;
        sym.is_static = is_static;
        let si = self.push_sym(sym);
        if kind == SymKind::TempTable {
            self.temp_table_members(s, name_i + 1, si);
            // BEFORE-TABLE bttName: a table of its own, with the same fields.
            if let Some(j) = (name_i + 1..s.len()).find(|&j| self.kw(s[j]) == Some("BEFORE-TABLE"))
            {
                if self.is_word(s, j + 1) {
                    let mut b = self.new_sym(s[j + 1], SymKind::Buffer, start, end);
                    b.type_name = Some(name.to_string());
                    b.detail = format!("BEFORE-TABLE of {name}");
                    b.decl = format!("BEFORE-TABLE {} (of {name})", self.text(s[j + 1]));
                    self.push_sym(b);
                }
            }
        }
        if kind == SymKind::Dataset {
            self.dataset_members(s, name_i + 1, si);
        }
        Some(si)
    }

    fn temp_table_members(&mut self, s: &[usize], from: usize, tt: usize) {
        let mut j = from;
        while j < s.len() {
            match self.kw(s[j]) {
                Some(k @ ("FIELD" | "INDEX")) if self.is_word(s, j + 1) => {
                    let seg_end = (j + 2..s.len())
                        .find(|&x| matches!(self.kw(s[x]), Some("FIELD" | "INDEX")))
                        .unwrap_or(s.len());
                    let start = self.toks[s[j]].start;
                    let end = self.toks[s[seg_end - 1]].end;
                    let kind = if k == "FIELD" {
                        SymKind::Field
                    } else {
                        SymKind::Index
                    };
                    let mut sym = self.new_sym(s[j + 1], kind, start, end);
                    sym.parent = Some(tt);
                    sym.decl = self.stmt_text(&s[j..seg_end]);
                    if kind == SymKind::Field {
                        let ty = self.parse_type(s, j + 2, seg_end);
                        sym.detail = ty.detail;
                        sym.type_name = ty.type_name;
                    }
                    self.push_sym(sym);
                    j = seg_end;
                }
                _ => j += 1,
            }
        }
    }

    /// `FOR a, b DATA-RELATION r FOR a, b RELATION-FIELDS (x, y) ...` of DEFINE DATASET.
    fn dataset_members(&mut self, s: &[usize], from: usize, ds: usize) {
        let up = |p: &Self, j: usize| p.text(s[j]).to_ascii_uppercase();
        let rel_starts: Vec<usize> = (from..s.len())
            .filter(|&j| up(self, j) == "DATA-RELATION")
            .collect();
        let first_rel = rel_starts.first().copied().unwrap_or(s.len());
        if let Some(f) = (from..first_rel).find(|&j| self.kw(s[j]) == Some("FOR")) {
            let buffers: Vec<&str> = (f + 1..first_rel)
                .filter(|&j| self.is_word(s, j))
                .map(|j| self.text(s[j]))
                .collect();
            self.symbols[ds].detail = format!("FOR {}", buffers.join(", "));
        }
        for (r, &j) in rel_starts.iter().enumerate() {
            let seg_end = rel_starts.get(r + 1).copied().unwrap_or(s.len());
            let Some(f) = (j + 1..seg_end).find(|&k| self.kw(s[k]) == Some("FOR")) else {
                continue;
            };
            let named = f > j + 1 && self.is_word(s, j + 1);
            let tables: Vec<&str> = (f + 1..seg_end)
                .filter(|&k| self.is_word(s, k))
                .take(2)
                .map(|k| self.text(s[k]))
                .collect();
            let fields: Vec<&str> = match (f..seg_end).find(|&k| up(self, k) == "RELATION-FIELDS") {
                Some(rf) => (rf + 1..seg_end)
                    .take_while(|&k| self.kind(s[k]) != TokKind::RParen)
                    .filter(|&k| self.is_word(s, k))
                    .map(|k| self.text(s[k]))
                    .collect(),
                None => Vec::new(),
            };
            let pairs: Vec<String> = fields.chunks(2).map(|c| c.join(" = ")).collect();
            let start = self.toks[s[j]].start;
            let end = self.toks[s[seg_end - 1]].end;
            let mut sym = self.new_sym(
                if named { s[j + 1] } else { s[j] },
                SymKind::Relation,
                start,
                end,
            );
            if !named {
                sym.name = tables.join("-");
            }
            sym.parent = Some(ds);
            sym.detail = format!(
                "{} -> {} ({})",
                tables.first().unwrap_or(&"?"),
                tables.get(1).unwrap_or(&"?"),
                pairs.join(", ")
            );
            sym.decl = self.stmt_text(&s[j..seg_end]);
            self.push_sym(sym);
        }
    }

    /// `DEFINE ENUM Red Green = 2 Blue = Red,Green.`
    fn enum_members(&mut self, s: &[usize], from: usize) {
        for j in from..s.len() {
            let t = s[j];
            if self.kind(t) != TokKind::Word {
                continue;
            }
            let prev = s[j - 1];
            if self.text(prev) == "=" || self.kind(prev) == TokKind::Comma {
                continue; // part of a value
            }
            let tok = self.toks[t].clone();
            let mut sym = self.new_sym(t, SymKind::EnumMember, tok.start, tok.end);
            sym.decl = self.text(t).to_string();
            self.push_sym(sym);
        }
    }

    /// Parameters in a `( ... )` list of a routine or accessor header.
    fn params(&mut self, s: &[usize], scope: usize, parent: usize) {
        let Some(open) = s.iter().position(|&t| self.kind(t) == TokKind::LParen) else {
            return;
        };
        let mut depth = 0;
        let mut group: Vec<usize> = Vec::new();
        for &t in &s[open..] {
            match self.kind(t) {
                TokKind::LParen => {
                    depth += 1;
                    if depth == 1 {
                        continue;
                    }
                }
                TokKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        self.param(&group, scope, parent);
                        return;
                    }
                }
                TokKind::Comma if depth == 1 => {
                    self.param(&group, scope, parent);
                    group.clear();
                    continue;
                }
                _ => {}
            }
            group.push(t);
        }
    }

    fn param(&mut self, g: &[usize], scope: usize, parent: usize) {
        if g.is_empty() {
            return;
        }
        let mut i = 0;
        let mut dir = "";
        if let Some(k @ ("INPUT" | "OUTPUT" | "INPUT-OUTPUT" | "RETURN")) = self.kw(g[0]) {
            dir = k;
            i = 1;
        }
        let (name_i, detail, tn) = match self.kw_at(g, i) {
            Some("TABLE") | Some("DATASET") => return,
            Some(k @ ("TABLE-HANDLE" | "DATASET-HANDLE")) => (i + 1, k.to_string(), None),
            Some("BUFFER") => {
                let t = self.after_kw(g, i + 2, "FOR");
                (
                    i + 1,
                    format!("BUFFER FOR {}", t.clone().unwrap_or_default()),
                    t,
                )
            }
            _ => {
                let ty = self.parse_type(g, i + 1, g.len());
                (i, ty.detail, ty.type_name)
            }
        };
        if !self.is_word(g, name_i) {
            return;
        }
        let start = self.toks[g[0]].start;
        let end = self.toks[*g.last().unwrap()].end;
        let mut sym = self.new_sym(g[name_i], SymKind::Parameter, start, end);
        sym.detail = format!("{dir} {detail}").trim().to_string();
        sym.type_name = tn;
        sym.decl = self.stmt_text(g);
        sym.scope = scope;
        sym.parent = Some(parent);
        self.push_sym(sym);
    }

    // ---- preprocessor & includes --------------------------------------

    fn preprocessor_defines(&mut self) {
        for t in self
            .toks
            .iter()
            .filter(|t| t.kind == TokKind::Preproc && !t.inactive)
        {
            let txt = &self.src[t.start + 1..t.end];
            let dir_len = txt.find(|c: char| c.is_whitespace()).unwrap_or(txt.len());
            let dir = txt[..dir_len].to_ascii_uppercase();
            let is_def = dir.len() >= 4
                && ("GLOBAL-DEFINE".starts_with(&dir) || "SCOPED-DEFINE".starts_with(&dir));
            if !is_def {
                continue;
            }
            let after = &txt[dir_len..];
            let lead = after.len() - after.trim_start().len();
            let Some(name) = after.split_whitespace().next() else {
                continue;
            };
            let name_start = t.start + 1 + dir_len + lead;
            self.symbols.push(Symbol {
                name: name.to_string(),
                kind: SymKind::Preprocessor,
                detail: if dir.starts_with('G') {
                    "global".into()
                } else {
                    "scoped".into()
                },
                type_name: None,
                decl: collapse_ws(&self.src[t.start..t.end]),
                start: t.start,
                end: t.end,
                sel_start: name_start,
                sel_end: name_start + name.len(),
                scope: 0,
                parent: None,
                access: None,
                is_static: false,
                proto: false,
                is_override: false,
            });
        }
    }

    /// `{&NAME}` references.
    fn preproc_refs(&self) -> Vec<FileRef> {
        let mut prefs = Vec::new();
        for t in self
            .toks
            .iter()
            .filter(|t| t.kind == TokKind::Include && !t.inactive && t.subst.is_none())
        {
            let raw = &self.src[t.start..t.end];
            let inner = raw.strip_prefix('{').unwrap_or(raw);
            let inner = inner.strip_suffix('}').unwrap_or(inner).trim_start();
            if let Some(rest) = inner.strip_prefix('&') {
                let name: String = rest
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != '}')
                    .collect();
                if !name.is_empty() {
                    prefs.push(FileRef {
                        start: t.start,
                        end: t.end,
                        target: name,
                        ctx: None,
                    });
                }
            }
        }
        prefs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors(a: &Analysis) -> Vec<String> {
        a.diags.iter().map(|d| d.message.clone()).collect()
    }

    fn find<'a>(a: &'a Analysis, name: &str) -> &'a Symbol {
        a.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol {name}"))
    }

    #[test]
    fn definitions_and_scopes() {
        let a = analyze(
            "DEF VAR cName AS CHAR NO-UNDO.\n\
             PROCEDURE doIt:\n  DEFINE INPUT PARAMETER piX AS INT NO-UNDO.\n  DEFINE VARIABLE cName AS CHARACTER NO-UNDO.\n  DO:\n    cName = 'x'.\n  END.\nEND PROCEDURE.\n"
                .into(),
            false,
        );
        assert!(a.diags.is_empty(), "{:?}", errors(&a));
        let outer = &a.symbols[0];
        assert_eq!(
            (outer.name.as_str(), outer.detail.as_str(), outer.scope),
            ("cName", "CHARACTER", 0)
        );
        let p = find(&a, "piX");
        assert_eq!(p.kind, SymKind::Parameter);
        assert_eq!(p.detail, "INPUT INTEGER");
        assert_ne!(p.scope, 0);
        let proc_idx = a.symbols.iter().position(|s| s.name == "doIt").unwrap();
        assert_eq!(p.parent, Some(proc_idx));
        // The inner cName shadows the outer one without a duplicate error.
        let off = a.text.find("cName = ").unwrap();
        assert_eq!(a.scope_chain(off).len(), 2);
    }

    #[test]
    fn temp_tables_and_buffers() {
        let a = analyze(
            "DEFINE TEMP-TABLE ttCust NO-UNDO\n  FIELD CustNum AS INTEGER\n  FIELD Name AS CHARACTER FORMAT \"x(30)\"\n  INDEX idx IS PRIMARY CustNum.\nDEFINE BUFFER bCust FOR ttCust.\n"
                .into(),
            false,
        );
        assert!(a.diags.is_empty(), "{:?}", errors(&a));
        let tt = a.symbols.iter().position(|s| s.name == "ttCust").unwrap();
        let fields: Vec<&str> = a
            .symbols
            .iter()
            .filter(|s| s.parent == Some(tt))
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(fields, vec!["CustNum", "Name", "idx"]);
        assert_eq!(find(&a, "bCust").type_name.as_deref(), Some("ttCust"));
    }

    #[test]
    fn classes_methods_properties() {
        let a = analyze(
            "USING app.*.\nCLASS app.Svc INHERITS Base IMPLEMENTS IA, IB:\n  DEFINE PUBLIC PROPERTY Name AS CHARACTER NO-UNDO\n    GET():\n      RETURN 'x'.\n    END GET.\n    SET(INPUT pc AS CHARACTER):\n    END SET.\n  METHOD PUBLIC Order Find (INPUT piId AS INTEGER):\n    RETURN ?.\n  END METHOD.\nEND CLASS.\n"
                .into(),
            false,
        );
        assert!(a.diags.is_empty(), "{:?}", errors(&a));
        assert_eq!(a.class_name.as_deref(), Some("app.Svc"));
        assert_eq!(a.inherits.as_deref(), Some("Base"));
        assert_eq!(a.implements, vec!["IA", "IB"]);
        assert_eq!(a.usings, vec!["app.*"]);
        let m = find(&a, "Find");
        assert_eq!(m.kind, SymKind::Method);
        assert_eq!(m.type_name.as_deref(), Some("Order"));
        assert_eq!(find(&a, "piId").kind, SymKind::Parameter);
        assert_eq!(find(&a, "pc").kind, SymKind::Parameter);
    }

    #[test]
    fn labels_includes_and_preprocessor() {
        let a = analyze(
            "&GLOBAL-DEFINE MAX 10\n{inc/defs.i}\nouter:\nREPEAT:\n  IF {&MAX} > 1 THEN LEAVE outer.\nEND.\n".into(),
            false,
        );
        assert!(a.diags.is_empty(), "{:?}", errors(&a));
        assert_eq!(a.includes[0].target, "inc/defs.i");
        assert_eq!(a.preproc_refs[0].target, "MAX");
        assert_eq!(find(&a, "MAX").kind, SymKind::Preprocessor);
    }

    #[test]
    fn run_targets() {
        let a = analyze(
            "RUN util/fmt.p (INPUT 1).\nRUN localProc.\nRUN VALUE(x).\n".into(),
            false,
        );
        let runs: Vec<(&str, bool)> = a
            .runs
            .iter()
            .map(|r| (r.target.as_str(), r.is_file))
            .collect();
        assert_eq!(runs, vec![("util/fmt.p", true), ("localProc", false)]);
    }

    #[test]
    fn structural_errors() {
        let a = analyze(
            "DEFINE VARIABLE x AS INTEGER NO-UNDO.\nDEFINE VARIABLE x AS INTEGER NO-UNDO.\nDEFINE VARIABLE y NO-UNDO.\nPROCEDURE p:\n  DO:\n    x = (1 + 2.\nEND PROCEDURE.\nEND.\n"
                .into(),
            false,
        );
        let e = errors(&a).join("\n");
        assert!(e.contains("'x' is already defined on line 1"), "{e}");
        assert!(e.contains("needs an AS"), "{e}");
        assert!(e.contains("'(' is never closed"), "{e}");
        assert!(
            e.contains("DO block starting on line 5 has no matching END"),
            "{e}"
        );
        assert!(e.contains("END without a matching block"), "{e}");
    }

    #[test]
    fn inactive_branch_is_ignored() {
        let a = analyze(
            "&IF TRUE &THEN\nPROCEDURE a:\nEND.\n&ELSE\nPROCEDURE a:\n  DO:\nEND.\n&ENDIF\n".into(),
            false,
        );
        assert!(a.diags.is_empty(), "{:?}", errors(&a));
        assert_eq!(a.symbols.iter().filter(|s| s.name == "a").count(), 1);
    }
}

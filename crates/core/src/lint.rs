//! Configurable lint rules, reported as LSP diagnostics with a rule code.
//!
//! Each rule has a name and a level (off / hint / info / warning / error), set
//! through the `abl.lint` setting. Unused-name rules carry the "unnecessary"
//! tag, so editors fade the name the way Go tooling does.
//!
//! Suppress a finding with a comment on the same line or the line above:
//!   /* lint-ignore */                    every rule
//!   // lint-ignore unused-parameter       only the listed rules
//!
//! Findings carry their quick fix in `Diagnostic::data`:
//!   { "rule": "...", "fix": { line, character, end?, text, title }?, "suppress": { line, character } }
//! `fix` replaces `line:character .. end` (an insert when `end` is missing) with `text`;
//! `suppress` is where a lint-ignore comment goes.
//!
//! Adding a rule: add it to RULES with its default level, write a `check_*`
//! function that calls `Emitter::push`, and call it from `run`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use lsp_types::{Diagnostic, DiagnosticSeverity, DiagnosticTag, NumberOrString};
use serde_json::{json, Value};

use std::collections::HashSet;

use crate::analysis::{Analysis, SymKind, Symbol};
use crate::features::Found;
use crate::keywords::keyword;
use crate::lexer::TokKind;
use crate::oo::{
    class_chain, file_members, interfaces_of, is_abstract, is_private, param_count, type_sym,
};
use crate::workspace::{is_include_file, Workspace};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Off,
    Hint,
    Info,
    Warning,
    Error,
}

impl Level {
    fn parse(v: &Value) -> Option<Level> {
        match v {
            Value::Bool(false) => Some(Level::Off),
            Value::Bool(true) => Some(Level::Warning),
            Value::String(s) => match s.to_ascii_lowercase().as_str() {
                "off" | "none" | "ignore" => Some(Level::Off),
                "hint" => Some(Level::Hint),
                "info" | "information" => Some(Level::Info),
                "warning" | "warn" => Some(Level::Warning),
                "error" => Some(Level::Error),
                _ => None,
            },
            _ => None,
        }
    }

    fn severity(self) -> Option<DiagnosticSeverity> {
        match self {
            Level::Off => None,
            Level::Hint => Some(DiagnosticSeverity::HINT),
            Level::Info => Some(DiagnosticSeverity::INFORMATION),
            Level::Warning => Some(DiagnosticSeverity::WARNING),
            Level::Error => Some(DiagnosticSeverity::ERROR),
        }
    }
}

pub const UNUSED_PARAMETER: &str = "unused-parameter";
pub const UNUSED_VARIABLE: &str = "unused-variable";
pub const MISSING_NO_UNDO: &str = "missing-no-undo";
pub const FIND_WITHOUT_NO_ERROR: &str = "find-without-no-error";
pub const UNUSED_BUFFER: &str = "unused-buffer";
pub const FOR_EACH_WITHOUT_LOCK: &str = "for-each-without-lock";
pub const FIND_WITHOUT_LOCK: &str = "find-without-lock";
pub const KEYWORD_ABBREVIATION: &str = "keyword-abbreviation";
pub const MISSING_IMPLEMENTATION: &str = "missing-implementation";
pub const MISSING_OVERRIDE: &str = "missing-override";
pub const OVERRIDE_WITHOUT_BASE: &str = "override-without-base";

/// Every rule with its default level.
pub const RULES: &[(&str, Level)] = &[
    (UNUSED_PARAMETER, Level::Warning),
    (UNUSED_VARIABLE, Level::Warning),
    (MISSING_NO_UNDO, Level::Warning),
    (FIND_WITHOUT_NO_ERROR, Level::Warning),
    (UNUSED_BUFFER, Level::Warning),
    (FOR_EACH_WITHOUT_LOCK, Level::Warning),
    (FIND_WITHOUT_LOCK, Level::Warning),
    (KEYWORD_ABBREVIATION, Level::Hint),
    (MISSING_IMPLEMENTATION, Level::Error),
    (MISSING_OVERRIDE, Level::Error),
    (OVERRIDE_WITHOUT_BASE, Level::Error),
];

#[derive(Clone, Debug)]
pub struct LintConfig {
    levels: HashMap<&'static str, Level>,
}

impl Default for LintConfig {
    fn default() -> Self {
        Self {
            levels: RULES.iter().copied().collect(),
        }
    }
}

impl LintConfig {
    /// Applies `{ "rule-name": "warning", ... }` on top of the defaults.
    /// Returns entries that were not understood, so they can be reported.
    pub fn apply(&mut self, v: &Value) -> Vec<String> {
        let mut unknown = Vec::new();
        let Some(map) = v.as_object() else {
            return unknown;
        };
        *self = Self::default();
        for (name, level) in map {
            match (RULES.iter().find(|(r, _)| r == name), Level::parse(level)) {
                (Some((rule, _)), Some(l)) => {
                    self.levels.insert(rule, l);
                }
                _ => unknown.push(format!("{name}: {level}")),
            }
        }
        unknown
    }

    pub fn level(&self, rule: &str) -> Level {
        self.levels.get(rule).copied().unwrap_or(Level::Off)
    }
}

/// A quick fix: replace bytes `at..end` with `text` (`at == end` inserts).
struct Fix {
    at: usize,
    end: usize,
    text: String,
    title: String,
}

impl Fix {
    fn insert(at: usize, text: &str, title: &str) -> Self {
        Fix {
            at,
            end: at,
            text: text.into(),
            title: title.into(),
        }
    }
}

struct Emitter<'a> {
    ana: &'a Analysis,
    cfg: &'a LintConfig,
    /// line -> rules ignored there ("*" = all rules)
    ignores: HashMap<usize, Vec<String>>,
    out: Vec<Diagnostic>,
}

impl<'a> Emitter<'a> {
    fn new(ana: &'a Analysis, cfg: &'a LintConfig) -> Self {
        let mut ignores: HashMap<usize, Vec<String>> = HashMap::new();
        for t in ana.tokens.iter().filter(|t| t.kind == TokKind::Comment) {
            let text = &ana.text[t.start..t.end];
            let Some(p) = text.find("lint-ignore") else {
                continue;
            };
            let mut rules: Vec<String> = text[p + "lint-ignore".len()..]
                .split(|c: char| c.is_whitespace() || c == ',')
                .map(|w| w.trim_end_matches("*/").to_string())
                .filter(|w| RULES.iter().any(|(r, _)| r == w))
                .collect();
            if rules.is_empty() {
                rules.push("*".into());
            }
            ignores
                .entry(ana.lines.line_of(t.start))
                .or_default()
                .extend(rules);
        }
        Self {
            ana,
            cfg,
            ignores,
            out: Vec::new(),
        }
    }

    fn enabled(&self, rule: &str) -> bool {
        self.cfg.level(rule) != Level::Off
    }

    fn suppressed(&self, off: usize, rule: &str) -> bool {
        let line = self.ana.lines.line_of(off);
        [Some(line), line.checked_sub(1)]
            .into_iter()
            .flatten()
            .any(|l| {
                self.ignores
                    .get(&l)
                    .is_some_and(|rules| rules.iter().any(|r| r == "*" || r == rule))
            })
    }

    fn pos(&self, off: usize) -> Value {
        let p = self.ana.lines.position(&self.ana.text, off);
        json!({ "line": p.line, "character": p.character })
    }

    fn push(
        &mut self,
        rule: &'static str,
        start: usize,
        end: usize,
        message: String,
        unnecessary: bool,
        fix: Option<Fix>,
    ) {
        let Some(severity) = self.cfg.level(rule).severity() else {
            return;
        };
        if self.suppressed(start, rule) {
            return;
        }
        let text = &self.ana.text;
        let eol = text[start..].find('\n').map_or(text.len(), |i| start + i);
        let eol = if eol > start && text.as_bytes()[eol - 1] == b'\r' {
            eol - 1
        } else {
            eol
        };
        let mut data = json!({ "rule": rule, "suppress": self.pos(eol) });
        if let Some(f) = fix {
            let mut p = self.pos(f.at);
            if f.end != f.at {
                p["end"] = self.pos(f.end);
            }
            p["text"] = f.text.into();
            p["title"] = f.title.into();
            data["fix"] = p;
        }
        self.out.push(Diagnostic {
            range: self.ana.lines.range(text, start, end),
            severity: Some(severity),
            code: Some(NumberOrString::String(rule.into())),
            source: Some("abl-lint".into()),
            message,
            tags: unnecessary.then(|| vec![DiagnosticTag::UNNECESSARY]),
            data: Some(data),
            ..Default::default()
        });
    }
}

pub fn run(ws: &Workspace, path: &Path, ana: &Analysis) -> Vec<Diagnostic> {
    if crate::builtins::is_builtin(path) {
        return Vec::new();
    }
    let mut e = Emitter::new(ana, &ws.lint);
    // Definitions in an include are used by the files that include it.
    if !is_include_file(path) {
        check_unused(ws, path, ana, &mut e);
    }
    check_no_undo(ana, &mut e);
    check_find_no_error(ana, &mut e);
    check_locks(ws, path, ana, &mut e);
    check_abbreviations(ana, &mut e);
    check_contracts(ws, path, ana, &mut e);
    e.out
}

// ---- unused-parameter / unused-variable ----------------------------------

/// Is `sym` mentioned anywhere in `start..=end` (other than its own declaration),
/// or in one of the included files?
fn is_used(
    ana: &Analysis,
    sym: &Symbol,
    start: usize,
    end: usize,
    included: &[Arc<Analysis>],
) -> bool {
    let mentions = |a: &Analysis, t: &crate::lexer::Token| {
        let w = t.text(&a.text);
        // `pBuffer.field` uses pBuffer; `obj:Method` has `obj` as its own token.
        w.eq_ignore_ascii_case(&sym.name)
            || w.split('.')
                .next()
                .is_some_and(|f| f.eq_ignore_ascii_case(&sym.name))
    };
    let here = ana
        .tokens
        .iter()
        .filter(|t| {
            t.kind == TokKind::Word
                && t.start >= start
                && t.start <= end
                && t.start != sym.sel_start
        })
        .any(|t| mentions(ana, t));
    here || included.iter().any(|a| {
        a.tokens
            .iter()
            .filter(|t| t.kind == TokKind::Word)
            .any(|t| mentions(a, t))
    })
}

/// A parameter whose signature is fixed by something else: an OVERRIDE, an
/// interface/superclass method of the same name, a DLL entry point, or a
/// routine without a body.
fn signature_is_fixed(ws: &Workspace, ana: &Analysis, param: &Symbol) -> bool {
    if !ana.scopes[param.scope].body {
        return true;
    }
    let Some(routine) = param.parent.map(|p| &ana.symbols[p]) else {
        return false;
    };
    match routine.kind {
        SymKind::Method => {
            routine.is_override
                || crate::oo::super_paths(ws, ana)
                    .iter()
                    .any(|p| crate::features::find_member(ws, p, &routine.name, 0).is_some())
        }
        SymKind::Procedure => routine.detail.contains("EXTERNAL"),
        _ => false,
    }
}

fn check_unused(ws: &Workspace, path: &Path, ana: &Analysis, e: &mut Emitter) {
    if !e.enabled(UNUSED_PARAMETER) && !e.enabled(UNUSED_VARIABLE) && !e.enabled(UNUSED_BUFFER) {
        return;
    }
    let included: Vec<Arc<Analysis>> = ws
        .includes_of(path, ana)
        .into_iter()
        .map(|(_, a)| a)
        .collect();
    let is_class = ana.class_name.is_some();
    for sym in &ana.symbols {
        let (rule, what) = match sym.kind {
            SymKind::Parameter if e.enabled(UNUSED_PARAMETER) => {
                if signature_is_fixed(ws, ana, sym) {
                    continue;
                }
                (UNUSED_PARAMETER, "Parameter")
            }
            SymKind::Variable if e.enabled(UNUSED_VARIABLE) => {
                let decl = sym.decl.to_ascii_uppercase();
                // CATCH variables, shared variables, and class members visible outside the class.
                let shared = decl.split_whitespace().any(|w| w == "SHARED");
                let visible =
                    is_class && sym.scope == 0 && matches!(sym.access, Some(a) if a != "PRIVATE");
                if decl.starts_with("CATCH") || shared || visible {
                    continue;
                }
                (UNUSED_VARIABLE, "Variable")
            }
            SymKind::Buffer if e.enabled(UNUSED_BUFFER) => {
                let decl = sym.decl.to_ascii_uppercase();
                let shared = decl.split_whitespace().any(|w| w == "SHARED");
                let visible =
                    is_class && sym.scope == 0 && matches!(sym.access, Some(a) if a != "PRIVATE");
                if shared || visible {
                    continue;
                }
                (UNUSED_BUFFER, "Buffer")
            }
            _ => continue,
        };
        let scope = &ana.scopes[sym.scope];
        if !is_used(ana, sym, scope.start, scope.end, &included) {
            e.push(
                rule,
                sym.sel_start,
                sym.sel_end,
                format!("{what} '{}' is never used", sym.name),
                true,
                None,
            );
        }
    }
}

// ---- missing-no-undo ----------------------------------------------------

/// Where NO-UNDO goes: after the `AS type` / `LIKE field` clause, else before the period.
fn no_undo_position(ana: &Analysis, sym: &Symbol) -> usize {
    let toks: Vec<usize> = (0..ana.tokens.len())
        .filter(|&i| {
            ana.tokens[i].start >= sym.sel_end
                && ana.tokens[i].end <= sym.end
                && ana.is_significant(i)
        })
        .collect();
    for (k, &i) in toks.iter().enumerate() {
        if matches!(ana.kw(i), Some("AS" | "LIKE")) {
            let mut j = k + 1;
            if toks.get(j).is_some_and(|&t| ana.kw(t) == Some("CLASS")) {
                j += 1;
            }
            if let Some(&t) = toks.get(j) {
                return ana.tokens[t].end;
            }
        }
    }
    match toks.last() {
        Some(&t) if ana.tokens[t].kind == TokKind::Period => ana.tokens[t].start,
        Some(&t) => ana.tokens[t].end,
        None => sym.sel_end,
    }
}

fn check_no_undo(ana: &Analysis, e: &mut Emitter) {
    if !e.enabled(MISSING_NO_UNDO) {
        return;
    }
    for sym in &ana.symbols {
        let applies = match sym.kind {
            SymKind::Variable | SymKind::TempTable | SymKind::Property => true,
            // DEFINE PARAMETER only; buffer/table parameters cannot be NO-UNDO.
            SymKind::Parameter => !sym.detail.contains("BUFFER") && !sym.detail.contains("-HANDLE"),
            _ => false,
        };
        // Only DEFINE statements: not CATCH variables or routine signature parameters.
        let is_define = sym.decl.split_whitespace().next().and_then(keyword) == Some("DEFINE");
        if !applies || !is_define {
            continue;
        }
        let has = ana
            .tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| t.start >= sym.start && t.end <= sym.end && t.kind == TokKind::Word)
            .any(|(i, _)| ana.kw(i) == Some("NO-UNDO"));
        if has {
            continue;
        }
        let at = if sym.kind == SymKind::TempTable {
            sym.sel_end
        } else {
            no_undo_position(ana, sym)
        };
        let what = match sym.kind {
            SymKind::TempTable => "Temp-table",
            SymKind::Property => "Property",
            SymKind::Parameter => "Parameter",
            _ => "Variable",
        };
        e.push(
            MISSING_NO_UNDO,
            sym.sel_start,
            sym.sel_end,
            format!(
                "{what} '{}' is not NO-UNDO, so every change is tracked for undo",
                sym.name
            ),
            false,
            Some(Fix::insert(at, " NO-UNDO", "Add NO-UNDO")),
        );
    }
}

// ---- find-without-no-error ----------------------------------------------

fn check_find_no_error(ana: &Analysis, e: &mut Emitter) {
    if !e.enabled(FIND_WITHOUT_NO_ERROR) {
        return;
    }
    for st in ana.statements.iter().filter(|s| !s.header) {
        if ana.kw(st.first) != Some("FIND") {
            continue;
        }
        let toks: Vec<usize> = ana.stmt_tokens(st).collect();
        // FIND CURRENT re-reads a record that is already there.
        if toks.get(1).is_some_and(|&t| ana.kw(t) == Some("CURRENT"))
            || toks.iter().any(|&t| ana.kw(t) == Some("NO-ERROR"))
        {
            continue;
        }
        let find = &ana.tokens[st.first];
        let at = match st.term {
            Some(t) => ana.tokens[t].start,
            None => ana.tokens[st.last].end,
        };
        e.push(
            FIND_WITHOUT_NO_ERROR,
            find.start,
            find.end,
            "FIND without NO-ERROR raises a run-time error when no record matches; add NO-ERROR and test AVAILABLE".into(),
            false,
            Some(Fix::insert(at, " NO-ERROR", "Add NO-ERROR")),
        );
    }
}

// ---- for-each-without-lock / find-without-lock --------------------------

#[derive(PartialEq)]
enum TableKind {
    Temp,
    Db,
    Unknown,
}

/// Is `name` a temp-table (or a buffer for one) or a database table?
fn table_kind(ws: &Workspace, path: &Path, ana: &Analysis, name: &str, depth: usize) -> TableKind {
    if name.contains('.') {
        return if crate::features::schema_table(ws, name).is_some() {
            TableKind::Db
        } else {
            TableKind::Unknown
        };
    }
    let follow = |s: &Symbol| -> Option<TableKind> {
        match s.kind {
            SymKind::TempTable => Some(TableKind::Temp),
            // `DEFINE BUFFER Customer FOR Customer`: the target is looked up further out.
            SymKind::Buffer | SymKind::Parameter if depth < 4 => {
                let target = s.type_name.as_deref()?;
                if target.eq_ignore_ascii_case(name) {
                    return Some(if crate::features::schema_table(ws, target).is_some() {
                        TableKind::Db
                    } else {
                        TableKind::Unknown
                    });
                }
                Some(table_kind(ws, path, ana, target, depth + 1))
            }
            _ => None,
        }
    };
    let named = |s: &&Symbol| s.name.eq_ignore_ascii_case(name);
    if let Some(k) = ana.symbols.iter().filter(named).find_map(follow) {
        return k;
    }
    for (_, a) in ws.includes_of(path, ana) {
        if let Some(k) = a.symbols.iter().filter(named).find_map(follow) {
            return k;
        }
    }
    for sup in ana.supertypes() {
        if let Some((_, a, i)) = ws
            .resolve_class(&sup, &ana.usings)
            .and_then(|p| crate::features::find_member(ws, &p, name, 1))
        {
            if a.symbols[i].kind == SymKind::TempTable {
                return TableKind::Temp;
            }
        }
    }
    if crate::features::schema_table(ws, name).is_some() {
        TableKind::Db
    } else {
        TableKind::Unknown
    }
}

const LOCKS: [&str; 3] = ["NO-LOCK", "SHARE-LOCK", "EXCLUSIVE-LOCK"];

fn check_locks(ws: &Workspace, path: &Path, ana: &Analysis, e: &mut Emitter) {
    if !e.enabled(FOR_EACH_WITHOUT_LOCK) && !e.enabled(FIND_WITHOUT_LOCK) {
        return;
    }
    // Without a schema every table that is not a known temp-table counts as a database table.
    let no_schema = ws.schema().is_empty();
    let check = |e: &mut Emitter,
                 rule: &'static str,
                 verb: &str,
                 name_tok: usize,
                 segment: &[usize]| {
        if segment
            .iter()
            .any(|&t| ana.kw(t).is_some_and(|k| LOCKS.contains(&k)))
        {
            return;
        }
        let name = ana.tok_text(name_tok);
        let kind = table_kind(ws, path, ana, name, 0);
        if kind == TableKind::Temp || (kind == TableKind::Unknown && !no_schema) {
            return;
        }
        let t = &ana.tokens[name_tok];
        e.push(
            rule,
            t.start,
            t.end,
            format!("{verb} {name} has no lock option, so it reads with SHARE-LOCK; add NO-LOCK, or EXCLUSIVE-LOCK to update"),
            false,
            Some(Fix::insert(t.end, " NO-LOCK", "Add NO-LOCK")),
        );
    };

    for st in &ana.statements {
        let toks: Vec<usize> = ana.stmt_tokens(st).collect();
        match ana.kw(st.first) {
            Some("FIND") if !st.header && e.enabled(FIND_WITHOUT_LOCK) => {
                let mut k = 1;
                match toks.get(k).and_then(|&t| ana.kw(t)) {
                    Some("CURRENT") => continue,
                    Some("FIRST" | "LAST" | "NEXT" | "PREV") => k += 1,
                    _ => {}
                }
                let Some(&name) = toks.get(k) else { continue };
                if ana.tokens[name].kind != TokKind::Word {
                    continue;
                }
                let verb = toks[..k]
                    .iter()
                    .map(|&t| ana.kw(t).unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join(" ");
                check(e, FIND_WITHOUT_LOCK, &verb, name, &toks[k..]);
            }
            Some("FOR") if e.enabled(FOR_EACH_WITHOUT_LOCK) => {
                // Record phrases separated by top-level commas: FOR EACH a ..., EACH b ...:
                let mut depth = 0i32;
                let mut segments: Vec<Vec<usize>> = vec![Vec::new()];
                for &t in &toks[1..] {
                    match ana.tokens[t].kind {
                        TokKind::LParen | TokKind::LBracket => depth += 1,
                        TokKind::RParen | TokKind::RBracket => depth -= 1,
                        TokKind::Comma if depth == 0 => {
                            segments.push(Vec::new());
                            continue;
                        }
                        _ => {}
                    }
                    segments.last_mut().unwrap().push(t);
                }
                for seg in &segments {
                    let Some(verb) = seg
                        .first()
                        .and_then(|&t| ana.kw(t))
                        .filter(|k| matches!(*k, "EACH" | "FIRST" | "LAST"))
                    else {
                        continue;
                    };
                    let Some(&name) = seg.get(1).filter(|&&t| ana.tokens[t].kind == TokKind::Word)
                    else {
                        continue;
                    };
                    check(e, FOR_EACH_WITHOUT_LOCK, &format!("FOR {verb}"), name, seg);
                }
            }
            _ => {}
        }
    }
}

// ---- keyword-abbreviation -----------------------------------------------

/// `full` written in the case style of `word`: lower, Title or UPPER.
fn match_case(word: &str, full: &str) -> String {
    if word.chars().all(|c| !c.is_ascii_uppercase()) {
        full.to_ascii_lowercase()
    } else if word.chars().skip(1).all(|c| !c.is_ascii_uppercase()) && word.len() > 1 {
        let lower = full.to_ascii_lowercase();
        let mut out = String::with_capacity(lower.len());
        let mut up = true;
        for c in lower.chars() {
            out.push(if up { c.to_ascii_uppercase() } else { c });
            up = c == '-';
        }
        out
    } else {
        full.to_string()
    }
}

fn check_abbreviations(ana: &Analysis, e: &mut Emitter) {
    if !e.enabled(KEYWORD_ABBREVIATION) {
        return;
    }
    // Names the code defines; an identifier that happens to look like an abbreviation is left alone.
    let names: std::collections::HashSet<String> = ana
        .symbols
        .iter()
        .map(|s| s.name.to_ascii_lowercase())
        .collect();
    for i in 0..ana.tokens.len() {
        let t = &ana.tokens[i];
        if t.kind != TokKind::Word || !ana.is_significant(i) || t.subst.is_some() {
            continue;
        }
        let word = ana.tok_text(i);
        let Some(full) = keyword(word) else { continue };
        if full.len() == word.len() || names.contains(&word.to_ascii_lowercase()) {
            continue;
        }
        let prev = ana.prev_sig(i);
        let prev_kw = prev.and_then(|p| ana.kw(p));
        // obj:Disp, RUN init, NEW Disp() are names, not keywords.
        if prev.is_some_and(|p| ana.tokens[p].kind == TokKind::Member)
            || matches!(prev_kw, Some("RUN" | "NEW"))
        {
            continue;
        }
        // INT / LOG / DEC are also functions (LOG is not LOGICAL), so types are only expanded after AS / RETURNS.
        if crate::keywords::type_name(word).is_some() && !matches!(prev_kw, Some("AS" | "RETURNS"))
        {
            continue;
        }
        let text = match_case(word, full);
        e.push(
            KEYWORD_ABBREVIATION,
            t.start,
            t.end,
            format!("'{word}' is an abbreviation of {full}"),
            false,
            Some(Fix {
                at: t.start,
                end: t.end,
                title: format!("Expand to {text}"),
                text,
            }),
        );
    }
}

// ---- missing-implementation / missing-override / override-without-base ----

/// What makes two members "the same" across a hierarchy: kind, name, parameter count.
fn member_key(f: &Found) -> Option<(SymKind, String, usize)> {
    let s = &f.1.symbols[f.2];
    match s.kind {
        SymKind::Method => Some((
            SymKind::Method,
            s.name.to_ascii_lowercase(),
            param_count(&f.1, f.2),
        )),
        SymKind::Property | SymKind::Event => Some((s.kind, s.name.to_ascii_lowercase(), 0)),
        _ => None,
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Where OVERRIDE goes: after METHOD and its access/STATIC/FINAL modifiers.
fn override_position(ana: &Analysis, sym: &Symbol) -> Option<usize> {
    let first = ana.tokens.iter().position(|t| t.start >= sym.start)?;
    let mut i = first;
    loop {
        i = (i + 1..ana.tokens.len()).find(|&j| ana.is_significant(j))?;
        match ana.kw(i) {
            Some(
                "PUBLIC" | "PROTECTED" | "PRIVATE" | "PACKAGE-PRIVATE" | "PACKAGE-PROTECTED"
                | "STATIC" | "ABSTRACT" | "FINAL",
            ) => continue,
            _ => return Some(ana.tokens[i].start),
        }
    }
}

fn check_contracts(ws: &Workspace, path: &Path, ana: &Analysis, e: &mut Emitter) {
    if !e.enabled(MISSING_IMPLEMENTATION)
        && !e.enabled(MISSING_OVERRIDE)
        && !e.enabled(OVERRIDE_WITHOUT_BASE)
    {
        return;
    }
    let Some(ti) = type_sym(ana) else { return };
    let me = &ana.symbols[ti];
    if me.kind != SymKind::Class {
        return;
    }
    // Every superclass must be found, or nothing can be said.
    let Some(chain) = class_chain(ws, path) else {
        return;
    };
    let Some(me_arc) = ws.get(path) else { return };
    let here = crate::workspace::key(path);
    let own: Vec<Found> = file_members(ws, path, &me_arc);
    let inherited: Vec<Found> = chain
        .iter()
        .flat_map(|(p, a)| file_members(ws, p, a))
        .collect();
    let class_name = |f: &Found| {
        f.1.class_name
            .clone()
            .unwrap_or_else(|| f.0.display().to_string())
    };

    // OVERRIDE checks, for methods declared in this file.
    for m in own.iter().filter(|f| crate::workspace::key(&f.0) == here) {
        let s = &m.1.symbols[m.2];
        if s.kind != SymKind::Method || s.is_static {
            continue;
        }
        let n = param_count(&m.1, m.2);
        let base = inherited.iter().find(|b| {
            let bs = &b.1.symbols[b.2];
            bs.kind == SymKind::Method
                && !bs.is_static
                && !is_private(bs)
                && bs.name.eq_ignore_ascii_case(&s.name)
                && param_count(&b.1, b.2) == n
        });
        match (s.is_override, base) {
            (true, None) => e.push(
                OVERRIDE_WITHOUT_BASE,
                s.sel_start,
                s.sel_end,
                format!(
                    "'{}' is marked OVERRIDE, but no superclass has a method {}({n} parameter{})",
                    s.name,
                    s.name,
                    plural(n)
                ),
                false,
                None,
            ),
            (false, Some(b)) => {
                let fix = override_position(ana, s)
                    .map(|at| Fix::insert(at, "OVERRIDE ", "Add OVERRIDE"));
                e.push(
                    MISSING_OVERRIDE,
                    s.sel_start,
                    s.sel_end,
                    format!(
                        "'{}' overrides {}:{} and must be marked OVERRIDE",
                        s.name,
                        class_name(b),
                        b.1.symbols[b.2].name
                    ),
                    false,
                    fix,
                );
            }
            _ => {}
        }
    }

    // Abstract members and interface members this concrete class must provide.
    if is_abstract(me) || !e.enabled(MISSING_IMPLEMENTATION) {
        return;
    }
    let mut types = vec![(path.to_path_buf(), me_arc.clone())];
    types.extend(chain.iter().cloned());
    let Some(interfaces) = interfaces_of(ws, &types) else {
        return;
    };
    let implemented: HashSet<(SymKind, String, usize)> = own
        .iter()
        .chain(&inherited)
        .filter(|f| !is_abstract(&f.1.symbols[f.2]))
        .filter_map(member_key)
        .collect();
    let required = inherited
        .iter()
        .filter(|f| is_abstract(&f.1.symbols[f.2]))
        .cloned()
        .chain(interfaces.iter().flat_map(|(p, a)| file_members(ws, p, a)));
    let mut reported = HashSet::new();
    for r in required {
        let Some(k) = member_key(&r) else { continue };
        if implemented.contains(&k) || !reported.insert(k.clone()) {
            continue;
        }
        let rs = &r.1.symbols[r.2];
        let what = match rs.kind {
            SymKind::Method if k.2 == 0 => format!("method {}()", rs.name),
            SymKind::Method => format!("method {}({} parameter{})", rs.name, k.2, plural(k.2)),
            SymKind::Property => format!("property {}", rs.name),
            _ => format!("event {}", rs.name),
        };
        e.push(
            MISSING_IMPLEMENTATION,
            me.sel_start,
            me.sel_end,
            format!(
                "{} does not implement {what} from {}",
                me.name,
                class_name(&r)
            ),
            false,
            None,
        );
    }
}

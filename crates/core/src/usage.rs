//! Where a database table or field is used, and how: read, created, updated
//! (a field assigned), deleted, defined against (buffers, LIKE) or just named.
//!
//! Names are resolved the way go-to-definition resolves them, so buffers
//! (`DEFINE BUFFER bCust FOR Customer`) and unqualified fields inside a
//! `FOR EACH Customer` block count as uses of Customer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::analysis::{Analysis, Stmt, SymKind};
use crate::features::{resolve_at, schema_table, Def, Found};
use crate::lexer::TokKind;
use crate::workspace::{key, Workspace};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    Create,
    Update,
    Delete,
    Read,
    Define,
    Reference,
}

impl Access {
    pub fn name(self) -> &'static str {
        match self {
            Access::Create => "create",
            Access::Update => "update",
            Access::Delete => "delete",
            Access::Read => "read",
            Access::Define => "define",
            Access::Reference => "reference",
        }
    }
}

/// What is searched for: a table, or one field of it.
pub struct Target {
    /// The table (in a .df) and, for a field, the field symbol in the same analysis.
    pub table: Found,
    pub field: Option<usize>,
}

impl Target {
    /// `Customer`, `sports.Customer`, `Customer.Name` or `sports.Customer.Name`.
    pub fn parse(ws: &Workspace, name: &str) -> Result<Target, String> {
        if let Some(t) = schema_table(ws, name) {
            return Ok(Target {
                table: t,
                field: None,
            });
        }
        let (table, field) = name.rsplit_once('.').ok_or_else(|| no_table(ws, name))?;
        let t = schema_table(ws, table).ok_or_else(|| no_table(ws, table))?;
        let f =
            t.1.symbols
                .iter()
                .position(|s| {
                    s.kind == SymKind::Field
                        && s.parent == Some(t.2)
                        && s.name.eq_ignore_ascii_case(field)
                })
                .ok_or_else(|| format!("table {} has no field {field}", t.1.symbols[t.2].name))?;
        Ok(Target {
            table: t,
            field: Some(f),
        })
    }

    pub fn table_name(&self) -> &str {
        &self.table.1.symbols[self.table.2].name
    }

    pub fn display(&self) -> String {
        match self.field {
            Some(f) => format!("{}.{}", self.table_name(), self.table.1.symbols[f].name),
            None => self.table_name().to_string(),
        }
    }

    fn is_table(&self, d: &Def) -> bool {
        self.is(d, self.table.2)
    }

    fn is(&self, d: &Def, idx: usize) -> bool {
        match d {
            Def::Sym((p, a, i)) => {
                key(p) == key(&self.table.0)
                    && a.symbols[*i].sel_start == self.table.1.symbols[idx].sel_start
            }
            _ => false,
        }
    }

    /// A field of the target table (any field, or the target field).
    fn is_field(&self, d: &Def) -> bool {
        match self.field {
            Some(f) => self.is(d, f),
            None => match d {
                Def::Sym((p, a, i)) => {
                    let s = &a.symbols[*i];
                    key(p) == key(&self.table.0)
                        && s.kind == SymKind::Field
                        && s.parent.is_some_and(|t| {
                            a.symbols[t].sel_start == self.table.1.symbols[self.table.2].sel_start
                        })
                }
                _ => false,
            },
        }
    }
}

fn no_table(ws: &Workspace, name: &str) -> String {
    if ws.schema().is_empty() {
        format!("no table {name}: no .df schema files are loaded (set `schema` under [db.x] in abl.toml)")
    } else {
        let dfs: Vec<String> = ws.schema().iter().map(|s| s.db.clone()).collect();
        format!("no table {name} in the schema ({})", dfs.join(", "))
    }
}

pub struct Usage {
    pub path: PathBuf,
    pub ana: Arc<Analysis>,
    pub start: usize,
    pub end: usize,
    pub access: Access,
    /// The field for an update/read found through a field, `via bCust` for a buffer.
    pub note: Option<String>,
    /// The statement holds EXCLUSIVE-LOCK.
    pub exclusive: bool,
}

impl Usage {
    /// 1-based line and column.
    pub fn line_col(&self) -> (u32, u32) {
        let p = self.ana.lines.position(&self.ana.text, self.start);
        (p.line + 1, p.character + 1)
    }

    /// The statement's source line, trimmed.
    pub fn line_text(&self) -> &str {
        let t = &self.ana.text;
        let s = t[..self.start].rfind('\n').map_or(0, |i| i + 1);
        let e = t[self.start..]
            .find('\n')
            .map_or(t.len(), |i| self.start + i);
        t[s..e].trim()
    }
}

/// Byte offset of each dot-separated segment of a word token.
fn segments(word: &str, start: usize) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut off = start;
    for s in word.split('.') {
        out.push((off, s));
        off += s.len() + 1;
    }
    out
}

fn first_kw(ana: &Analysis, st: &Stmt) -> Option<&'static str> {
    ana.stmt_tokens(st).next().and_then(|t| ana.kw(t))
}

/// How a table named by token `ti` (in statement `st`) is used.
fn table_access(ana: &Analysis, st: &Stmt, ti: usize) -> Access {
    let toks: Vec<usize> = ana.stmt_tokens(st).collect();
    let k = toks.iter().position(|&t| t == ti).unwrap_or(0);
    let prev = k.checked_sub(1).and_then(|p| ana.kw(toks[p]));
    let first = first_kw(ana, st);
    match (first, prev) {
        (_, Some("EACH" | "FIRST" | "LAST" | "FIND" | "NEXT" | "PREV" | "CURRENT")) => Access::Read,
        (Some("CREATE"), _) if k == 1 => Access::Create,
        (Some("DELETE"), _) if k == 1 => Access::Delete,
        (Some("BUFFER-COPY"), _) if k == 1 => Access::Read,
        (Some("BUFFER-COPY"), Some("TO")) => Access::Update,
        (Some("DEFINE"), Some("FOR" | "LIKE")) => Access::Define,
        _ => {
            // CAN-FIND(Customer ...), CAN-FIND(FIRST Customer ...)
            let can_find = k >= 2
                && ana.tokens[toks[k - 1]].kind == TokKind::LParen
                && ana.kw(toks[k - 2]) == Some("CAN-FIND");
            if can_find {
                Access::Read
            } else {
                Access::Reference
            }
        }
    }
}

fn ends_value(ana: &Analysis, t: usize) -> bool {
    match ana.tokens[t].kind {
        TokKind::String | TokKind::Number | TokKind::RParen | TokKind::RBracket => true,
        TokKind::Word => ana.kw(t).is_none_or(|k| {
            // Keywords that are values; anything else (ASSIGN, WHEN, THEN...) is not.
            matches!(k, "TRUE" | "FALSE" | "YES" | "NO" | "TODAY" | "NOW" | "?")
        }),
        _ => false,
    }
}

/// Is the field at token `ti` assigned (left of `=`, or in UPDATE/SET/IMPORT)?
fn field_written(ana: &Analysis, st: &Stmt, ti: usize) -> bool {
    let toks: Vec<usize> = ana.stmt_tokens(st).collect();
    let Some(k) = toks.iter().position(|&t| t == ti) else {
        return false;
    };
    if matches!(
        first_kw(ana, st),
        Some("UPDATE" | "SET" | "PROMPT-FOR" | "IMPORT" | "INSERT")
    ) {
        return true;
    }
    // A subscript (`f[2] = x`) sits between the field and the `=`.
    let mut n = k + 1;
    if toks
        .get(n)
        .is_some_and(|&t| ana.tokens[t].kind == TokKind::LBracket)
    {
        while toks
            .get(n)
            .is_some_and(|&t| ana.tokens[t].kind != TokKind::RBracket)
        {
            n += 1;
        }
        n += 1;
    }
    let is_eq = toks
        .get(n)
        .is_some_and(|&t| ana.tokens[t].kind == TokKind::Operator && ana.tok_text(t) == "=");
    if !is_eq {
        return false;
    }
    if k == 0 {
        return true;
    }
    let prev = toks[k - 1];
    if matches!(ana.kw(prev), Some("ASSIGN" | "THEN" | "ELSE")) {
        return true;
    }
    // Later pairs of an ASSIGN: `ASSIGN a = 1 b = 2`.
    let in_assign = toks[..k].iter().any(|&t| ana.kw(t) == Some("ASSIGN"));
    in_assign && ends_value(ana, prev)
}

fn statement_of(ana: &Analysis, ti: usize) -> Option<&Stmt> {
    ana.statements
        .iter()
        .find(|s| s.first <= ti && ti <= s.last)
}

fn has_exclusive(ana: &Analysis, st: &Stmt) -> bool {
    ana.stmt_tokens(st)
        .any(|t| ana.kw(t) == Some("EXCLUSIVE-LOCK"))
}

/// Uses of `target` in one file.
pub fn in_file(ws: &Workspace, path: &Path, ana: &Arc<Analysis>, target: &Target) -> Vec<Usage> {
    let table = target.table_name().to_ascii_lowercase();
    let field = target
        .field
        .map(|f| target.table.1.symbols[f].name.to_ascii_lowercase());
    // Buffers for the table in this file, so `bCust.x` is looked at.
    let buffers: Vec<String> = ana
        .symbols
        .iter()
        .filter(|s| {
            s.kind == SymKind::Buffer
                && s.type_name.as_deref().is_some_and(|t| {
                    let t = t.rsplit('.').next().unwrap_or(t);
                    t.eq_ignore_ascii_case(&table)
                })
        })
        .map(|s| s.name.to_ascii_lowercase())
        .collect();

    let mut out = Vec::new();
    for ti in 0..ana.tokens.len() {
        let tok = &ana.tokens[ti];
        if tok.kind != TokKind::Word || tok.inactive || tok.subst.is_some() {
            continue;
        }
        let word = ana.tok_text(ti);
        let segs = segments(word, tok.start);
        let lower: Vec<String> = segs.iter().map(|(_, s)| s.to_ascii_lowercase()).collect();
        let Some(st) = statement_of(ana, ti) else {
            continue;
        };

        // The field, qualified or not: the last segment.
        let (last_off, _) = *segs.last().unwrap();
        let last = lower.last().unwrap();
        let field_candidate = match &field {
            Some(f) => last == f,
            None => segs.len() > 1 || ana.kw(ti).is_none(),
        };
        if field_candidate {
            if let Some(d) = resolve_at(ws, path, ana, last_off) {
                if target.is_field(&d) {
                    let fname = match &d {
                        Def::Sym((_, a, i)) => a.symbols[*i].name.clone(),
                        _ => String::new(),
                    };
                    let written = field_written(ana, st, ti);
                    if target.field.is_some() || written {
                        out.push(Usage {
                            path: path.to_path_buf(),
                            ana: ana.clone(),
                            start: tok.start,
                            end: tok.end,
                            access: if written {
                                Access::Update
                            } else {
                                Access::Read
                            },
                            note: target.field.is_none().then_some(fname),
                            exclusive: false,
                        });
                    }
                    continue;
                }
            }
        }
        if target.field.is_some() {
            continue;
        }

        // The table: the whole word (`Customer`, `sports.Customer`) or the
        // segment before a field (`Customer.Name`, `bCust.Name`).
        // (`Customer.Name` ends in a field, handled above.)
        let table_seg = if *last == table || buffers.contains(last) {
            Some(last_off)
        } else {
            None
        };
        let Some(off) = table_seg else { continue };
        let Some(d) = resolve_at(ws, path, ana, off) else {
            continue;
        };
        let via = match &d {
            Def::Sym((_, a, i)) if a.symbols[*i].kind == SymKind::Buffer => {
                Some(format!("via {}", a.symbols[*i].name))
            }
            _ => None,
        };
        if !target.is_table(&d) && via.is_none() {
            continue;
        }
        let access = table_access(ana, st, ti);
        // The buffer's own DEFINE is where it is named, not a use.
        let decl = ana
            .symbols
            .iter()
            .any(|s| s.kind == SymKind::Buffer && s.sel_start == tok.start);
        if decl {
            continue;
        }
        out.push(Usage {
            path: path.to_path_buf(),
            ana: ana.clone(),
            start: tok.start,
            end: tok.end,
            access,
            note: via,
            exclusive: access == Access::Read && has_exclusive(ana, st),
        });
    }
    out
}

/// Uses of `target` across `files`, sorted by access, then file and position.
pub fn find(ws: &Workspace, files: &[(PathBuf, Arc<Analysis>)], target: &Target) -> Vec<Usage> {
    let needles: Vec<String> = {
        let mut v = vec![target.table_name().to_ascii_lowercase()];
        if let Some(f) = target.field {
            v = vec![target.table.1.symbols[f].name.to_ascii_lowercase()];
        }
        v
    };
    let mut out: Vec<Usage> = Vec::new();
    for (p, a) in files {
        let text = a.text.to_ascii_lowercase();
        // A file that never names the table (or field) can't use it, except
        // through a buffer or block from an include, which the include's own
        // entry covers.
        if !needles.iter().any(|n| text.contains(n.as_str())) {
            continue;
        }
        out.extend(in_file(ws, p, a, target));
    }
    out.sort_by(|a, b| (a.access, key(&a.path), a.start).cmp(&(b.access, key(&b.path), b.start)));
    out
}

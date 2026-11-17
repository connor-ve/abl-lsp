//! LSP features built on top of [`Analysis`] and [`Workspace`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tower_lsp::lsp_types::*;

use crate::analysis::{Analysis, Severity, SymKind};
use crate::keywords::{all_keywords, keyword, type_name};
use crate::lexer::{is_word_char, TokKind};
use crate::workspace::{key, Workspace};

/// A symbol in some file.
pub type Found = (PathBuf, Arc<Analysis>, usize);

pub enum Def {
    Sym(Found),
    File(PathBuf),
    Keyword(&'static str),
    /// Built-in function, system handle or handle attribute/method.
    Builtin(&'static crate::langref::Entry),
}

// ---- name resolution ---------------------------------------------------

/// What the cursor at `off` refers to.
pub fn resolve_at(ws: &Workspace, path: &Path, ana: &Arc<Analysis>, off: usize) -> Option<Def> {
    if let Some(r) = ana.includes.iter().find(|r| r.start <= off && off < r.end) {
        return ws.resolve_file(&r.target, Some(path)).map(Def::File);
    }
    if let Some(r) = ana
        .preproc_refs
        .iter()
        .find(|r| r.start <= off && off < r.end)
    {
        return find_preproc(ws, path, ana, &r.target).map(Def::Sym);
    }
    if let Some(r) = ana.runs.iter().find(|r| r.start <= off && off <= r.end) {
        return if r.is_file {
            ws.resolve_run(&r.target, Some(path)).map(Def::File)
        } else {
            find_procedure(ws, path, ana, &r.target).map(Def::Sym)
        };
    }

    let ti = ana.word_at(off)?;
    let tok = &ana.tokens[ti];
    let word = ana.tok_text(ti);

    // A declaration's own name (tells overloads apart).
    if let Some(i) = ana
        .symbols
        .iter()
        .position(|s| s.sel_start == tok.start && s.sel_end == tok.end && !s.kind.is_nested())
    {
        if matches!(
            ana.symbols[i].kind,
            SymKind::Method | SymKind::Constructor | SymKind::Property | SymKind::Event
        ) {
            return Some(Def::Sym((path.to_path_buf(), ana.clone(), i)));
        }
    }

    // obj:Member, a:b():c, CAST(x, T):Member, NEW T():Member, Class:StaticMember
    if let Some(prev) = ana.prev_sig(ti) {
        let p = &ana.tokens[prev];
        if p.kind == TokKind::Member && p.end == tok.start {
            if let Some(cls) = ana
                .prev_sig(prev)
                .and_then(|recv| crate::oo::expr_class(ws, path, ana, recv, 0))
            {
                if let Some(f) =
                    crate::oo::pick_at(ws, path, ana, crate::oo::members_named(ws, &cls, word), ti)
                {
                    return Some(Def::Sym(f));
                }
            } else if let Some(m) = ana
                .prev_sig(prev)
                .and_then(|recv| crate::handles::handle_kind(ws, path, ana, recv))
                .and_then(|k| crate::langref::member(&k, word))
            {
                return Some(Def::Builtin(m));
            }
            return keyword(word).map(Def::Keyword);
        }
    }

    // Qualified names: a class (app.Svc) or table.field / db.table.field.
    if word.contains('.') {
        if let Some(p) = ws.resolve_class(word, &ana.usings) {
            return Some(class_def(ws, p));
        }
        let segs: Vec<&str> = word.split('.').collect();
        let rel = off - tok.start;
        let mut acc = 0;
        let mut seg_i = segs.len() - 1;
        for (i, s) in segs.iter().enumerate() {
            if rel <= acc + s.len() {
                seg_i = i;
                break;
            }
            acc += s.len() + 1;
        }
        // db.table: the whole word names a table.
        if segs.len() == 2 && ws.is_db(segs[0]) {
            return if seg_i == 1 {
                schema_table(ws, word).map(Def::Sym)
            } else {
                None
            };
        }
        let (table, field) = (segs[segs.len() - 2], segs[segs.len() - 1]);
        // db.table.field keeps the database qualifier on the table.
        let table_q = if segs.len() == 3 {
            format!("{}.{}", segs[0], segs[1])
        } else {
            table.to_string()
        };
        if seg_i == segs.len() - 1 {
            return find_field(ws, path, ana, &table_q, field, off).map(Def::Sym);
        }
        if seg_i == segs.len() - 2 {
            let t = if segs.len() == 3 {
                schema_table(ws, &table_q)
            } else {
                lookup(ws, path, ana, table, off)
            };
            return t.map(Def::Sym);
        }
        return None;
    }

    if let Some(f) = lookup(ws, path, ana, word, off) {
        // Unqualified method call inside a class: pick the overload.
        if f.1.symbols[f.2].kind == SymKind::Method {
            if let Some(o) =
                crate::oo::pick_at(ws, path, ana, crate::oo::members_named(ws, &f.0, word), ti)
            {
                return Some(Def::Sym(o));
            }
        }
        return Some(Def::Sym(f));
    }
    if let Some(f) = find_unqualified_field(ws, path, ana, word, off) {
        return Some(Def::Sym(f));
    }
    if let Some(b) = builtin_word(ana, ti) {
        return Some(Def::Builtin(b));
    }
    if let Some(k) = keyword(word) {
        return Some(Def::Keyword(k));
    }
    if let Some(p) = ws.resolve_class(word, &ana.usings) {
        return Some(class_def(ws, p));
    }
    find_in_index(ws, word, &[SymKind::Procedure, SymKind::Function]).map(Def::Sym)
}

/// A built-in function call (`ENTRY(`, `TODAY`) or system handle (`SESSION`) at token `ti`.
fn builtin_word(ana: &Analysis, ti: usize) -> Option<&'static crate::langref::Entry> {
    let word = ana.tok_text(ti);
    if let Some((_, e)) = crate::langref::system_handle(word) {
        return Some(e);
    }
    let f = crate::langref::function(word)?;
    let call = crate::oo::next_sig(ana, ti).is_some_and(|n| {
        ana.tokens[n].kind == TokKind::LParen && ana.tokens[n].start == ana.tokens[ti].end
    });
    // `AS DATE`, `INDEX idx`, `ENTRY 2 OF` are not calls.
    let after_as = ana
        .prev_sig(ti)
        .and_then(|p| ana.kw(p))
        .is_some_and(|k| matches!(k, "AS" | "RETURNS" | "LIKE"));
    (!after_as && (call || !f.has_parens())).then_some(f)
}

fn class_def(ws: &Workspace, p: PathBuf) -> Def {
    if let Some(a) = ws.get(&p) {
        if let Some(i) = a
            .symbols
            .iter()
            .position(|s| matches!(s.kind, SymKind::Class | SymKind::Interface | SymKind::Enum))
        {
            return Def::Sym((p, a, i));
        }
    }
    Def::File(p)
}

/// Unqualified name visible at `off`: local scopes, the file, include files, then
/// inherited class members.
pub fn lookup(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    name: &str,
    off: usize,
) -> Option<Found> {
    let chain = ana.scope_chain(off);
    let mut best: Option<(usize, (bool, usize))> = None;
    for (i, s) in ana.symbols.iter().enumerate() {
        if s.kind.is_nested() || !s.name.eq_ignore_ascii_case(name) {
            continue;
        }
        let Some(depth) = chain.iter().position(|&c| c == s.scope) else {
            continue;
        };
        let rank = (s.proto, depth);
        if best.as_ref().is_none_or(|(_, b)| rank < *b) {
            best = Some((i, rank));
        }
    }
    if let Some((i, _)) = best {
        return Some((path.to_path_buf(), ana.clone(), i));
    }
    for (p, a) in ws.includes_of(path, ana) {
        if let Some(i) = a
            .symbols
            .iter()
            .position(|s| s.scope == 0 && !s.kind.is_nested() && s.name.eq_ignore_ascii_case(name))
        {
            return Some((p, a, i));
        }
    }
    if ana.class_name.is_some() {
        for cls in crate::oo::super_paths(ws, ana) {
            if let Some(f) = find_member(ws, &cls, name, 1) {
                return Some(f);
            }
        }
    }
    schema_table(ws, name)
}

/// Database table by name, optionally qualified with the database (`shop.Customer`).
pub fn schema_table(ws: &Workspace, name: &str) -> Option<Found> {
    let (db, table) = match name.split_once('.') {
        Some((d, t)) => (Some(d), t),
        None => (None, name),
    };
    ws.schema()
        .iter()
        .filter(|s| db.is_none_or(|d| s.db.eq_ignore_ascii_case(d)))
        .find_map(|s| {
            s.ana
                .symbols
                .iter()
                .position(|x| x.kind == SymKind::Table && x.name.eq_ignore_ascii_case(table))
                .map(|i| (s.path.clone(), s.ana.clone(), i))
        })
}

/// Member `name` of the class in `class_path` or one of its supertypes.
pub fn find_member(ws: &Workspace, class_path: &Path, name: &str, depth: usize) -> Option<Found> {
    if depth > 12 {
        return None;
    }
    let a = ws.get(class_path)?;
    if let Some(i) = a
        .symbols
        .iter()
        .position(|s| s.scope == 0 && is_member_kind(s.kind) && s.name.eq_ignore_ascii_case(name))
    {
        return Some((class_path.to_path_buf(), a, i));
    }
    for p in crate::oo::super_paths(ws, &a) {
        if let Some(f) = find_member(ws, &p, name, depth + 1) {
            return Some(f);
        }
    }
    None
}

pub fn is_member_kind(k: SymKind) -> bool {
    !matches!(
        k,
        SymKind::Class
            | SymKind::Interface
            | SymKind::Enum
            | SymKind::Constructor
            | SymKind::Destructor
            | SymKind::Field
            | SymKind::Index
            | SymKind::Trigger
            | SymKind::Preprocessor
    )
}

/// Every member of a class, nearest definition first.
fn class_members(ws: &Workspace, class_path: &Path) -> Vec<(Arc<Analysis>, usize)> {
    let mut out = Vec::new();
    let mut seen_names = HashSet::new();
    let mut seen_files = HashSet::new();
    let mut queue = vec![class_path.to_path_buf()];
    while let Some(p) = queue.pop() {
        if !seen_files.insert(key(&p)) || seen_files.len() > 16 {
            continue;
        }
        let Some(a) = ws.get(&p) else { continue };
        for (i, s) in a.symbols.iter().enumerate() {
            if s.scope == 0
                && is_member_kind(s.kind)
                && seen_names.insert(s.name.to_ascii_lowercase())
            {
                out.push((a.clone(), i));
            }
        }
        for sp in crate::oo::super_paths(ws, &a).into_iter().rev() {
            queue.push(sp);
        }
    }
    out
}

/// The class file for the type of the expression word `recv` (variable, THIS-OBJECT,
/// SUPER, or a class name used for static access).
fn class_of(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    recv: &str,
    off: usize,
) -> Option<PathBuf> {
    match recv.to_ascii_uppercase().as_str() {
        "THIS-OBJECT" => return ana.class_name.as_ref().map(|_| path.to_path_buf()),
        "SUPER" => {
            return ana
                .inherits
                .as_ref()
                .and_then(|i| ws.resolve_class(i, &ana.usings))
        }
        _ => {}
    }
    if let Some((p, a, i)) = lookup(ws, path, ana, recv, off) {
        let s = &a.symbols[i];
        if matches!(s.kind, SymKind::Class | SymKind::Interface | SymKind::Enum) {
            return Some(p);
        }
        return s
            .type_name
            .as_ref()
            .and_then(|t| ws.resolve_class(t, &a.usings));
    }
    ws.resolve_class(recv, &ana.usings)
}

/// Temp-table behind a table or buffer name.
fn table_of(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    name: &str,
    off: usize,
    depth: usize,
) -> Option<Found> {
    let found = if name.contains('.') {
        schema_table(ws, name)
    } else {
        lookup(ws, path, ana, name, off)
    };
    let (p, a, i) = found?;
    match a.symbols[i].kind {
        SymKind::TempTable | SymKind::Table => Some((p, a, i)),
        SymKind::Buffer | SymKind::Parameter if depth < 4 => {
            let target = a.symbols[i].type_name.clone()?;
            let at = a.symbols[i].start;
            table_of(ws, &p, &a, &target, at, depth + 1)
        }
        _ => None,
    }
}

/// Fields of a temp-table, following `LIKE other-table`.
fn table_fields(ws: &Workspace, tt: &Found, depth: usize) -> Vec<Found> {
    let (p, a, i) = tt;
    let mut out: Vec<Found> = a
        .symbols
        .iter()
        .enumerate()
        .filter(|(_, s)| s.parent == Some(*i) && s.kind == SymKind::Field)
        .map(|(j, _)| (p.clone(), a.clone(), j))
        .collect();
    if depth < 4 {
        if let Some(like) = a.symbols[*i].type_name.clone() {
            if let Some(base) = table_of(ws, p, a, &like, a.symbols[*i].start, depth + 1) {
                out.extend(table_fields(ws, &base, depth + 1));
            }
        }
    }
    out
}

/// What `LIKE target` refers to: `table.field`, `db.table.field` or a variable.
pub fn like_target(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    target: &str,
    off: usize,
) -> Option<Found> {
    let segs: Vec<&str> = target.split('.').collect();
    match segs.len() {
        1 => lookup(ws, path, ana, target, off),
        2 => find_field(ws, path, ana, segs[0], segs[1], off),
        _ => find_field(
            ws,
            path,
            ana,
            &format!("{}.{}", segs[0], segs[1]),
            segs[2],
            off,
        ),
    }
}

fn find_field(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    table: &str,
    field: &str,
    off: usize,
) -> Option<Found> {
    let tt = table_of(ws, path, ana, table, off, 0)?;
    table_fields(ws, &tt, 0)
        .into_iter()
        .find(|(_, a, j)| a.symbols[*j].name.eq_ignore_ascii_case(field))
}

/// Tables whose fields can be named without a qualifier at `off`, best first:
/// the record phrase `off` is in (`FOR EACH Customer WHERE |`), the other record
/// phrases of that statement (joins, CAN-FIND), then enclosing FOR blocks, innermost first.
pub fn record_tables(ana: &Analysis, off: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let add = |name: &str, out: &mut Vec<String>| {
        if !out.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            out.push(name.to_string());
        }
    };
    // (phrase start, table) for every record phrase in a statement.
    let phrases = |st: &crate::analysis::Stmt| -> Vec<(usize, String)> {
        let toks: Vec<usize> = ana.stmt_tokens(st).collect();
        let mut found = Vec::new();
        for (k, &t) in toks.iter().enumerate() {
            let name_k = match ana.kw(t) {
                Some("EACH" | "FIRST" | "LAST") => Some(k + 1),
                Some("FIND") if k == 0 => match toks.get(1).and_then(|&n| ana.kw(n)) {
                    Some("FIRST" | "LAST") => None, // the FIRST/LAST arm takes it
                    Some("NEXT" | "PREV" | "CURRENT") => Some(2),
                    _ => Some(1),
                },
                // CAN-FIND(Customer WHERE ...)
                Some("CAN-FIND")
                    if toks
                        .get(k + 1)
                        .is_some_and(|&n| ana.tokens[n].kind == TokKind::LParen) =>
                {
                    match toks.get(k + 2).and_then(|&n| ana.kw(n)) {
                        Some("FIRST" | "LAST") => None,
                        _ => Some(k + 2),
                    }
                }
                _ => None,
            };
            let Some(nk) = name_k else { continue };
            let Some(&n) = toks
                .get(nk)
                .filter(|&&n| ana.tokens[n].kind == TokKind::Word)
            else {
                continue;
            };
            found.push((ana.tokens[t].start, ana.tok_text(n).to_string()));
        }
        found
    };
    let stmt_end = |st: &crate::analysis::Stmt| ana.tokens[st.term.unwrap_or(st.last)].end;

    // The statement under the cursor.
    if let Some(st) = ana
        .statements
        .iter()
        .find(|st| ana.tokens[st.first].start <= off && off <= stmt_end(st))
    {
        let mut ph = phrases(st);
        // DEFINE DATASET ... RELATION-FIELDS (a, b): the member tables.
        if ana.kw(st.first) == Some("DEFINE")
            && ana
                .stmt_tokens(st)
                .take(6)
                .any(|t| ana.kw(t) == Some("DATASET"))
        {
            for (k, t) in ana.stmt_tokens(st).enumerate().skip(2) {
                if ana.tokens[t].kind == TokKind::Word && ana.kw(t).is_none() && k > 2 {
                    add(ana.tok_text(t), &mut out);
                }
            }
        }
        // The phrase that starts last before `off` is the one being written.
        if let Some(k) = ph.iter().rposition(|(start, _)| *start <= off) {
            let cur = ph.remove(k);
            add(&cur.1, &mut out);
        }
        for (_, name) in ph {
            add(&name, &mut out);
        }
    }
    // Enclosing FOR blocks, innermost first.
    let mut enclosing: Vec<(usize, &crate::analysis::Stmt)> = ana
        .statements
        .iter()
        .filter(|st| st.header && ana.kw(st.first) == Some("FOR"))
        .filter_map(|st| {
            let start = ana.tokens[st.first].start;
            let (_, end) = ana.blocks.iter().find(|(s, _)| *s == start)?;
            (start <= off && off <= *end).then_some((start, st))
        })
        .collect();
    enclosing.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, st) in enclosing {
        for (_, name) in phrases(st) {
            add(&name, &mut out);
        }
    }
    out
}

fn find_unqualified_field(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    name: &str,
    off: usize,
) -> Option<Found> {
    for table in record_tables(ana, off) {
        if let Some(f) = find_field(ws, path, ana, &table, name, off) {
            return Some(f);
        }
    }
    let is_field =
        |s: &crate::analysis::Symbol| s.kind == SymKind::Field && s.name.eq_ignore_ascii_case(name);
    if let Some(i) = ana.symbols.iter().position(is_field) {
        return Some((path.to_path_buf(), ana.clone(), i));
    }
    ws.includes_of(path, ana)
        .into_iter()
        .find_map(|(p, a)| {
            a.symbols
                .iter()
                .position(is_field)
                .map(|i| (p, a.clone(), i))
        })
        .or_else(|| {
            ws.schema().iter().find_map(|s| {
                s.ana
                    .symbols
                    .iter()
                    .position(is_field)
                    .map(|i| (s.path.clone(), s.ana.clone(), i))
            })
        })
}

fn find_kind_here_or_included(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    name: &str,
    kind: SymKind,
) -> Option<Found> {
    let hit = |s: &crate::analysis::Symbol| {
        s.kind == kind && !s.proto && s.name.eq_ignore_ascii_case(name)
    };
    if let Some(i) = ana.symbols.iter().position(hit) {
        return Some((path.to_path_buf(), ana.clone(), i));
    }
    ws.includes_of(path, ana)
        .into_iter()
        .find_map(|(p, a)| a.symbols.iter().position(hit).map(|i| (p, a.clone(), i)))
}

fn find_preproc(ws: &Workspace, path: &Path, ana: &Arc<Analysis>, name: &str) -> Option<Found> {
    find_kind_here_or_included(ws, path, ana, name, SymKind::Preprocessor)
}

fn find_procedure(ws: &Workspace, path: &Path, ana: &Arc<Analysis>, name: &str) -> Option<Found> {
    find_kind_here_or_included(ws, path, ana, name, SymKind::Procedure)
        .or_else(|| find_in_index(ws, name, &[SymKind::Procedure]))
}

/// First definition of `name` with one of `kinds` anywhere in the workspace index.
fn find_in_index(ws: &Workspace, name: &str, kinds: &[SymKind]) -> Option<Found> {
    let (p, _) = ws.index().find(|(_, syms)| {
        syms.iter()
            .any(|s| kinds.contains(&s.kind) && s.name.eq_ignore_ascii_case(name))
    })?;
    let a = ws.get(p)?;
    let i = a
        .symbols
        .iter()
        .position(|s| kinds.contains(&s.kind) && !s.proto && s.name.eq_ignore_ascii_case(name))?;
    Some((p.clone(), a, i))
}

// ---- LSP conversions ---------------------------------------------------

pub fn location(def: &Def) -> Option<Location> {
    match def {
        Def::Sym((p, a, i)) => {
            let s = &a.symbols[*i];
            Some(Location {
                uri: Url::from_file_path(p).ok()?,
                range: a.lines.range(&a.text, s.sel_start, s.sel_end),
            })
        }
        Def::File(p) => Some(Location {
            uri: Url::from_file_path(p).ok()?,
            range: Range::default(),
        }),
        Def::Keyword(_) | Def::Builtin(_) => None,
    }
}

pub fn hover(ws: &Workspace, path: &Path, ana: &Arc<Analysis>, off: usize) -> Option<Hover> {
    // `{&NAME}` that was expanded: show its value.
    if let Some(x) = ana
        .expansions
        .iter()
        .find(|x| x.start <= off && off < x.end)
    {
        let value = if x.value.trim().is_empty() {
            "(empty)".to_string()
        } else {
            x.value.clone()
        };
        let mut md = format!(
            "```abl
{value}
```
"
        );
        let origin = x
            .name
            .strip_prefix('&')
            .and_then(|n| find_preproc(ws, path, ana, n));
        match origin {
            Some((p, a, i)) => md.push_str(&format!(
                "preprocessor `{{{}}}` · {}:{}",
                x.name,
                ws.display_path(&p),
                a.lines.line_of(a.symbols[i].sel_start) + 1
            )),
            None if x.name.starts_with('&') => md.push_str(&format!(
                "preprocessor `{{{}}}` (include argument or built-in)",
                x.name
            )),
            None => md.push_str(&format!("include argument `{{{}}}`", x.name)),
        }
        let range = Some(ana.lines.range(&ana.text, x.start, x.end));
        return Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md,
            }),
            range,
        });
    }
    let def = resolve_at(ws, path, ana, off)?;
    let word_ti = ana.word_at(off);
    let range = word_ti.map(|ti| {
        ana.lines
            .range(&ana.text, ana.tokens[ti].start, ana.tokens[ti].end)
    });
    let value = match &def {
        Def::Sym((p, a, i)) => {
            let s = &a.symbols[*i];
            let decl = if s.decl.is_empty() { &s.name } else { &s.decl };
            let mut meta = s.kind.label().to_string();
            if let Some(par) = s.parent {
                meta.push_str(&format!(
                    " in {} `{}`",
                    a.symbols[par].kind.label(),
                    a.symbols[par].name
                ));
            }
            if s.detail.split_whitespace().any(|w| w == "LIKE") {
                if let Some(t) = crate::oo::sym_prim_type(ws, &(p.clone(), a.clone(), *i), 0) {
                    meta.push_str(&format!(" · {t}"));
                }
            }
            if key(p) != key(path) {
                meta.push_str(&format!(
                    " · {}:{}",
                    ws.display_path(p),
                    a.lines.line_of(s.sel_start) + 1
                ));
            }
            format!("```abl\n{decl}\n```\n{meta}")
        }
        Def::File(p) => format!("`{}`", ws.display_path(p)),
        Def::Builtin(e) => e.markdown(),
        Def::Keyword(k) => {
            let w = word_ti.map(|ti| ana.tok_text(ti)).unwrap_or("");
            if w.is_empty() || w.eq_ignore_ascii_case(k) {
                format!("**{k}** · ABL keyword")
            } else {
                format!("**{k}** · ABL keyword (`{w}` is an abbreviation)")
            }
        }
    };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range,
    })
}

fn lsp_symbol_kind(k: SymKind) -> SymbolKind {
    use SymKind::*;
    match k {
        Procedure | Function => SymbolKind::FUNCTION,
        Method | Destructor => SymbolKind::METHOD,
        Constructor => SymbolKind::CONSTRUCTOR,
        Class => SymbolKind::CLASS,
        Interface => SymbolKind::INTERFACE,
        Enum => SymbolKind::ENUM,
        EnumMember => SymbolKind::ENUM_MEMBER,
        Property => SymbolKind::PROPERTY,
        Event | Trigger => SymbolKind::EVENT,
        Variable | Parameter => SymbolKind::VARIABLE,
        TempTable | Table => SymbolKind::STRUCT,
        Field => SymbolKind::FIELD,
        Index => SymbolKind::KEY,
        Preprocessor => SymbolKind::CONSTANT,
        Buffer | Query | Dataset | DataSource | Frame | Stream | Widget => SymbolKind::OBJECT,
        Relation => SymbolKind::KEY,
    }
}

fn completion_kind(k: SymKind) -> CompletionItemKind {
    use SymKind::*;
    match k {
        Procedure | Function => CompletionItemKind::FUNCTION,
        Method => CompletionItemKind::METHOD,
        Constructor | Destructor => CompletionItemKind::CONSTRUCTOR,
        Class => CompletionItemKind::CLASS,
        Interface => CompletionItemKind::INTERFACE,
        Enum => CompletionItemKind::ENUM,
        EnumMember => CompletionItemKind::ENUM_MEMBER,
        Property => CompletionItemKind::PROPERTY,
        Event | Trigger => CompletionItemKind::EVENT,
        Field => CompletionItemKind::FIELD,
        TempTable | Table => CompletionItemKind::STRUCT,
        Preprocessor => CompletionItemKind::CONSTANT,
        _ => CompletionItemKind::VARIABLE,
    }
}

pub fn document_symbols(ana: &Analysis) -> Vec<DocumentSymbol> {
    let n = ana.symbols.len();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut roots = Vec::new();
    for (i, s) in ana.symbols.iter().enumerate() {
        match s.parent {
            Some(p) => children[p].push(i),
            None => roots.push(i),
        }
    }
    fn build(ana: &Analysis, i: usize, children: &[Vec<usize>]) -> DocumentSymbol {
        let s = &ana.symbols[i];
        let mut kids: Vec<usize> = children[i].clone();
        kids.sort_by_key(|&k| ana.symbols[k].start);
        let range = ana
            .lines
            .range(&ana.text, s.start.min(s.sel_start), s.end.max(s.sel_end));
        #[allow(deprecated)]
        DocumentSymbol {
            name: if s.name.is_empty() {
                "?".into()
            } else {
                s.name.clone()
            },
            detail: (!s.detail.is_empty()).then(|| s.detail.clone()),
            kind: lsp_symbol_kind(s.kind),
            tags: None,
            deprecated: None,
            range,
            selection_range: ana.lines.range(&ana.text, s.sel_start, s.sel_end),
            children: (!kids.is_empty())
                .then(|| kids.iter().map(|&k| build(ana, k, children)).collect()),
        }
    }
    roots.sort_by_key(|&r| ana.symbols[r].start);
    roots.iter().map(|&r| build(ana, r, &children)).collect()
}

pub fn workspace_symbols(ws: &Workspace, query: &str) -> Vec<SymbolInformation> {
    let q = query.to_lowercase();
    let mut out = Vec::new();
    for (path, syms) in ws.index() {
        let Ok(uri) = Url::from_file_path(path) else {
            continue;
        };
        for s in syms {
            if q.is_empty() || s.name.to_lowercase().contains(&q) {
                #[allow(deprecated)]
                out.push(SymbolInformation {
                    name: s.name.clone(),
                    kind: lsp_symbol_kind(s.kind),
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range: s.range,
                    },
                    container_name: s.container.clone(),
                });
                if out.len() >= 1000 {
                    return out;
                }
            }
        }
    }
    for sf in ws.schema() {
        let Ok(uri) = Url::from_file_path(&sf.path) else {
            continue;
        };
        for s in sf.ana.symbols.iter().filter(|s| s.kind == SymKind::Table) {
            if q.is_empty() || s.name.to_lowercase().contains(&q) {
                #[allow(deprecated)]
                out.push(SymbolInformation {
                    name: s.name.clone(),
                    kind: SymbolKind::STRUCT,
                    tags: None,
                    deprecated: None,
                    location: Location {
                        uri: uri.clone(),
                        range: sf.ana.lines.range(&sf.ana.text, s.sel_start, s.sel_end),
                    },
                    container_name: Some(sf.db.clone()),
                });
            }
        }
    }
    out
}

pub fn diagnostics(ws: &Workspace, path: &Path, ana: &Analysis) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = ana
        .diags
        .iter()
        .map(|d| Diagnostic {
            range: ana.lines.range(&ana.text, d.start, d.end),
            severity: Some(match d.severity {
                Severity::Error => DiagnosticSeverity::ERROR,
                Severity::Warning => DiagnosticSeverity::WARNING,
            }),
            source: Some("abl".into()),
            message: d.message.clone(),
            ..Default::default()
        })
        .collect();
    if ws.unresolved_include_diags {
        for r in &ana.includes {
            if ws.resolve_file(&r.target, Some(path)).is_none() {
                out.push(Diagnostic {
                    range: ana.lines.range(&ana.text, r.start, r.end),
                    severity: Some(DiagnosticSeverity::WARNING),
                    source: Some("abl".into()),
                    message: format!("Include file '{}' was not found on the PROPATH", r.target),
                    ..Default::default()
                });
            }
        }
    }
    out.extend(crate::lint::run(ws, path, ana));
    out
}

// ---- completion --------------------------------------------------------

fn builtin_item(e: &'static crate::langref::Entry) -> CompletionItem {
    use crate::langref::EntryKind;
    CompletionItem {
        label: e.name.clone(),
        kind: Some(match e.kind {
            EntryKind::Method => CompletionItemKind::METHOD,
            EntryKind::Attribute => CompletionItemKind::PROPERTY,
            EntryKind::Function => CompletionItemKind::FUNCTION,
            EntryKind::Handle => CompletionItemKind::VARIABLE,
        }),
        detail: Some(e.signature()),
        documentation: (!e.doc.is_empty()).then(|| {
            Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: e.doc.clone(),
            })
        }),
        sort_text: Some(format!("0{}", e.name.to_ascii_lowercase())),
        ..Default::default()
    }
}

fn sym_item(a: &Analysis, i: usize) -> CompletionItem {
    let s = &a.symbols[i];
    let detail = format!("{} {}", s.kind.label(), s.detail);
    CompletionItem {
        label: s.name.clone(),
        kind: Some(completion_kind(s.kind)),
        detail: Some(detail.trim().to_string()),
        sort_text: Some(format!("0{}", s.name.to_ascii_lowercase())),
        ..Default::default()
    }
}

pub fn completion(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    off: usize,
) -> Vec<CompletionItem> {
    let text = &ana.text;
    let off = off.min(text.len());
    let line_start = text[..off].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line_start..off];
    let b = before.as_bytes();
    let mut p = b.len();
    while p > 0 && is_word_char(b[p - 1]) {
        p -= 1;
    }
    let prefix = &before[p..];
    let trigger = if p > 0 { b[p - 1] } else { 0 };

    if trigger == b'.' || trigger == b':' {
        let q_end = p - 1;
        let mut q = q_end;
        while q > 0 && (is_word_char(b[q - 1]) || (trigger == b':' && b[q - 1] == b'.')) {
            q -= 1;
        }
        let qualifier = &before[q..q_end];
        let at = line_start + q;
        if trigger == b':' {
            // The token before the `:` ends the receiver expression (a:b():c, CAST(...), NEW T()).
            let colon = line_start + q_end;
            let cls = ana
                .tokens
                .iter()
                .position(|t| t.start == colon)
                .and_then(|ci| ana.prev_sig(ci))
                .and_then(|r| crate::oo::expr_class(ws, path, ana, r, 0))
                .or_else(|| {
                    (!qualifier.is_empty())
                        .then(|| class_of(ws, path, ana, qualifier, at))
                        .flatten()
                });
            if let Some(cls) = cls {
                return class_members(ws, &cls)
                    .iter()
                    .map(|(a, j)| sym_item(a, *j))
                    .collect();
            }
            // A handle: SESSION:, hBuf:, BUFFER Customer:, hQuery:GET-BUFFER-HANDLE(1):
            let kind = ana
                .tokens
                .iter()
                .position(|t| t.start == colon)
                .and_then(|ci| ana.prev_sig(ci))
                .and_then(|r| crate::handles::handle_kind(ws, path, ana, r));
            return match kind {
                Some(k) => crate::langref::members(&k)
                    .into_iter()
                    .map(builtin_item)
                    .collect(),
                None => Vec::new(),
            };
        }
        if qualifier.is_empty() {
            return Vec::new();
        }
        if trigger == b'.' && ws.is_db(qualifier) {
            return ws
                .schema()
                .iter()
                .filter(|s| s.db.eq_ignore_ascii_case(qualifier))
                .flat_map(|s| {
                    s.ana
                        .symbols
                        .iter()
                        .enumerate()
                        .filter(|(_, x)| x.kind == SymKind::Table)
                        .map(|(i, _)| sym_item(&s.ana, i))
                })
                .collect();
        }
        return match table_of(ws, path, ana, qualifier, at, 0) {
            Some(tt) => table_fields(ws, &tt, 0)
                .iter()
                .map(|(_, a, j)| sym_item(a, *j))
                .collect(),
            None => Vec::new(),
        };
    }

    let prev_word = before[..p].trim_end();
    let prev_word = &prev_word[prev_word
        .rfind(|c: char| !is_word_char(c as u8))
        .map_or(0, |i| i + 1)..];
    let after_run = prev_word.eq_ignore_ascii_case("RUN");

    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut add = |a: &Analysis, i: usize, items: &mut Vec<CompletionItem>| {
        let s = &a.symbols[i];
        if after_run && s.kind != SymKind::Procedure {
            return;
        }
        if seen.insert(s.name.to_ascii_lowercase()) {
            items.push(sym_item(a, i));
        }
    };

    // Fields of the FOR EACH / FIND tables in scope come first.
    if !after_run {
        for table in record_tables(ana, off) {
            if let Some(tt) = table_of(ws, path, ana, &table, off, 0) {
                for (_, a, j) in table_fields(ws, &tt, 0) {
                    add(&a, j, &mut items);
                }
            }
        }
    }

    let chain = ana.scope_chain(off);
    for (i, s) in ana.symbols.iter().enumerate() {
        if !s.kind.is_nested() && chain.contains(&s.scope) {
            add(ana, i, &mut items);
        }
    }
    for (_, a) in ws.includes_of(path, ana) {
        for (i, s) in a.symbols.iter().enumerate() {
            if s.scope == 0 && !s.kind.is_nested() {
                add(&a, i, &mut items);
            }
        }
    }
    if ana.class_name.is_some() {
        for sup in ana.supertypes() {
            if let Some(cls) = ws.resolve_class(&sup, &ana.usings) {
                for (a, i) in class_members(ws, &cls) {
                    add(&a, i, &mut items);
                }
            }
        }
    }
    if after_run {
        return items;
    }
    for s in ws.schema() {
        for (i, x) in s.ana.symbols.iter().enumerate() {
            if x.kind == SymKind::Table {
                add(&s.ana, i, &mut items);
            }
        }
    }

    let lower = !prefix.is_empty() && prefix.chars().all(|c| !c.is_ascii_uppercase());
    let case = |s: &str| {
        if lower {
            s.to_ascii_lowercase()
        } else {
            s.to_string()
        }
    };
    for e in crate::langref::functions().chain(crate::langref::system_handles()) {
        let mut it = builtin_item(e);
        it.label = case(&e.name);
        it.sort_text = Some(format!("1{}", e.name.to_ascii_lowercase()));
        items.push(it);
    }
    for k in all_keywords() {
        if crate::langref::function(k).is_some_and(|f| f.name == *k)
            || crate::langref::system_handle(k).is_some()
        {
            continue;
        }
        items.push(CompletionItem {
            label: if lower {
                k.to_ascii_lowercase()
            } else {
                k.to_string()
            },
            kind: Some(CompletionItemKind::KEYWORD),
            sort_text: Some(format!("1{}", k.to_ascii_lowercase())),
            ..Default::default()
        });
    }
    items
}

// ---- semantic tokens ---------------------------------------------------

const TOKEN_TYPES: [SemanticTokenType; 15] = [
    SemanticTokenType::KEYWORD,
    SemanticTokenType::STRING,
    SemanticTokenType::COMMENT,
    SemanticTokenType::NUMBER,
    SemanticTokenType::MACRO,
    SemanticTokenType::TYPE,
    SemanticTokenType::CLASS,
    SemanticTokenType::ENUM,
    SemanticTokenType::ENUM_MEMBER,
    SemanticTokenType::FUNCTION,
    SemanticTokenType::METHOD,
    SemanticTokenType::PROPERTY,
    SemanticTokenType::EVENT,
    SemanticTokenType::VARIABLE,
    SemanticTokenType::PARAMETER,
];
const T_KEYWORD: u32 = 0;
const T_STRING: u32 = 1;
const T_COMMENT: u32 = 2;
const T_NUMBER: u32 = 3;
const T_MACRO: u32 = 4;
const T_TYPE: u32 = 5;
const T_CLASS: u32 = 6;
const T_ENUM: u32 = 7;
const T_ENUM_MEMBER: u32 = 8;
const T_FUNCTION: u32 = 9;
const T_METHOD: u32 = 10;
const T_PROPERTY: u32 = 11;
const T_EVENT: u32 = 12;
const T_VARIABLE: u32 = 13;
const T_PARAMETER: u32 = 14;

pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES.to_vec(),
        token_modifiers: Vec::new(),
    }
}

fn sym_token_type(k: SymKind) -> Option<u32> {
    use SymKind::*;
    Some(match k {
        Procedure | Function => T_FUNCTION,
        Method | Constructor | Destructor => T_METHOD,
        Class | Interface => T_CLASS,
        Enum => T_ENUM,
        EnumMember => T_ENUM_MEMBER,
        Property | Field => T_PROPERTY,
        Event => T_EVENT,
        Parameter => T_PARAMETER,
        Preprocessor => T_MACRO,
        Trigger | Index | Relation => return None,
        _ => T_VARIABLE,
    })
}

pub fn semantic_tokens(ws: &Workspace, path: &Path, ana: &Arc<Analysis>) -> Vec<SemanticToken> {
    let mut names: HashMap<String, SymKind> = HashMap::new();
    for s in &ana.symbols {
        names.entry(s.name.to_ascii_lowercase()).or_insert(s.kind);
    }
    for (_, a) in ws.includes_of(path, ana) {
        for s in a.symbols.iter().filter(|s| s.scope == 0) {
            names.entry(s.name.to_ascii_lowercase()).or_insert(s.kind);
        }
    }
    for s in ws.schema() {
        for t in s.ana.symbols.iter().filter(|t| t.kind == SymKind::Table) {
            names.entry(t.name.to_ascii_lowercase()).or_insert(t.kind);
        }
    }

    let mut spans: Vec<(usize, usize, u32)> = Vec::new();
    let mut last_sig: Option<usize> = None;
    for (ti, t) in ana.tokens.iter().enumerate() {
        let ty = if t.inactive {
            Some(T_COMMENT)
        } else {
            match t.kind {
                TokKind::Comment => Some(T_COMMENT),
                TokKind::String => Some(T_STRING),
                TokKind::Number => Some(T_NUMBER),
                TokKind::Preproc | TokKind::Include => Some(T_MACRO),
                TokKind::Word => {
                    let word = ana.tok_text(ti);
                    let prev = last_sig.map(|p| &ana.tokens[p]);
                    let prev_kw = last_sig
                        .filter(|&p| ana.tokens[p].kind == TokKind::Word)
                        .and_then(|p| keyword(ana.tok_text(p)));
                    if prev.is_some_and(|p| p.kind == TokKind::Member && p.end == t.start) {
                        let call = ana
                            .tokens
                            .get(ti + 1)
                            .is_some_and(|n| n.kind == TokKind::LParen && n.start == t.end);
                        Some(if call { T_METHOD } else { T_PROPERTY })
                    } else if matches!(prev_kw, Some("AS" | "RETURNS")) && type_name(word).is_some()
                    {
                        Some(T_TYPE)
                    } else if matches!(
                        prev_kw,
                        Some(
                            "AS" | "INHERITS"
                                | "IMPLEMENTS"
                                | "RETURNS"
                                | "NEW"
                                | "CLASS"
                                | "INTERFACE"
                                | "ENUM"
                        )
                    ) && keyword(word).is_none()
                    {
                        Some(T_CLASS)
                    } else if let Some(k) = names.get(&word.to_ascii_lowercase()) {
                        sym_token_type(*k)
                    } else if keyword(word).is_some() {
                        Some(T_KEYWORD)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        };
        if !t.inactive && !matches!(t.kind, TokKind::Comment | TokKind::Preproc) {
            last_sig = Some(ti);
        }
        // Tokens from a preprocessor value sit on the reference, which is coloured as a macro.
        if let Some(ty) = ty.filter(|_| t.subst.is_none()) {
            spans.push((t.start, t.end, ty));
        }
    }

    // Delta-encode, splitting multi-line tokens into one span per line.
    let text = &ana.text;
    let mut out = Vec::with_capacity(spans.len());
    let (mut prev_line, mut prev_col) = (0u32, 0u32);
    for (start, end, ty) in spans {
        let mut seg = start;
        loop {
            let line_end = text[seg..end].find('\n').map_or(end, |i| seg + i);
            let seg_end = if line_end > seg && text.as_bytes()[line_end - 1] == b'\r' {
                line_end - 1
            } else {
                line_end
            };
            if seg_end > seg {
                let pos = ana.lines.position(text, seg);
                let len = text[seg..seg_end].encode_utf16().count() as u32;
                let delta_line = pos.line - prev_line;
                let delta_start = if delta_line == 0 {
                    pos.character - prev_col
                } else {
                    pos.character
                };
                out.push(SemanticToken {
                    delta_line,
                    delta_start,
                    length: len,
                    token_type: ty,
                    token_modifiers_bitset: 0,
                });
                prev_line = pos.line;
                prev_col = pos.character;
            }
            if line_end >= end {
                break;
            }
            seg = line_end + 1;
        }
    }
    out
}

// ---- references, rename, highlights -------------------------------------

/// A name occurrence: file, its analysis, byte span of the name.
pub type Span = (PathBuf, Arc<Analysis>, usize, usize);

fn same_symbol(def: &Def, target: &Found) -> bool {
    match def {
        Def::Sym((p, a, i)) => {
            let (s, t) = (&a.symbols[*i], &target.1.symbols[target.2]);
            key(p) == key(&target.0) && s.sel_start == t.sel_start && s.kind == t.kind
        }
        _ => false,
    }
}

/// Places in `a` where one of `names` appears: (offset to resolve at, span start, span end).
fn candidate_spans(a: &Analysis, names: &[String]) -> Vec<(usize, usize, usize)> {
    let eq = |s: &str| names.iter().any(|n| n.eq_ignore_ascii_case(s));
    let mut out = Vec::new();
    for t in a
        .tokens
        .iter()
        .filter(|t| t.kind == TokKind::Word && !t.inactive)
    {
        let w = &a.text[t.start..t.end];
        if eq(w) {
            out.push((t.start, t.start, t.end));
        } else if w.contains('.') {
            let mut off = t.start;
            for seg in w.split('.') {
                if eq(seg) {
                    out.push((off, off, off + seg.len()));
                }
                off += seg.len() + 1;
            }
        }
    }
    for r in a.preproc_refs.iter().filter(|r| eq(&r.target)) {
        if let Some(p) = a.text[r.start..r.end].find('&') {
            let s = r.start + p + 1;
            out.push((r.start, s, s + r.target.len()));
        }
    }
    for r in a.runs.iter().filter(|r| !r.is_file && eq(&r.target)) {
        out.push((r.start, r.start, r.end));
    }
    out
}

/// Every occurrence of the symbol at `off`. With `this_file_only`, only `path` is searched.
pub fn references(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    off: usize,
    include_decl: bool,
    this_file_only: bool,
) -> Vec<Span> {
    let Some(Def::Sym(target)) = resolve_at(ws, path, ana, off) else {
        return Vec::new();
    };
    // A method and everything it overrides or is overridden by is one name.
    let family = if this_file_only {
        vec![target.clone()]
    } else {
        crate::oo::override_family(ws, &target)
    };
    let ts = target.1.symbols[target.2].clone();
    let mut names = vec![ts.name.clone()];
    if matches!(ts.kind, SymKind::Class | SymKind::Interface | SymKind::Enum) {
        if let Some(short) = ts.name.rsplit('.').next().filter(|s| *s != ts.name) {
            names.push(short.to_string()); // `Circle` for shapes.Circle
        }
    }

    let mut files: Vec<PathBuf> = if this_file_only {
        vec![path.to_path_buf()]
    } else if ts.scope != 0 {
        vec![target.0.clone()] // local to a routine
    } else {
        let mut v: Vec<PathBuf> = ws.index().map(|(p, _)| p.clone()).collect();
        v.push(path.to_path_buf());
        v
    };
    let mut seen_files = HashSet::new();
    files.retain(|f| seen_files.insert(key(f)));

    let lowered: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    let mut out: Vec<Span> = Vec::new();
    let mut seen = HashSet::new();
    for f in files {
        let Some(a) = ws.get(&f) else { continue };
        let text = a.text.to_ascii_lowercase();
        if !lowered.iter().any(|n| text.contains(n.as_str())) {
            continue;
        }
        for (probe, s, e) in candidate_spans(&a, &names) {
            if resolve_at(ws, &f, &a, probe)
                .is_some_and(|d| family.iter().any(|t| same_symbol(&d, t)))
                && seen.insert((key(&f), s))
            {
                out.push((f.clone(), a.clone(), s, e));
            }
        }
    }

    let decls: Vec<(String, usize)> = family
        .iter()
        .map(|(p, a, i)| (key(p), a.symbols[*i].sel_start))
        .collect();
    if include_decl {
        for (p, a, i) in &family {
            let s = &a.symbols[*i];
            let in_scope = !this_file_only || key(p) == key(path);
            if in_scope && seen.insert((key(p), s.sel_start)) {
                out.push((p.clone(), a.clone(), s.sel_start, s.sel_end));
            }
        }
    } else {
        out.retain(|(p, _, s, _)| !decls.iter().any(|(dp, ds)| *dp == key(p) && ds == s));
    }
    out
}

/// Why the symbol at `off` cannot be renamed, if it cannot.
pub fn rename_blocker(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    off: usize,
) -> Option<&'static str> {
    match resolve_at(ws, path, ana, off) {
        Some(Def::Sym((p, a, i))) => {
            let k = a.symbols[i].kind;
            if crate::builtins::is_builtin(&p) {
                Some("Built-in OpenEdge classes cannot be renamed")
            } else if crate::workspace::is_schema_file(&p) {
                Some("Database tables and fields are defined in the schema and cannot be renamed here")
            } else if matches!(
                k,
                SymKind::Class
                    | SymKind::Interface
                    | SymKind::Enum
                    | SymKind::Constructor
                    | SymKind::Destructor
            ) {
                Some("Classes are named after their file; rename the file instead")
            } else if k == SymKind::Trigger {
                Some("Triggers have no name to rename")
            } else {
                None
            }
        }
        Some(Def::File(_)) => Some("This is a file reference"),
        Some(Def::Keyword(_)) => Some("Keywords cannot be renamed"),
        Some(Def::Builtin(_)) => Some("Built-in functions and attributes cannot be renamed"),
        None => Some("No symbol here"),
    }
}

pub fn is_valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && crate::lexer::is_word_start(b[0])
        && b.iter().all(|&c| is_word_char(c))
        && keyword(name).is_none()
}

// ---- signature help ----------------------------------------------------

fn param_label(s: &crate::analysis::Symbol) -> String {
    let mut parts: Vec<&str> = s.detail.split_whitespace().collect();
    let dir = match parts.first() {
        Some(&d @ ("INPUT" | "OUTPUT" | "INPUT-OUTPUT" | "RETURN")) => {
            parts.remove(0);
            Some(d)
        }
        _ => None,
    };
    let rest = parts.join(" ");
    let body = if let Some(r) = rest.strip_prefix("BUFFER") {
        format!("BUFFER {}{}", s.name, r)
    } else if rest.starts_with("TABLE-HANDLE") || rest.starts_with("DATASET-HANDLE") {
        format!("{rest} {}", s.name)
    } else if rest.starts_with("LIKE") {
        format!("{} {rest}", s.name)
    } else if rest.is_empty() {
        s.name.clone()
    } else {
        format!("{} AS {rest}", s.name)
    };
    match dir {
        Some(d) => format!("{d} {body}"),
        None => body,
    }
}

pub fn signature_help(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    off: usize,
) -> Option<SignatureHelp> {
    // Walk back to the unclosed '(' the cursor is inside, counting commas on the way.
    let before = ana.tokens.partition_point(|t| t.end <= off);
    let mut depth = 0;
    let mut commas = 0;
    let mut open = None;
    for i in (0..before).rev() {
        if !ana.is_significant(i) {
            continue;
        }
        match ana.tokens[i].kind {
            TokKind::RParen => depth += 1,
            TokKind::LParen if depth == 0 => {
                open = Some(i);
                break;
            }
            TokKind::LParen => depth -= 1,
            TokKind::Comma if depth == 0 => commas += 1,
            TokKind::Period | TokKind::Colon => return None,
            _ => {}
        }
    }
    let name_ti = ana.prev_sig(open?)?;
    if ana.tokens[name_ti].kind != TokKind::Word {
        return None;
    }
    // Every overload as (title, params); the resolved one first.
    let mut sigs: Vec<(String, Vec<String>)> = Vec::new();
    // Overload chosen by argument types, when the call is closed.
    let mut typed_active: Option<usize> = None;
    let routine_sig = |a: &Analysis, r: usize| -> (String, Vec<String>) {
        (
            a.symbols[r].name.clone(),
            crate::oo::params(a, r)
                .map(|j| param_label(&a.symbols[j]))
                .collect(),
        )
    };
    match resolve_at(ws, path, ana, ana.tokens[name_ti].start)? {
        Def::Sym(f) => {
            let (p, a, i) = &f;
            let s = &a.symbols[*i];
            let overloads: Vec<Found> = match s.kind {
                SymKind::Method => {
                    let mut v = vec![f.clone()];
                    // Overloads in the same class and its supertypes.
                    v.extend(
                        crate::oo::members_named(ws, p, &s.name)
                            .into_iter()
                            .filter(|o| {
                                o.1.symbols[o.2].kind == SymKind::Method
                                    && !(key(&o.0) == key(p) && o.2 == *i)
                            }),
                    );
                    v
                }
                SymKind::Function | SymKind::Procedure | SymKind::Constructor => vec![f.clone()],
                // NEW Class(...) -> its constructors
                SymKind::Class => crate::oo::constructors(ws, p),
                _ => return None,
            };
            if overloads.is_empty() {
                return None;
            }
            for (_, a, r) in &overloads {
                sigs.push(routine_sig(a, *r));
            }
            if overloads.len() > 1 {
                if let Some(types) = crate::oo::call_types(ws, path, ana, name_ti) {
                    let same = |x: &Found, y: &Found| {
                        key(&x.0) == key(&y.0)
                            && x.1.symbols[x.2].sel_start == y.1.symbols[y.2].sel_start
                    };
                    typed_active = crate::oo::pick_typed(ws, overloads.clone(), &types)
                        .and_then(|best| overloads.iter().position(|o| same(o, &best)));
                }
            }
        }
        // RUN file.p (...): the file's top-level DEFINE PARAMETERs
        Def::File(p) => {
            let a = ws.get(&p)?;
            let params = a
                .symbols
                .iter()
                .filter(|x| x.scope == 0 && x.kind == SymKind::Parameter)
                .map(param_label)
                .collect();
            sigs.push((ws.display_path(&p), params));
        }
        Def::Builtin(e) if e.has_parens() => {
            sigs.push((e.name.clone(), e.param_labels()));
        }
        Def::Keyword(_) | Def::Builtin(_) => return None,
    };
    // Active: the overload whose parameter count matches a closed call, else the
    // first with room for the arguments typed so far.
    let argc = crate::oo::arg_count(ana, open?);
    let active = typed_active
        .or_else(|| argc.and_then(|n| sigs.iter().position(|(_, p)| p.len() == n)))
        .or_else(|| sigs.iter().position(|(_, p)| p.len() > commas))
        .unwrap_or(0);
    let n = sigs[active].1.len() as u32;
    let signatures = sigs
        .into_iter()
        .map(|(title, params)| SignatureInformation {
            label: format!("{title}({})", params.join(", ")),
            documentation: None,
            parameters: Some(
                params
                    .into_iter()
                    .map(|p| ParameterInformation {
                        label: ParameterLabel::Simple(p),
                        documentation: None,
                    })
                    .collect(),
            ),
            active_parameter: None,
        })
        .collect();
    Some(SignatureHelp {
        signatures,
        active_signature: Some(active as u32),
        active_parameter: Some(if n == 0 {
            0
        } else {
            (commas as u32).min(n - 1)
        }),
    })
}

// ---- formatting ----------------------------------------------------------

/// Formatting edits for a document, or for lines `first..=last` of it.
pub fn format(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    opts: &FormattingOptions,
    lines: Option<(usize, usize)>,
) -> Vec<TextEdit> {
    // Identifiers that look like keywords keep their case.
    let mut names: HashSet<String> = ana
        .symbols
        .iter()
        .map(|s| s.name.to_ascii_lowercase())
        .collect();
    if ws.keyword_case != crate::format::KeywordCase::Preserve {
        for (_, a) in ws.includes_of(path, ana) {
            names.extend(a.symbols.iter().map(|s| s.name.to_ascii_lowercase()));
        }
        for s in ws.schema() {
            names.extend(s.ana.symbols.iter().map(|x| x.name.to_ascii_lowercase()));
        }
    }
    let o = crate::format::Options {
        tab_size: opts.tab_size as usize,
        insert_spaces: opts.insert_spaces,
        keyword_case: ws.keyword_case,
    };
    crate::format::format(ana, &o, lines, &names)
}

// ---- folding -----------------------------------------------------------

pub fn folding_ranges(ana: &Analysis) -> Vec<FoldingRange> {
    let line = |off: usize| ana.lines.line_of(off) as u32;
    let mut out = Vec::new();
    let mut push = |start: u32, end: u32, kind: FoldingRangeKind| {
        if end > start {
            out.push(FoldingRange {
                start_line: start,
                start_character: None,
                end_line: end,
                end_character: None,
                kind: Some(kind),
                collapsed_text: None,
            });
        }
    };
    // Keep the END line visible: fold the body only.
    for &(s, e) in &ana.blocks {
        push(line(s), line(e).saturating_sub(1), FoldingRangeKind::Region);
    }
    for t in ana.tokens.iter().filter(|t| t.kind == TokKind::Comment) {
        push(line(t.start), line(t.end), FoldingRangeKind::Comment);
    }
    for s in ana.symbols.iter().filter(|s| s.kind == SymKind::TempTable) {
        push(line(s.start), line(s.end), FoldingRangeKind::Region);
    }
    out
}

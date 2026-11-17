//! Classes: expression types (method chains, CAST, NEW, static access), overloads,
//! the inheritance hierarchy (with the implicit Progress.Lang.Object) and override
//! families.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::analysis::{Analysis, SymKind, Symbol};
use crate::features::{is_member_kind, lookup, Found};
use crate::lexer::TokKind;
use crate::workspace::{key, Workspace};

pub fn is_type_kind(k: SymKind) -> bool {
    matches!(k, SymKind::Class | SymKind::Interface | SymKind::Enum)
}

/// The CLASS / INTERFACE / ENUM header symbol of a file.
pub fn type_sym(a: &Analysis) -> Option<usize> {
    a.symbols.iter().position(|s| is_type_kind(s.kind))
}

pub fn params(a: &Analysis, i: usize) -> impl Iterator<Item = usize> + '_ {
    (0..a.symbols.len())
        .filter(move |&j| a.symbols[j].parent == Some(i) && a.symbols[j].kind == SymKind::Parameter)
}

pub fn param_count(a: &Analysis, i: usize) -> usize {
    params(a, i).count()
}

/// Words of a declaration before its parameter list: `METHOD PUBLIC ABSTRACT VOID Run`.
fn header_has(decl: &str, word: &str) -> bool {
    let head = decl.split('(').next().unwrap_or(decl);
    head.split(|c: char| c.is_whitespace() || c == ':')
        .any(|w| w.eq_ignore_ascii_case(word))
}

pub fn is_abstract(s: &Symbol) -> bool {
    header_has(&s.decl, "ABSTRACT")
}

pub fn is_private(s: &Symbol) -> bool {
    s.access == Some("PRIVATE") || header_has(&s.decl, "PRIVATE")
}

// ---- hierarchy -----------------------------------------------------------

/// Direct supertypes of the type in `a`. `Err` holds a name that did not resolve.
pub struct Supers {
    pub class: Option<Result<PathBuf, String>>,
    pub interfaces: Vec<Result<PathBuf, String>>,
}

pub fn supers(ws: &Workspace, a: &Analysis) -> Supers {
    let res = |n: &String| ws.resolve_class(n, &a.usings).ok_or_else(|| n.clone());
    match type_sym(a).map(|i| a.symbols[i].kind) {
        // INTERFACE I INHERITS A, B: all of them are interfaces.
        Some(SymKind::Interface) => Supers {
            class: None,
            interfaces: a.supertypes().iter().map(res).collect(),
        },
        Some(SymKind::Class) => {
            let is_object = a
                .class_name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case("Progress.Lang.Object"));
            let class = match &a.inherits {
                Some(n) => Some(res(n)),
                None if !is_object => crate::builtins::class_file("Progress/Lang/Object").map(Ok),
                None => None,
            };
            Supers {
                class,
                interfaces: a.implements.iter().map(res).collect(),
            }
        }
        _ => Supers {
            class: None,
            interfaces: Vec::new(),
        },
    }
}

/// Resolved direct supertypes: the superclass first, then interfaces.
pub fn super_paths(ws: &Workspace, a: &Analysis) -> Vec<PathBuf> {
    let s = supers(ws, a);
    s.class
        .into_iter()
        .chain(s.interfaces)
        .filter_map(Result::ok)
        .collect()
}

/// Superclasses of `path`, nearest first. `None` when one of them cannot be found.
pub fn class_chain(ws: &Workspace, path: &Path) -> Option<Vec<(PathBuf, Arc<Analysis>)>> {
    let mut out: Vec<(PathBuf, Arc<Analysis>)> = Vec::new();
    let mut cur = ws.get(path)?;
    let mut seen = HashSet::from([key(path)]);
    loop {
        match supers(ws, &cur).class {
            None => return Some(out),
            Some(Err(_)) => return None,
            Some(Ok(p)) => {
                if !seen.insert(key(&p)) || out.len() > 32 {
                    return Some(out);
                }
                let a = ws.get(&p)?;
                out.push((p, a.clone()));
                cur = a;
            }
        }
    }
}

/// Every interface implemented by `types` (directly, or inherited by those interfaces).
/// `None` when one of them cannot be found.
pub fn interfaces_of(
    ws: &Workspace,
    types: &[(PathBuf, Arc<Analysis>)],
) -> Option<Vec<(PathBuf, Arc<Analysis>)>> {
    let mut out: Vec<(PathBuf, Arc<Analysis>)> = Vec::new();
    let mut seen = HashSet::new();
    let mut queue: Vec<Result<PathBuf, String>> = types
        .iter()
        .flat_map(|(_, a)| supers(ws, a).interfaces)
        .collect();
    while let Some(r) = queue.pop() {
        let p = r.ok()?;
        if !seen.insert(key(&p)) || seen.len() > 64 {
            continue;
        }
        let a = ws.get(&p)?;
        queue.extend(supers(ws, &a).interfaces);
        out.push((p, a));
    }
    Some(out)
}

/// Is `sub` the class `base`, or does it inherit or implement it?
pub fn is_subtype(ws: &Workspace, sub: &Path, base: &Path) -> bool {
    let target = key(base);
    let mut seen = HashSet::new();
    let mut queue = vec![sub.to_path_buf()];
    while let Some(p) = queue.pop() {
        if key(&p) == target {
            return true;
        }
        if !seen.insert(key(&p)) || seen.len() > 64 {
            continue;
        }
        if let Some(a) = ws.get(&p) {
            queue.extend(super_paths(ws, &a));
        }
    }
    false
}

// ---- members and overloads ---------------------------------------------

/// Top-level symbols of a class file, including those from its include files.
pub fn file_members(ws: &Workspace, p: &Path, a: &Arc<Analysis>) -> Vec<Found> {
    let mut out: Vec<Found> = (0..a.symbols.len())
        .filter(|&i| a.symbols[i].scope == 0)
        .map(|i| (p.to_path_buf(), a.clone(), i))
        .collect();
    for (ip, ia) in ws.includes_of(p, a) {
        out.extend(
            (0..ia.symbols.len())
                .filter(|&i| ia.symbols[i].scope == 0)
                .map(|i| (ip.clone(), ia.clone(), i)),
        );
    }
    out
}

/// Every member called `name` in the class and its supertypes, nearest class first.
/// Overloads all appear.
pub fn members_named(ws: &Workspace, class_path: &Path, name: &str) -> Vec<Found> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = std::collections::VecDeque::from([class_path.to_path_buf()]);
    while let Some(p) = queue.pop_front() {
        if !seen.insert(key(&p)) || seen.len() > 32 {
            continue;
        }
        let Some(a) = ws.get(&p) else { continue };
        for f in file_members(ws, &p, &a) {
            let s = &f.1.symbols[f.2];
            if is_member_kind(s.kind) && s.name.eq_ignore_ascii_case(name) {
                out.push(f);
            }
        }
        queue.extend(super_paths(ws, &a));
    }
    out
}

/// Constructors of a class (not inherited).
pub fn constructors(ws: &Workspace, class_path: &Path) -> Vec<Found> {
    let Some(a) = ws.get(class_path) else {
        return Vec::new();
    };
    (0..a.symbols.len())
        .filter(|&i| a.symbols[i].kind == SymKind::Constructor)
        .map(|i| (class_path.to_path_buf(), a.clone(), i))
        .collect()
}

// ---- argument types and typed overload resolution -----------------------

/// Type of a call argument, as far as it can be told.
#[derive(Clone, Debug)]
pub enum ArgType {
    Prim(String),
    Class(PathBuf),
    Unknown,
}

/// Built-in type of a symbol (variable, parameter, field, property, method or function result).
pub fn sym_prim_type(ws: &Workspace, f: &Found, depth: usize) -> Option<String> {
    let (p, a, i) = f;
    let s = &a.symbols[*i];
    let skip = [
        "INPUT",
        "OUTPUT",
        "INPUT-OUTPUT",
        "RETURN",
        "RETURNS",
        "PUBLIC",
        "PRIVATE",
        "PROTECTED",
        "STATIC",
        "ABSTRACT",
        "OVERRIDE",
        "FINAL",
        "PACKAGE-PRIVATE",
        "PACKAGE-PROTECTED",
        "EXTENT",
    ];
    let rest: Vec<&str> = s
        .detail
        .split_whitespace()
        .filter(|w| !skip.contains(w))
        .collect();
    if rest.first() == Some(&"LIKE") {
        // LIKE table.field / LIKE variable
        if depth >= 4 {
            return None;
        }
        let target = rest.get(1)?;
        let found = crate::features::like_target(ws, p, a, target, s.start)?;
        return sym_prim_type(ws, &found, depth + 1);
    }
    let t = match s.kind {
        SymKind::Method | SymKind::Function => rest.last()?,
        _ => rest.first()?,
    };
    crate::keywords::type_name(t).map(str::to_string)
}

fn arg_type_of_symbol(ws: &Workspace, f: &Found) -> ArgType {
    if let Some(c) = type_of_found(ws, f) {
        return ArgType::Class(c);
    }
    sym_prim_type(ws, f, 0).map_or(ArgType::Unknown, ArgType::Prim)
}

fn depth_at(ana: &Analysis, toks: &[usize], at: usize) -> i32 {
    let mut d = 0;
    for &t in toks {
        if t == at {
            return d;
        }
        match ana.tokens[t].kind {
            TokKind::LParen | TokKind::LBracket => d += 1,
            TokKind::RParen | TokKind::RBracket => d -= 1,
            _ => {}
        }
    }
    d
}

/// Type of the expression made of significant tokens `toks`.
fn expr_type(ws: &Workspace, path: &Path, ana: &Arc<Analysis>, toks: &[usize]) -> ArgType {
    let mut toks = toks;
    while let Some(&t) = toks.first() {
        match ana.kw(t) {
            Some("INPUT" | "OUTPUT" | "INPUT-OUTPUT") => toks = &toks[1..],
            Some("TABLE" | "DATASET" | "TABLE-HANDLE" | "DATASET-HANDLE" | "BUFFER") => {
                return ArgType::Unknown
            }
            _ => break,
        }
    }
    while toks.len() > 1
        && matches!(
            ana.kw(*toks.last().unwrap()),
            Some("BY-REFERENCE" | "BY-VALUE" | "BIND" | "APPEND")
        )
    {
        toks = &toks[..toks.len() - 1];
    }
    let Some(&last) = toks.last() else {
        return ArgType::Unknown;
    };
    let prim = |s: &str| ArgType::Prim(s.to_string());
    if toks.len() == 1 {
        match ana.tokens[last].kind {
            TokKind::String => return prim("CHARACTER"),
            TokKind::Number => {
                return prim(if ana.tok_text(last).contains('.') {
                    "DECIMAL"
                } else {
                    "INTEGER"
                })
            }
            TokKind::Word if matches!(ana.kw(last), Some("TRUE" | "FALSE" | "YES" | "NO")) => {
                return prim("LOGICAL")
            }
            _ => {}
        }
    }
    // Operators at the top level decide the type of a compound expression.
    let top_op = |t: usize| depth_at(ana, toks, t) == 0;
    let cmp_op = |t: usize| match ana.tokens[t].kind {
        TokKind::Operator => matches!(ana.tok_text(t), "=" | "<>" | "<" | ">" | "<=" | ">="),
        TokKind::Word => matches!(
            ana.kw(t),
            Some(
                "AND"
                    | "OR"
                    | "NOT"
                    | "EQ"
                    | "NE"
                    | "LT"
                    | "GT"
                    | "LE"
                    | "GE"
                    | "BEGINS"
                    | "MATCHES"
            )
        ),
        _ => false,
    };
    if toks.iter().any(|&t| top_op(t) && cmp_op(t)) {
        return prim("LOGICAL");
    }
    let has_top_operator = toks
        .iter()
        .any(|&t| top_op(t) && ana.tokens[t].kind == TokKind::Operator);
    if has_top_operator {
        // "a" + x is CHARACTER; other arithmetic is left alone.
        return if ana.tokens[toks[0]].kind == TokKind::String {
            prim("CHARACTER")
        } else {
            ArgType::Unknown
        };
    }
    if let Some(c) = expr_class(ws, path, ana, last, 0) {
        return ArgType::Class(c);
    }
    // A variable, a member, or a function / method call.
    let name_tok = match ana.tokens[last].kind {
        TokKind::RParen => matching_open(ana, last).and_then(|o| ana.prev_sig(o)),
        TokKind::Word => Some(last),
        _ => None,
    };
    let Some(k) = name_tok.filter(|&k| ana.tokens[k].kind == TokKind::Word) else {
        return ArgType::Unknown;
    };
    let word = ana.tok_text(k);
    if let Some(member) = ana
        .prev_sig(k)
        .filter(|&p| ana.tokens[p].kind == TokKind::Member)
    {
        let recv = ana.prev_sig(member);
        if let Some(cls) = recv.and_then(|r| expr_class(ws, path, ana, r, 0)) {
            if let Some(f) = members_named(ws, &cls, word).into_iter().next() {
                return arg_type_of_symbol(ws, &f);
            }
        }
        if let Some(m) = recv
            .and_then(|r| crate::handles::handle_kind(ws, path, ana, r))
            .and_then(|kd| crate::langref::member(&kd, word))
        {
            return m
                .return_type()
                .and_then(crate::keywords::type_name)
                .map_or(ArgType::Unknown, prim);
        }
        return ArgType::Unknown;
    }
    if let Some(f) = lookup(ws, path, ana, word, ana.tokens[k].start) {
        return arg_type_of_symbol(ws, &f);
    }
    if let Some(e) = crate::langref::function(word) {
        if let Some(t) = e.return_type().and_then(crate::keywords::type_name) {
            return prim(t);
        }
    }
    ArgType::Unknown
}

/// Argument types of the call whose name is at `name_ti` (`None` when it is not a closed call).
pub fn call_types(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    name_ti: usize,
) -> Option<Vec<ArgType>> {
    let open = next_sig(ana, name_ti).filter(|&n| ana.tokens[n].kind == TokKind::LParen)?;
    arg_count(ana, open)?;
    let mut args: Vec<Vec<usize>> = vec![Vec::new()];
    let mut depth = 0;
    for i in (open + 1..ana.tokens.len()).filter(|&i| ana.is_significant(i)) {
        match ana.tokens[i].kind {
            TokKind::LParen | TokKind::LBracket => depth += 1,
            TokKind::RParen | TokKind::RBracket if depth == 0 => break,
            TokKind::RParen | TokKind::RBracket => depth -= 1,
            TokKind::Comma if depth == 0 => {
                args.push(Vec::new());
                continue;
            }
            _ => {}
        }
        args.last_mut().unwrap().push(i);
    }
    if args.len() == 1 && args[0].is_empty() {
        return Some(Vec::new());
    }
    Some(args.iter().map(|a| expr_type(ws, path, ana, a)).collect())
}

fn numeric_rank(t: &str) -> Option<u8> {
    match t {
        "INTEGER" => Some(0),
        "INT64" => Some(1),
        "DECIMAL" => Some(2),
        _ => None,
    }
}

fn is_object_class(p: &Path) -> bool {
    p.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
        .ends_with("progress/lang/object.cls")
}

/// How well an argument fits a parameter: 0 = it does not, 3 = exact.
fn fit(ws: &Workspace, param: &Found, arg: &ArgType) -> u8 {
    let ptype = type_of_found(ws, param)
        .map(ArgType::Class)
        .or_else(|| sym_prim_type(ws, param, 0).map(ArgType::Prim));
    let Some(ptype) = ptype else { return 1 };
    match (&ptype, arg) {
        (_, ArgType::Unknown) => 1,
        (ArgType::Prim(p), ArgType::Prim(a)) if p == a => 3,
        (ArgType::Prim(p), ArgType::Prim(a)) => match (numeric_rank(a), numeric_rank(p)) {
            (Some(x), Some(y)) if x < y => 2,
            _ if matches!(
                (a.as_str(), p.as_str()),
                ("CHARACTER", "LONGCHAR") | ("LONGCHAR", "CHARACTER")
            ) =>
            {
                2
            }
            _ if matches!(
                (a.as_str(), p.as_str()),
                ("DATE", "DATETIME" | "DATETIME-TZ") | ("DATETIME", "DATETIME-TZ")
            ) =>
            {
                2
            }
            _ => 0,
        },
        (ArgType::Class(p), ArgType::Class(a)) if key(p) == key(a) => 3,
        (ArgType::Class(p), ArgType::Class(_)) if is_object_class(p) => 1,
        (ArgType::Class(p), ArgType::Class(a)) => {
            if is_subtype(ws, a, p) {
                2
            } else {
                0
            }
        }
        _ => 0,
    }
}

/// The overload whose parameters best fit the argument types.
pub fn pick_typed(ws: &Workspace, cands: Vec<Found>, args: &[ArgType]) -> Option<Found> {
    let n = args.len();
    let mut best: Option<(usize, u32)> = None;
    for (ci, f) in cands.iter().enumerate() {
        if !matches!(
            f.1.symbols[f.2].kind,
            SymKind::Method | SymKind::Constructor | SymKind::Function
        ) {
            continue;
        }
        let ps: Vec<usize> = params(&f.1, f.2).collect();
        if ps.len() != n {
            continue;
        }
        let scores: Vec<u8> = ps
            .iter()
            .zip(args)
            .map(|(pi, a)| fit(ws, &(f.0.clone(), f.1.clone(), *pi), a))
            .collect();
        if scores.contains(&0) {
            continue;
        }
        let total: u32 = scores.iter().map(|&s| s as u32).sum();
        if best.is_none_or(|(_, b)| total > b) {
            best = Some((ci, total));
        }
    }
    match best {
        Some((i, _)) => cands.into_iter().nth(i),
        None => pick(cands, Some(n)),
    }
}

/// Overload for the call at `name_ti`: by argument types when the call is closed.
pub fn pick_at(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    cands: Vec<Found>,
    name_ti: usize,
) -> Option<Found> {
    if cands.len() > 1 {
        if let Some(types) = call_types(ws, path, ana, name_ti) {
            return pick_typed(ws, cands, &types);
        }
    }
    pick(cands, call_argc(ana, name_ti))
}

/// The overload taking `argc` arguments, else the first candidate.
pub fn pick(cands: Vec<Found>, argc: Option<usize>) -> Option<Found> {
    if let Some(n) = argc {
        let fits = |f: &Found| {
            matches!(
                f.1.symbols[f.2].kind,
                SymKind::Method | SymKind::Constructor | SymKind::Function
            ) && param_count(&f.1, f.2) == n
        };
        if let Some(i) = cands.iter().position(fits) {
            return cands.into_iter().nth(i);
        }
    }
    cands.into_iter().next()
}

// ---- tokens --------------------------------------------------------------

pub fn next_sig(ana: &Analysis, i: usize) -> Option<usize> {
    (i + 1..ana.tokens.len()).find(|&j| ana.is_significant(j))
}

fn pair(k: TokKind) -> Option<(TokKind, TokKind)> {
    match k {
        TokKind::LParen | TokKind::RParen => Some((TokKind::LParen, TokKind::RParen)),
        TokKind::LBracket | TokKind::RBracket => Some((TokKind::LBracket, TokKind::RBracket)),
        _ => None,
    }
}

/// The `(` or `[` that `close` closes.
pub fn matching_open(ana: &Analysis, close: usize) -> Option<usize> {
    let (open_k, close_k) = pair(ana.tokens[close].kind)?;
    let mut depth = 0;
    for i in (0..=close).rev().filter(|&i| ana.is_significant(i)) {
        match ana.tokens[i].kind {
            k if k == close_k => depth += 1,
            k if k == open_k => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            TokKind::Period => return None,
            _ => {}
        }
    }
    None
}

/// Number of arguments of the call whose `(` is `open`; `None` if it is not closed.
pub fn arg_count(ana: &Analysis, open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut commas = 0;
    let mut any = false;
    for i in (open..ana.tokens.len()).filter(|&i| ana.is_significant(i)) {
        match ana.tokens[i].kind {
            TokKind::LParen | TokKind::LBracket => depth += 1,
            TokKind::RParen | TokKind::RBracket => {
                depth -= 1;
                if depth == 0 {
                    return Some(if any { commas + 1 } else { 0 });
                }
            }
            TokKind::Comma if depth == 1 => commas += 1,
            TokKind::Period => return None,
            _ => {}
        }
        if i != open && depth >= 1 {
            any = true;
        }
    }
    None
}

/// Argument count when the name at `name_ti` is followed by `(...)`.
pub fn call_argc(ana: &Analysis, name_ti: usize) -> Option<usize> {
    let n = next_sig(ana, name_ti)?;
    (ana.tokens[n].kind == TokKind::LParen)
        .then(|| arg_count(ana, n))
        .flatten()
}

// ---- expression types ---------------------------------------------------

/// The class a symbol holds or returns (or the class itself for a type name).
pub fn type_of_found(ws: &Workspace, f: &Found) -> Option<PathBuf> {
    let (p, a, i) = f;
    let s = &a.symbols[*i];
    if is_type_kind(s.kind) {
        return Some(p.clone());
    }
    s.type_name
        .as_ref()
        .and_then(|t| ws.resolve_class(t, &a.usings))
}

/// Class of the expression whose last token is `end`: `obj`, `obj:Prop`,
/// `obj:Method(...)`, `Func(...)`, `NEW Cls(...)`, `CAST(x, Cls)`, a class name
/// (static access), `arr[i]`, `(expr)`, and chains of these.
pub fn expr_class(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    end: usize,
    depth: usize,
) -> Option<PathBuf> {
    if depth > 12 {
        return None;
    }
    match ana.tokens[end].kind {
        TokKind::RBracket => {
            let o = matching_open(ana, end)?;
            expr_class(ws, path, ana, ana.prev_sig(o)?, depth + 1)
        }
        TokKind::RParen => {
            let o = matching_open(ana, end)?;
            let k = ana
                .prev_sig(o)
                .filter(|&k| ana.tokens[k].kind == TokKind::Word);
            let Some(k) = k else {
                // (expr):Member
                return expr_class(ws, path, ana, ana.prev_sig(end)?, depth + 1);
            };
            let is_member = ana
                .prev_sig(k)
                .is_some_and(|p| ana.tokens[p].kind == TokKind::Member);
            if !is_member && matches!(ana.kw(k), Some("CAST" | "DYNAMIC-CAST")) {
                // CAST(expr, Type): the word after the top-level comma.
                let mut d = 0;
                for i in (o..end).filter(|&i| ana.is_significant(i)) {
                    match ana.tokens[i].kind {
                        TokKind::LParen | TokKind::LBracket => d += 1,
                        TokKind::RParen | TokKind::RBracket => d -= 1,
                        TokKind::Comma if d == 1 => {
                            let t = next_sig(ana, i)
                                .filter(|&t| ana.tokens[t].kind == TokKind::Word)?;
                            return ws.resolve_class(ana.tok_text(t), &ana.usings);
                        }
                        _ => {}
                    }
                }
                return None;
            }
            word_class(ws, path, ana, k, arg_count(ana, o), depth)
        }
        TokKind::Word => word_class(ws, path, ana, end, None, depth),
        _ => None,
    }
}

fn word_class(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    k: usize,
    argc: Option<usize>,
    depth: usize,
) -> Option<PathBuf> {
    let name = ana.tok_text(k);
    if let Some(prev) = ana.prev_sig(k) {
        let p = &ana.tokens[prev];
        if p.kind == TokKind::Member && p.end == ana.tokens[k].start {
            let recv = ana.prev_sig(prev)?;
            let cls = expr_class(ws, path, ana, recv, depth + 1)?;
            let cands = members_named(ws, &cls, name);
            let f = if argc.is_some() && depth < 4 {
                pick_at(ws, path, ana, cands, k)
            } else {
                pick(cands, argc)
            }?;
            return type_of_found(ws, &f);
        }
        if ana.kw(prev) == Some("NEW") {
            return ws.resolve_class(name, &ana.usings);
        }
    }
    match name.to_ascii_uppercase().as_str() {
        "THIS-OBJECT" => return ana.class_name.as_ref().map(|_| path.to_path_buf()),
        "SUPER" => return supers(ws, ana).class.and_then(Result::ok),
        _ => {}
    }
    if let Some(f) = lookup(ws, path, ana, name, ana.tokens[k].start) {
        let f = if argc.is_some() && f.1.symbols[f.2].kind == SymKind::Method {
            let cands = members_named(ws, &f.0, name);
            if depth < 4 {
                pick_at(ws, path, ana, cands, k)
            } else {
                pick(cands, argc)
            }
            .unwrap_or(f)
        } else {
            f
        };
        return type_of_found(ws, &f);
    }
    ws.resolve_class(name, &ana.usings)
}

// ---- overrides -------------------------------------------------------------

fn same_signature(a: &Found, name: &str, argc: usize) -> bool {
    let s = &a.1.symbols[a.2];
    s.kind == SymKind::Method && s.name.eq_ignore_ascii_case(name) && param_count(&a.1, a.2) == argc
}

/// Methods with the same name and parameter count declared in classes that are
/// `class_path` or inherit from it, across the workspace.
fn overriders(ws: &Workspace, class_path: &Path, name: &str, argc: usize) -> Vec<Found> {
    let mut out = Vec::new();
    let files: Vec<PathBuf> = ws
        .index()
        .map(|(p, _)| p.clone())
        .filter(|p| crate::workspace::ext(p) == "cls")
        .collect();
    for p in files {
        if !is_subtype(ws, &p, class_path) {
            continue;
        }
        let Some(a) = ws.get(&p) else { continue };
        out.extend(
            file_members(ws, &p, &a)
                .into_iter()
                .filter(|f| same_signature(f, name, argc)),
        );
    }
    out
}

/// The method `f`, the methods it overrides and the methods overriding any of them.
pub fn override_family(ws: &Workspace, f: &Found) -> Vec<Found> {
    let s = &f.1.symbols[f.2];
    if s.kind != SymKind::Method || s.is_static || is_private(s) {
        return vec![f.clone()];
    }
    let (name, argc) = (s.name.clone(), param_count(&f.1, f.2));
    let up: Vec<Found> = members_named(ws, &f.0, &name)
        .into_iter()
        .filter(|m| same_signature(m, &name, argc))
        .collect();
    let mut out: Vec<Found> = vec![f.clone()];
    let mut seen = HashSet::from([(key(&f.0), s.sel_start)]);
    let mut bases: Vec<PathBuf> = up.iter().map(|m| m.0.clone()).collect();
    bases.push(f.0.clone());
    let mut done = HashSet::new();
    for b in bases {
        if !done.insert(key(&b)) {
            continue;
        }
        for m in up.iter().cloned().chain(overriders(ws, &b, &name, argc)) {
            if seen.insert((key(&m.0), m.1.symbols[m.2].sel_start)) {
                out.push(m);
            }
        }
    }
    out
}

//! Handle kinds of expressions: `SESSION`, `BUFFER Customer`, `hBuf:BUFFER-FIELD("x")`,
//! and HANDLE variables, whose kind comes from `CREATE QUERY h` or `h = <expr>`.

use std::path::Path;
use std::sync::Arc;

use crate::analysis::{Analysis, SymKind};
use crate::langref;
use crate::lexer::TokKind;
use crate::workspace::Workspace;

/// Keywords that turn a static name into a handle: `BUFFER Customer:`, `FRAME f:`.
fn static_kind(kw: &str) -> Option<&'static str> {
    Some(match kw {
        "BUFFER" => "BUFFER",
        "TEMP-TABLE" => "TEMP-TABLE",
        "DATASET" => "DATASET",
        "QUERY" => "QUERY",
        "DATA-SOURCE" => "DATA-SOURCE",
        "FRAME" | "BROWSE" | "MENU" | "WINDOW" => "WIDGET",
        _ => return None,
    })
}

/// Handle kind made by `CREATE <word> h`.
fn created_kind(word: &str) -> Option<&'static str> {
    Some(match word.to_ascii_uppercase().as_str() {
        "BUFFER" => "BUFFER",
        "QUERY" => "QUERY",
        "TEMP-TABLE" => "TEMP-TABLE",
        "DATASET" => "DATASET",
        "DATA-SOURCE" => "DATA-SOURCE",
        "X-DOCUMENT" => "X-DOCUMENT",
        "X-NODEREF" => "X-NODEREF",
        "SERVER" => "SERVER",
        "BUTTON" | "FRAME" | "WINDOW" | "FILL-IN" | "TOGGLE-BOX" | "COMBO-BOX" | "EDITOR"
        | "BROWSE" | "IMAGE" | "TEXT" | "SELECTION-LIST" | "RADIO-SET" | "RECTANGLE" | "MENU"
        | "SUB-MENU" | "MENU-ITEM" | "DIALOG-BOX" => "WIDGET",
        _ => return None,
    })
}

/// Does a declared type mean "handle"?
fn is_handle_type(detail: &str) -> bool {
    detail.split_whitespace().any(|w| {
        matches!(
            w,
            "HANDLE" | "WIDGET-HANDLE" | "TABLE-HANDLE" | "DATASET-HANDLE"
        )
    })
}

/// Handle kind of the expression ending at token `end`; `*` when it is a handle of unknown kind.
pub fn handle_kind(ws: &Workspace, path: &Path, ana: &Arc<Analysis>, end: usize) -> Option<String> {
    kind_of(ws, path, ana, end, &mut Vec::new())
}

fn kind_of(
    ws: &Workspace,
    path: &Path,
    ana: &Arc<Analysis>,
    end: usize,
    stack: &mut Vec<String>,
) -> Option<String> {
    if stack.len() > 8 {
        return None;
    }
    let k = match ana.tokens[end].kind {
        TokKind::RParen | TokKind::RBracket => {
            let o = crate::oo::matching_open(ana, end)?;
            ana.prev_sig(o)
                .filter(|&k| ana.tokens[k].kind == TokKind::Word)?
        }
        TokKind::Word => end,
        _ => return None,
    };
    let word = ana.tok_text(k);
    if let Some(prev) = ana.prev_sig(k) {
        let p = &ana.tokens[prev];
        if p.kind == TokKind::Member && p.end == ana.tokens[k].start {
            let recv = ana.prev_sig(prev)?;
            // A class member typed HANDLE: obj:Prop:...
            if let Some(cls) = crate::oo::expr_class(ws, path, ana, recv, 0) {
                let f = crate::oo::members_named(ws, &cls, word)
                    .into_iter()
                    .next()?;
                return is_handle_type(&f.1.symbols[f.2].detail).then(|| "*".to_string());
            }
            let rk = kind_of(ws, path, ana, recv, stack)?;
            let m = langref::member(&rk, word)?;
            return match (m.returns_handle(), m.return_type()) {
                (Some(h), _) => Some(h.to_string()),
                (None, Some("HANDLE")) => Some("*".into()),
                _ => None,
            };
        }
        if let Some(sk) = ana.kw(prev).and_then(static_kind) {
            if ana.tokens[end].kind == TokKind::Word {
                return Some(sk.to_string());
            }
        }
    }
    if ana.tokens[end].kind != TokKind::Word {
        return None;
    }
    let found = crate::features::lookup(ws, path, ana, word, ana.tokens[k].start);
    let Some((p, a, i)) = found else {
        return langref::system_handle(word).map(|(kind, _)| kind.to_string());
    };
    let s = &a.symbols[i];
    if !matches!(
        s.kind,
        SymKind::Variable | SymKind::Parameter | SymKind::Property
    ) || !is_handle_type(&s.detail)
    {
        return None;
    }
    if s.detail.contains("TABLE-HANDLE") {
        return Some("TEMP-TABLE".into());
    }
    if s.detail.contains("DATASET-HANDLE") {
        return Some("DATASET".into());
    }
    let key = format!(
        "{}:{}",
        crate::workspace::key(&p),
        s.name.to_ascii_lowercase()
    );
    if stack.contains(&key) {
        return None;
    }
    stack.push(key);
    let r = assigned_kind(ws, &p, &a, &s.name.clone(), stack);
    stack.pop();
    Some(r.unwrap_or_else(|| "*".into()))
}

/// Kind of handle `name` gets from `CREATE <kind> name` or `name = <expr>` in `a`.
fn assigned_kind(
    ws: &Workspace,
    p: &Path,
    a: &Arc<Analysis>,
    name: &str,
    stack: &mut Vec<String>,
) -> Option<String> {
    let is_name =
        |t: usize| a.tokens[t].kind == TokKind::Word && a.tok_text(t).eq_ignore_ascii_case(name);
    let is_eq = |t: usize| a.tokens[t].kind == TokKind::Operator && a.tok_text(t) == "=";
    for st in &a.statements {
        let toks: Vec<usize> = a.stmt_tokens(st).collect();
        let first = a.kw(toks[0]);
        if first == Some("CREATE") && toks.len() >= 3 && is_name(toks[2]) {
            if let Some(k) = created_kind(a.tok_text(toks[1])) {
                return Some(k.into());
            }
        }
        let assign = first == Some("ASSIGN");
        for j in 0..toks.len().saturating_sub(2) {
            if !(is_name(toks[j]) && is_eq(toks[j + 1]) && (j == 0 || (assign && j >= 1))) {
                continue;
            }
            // The value runs to the next `x =` of an ASSIGN, or to the end of the statement.
            let mut depth = 0i32;
            let mut stop = toks.len();
            for (k, &t) in toks.iter().enumerate().skip(j + 2) {
                match a.tokens[t].kind {
                    TokKind::LParen | TokKind::LBracket => depth += 1,
                    TokKind::RParen | TokKind::RBracket => depth -= 1,
                    TokKind::Word
                        if depth == 0 && assign && toks.get(k + 1).is_some_and(|&n| is_eq(n)) =>
                    {
                        stop = k;
                        break;
                    }
                    _ => {}
                }
                if depth == 0 && matches!(a.kw(t), Some("NO-ERROR" | "WHEN")) {
                    stop = k;
                    break;
                }
            }
            if stop <= j + 2 {
                continue;
            }
            if let Some(k) = kind_of(ws, p, a, toks[stop - 1], stack).filter(|k| k != "*") {
                return Some(k);
            }
        }
    }
    None
}

//! Document formatting: re-indents by block structure, trims trailing white space,
//! and optionally sets the case of keywords.
//!
//! - A statement is indented one level per enclosing block; `END` lines up with
//!   its block header.
//! - Continuation lines of a multi-line statement keep their offset from the first
//!   line (at least one level deeper).
//! - Comment and include lines between statements take the depth of the code
//!   around them; the inner lines of a block comment move with its first line.
//! - Preprocessor directives, inactive `&IF` branches and the inside of
//!   multi-line strings are left as they are.

use std::collections::{HashMap, HashSet};

use lsp_types::TextEdit;

use crate::analysis::Analysis;
use crate::keywords::keyword;
use crate::lexer::TokKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KeywordCase {
    #[default]
    Preserve,
    Upper,
    Lower,
}

impl KeywordCase {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "upper" => KeywordCase::Upper,
            "lower" => KeywordCase::Lower,
            _ => KeywordCase::Preserve,
        }
    }
}

pub struct Options {
    pub tab_size: usize,
    pub insert_spaces: bool,
    pub keyword_case: KeywordCase,
}

fn columns(ws: &str, tab: usize) -> usize {
    ws.chars().fold(0, |c, ch| {
        if ch == '\t' {
            (c / tab + 1) * tab
        } else {
            c + 1
        }
    })
}

/// Formatting edits for the lines `first..=last` (all lines when `None`).
/// `names` are identifiers that must keep their case even if they look like keywords.
pub fn format(
    ana: &Analysis,
    opts: &Options,
    lines: Option<(usize, usize)>,
    names: &HashSet<String>,
) -> Vec<TextEdit> {
    let text = &ana.text;
    let tab = opts.tab_size.max(1);
    let unit = if opts.insert_spaces {
        " ".repeat(tab)
    } else {
        "\t".to_string()
    };
    let indent_str = |cols: usize| -> String {
        if opts.insert_spaces {
            " ".repeat(cols)
        } else {
            "\t".repeat(cols / tab) + &" ".repeat(cols % tab)
        }
    };
    let unit_cols = columns(&unit, tab);

    // Line starts.
    let mut starts = vec![0usize];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    let line_end = |l: usize| -> usize {
        let e = starts.get(l + 1).map_or(text.len(), |&s| s - 1);
        if e > starts[l] && text.as_bytes()[e - 1] == b'\r' {
            e - 1
        } else {
            e
        }
    };

    // Statement of each token, and the depth from the blocks around an offset.
    let mut stmt_of: HashMap<usize, usize> = HashMap::new();
    for (si, st) in ana.statements.iter().enumerate() {
        for t in st.first..=st.term.unwrap_or(st.last) {
            stmt_of.insert(t, si);
        }
    }
    // Property accessors sit one level inside their DEFINE PROPERTY, and so do their bodies:
    // ranges whose statements get one extra level.
    let mut accessor: Vec<(usize, usize)> = Vec::new();
    let mut prop_depth: Option<usize> = None;
    let block_end = |start: usize| {
        ana.blocks
            .iter()
            .find(|(s, _)| *s == start)
            .map(|(_, e)| *e)
    };
    for st in &ana.statements {
        let toks: Vec<usize> = ana.stmt_tokens(st).collect();
        let start = ana.tokens[st.first].start;
        let stmt_end = ana.tokens[st.term.unwrap_or(st.last)].end;
        let mut k = 0;
        while matches!(
            toks.get(k).and_then(|&t| ana.kw(t)),
            Some("PUBLIC" | "PROTECTED" | "PRIVATE" | "PACKAGE-PRIVATE" | "PACKAGE-PROTECTED")
        ) {
            k += 1;
        }
        let is_accessor = prop_depth == Some(st.depth)
            && matches!(toks.get(k).and_then(|&t| ana.kw(t)), Some("GET" | "SET"));
        if is_accessor {
            accessor.push((
                start,
                if st.header {
                    block_end(start).unwrap_or(stmt_end)
                } else {
                    stmt_end
                },
            ));
        } else if ana.kw(st.first) == Some("DEFINE")
            && toks.iter().take(6).any(|&t| ana.kw(t) == Some("PROPERTY"))
        {
            prop_depth = Some(st.depth);
            // DEFINE PROPERTY x AS y NO-UNDO <newline> GET(): the body is inside the GET line.
            let acc = toks
                .iter()
                .rev()
                .find(|&&t| matches!(ana.kw(t), Some("GET" | "SET")));
            if let (true, Some(&a)) = (st.header, acc) {
                if ana.lines.line_of(ana.tokens[a].start) != ana.lines.line_of(start) {
                    accessor.push((stmt_end, block_end(start).unwrap_or(stmt_end)));
                }
            }
        } else if ana.kw(st.first) != Some("END") && prop_depth.is_some_and(|d| st.depth <= d) {
            prop_depth = None;
        }
    }
    let bonus = |off: usize| {
        accessor
            .iter()
            .filter(|(s, e)| *s <= off && off <= *e)
            .count()
    };
    let block_depth = |off: usize| {
        ana.blocks
            .iter()
            .filter(|(s, e)| *s < off && off < *e)
            .count()
            + bonus(off)
    };

    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    // New indent (in columns) of each line that starts a statement, by statement.
    // stmt -> (old cols, new cols)
    let mut stmt_new: HashMap<usize, (usize, usize)> = HashMap::new();
    // Column change of a multi-line comment, for its inner lines.
    let mut comment_delta: Vec<(usize, usize, isize)> = Vec::new();
    for (l, &ls) in starts.iter().enumerate() {
        let le = line_end(l);
        let content = &text[ls..le];
        let lead_len = content.len() - content.trim_start_matches([' ', '\t']).len();
        let old_cols = columns(&content[..lead_len], tab);
        let in_range = lines.is_none_or(|(a, b)| l >= a && l <= b);
        // Tokens do not overlap, so only the last one starting before the line can run into it.
        let before = ana.tokens.partition_point(|t| t.start < ls);
        let spanning = before
            .checked_sub(1)
            .map(|i| &ana.tokens[i])
            .filter(|t| t.end > ls);
        let mut new_cols: Option<usize> = None;
        if let Some(t) = spanning {
            // Inside a block comment: move with its first line. Strings and others: untouched.
            if t.kind == TokKind::Comment {
                if let Some(&(_, _, d)) = comment_delta
                    .iter()
                    .find(|(s, e, _)| *s == t.start && *e == t.end)
                {
                    new_cols = Some((old_cols as isize + d).max(0) as usize);
                }
            }
        } else if lead_len < content.len() {
            // First token on this line.
            let fi = ana.tokens.partition_point(|t| t.start < ls + lead_len);
            if fi < ana.tokens.len() && ana.tokens[fi].start < le {
                let t = &ana.tokens[fi];
                if t.kind == TokKind::Preproc || t.inactive {
                    // directives and code in branches not taken: as written
                } else if let Some(&si) = stmt_of.get(&fi) {
                    let st = &ana.statements[si];
                    if st.first == fi {
                        let n = (st.depth + bonus(t.start)) * unit_cols;
                        stmt_new.insert(si, (old_cols, n));
                        new_cols = Some(n);
                    } else if let Some(&(first_old, first_new)) = stmt_new.get(&si) {
                        // `IF x THEN` / `ON CHOOSE OF b` with DO: on the next line: DO lines up with them.
                        let lone_do = st.header && fi == st.last && ana.kw(fi) == Some("DO");
                        new_cols = Some(if lone_do {
                            first_new
                        } else if old_cols > first_old {
                            first_new + (old_cols - first_old)
                        } else {
                            first_new + unit_cols
                        });
                    }
                } else {
                    // A comment or include between statements: the depth of the code around it.
                    let n = block_depth(t.start) * unit_cols;
                    if t.kind == TokKind::Comment && t.end > le {
                        comment_delta.push((t.start, t.end, n as isize - old_cols as isize));
                    }
                    new_cols = Some(n);
                }
            }
        }
        if !in_range {
            continue;
        }
        if let Some(n) = new_cols {
            let want = indent_str(n);
            if content[..lead_len] != want {
                edits.push((ls, ls + lead_len, want));
            }
        }
        // Trailing white space, unless the line ends inside a string or comment.
        let trimmed = content.trim_end_matches([' ', '\t']);
        if trimmed.len() < content.len() && trimmed.len() >= lead_len {
            let cut = ls + trimmed.len();
            let inside = ana
                .tokens
                .iter()
                .any(|t| t.start < cut && t.end > le && t.subst.is_none());
            if !inside {
                edits.push((cut, le, String::new()));
            }
        }
    }

    if opts.keyword_case != KeywordCase::Preserve {
        for i in 0..ana.tokens.len() {
            let t = &ana.tokens[i];
            if t.kind != TokKind::Word || t.subst.is_some() || !ana.is_significant(i) {
                continue;
            }
            let l = ana.lines.line_of(t.start);
            if lines.is_some_and(|(a, b)| l < a || l > b) {
                continue;
            }
            let word = &text[t.start..t.end];
            if word.contains('.')
                || keyword(word).is_none()
                || names.contains(&word.to_ascii_lowercase())
            {
                continue;
            }
            // obj:Add, RUN init, NEW Thing: names, not keywords.
            let prev = ana.prev_sig(i);
            if prev.is_some_and(|p| ana.tokens[p].kind == TokKind::Member)
                || matches!(prev.and_then(|p| ana.kw(p)), Some("RUN" | "NEW"))
            {
                continue;
            }
            let want = if opts.keyword_case == KeywordCase::Upper {
                word.to_ascii_uppercase()
            } else {
                word.to_ascii_lowercase()
            };
            if want != word {
                edits.push((t.start, t.end, want));
            }
        }
    }
    // An indent inserted where a keyword is re-cased becomes one edit.
    edits.sort_by_key(|e| (e.0, e.1));
    let mut merged: Vec<(usize, usize, String)> = Vec::new();
    for e in edits {
        match merged.last_mut() {
            Some(last) if last.1 == e.0 && last.0 == last.1 => {
                last.1 = e.1;
                last.2.push_str(&e.2);
            }
            _ => merged.push(e),
        }
    }
    merged
        .into_iter()
        .map(|(s, e, t)| TextEdit {
            range: ana.lines.range(&ana.text, s, e),
            new_text: t,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::analyze;

    fn apply(src: &str, opts: &Options) -> String {
        let a = analyze(src.to_string(), false);
        let mut edits: Vec<(usize, usize, String)> = format(&a, opts, None, &HashSet::new())
            .into_iter()
            .map(|e| {
                (
                    a.lines.offset(&a.text, e.range.start),
                    a.lines.offset(&a.text, e.range.end),
                    e.new_text,
                )
            })
            .collect();
        edits.sort_by_key(|x| std::cmp::Reverse(x.0));
        let mut out = src.to_string();
        for (s, e, t) in edits {
            out.replace_range(s..e, &t);
        }
        out
    }

    const SPACES: Options = Options {
        tab_size: 4,
        insert_spaces: true,
        keyword_case: KeywordCase::Preserve,
    };

    #[test]
    fn reindents_blocks_and_continuations() {
        let src = "PROCEDURE p:   \n  DEFINE VARIABLE i AS INTEGER NO-UNDO.\nDO i = 1 TO 3:\n/* note */\nIF i > 1 THEN\nMESSAGE i\n        \"x\".\n      END.\nCASE i:\nWHEN 1 THEN DO:\nEND.\nEND CASE.\nEND PROCEDURE.\n";
        let want = "PROCEDURE p:\n    DEFINE VARIABLE i AS INTEGER NO-UNDO.\n    DO i = 1 TO 3:\n        /* note */\n        IF i > 1 THEN\n            MESSAGE i\n                \"x\".\n    END.\n    CASE i:\n        WHEN 1 THEN DO:\n        END.\n    END CASE.\nEND PROCEDURE.\n";
        assert_eq!(apply(src, &SPACES), want);
        // Formatting is stable.
        assert_eq!(apply(want, &SPACES), want);
    }

    #[test]
    fn property_accessors_and_lone_do() {
        let src = "CLASS c:
DEFINE PUBLIC PROPERTY A AS INTEGER NO-UNDO
GET.
PRIVATE SET.
DEFINE PUBLIC PROPERTY B AS INTEGER NO-UNDO
GET():
RETURN 1.
END GET.
SET(INPUT v AS INTEGER):
END SET.
METHOD PUBLIC VOID m():
IF TRUE THEN
DO:
END.
END METHOD.
END CLASS.
";
        let want = "CLASS c:
    DEFINE PUBLIC PROPERTY A AS INTEGER NO-UNDO
        GET.
        PRIVATE SET.
    DEFINE PUBLIC PROPERTY B AS INTEGER NO-UNDO
        GET():
            RETURN 1.
        END GET.
        SET(INPUT v AS INTEGER):
        END SET.
    METHOD PUBLIC VOID m():
        IF TRUE THEN
        DO:
        END.
    END METHOD.
END CLASS.
";
        assert_eq!(apply(src, &SPACES), want);
        assert_eq!(apply(want, &SPACES), want);
    }

    #[test]
    fn leaves_strings_directives_and_comment_bodies() {
        let src = "DO:\nx = \"line one\n   line two\".\n&IF DEFINED(A) &THEN\n&ENDIF\n  /* a\n     b */\nEND.\n";
        let want = "DO:\n    x = \"line one\n   line two\".\n&IF DEFINED(A) &THEN\n&ENDIF\n    /* a\n       b */\nEND.\n";
        assert_eq!(apply(src, &SPACES), want);
    }

    #[test]
    fn keyword_case_and_tabs() {
        let opts = Options {
            tab_size: 4,
            insert_spaces: false,
            keyword_case: KeywordCase::Upper,
        };
        let src = "do:\n  oList:add(1).\n  def var x as int no-undo.\nend.\n";
        assert_eq!(
            apply(src, &opts),
            "DO:\n\toList:add(1).\n\tDEF VAR x AS INT NO-UNDO.\nEND.\n"
        );
    }
}

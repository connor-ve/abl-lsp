//! Reads OpenEdge `.df` schema dumps (Data Dictionary > Dump Data Definitions).
//!
//! The result is an [`Analysis`] whose symbols are database tables (with field
//! and index children), so lookup, hover, completion and go-to-definition treat
//! database tables exactly like temp-tables, and locations point into the .df.
//!
//! Recognised statements (attributes are the indented lines that follow):
//!   ADD TABLE "Customer"              LABEL, DESCRIPTION, DUMP-NAME
//!   ADD FIELD "Name" OF "Customer" AS character    LABEL, FORMAT, INITIAL, DESCRIPTION, EXTENT
//!   ADD INDEX "Name" ON "Customer"    PRIMARY, UNIQUE, INDEX-FIELD "x" ASCENDING

use std::collections::HashMap;

use crate::analysis::{Analysis, SymKind, Symbol};

/// First double-quoted string in `s` (with `""` escapes): (offset of content, content).
fn quoted(s: &str) -> Option<(usize, String)> {
    let open = s.find('"')?;
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = open + 1;
    while i < b.len() {
        if b[i] == b'"' {
            if b.get(i + 1) == Some(&b'"') {
                out.push('"');
                i += 2;
                continue;
            }
            return Some((open + 1, out));
        }
        let ch = s[i..].chars().next()?;
        out.push(ch);
        i += ch.len_utf8();
    }
    None
}

fn sym(
    name: String,
    kind: SymKind,
    line_start: usize,
    line_end: usize,
    sel: usize,
    parent: Option<usize>,
) -> Symbol {
    let sel_end = sel + name.len();
    Symbol {
        name,
        kind,
        detail: String::new(),
        type_name: None,
        decl: String::new(),
        start: line_start,
        end: line_end,
        sel_start: sel,
        sel_end,
        scope: 0,
        parent,
        access: None,
        is_static: false,
        proto: false,
        is_override: false,
    }
}

pub fn analyze_df(text: String, db: &str) -> Analysis {
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut tables: HashMap<String, usize> = HashMap::new();
    let mut cur: Option<usize> = None;
    let mut index_fields: HashMap<usize, Vec<String>> = HashMap::new();

    let mut off = 0;
    for raw in text.split_inclusive('\n') {
        let line_start = off;
        off += raw.len();
        let line = raw.trim_end_matches(['\r', '\n']);
        let line_end = line_start + line.len();
        if line == "PSC" {
            break; // trailer
        }

        if let Some(rest) = line.strip_prefix("ADD TABLE ") {
            cur = None;
            if let Some((q, name)) = quoted(rest) {
                let sel = line_start + "ADD TABLE ".len() + q;
                let mut s = sym(
                    name.clone(),
                    SymKind::Table,
                    line_start,
                    line_end,
                    sel,
                    None,
                );
                s.detail = db.to_string();
                s.decl = format!("TABLE {db}.{name}");
                symbols.push(s);
                tables.insert(name.to_ascii_lowercase(), symbols.len() - 1);
                cur = Some(symbols.len() - 1);
            }
        } else if let Some(rest) = line.strip_prefix("ADD FIELD ") {
            cur = None;
            let Some((q, name)) = quoted(rest) else {
                continue;
            };
            let after = &rest[q + name.len() + 1..];
            let table = after
                .find(" OF ")
                .and_then(|p| quoted(&after[p..]))
                .map(|(_, t)| t)
                .unwrap_or_default();
            let ty = after
                .find(" AS ")
                .map(|p| {
                    after[p + 4..]
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                })
                .unwrap_or_default();
            let Some(&parent) = tables.get(&table.to_ascii_lowercase()) else {
                continue;
            };
            let sel = line_start + "ADD FIELD ".len() + q;
            let mut s = sym(
                name.clone(),
                SymKind::Field,
                line_start,
                line_end,
                sel,
                Some(parent),
            );
            s.decl = format!("FIELD {}.{} AS {}", symbols[parent].name, name, ty);
            s.detail = ty;
            symbols.push(s);
            cur = Some(symbols.len() - 1);
        } else if let Some(rest) = line.strip_prefix("ADD INDEX ") {
            cur = None;
            let Some((q, name)) = quoted(rest) else {
                continue;
            };
            let after = &rest[q + name.len() + 1..];
            let table = after
                .find(" ON ")
                .and_then(|p| quoted(&after[p..]))
                .map(|(_, t)| t)
                .unwrap_or_default();
            let Some(&parent) = tables.get(&table.to_ascii_lowercase()) else {
                continue;
            };
            let sel = line_start + "ADD INDEX ".len() + q;
            let mut s = sym(
                name.clone(),
                SymKind::Index,
                line_start,
                line_end,
                sel,
                Some(parent),
            );
            s.decl = format!("INDEX {} ON {}", name, symbols[parent].name);
            symbols.push(s);
            cur = Some(symbols.len() - 1);
        } else if line.starts_with([' ', '\t']) {
            let Some(ci) = cur else { continue };
            let t = line.trim_start();
            let key = t
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_uppercase();
            let s = &mut symbols[ci];
            match (s.kind, key.as_str()) {
                (_, "LABEL" | "DESCRIPTION")
                | (SymKind::Field, "FORMAT" | "INITIAL" | "EXTENT" | "MANDATORY") => {
                    s.decl.push_str("\n  ");
                    s.decl.push_str(t);
                }
                (SymKind::Index, "PRIMARY" | "UNIQUE" | "WORD") => {
                    s.decl.push(' ');
                    s.decl.push_str(&key);
                }
                (SymKind::Index, "INDEX-FIELD") => {
                    if let Some((_, f)) = quoted(t) {
                        index_fields.entry(ci).or_default().push(f);
                    }
                }
                _ => {}
            }
        } else if !line.trim().is_empty() {
            cur = None; // UPDATE / RENAME / DROP / ADD SEQUENCE ...: not tracked
        }
    }
    for (i, fields) in index_fields {
        symbols[i].detail = fields.join(", ");
        symbols[i]
            .decl
            .push_str(&format!("\n  fields: {}", fields.join(", ")));
    }

    let mut out = Analysis::empty(text);
    out.symbols = symbols;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DF: &str = "ADD TABLE \"Customer\"\n  AREA \"Schema Area\"\n  LABEL \"Customer\"\n  DESCRIPTION \"Customer master\"\n  DUMP-NAME \"customer\"\n\nADD FIELD \"CustNum\" OF \"Customer\" AS integer \n  FORMAT \">>>>9\"\n  INITIAL \"0\"\n  LABEL \"Cust Num\"\n  POSITION 2\n\nADD FIELD \"Name\" OF \"Customer\" AS character \n  FORMAT \"x(30)\"\n\nADD INDEX \"CustNum\" ON \"Customer\" \n  AREA \"Schema Area\"\n  UNIQUE\n  PRIMARY\n  INDEX-FIELD \"CustNum\" ASCENDING \n\n.\nPSC\ncpstream=UTF-8\n.\n0000000999\n";

    #[test]
    fn parses_tables_fields_and_indexes() {
        let a = analyze_df(DF.to_string(), "shop");
        let names: Vec<(&str, SymKind)> = a
            .symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind))
            .collect();
        assert_eq!(
            names,
            vec![
                ("Customer", SymKind::Table),
                ("CustNum", SymKind::Field),
                ("Name", SymKind::Field),
                ("CustNum", SymKind::Index)
            ]
        );
        let f = &a.symbols[1];
        assert_eq!(f.parent, Some(0));
        assert_eq!(f.detail, "INTEGER");
        assert!(f.decl.contains("FIELD Customer.CustNum AS INTEGER"));
        assert!(f.decl.contains("LABEL \"Cust Num\""));
        assert_eq!(&a.text[f.sel_start..f.sel_end], "CustNum");
        assert!(a.symbols[3].decl.contains("UNIQUE PRIMARY"));
        assert!(a.symbols[0].decl.contains("Customer master"));
    }
}

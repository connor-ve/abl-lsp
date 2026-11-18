//! `abl db schema`: the tables, fields, indexes and sequences of the project's
//! databases, read from their `.df` files. With `--json`, the data the VS Code
//! database browser shows.

use std::fs;
use std::path::Path;

use abl_core::df::{self, Attr};
use abl_oe::config::{self, Loaded};
use serde_json::{json, Value};

/// A value as written in a .df, without its quotes: `"Cust Num"` -> `Cust Num`.
fn unquote(v: &str) -> String {
    let v = v.trim();
    if let Some(rest) = v.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    out.push('"');
                }
                '"' => return out,
                c => out.push(c),
            }
        }
        out
    } else {
        v.to_string()
    }
}

fn attr(attrs: &[Attr], key: &str) -> Option<String> {
    attrs
        .iter()
        .find(|a| a.key == key)
        .map(|a| unquote(&a.value))
        .filter(|v| !v.is_empty() && v != "?")
}

fn flag(attrs: &[Attr], key: &str) -> bool {
    attrs.iter().any(|a| a.key == key)
}

/// 1-based line of the first line starting with `prefix` (case-insensitive).
fn line_of(lines: &[String], prefix: &str) -> usize {
    let p = prefix.to_ascii_uppercase();
    lines
        .iter()
        .position(|l| l.starts_with(&p))
        .map_or(0, |i| i + 1)
}

fn q(name: &str) -> String {
    format!("\"{}\"", name.to_ascii_uppercase())
}

pub fn schema_json(name: &str, text: &str) -> (Value, Vec<String>) {
    let s = df::parse(text);
    let lines: Vec<String> = text
        .lines()
        .map(|l| l.trim_start().to_ascii_uppercase())
        .collect();
    let tables: Vec<Value> = s
        .tables
        .iter()
        .map(|t| {
            let in_index = |f: &str| -> Vec<&str> {
                t.indexes
                    .iter()
                    .filter(|i| i.fields.iter().any(|x| x.name.eq_ignore_ascii_case(f)))
                    .map(|i| i.name.as_str())
                    .collect()
            };
            let mut fields: Vec<Value> = t
                .fields
                .iter()
                .map(|f| {
                    let order: i64 = attr(&f.attrs, "ORDER")
                        .and_then(|o| o.parse().ok())
                        .unwrap_or(0);
                    json!({
                        "name": f.name,
                        "type": f.data_type.to_ascii_lowercase(),
                        "format": attr(&f.attrs, "FORMAT"),
                        "label": attr(&f.attrs, "LABEL"),
                        "columnLabel": attr(&f.attrs, "COLUMN-LABEL"),
                        "initial": attr(&f.attrs, "INITIAL"),
                        "help": attr(&f.attrs, "HELP"),
                        "description": attr(&f.attrs, "DESCRIPTION"),
                        "extent": attr(&f.attrs, "EXTENT").and_then(|e| e.parse::<u32>().ok()).unwrap_or(0),
                        "decimals": attr(&f.attrs, "DECIMALS").and_then(|e| e.parse::<u32>().ok()),
                        "mandatory": flag(&f.attrs, "MANDATORY"),
                        "caseSensitive": flag(&f.attrs, "CASE-SENSITIVE"),
                        "order": order,
                        "indexes": in_index(&f.name),
                        "line": line_of(&lines, &format!("ADD FIELD {} OF {}", q(&f.name), q(&t.name))),
                    })
                })
                .collect();
            fields.sort_by_key(|f| f["order"].as_i64().unwrap_or(0));
            let indexes: Vec<Value> = t
                .indexes
                .iter()
                .map(|i| {
                    json!({
                        "name": i.name,
                        "primary": flag(&i.attrs, "PRIMARY"),
                        "unique": flag(&i.attrs, "UNIQUE"),
                        "word": flag(&i.attrs, "WORD"),
                        "inactive": flag(&i.attrs, "INACTIVE"),
                        "area": attr(&i.attrs, "AREA"),
                        "fields": i.fields.iter().map(|f| json!({
                            "name": f.name,
                            "descending": f.options.to_ascii_uppercase().contains("DESC"),
                        })).collect::<Vec<_>>(),
                        "line": line_of(&lines, &format!("ADD INDEX {} ON {}", q(&i.name), q(&t.name))),
                    })
                })
                .collect();
            let triggers: Vec<String> = t
                .attrs
                .iter()
                .filter(|a| a.key == "TABLE-TRIGGER")
                .map(|a| a.value.split_whitespace().next().map(unquote).unwrap_or_default())
                .collect();
            json!({
                "name": t.name,
                "label": attr(&t.attrs, "LABEL"),
                "description": attr(&t.attrs, "DESCRIPTION"),
                "dumpName": attr(&t.attrs, "DUMP-NAME"),
                "area": attr(&t.attrs, "AREA"),
                "hidden": flag(&t.attrs, "HIDDEN"),
                "triggers": triggers,
                "fields": fields,
                "indexes": indexes,
                "line": line_of(&lines, &format!("ADD TABLE {}", q(&t.name))),
            })
        })
        .collect();
    let sequences: Vec<Value> = s
        .sequences
        .iter()
        .map(|q| {
            json!({
                "name": q.name,
                "initial": attr(&q.attrs, "INITIAL"),
                "increment": attr(&q.attrs, "INCREMENT"),
                "min": attr(&q.attrs, "MIN-VAL"),
                "max": attr(&q.attrs, "MAX-VAL"),
                "cycle": attr(&q.attrs, "CYCLE-ON-LIMIT").is_some_and(|c| c.eq_ignore_ascii_case("yes")),
            })
        })
        .collect();
    (
        json!({ "name": name, "tables": tables, "sequences": sequences }),
        s.warnings,
    )
}

pub fn run(cwd: &Path, name: Option<&str>, as_json: bool) -> Result<u8, String> {
    let l: Loaded =
        config::load_project(cwd)?.ok_or("no abl.toml here or above; run `abl init`")?;
    let mut dbs = Vec::new();
    for (n, d) in &l.project.db {
        if name.is_some_and(|w| !w.eq_ignore_ascii_case(n)) {
            continue;
        }
        let managed = d.connect.is_none();
        let state = if managed {
            crate::db::state(&l.db_path(n, d))
        } else {
            "external"
        };
        let mut v = match &d.schema {
            Some(s) => {
                let f = l.path(s);
                let text = fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
                let (mut v, warnings) = schema_json(n, &text);
                v["schemaFile"] = json!(f.display().to_string());
                v["warnings"] = json!(warnings);
                v
            }
            None => {
                json!({ "name": n, "tables": [], "sequences": [], "schemaFile": null, "warnings": [] })
            }
        };
        v["managed"] = json!(managed);
        v["state"] = json!(state);
        v["connect"] = json!(d.connect);
        dbs.push(v);
    }
    if let (Some(n), true) = (name, dbs.is_empty()) {
        return Err(format!("no database {n} in abl.toml"));
    }
    if as_json {
        let doc = json!({ "root": l.root.display().to_string(), "databases": dbs });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        return Ok(0);
    }
    if dbs.is_empty() {
        println!("No databases in abl.toml");
    }
    for d in &dbs {
        let tables = d["tables"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        println!(
            "{} ({}): {} tables, {} sequences",
            d["name"].as_str().unwrap_or(""),
            d["state"].as_str().unwrap_or(""),
            tables.len(),
            d["sequences"].as_array().map_or(0, Vec::len)
        );
        let width = tables
            .iter()
            .filter_map(|t| t["name"].as_str())
            .map(str::len)
            .max()
            .unwrap_or(0);
        for t in tables {
            println!(
                "  {:<width$}  {:>3} fields  {:>2} indexes  {}",
                t["name"].as_str().unwrap_or(""),
                t["fields"].as_array().map_or(0, Vec::len),
                t["indexes"].as_array().map_or(0, Vec::len),
                t["label"].as_str().unwrap_or("")
            );
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_df() {
        let df = "ADD TABLE \"Customer\"\n  AREA \"Data\"\n  LABEL \"Customer\"\n  DUMP-NAME \"customer\"\n  TABLE-TRIGGER \"WRITE\" OVERRIDE PROCEDURE \"trg/wrcust.p\" CRC \"?\"\n\n\
                  ADD FIELD \"Name\" OF \"Customer\" AS character\n  FORMAT \"x(30)\"\n  LABEL \"Name\"\n  ORDER 20\n  MANDATORY\n\n\
                  ADD FIELD \"CustNum\" OF \"Customer\" AS integer\n  FORMAT \">>>>9\"\n  INITIAL \"0\"\n  LABEL \"Cust \"\"Num\"\"\"\n  ORDER 10\n\n\
                  ADD INDEX \"CustNum\" ON \"Customer\"\n  AREA \"Index\"\n  UNIQUE\n  PRIMARY\n  INDEX-FIELD \"CustNum\" ASCENDING\n\n\
                  ADD INDEX \"Name\" ON \"Customer\"\n  INDEX-FIELD \"Name\" DESCENDING\n\n\
                  ADD SEQUENCE \"NextCust\"\n  INITIAL 1000\n  INCREMENT 5\n  CYCLE-ON-LIMIT no\n\n.\nPSC\ncodepage=UTF-8\n.\n";
        let (v, _) = schema_json("sports", df);
        let t = &v["tables"][0];
        assert_eq!(t["name"], "Customer");
        assert_eq!(t["dumpName"], "customer");
        assert_eq!(t["line"], 1);
        assert_eq!(t["triggers"][0], "WRITE");
        assert_eq!(t["fields"][0]["name"], "CustNum", "sorted by ORDER");
        assert_eq!(t["fields"][0]["label"], "Cust \"Num\"");
        assert_eq!(t["fields"][0]["indexes"][0], "CustNum");
        assert_eq!(t["fields"][1]["mandatory"], true);
        assert_eq!(t["fields"][1]["line"], 7);
        assert_eq!(t["indexes"][0]["primary"], true);
        assert_eq!(t["indexes"][0]["unique"], true);
        assert_eq!(t["indexes"][1]["fields"][0]["descending"], true);
        assert_eq!(v["sequences"][0]["increment"], "5");
    }
}

//! `abl query`, `abl export` and `abl import`: table data through the query
//! and import drivers, shown as a table or moved as JSON or CSV.

use std::fs;
use std::path::Path;

use abl_oe::config;
use abl_oe::db::{self, Column, Query, Rows};
use abl_oe::session::Env;
use clap::ValueEnum;
use serde_json::{Map, Number, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Table,
    Json,
    Csv,
    /// Columns, rows and whether there are more: for the VS Code browser.
    #[value(hide = true)]
    Page,
}

/// A session for data commands: needs a database to connect to.
fn env(cwd: &Path, flag: Option<&str>) -> Result<Env, String> {
    let loaded = config::load_project(cwd)?;
    if let Some(l) = &loaded {
        let missing: Vec<&str> = l
            .project
            .db
            .iter()
            .filter(|(n, d)| d.connect.is_none() && !l.db_path(n, d).is_file())
            .map(|(n, _)| n.as_str())
            .collect();
        if l.project.db.is_empty() {
            return Err("no database in abl.toml: add a [db.name] table".into());
        }
        if missing.len() == l.project.db.len() {
            return Err(format!(
                "{} isn't created yet (abl db create)",
                missing.join(", ")
            ));
        }
    }
    let env = Env::new(loaded.as_ref(), flag, cwd)?;
    if !env.params.iter().any(|p| p == "-db" || p == "-pf") {
        return Err("no database to connect to: add a [db.name] table to abl.toml".into());
    }
    Ok(env)
}

/// `FOR EACH customer WHERE x > 1`, `customer where x > 1`: (table, phrase).
pub fn parse_query(text: &str) -> Result<(String, String), String> {
    let t = text.trim().trim_end_matches(['.', ':']).trim();
    let mut words = t.split_whitespace().peekable();
    let mut table = None;
    for w in words.by_ref() {
        let u = w.to_ascii_uppercase();
        if matches!(u.as_str(), "FOR" | "EACH" | "PRESELECT") {
            continue;
        }
        table = Some(w.to_string());
        break;
    }
    let table = table.ok_or("say which table: abl query \"customer WHERE balance > 1000\"")?;
    // The rest, without lock options: the driver reads NO-LOCK.
    let rest: Vec<&str> = words
        .filter(|w| {
            !matches!(
                w.to_ascii_uppercase().as_str(),
                "NO-LOCK" | "SHARE-LOCK" | "EXCLUSIVE-LOCK"
            )
        })
        .collect();
    let phrase = rest.join(" ");
    let first = phrase
        .split_whitespace()
        .next()
        .map(str::to_ascii_uppercase);
    if !matches!(
        first.as_deref(),
        None | Some("WHERE" | "BY" | "USE-INDEX" | "OF")
    ) {
        return Err(format!(
            "after the table name, expected WHERE or BY, not {:?}",
            phrase.split_whitespace().next().unwrap_or("")
        ));
    }
    Ok((table, phrase))
}

pub struct QueryOptions {
    pub format: Format,
    pub max: u64,
    pub fields: Vec<String>,
}

pub fn query(
    cwd: &Path,
    text: &str,
    opts: &QueryOptions,
    flag: Option<&str>,
) -> Result<u8, String> {
    let (table, phrase) = parse_query(text)?;
    let env = env(cwd, flag)?;
    let rows = db::query(
        &env,
        &Query {
            table: &table,
            phrase: &phrase,
            max: opts.max,
            fields: &opts.fields,
        },
    )?;
    match opts.format {
        Format::Table => {
            print!("{}", render_table(&rows));
            let n = rows.rows.len();
            let s = if n == 1 { "" } else { "s" };
            if rows.more {
                println!("({n} row{s} shown; there are more: --max 0 shows all)");
            } else {
                println!("({n} row{s})");
            }
        }
        Format::Json => println!("{}", serde_json::to_string_pretty(&to_json(&rows)).unwrap()),
        Format::Csv => print!("{}", to_csv(&rows)),
        Format::Page => {
            let columns: Vec<Value> = rows
                .columns
                .iter()
                .map(|c| serde_json::json!({"name": c.name, "type": c.data_type, "extent": c.extent}))
                .collect();
            let v =
                serde_json::json!({"columns": columns, "rows": to_json(&rows), "more": rows.more});
            println!("{}", serde_json::to_string(&v).unwrap());
        }
    }
    Ok(0)
}

pub fn export(
    cwd: &Path,
    table: &str,
    filter: Option<&str>,
    out: Option<&Path>,
    format: Option<Format>,
    flag: Option<&str>,
) -> Result<u8, String> {
    let format = format
        .or_else(|| {
            out.and_then(|p| p.extension())
                .and_then(|e| Format::from_str(&e.to_string_lossy(), true).ok())
        })
        .unwrap_or(Format::Json);
    if format == Format::Table {
        return Err("export writes json or csv".into());
    }
    let phrase = match filter.map(str::trim) {
        Some(f) if !f.is_empty() => {
            let u = f.to_ascii_uppercase();
            if u.starts_with("WHERE ") || u.starts_with("BY ") {
                f.to_string()
            } else {
                format!("WHERE {f}")
            }
        }
        _ => String::new(),
    };
    let env = env(cwd, flag)?;
    let rows = db::query(
        &env,
        &Query {
            table,
            phrase: &phrase,
            max: 0,
            fields: &[],
        },
    )?;
    let text = match format {
        Format::Csv => to_csv(&rows),
        _ => format!(
            "{}\n",
            serde_json::to_string_pretty(&to_json(&rows)).unwrap()
        ),
    };
    match out {
        Some(p) => {
            fs::write(p, text).map_err(|e| format!("{}: {e}", p.display()))?;
            let n = rows.rows.len();
            eprintln!(
                "Exported {n} row{} of {table} to {}",
                if n == 1 { "" } else { "s" },
                p.display()
            );
        }
        None => print!("{text}"),
    }
    Ok(0)
}

pub fn import(
    cwd: &Path,
    table: &str,
    file: &Path,
    replace: bool,
    format: Option<Format>,
    flag: Option<&str>,
) -> Result<u8, String> {
    let text = fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let format = format
        .or_else(|| {
            file.extension()
                .and_then(|e| Format::from_str(&e.to_string_lossy(), true).ok())
        })
        .ok_or("say --format json or --format csv (the file name doesn't tell)")?;
    let rows = match format {
        Format::Json => from_json(&text)?,
        Format::Csv => from_csv(&text)?,
        Format::Table | Format::Page => return Err("import reads json or csv".into()),
    };
    let env = env(cwd, flag)?;
    let (created, deleted) = db::import(&env, table, &rows, replace)?;
    if replace {
        println!("Replaced {deleted} rows of {table} with {created}");
    } else {
        println!(
            "Imported {created} row{} into {table}",
            if created == 1 { "" } else { "s" }
        );
    }
    Ok(0)
}

// ---- formats ------------------------------------------------------------

/// Column headings, one per value: `Tags[1]`, `Tags[2]` for an extent.
fn headings(rows: &Rows) -> Vec<String> {
    let mut out = Vec::new();
    for c in &rows.columns {
        if c.extent == 0 {
            out.push(c.name.clone());
        } else {
            for i in 1..=c.extent {
                out.push(format!("{}[{i}]", c.name));
            }
        }
    }
    out
}

fn is_number(ty: &str) -> bool {
    matches!(ty, "integer" | "int64" | "decimal")
}

pub fn render_table(rows: &Rows) -> String {
    const MAX: usize = 40;
    let heads = headings(rows);
    let types: Vec<&str> = rows
        .columns
        .iter()
        .flat_map(|c| std::iter::repeat_n(c.data_type.as_str(), c.extent.max(1) as usize))
        .collect();
    let cell = |v: &Option<String>| -> String {
        let s = v.as_deref().unwrap_or("?").replace(['\n', '\r', '\t'], " ");
        if s.chars().count() > MAX {
            format!("{}...", s.chars().take(MAX - 3).collect::<String>())
        } else {
            s
        }
    };
    let cells: Vec<Vec<String>> = rows
        .rows
        .iter()
        .map(|r| r.iter().map(cell).collect())
        .collect();
    let widths: Vec<usize> = heads
        .iter()
        .enumerate()
        .map(|(i, h)| {
            cells
                .iter()
                .map(|r| r[i].chars().count())
                .chain([h.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |vals: &[String], right: &dyn Fn(usize) -> bool| -> String {
        let parts: Vec<String> = vals
            .iter()
            .enumerate()
            .map(|(i, v)| {
                if right(i) {
                    format!("{v:>w$}", w = widths[i])
                } else {
                    format!("{v:<w$}", w = widths[i])
                }
            })
            .collect();
        format!("{}\n", parts.join("  ").trim_end())
    };
    let mut out = line(&heads, &|i| is_number(types[i]));
    let rule: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    out.push_str(&line(&rule, &|_| false));
    for r in &cells {
        out.push_str(&line(r, &|i| is_number(types[i])));
    }
    out
}

fn json_value(ty: &str, v: &Option<String>) -> Value {
    let Some(s) = v else { return Value::Null };
    match ty {
        "integer" | "int64" => s
            .parse::<i64>()
            .map_or_else(|_| Value::String(s.clone()), Value::from),
        // A decimal becomes a JSON number when that loses nothing.
        "decimal" => {
            if let Ok(i) = s.parse::<i64>() {
                return Value::from(i);
            }
            match s.parse::<f64>().ok().and_then(Number::from_f64) {
                Some(n) if n.to_string() == *s => Value::Number(n),
                _ => Value::String(s.clone()),
            }
        }
        "logical" => match s.to_ascii_lowercase().as_str() {
            "yes" | "true" => Value::Bool(true),
            "no" | "false" => Value::Bool(false),
            _ => Value::String(s.clone()),
        },
        _ => Value::String(s.clone()),
    }
}

pub fn to_json(rows: &Rows) -> Value {
    let mut out = Vec::new();
    for r in &rows.rows {
        let mut obj = Map::new();
        let mut i = 0;
        for c in &rows.columns {
            if c.extent == 0 {
                obj.insert(c.name.clone(), json_value(&c.data_type, &r[i]));
                i += 1;
            } else {
                let n = c.extent as usize;
                let vals = r[i..i + n]
                    .iter()
                    .map(|v| json_value(&c.data_type, v))
                    .collect();
                obj.insert(c.name.clone(), Value::Array(vals));
                i += n;
            }
        }
        out.push(Value::Object(obj));
    }
    Value::Array(out)
}

fn csv_field(v: &Option<String>) -> String {
    match v {
        None => "?".into(),
        Some(s) if s.contains([',', '"', '\n', '\r']) || s == "?" => {
            format!("\"{}\"", s.replace('"', "\"\""))
        }
        Some(s) => s.clone(),
    }
}

/// CSV with a heading line; `?` is the unknown value (a quoted "?" is the text).
pub fn to_csv(rows: &Rows) -> String {
    let mut out = headings(rows)
        .iter()
        .map(|h| csv_field(&Some(h.clone())))
        .collect::<Vec<_>>()
        .join(",");
    out.push('\n');
    for r in &rows.rows {
        out.push_str(&r.iter().map(csv_field).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

fn json_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::Bool(b) => Some(if *b { "yes" } else { "no" }.into()),
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

pub fn from_json(text: &str) -> Result<Rows, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let items = v
        .as_array()
        .ok_or("expected a JSON array of objects, one per row")?;
    let mut columns: Vec<Column> = Vec::new();
    for (n, item) in items.iter().enumerate() {
        let obj = item
            .as_object()
            .ok_or_else(|| format!("row {}: expected an object", n + 1))?;
        for (k, v) in obj {
            let ext = v.as_array().map_or(0, |a| a.len() as u32);
            match columns.iter_mut().find(|c| c.name == *k) {
                Some(c) => c.extent = c.extent.max(ext),
                None => columns.push(Column {
                    name: k.clone(),
                    data_type: String::new(),
                    extent: ext,
                }),
            }
        }
    }
    let mut rows = Vec::new();
    for item in items {
        let obj = item.as_object().unwrap();
        let mut row = Vec::new();
        for c in &columns {
            let v = obj.get(&c.name).unwrap_or(&Value::Null);
            if c.extent == 0 {
                if v.is_array() {
                    return Err(format!(
                        "{}: an array in one row, a value in another",
                        c.name
                    ));
                }
                row.push(json_text(v));
            } else {
                let a = v.as_array().cloned().unwrap_or_default();
                for i in 0..c.extent as usize {
                    row.push(a.get(i).and_then(json_text));
                }
            }
        }
        rows.push(row);
    }
    Ok(Rows {
        columns,
        rows,
        more: false,
    })
}

/// Splits CSV text into records of fields; `quoted` tells a quoted field apart.
fn csv_records(text: &str) -> Result<Vec<Vec<(String, bool)>>, String> {
    let mut out = Vec::new();
    let mut rec = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => in_quotes = false,
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => {
                in_quotes = true;
                quoted = true;
            }
            ',' => rec.push((
                std::mem::take(&mut field),
                std::mem::replace(&mut quoted, false),
            )),
            '\r' => {}
            '\n' => {
                rec.push((
                    std::mem::take(&mut field),
                    std::mem::replace(&mut quoted, false),
                ));
                out.push(std::mem::take(&mut rec));
                any = false;
            }
            c => field.push(c),
        }
    }
    if in_quotes {
        return Err("CSV ends inside a quoted field".into());
    }
    if any {
        rec.push((field, quoted));
        out.push(rec);
    }
    Ok(out)
}

pub fn from_csv(text: &str) -> Result<Rows, String> {
    let mut recs = csv_records(text.trim_start_matches('\u{feff}'))?.into_iter();
    let head = recs.next().ok_or("the CSV file is empty")?;
    // `Tags[1]`, `Tags[2]` ... make one column with an extent.
    let mut columns: Vec<Column> = Vec::new();
    for (h, _) in &head {
        let (name, sub) = match h.strip_suffix(']').and_then(|s| s.rsplit_once('[')) {
            Some((n, i)) => (n.to_string(), i.parse::<u32>().ok()),
            None => (h.clone(), None),
        };
        match (sub, columns.last_mut()) {
            (Some(i), Some(c)) if c.name == name && c.extent + 1 == i => c.extent = i,
            (Some(1), _) => columns.push(Column {
                name,
                data_type: String::new(),
                extent: 1,
            }),
            (Some(_), _) => {
                return Err(format!(
                    "{h}: extent columns must run [1], [2], ... in order"
                ))
            }
            (None, _) => columns.push(Column {
                name,
                data_type: String::new(),
                extent: 0,
            }),
        }
    }
    let width = head.len();
    let mut rows = Vec::new();
    for (n, r) in recs.enumerate() {
        if r.len() == 1 && r[0].0.is_empty() && !r[0].1 {
            continue; // blank line
        }
        if r.len() != width {
            return Err(format!(
                "line {}: {} fields, the heading has {width}",
                n + 2,
                r.len()
            ));
        }
        rows.push(
            r.into_iter()
                .map(|(v, q)| if v == "?" && !q { None } else { Some(v) })
                .collect(),
        );
    }
    Ok(Rows {
        columns,
        rows,
        more: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Rows {
        Rows {
            columns: vec![
                Column {
                    name: "Id".into(),
                    data_type: "integer".into(),
                    extent: 0,
                },
                Column {
                    name: "Name".into(),
                    data_type: "character".into(),
                    extent: 0,
                },
                Column {
                    name: "Amt".into(),
                    data_type: "decimal".into(),
                    extent: 0,
                },
                Column {
                    name: "Tag".into(),
                    data_type: "character".into(),
                    extent: 2,
                },
            ],
            rows: vec![
                vec![
                    Some("1".into()),
                    Some("Lift, \"Inc\"".into()),
                    Some("10.5".into()),
                    Some("a".into()),
                    None,
                ],
                vec![
                    Some("2".into()),
                    Some("?".into()),
                    None,
                    Some("".into()),
                    Some("b".into()),
                ],
            ],
            more: false,
        }
    }

    #[test]
    fn queries() {
        assert_eq!(
            parse_query("FOR EACH customer NO-LOCK WHERE balance > 1.").unwrap(),
            ("customer".into(), "WHERE balance > 1".into())
        );
        assert_eq!(
            parse_query("sports.order").unwrap(),
            ("sports.order".into(), String::new())
        );
        assert!(parse_query("customer balance > 1").is_err());
        assert!(parse_query("").is_err());
    }

    #[test]
    fn csv_round_trip() {
        let r = sample();
        let text = to_csv(&r);
        assert!(text.starts_with("Id,Name,Amt,Tag[1],Tag[2]\n1,\"Lift, \"\"Inc\"\"\",10.5,a,?\n"));
        let back = from_csv(&text).unwrap();
        assert_eq!(back.rows, r.rows);
        assert_eq!(back.columns[3].extent, 2);
    }

    #[test]
    fn json_round_trip() {
        let r = sample();
        let v = to_json(&r);
        assert_eq!(v[0]["Id"], 1);
        assert_eq!(v[0]["Amt"], 10.5);
        assert_eq!(v[0]["Tag"][1], Value::Null);
        let back = from_json(&v.to_string()).unwrap();
        assert_eq!(back.rows, r.rows);
    }

    #[test]
    fn table_rendering() {
        let t = render_table(&sample());
        assert!(t.starts_with("Id  Name"), "{t}");
        assert!(t.contains("\n 1  Lift, \"Inc\""), "{t}");
    }
}

//! Databases `abl db` manages: creating them with `prodb`, serving them with
//! `proserve`, backups with `probkup`/`prorest`, and the data file format the
//! query and import drivers use.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::driver;
use crate::session::{self, Env};

/// A database is in use (served, or open single-user) while its `.lk` file exists.
pub fn in_use(db: &Path) -> bool {
    db.with_extension("lk").is_file()
}

/// An OpenEdge tool in `DLC/bin` (`prodb`, `proserve`...). On Windows they
/// are batch files.
pub fn tool(dlc: &Path, name: &str) -> Option<PathBuf> {
    let bin = dlc.join("bin");
    let names = if cfg!(windows) {
        vec![
            format!("{name}.bat"),
            format!("{name}.exe"),
            format!("{name}.cmd"),
        ]
    } else {
        vec![name.to_string()]
    };
    names.into_iter().map(|n| bin.join(n)).find(|p| p.is_file())
}

fn command(env: &Env, name: &str, cwd: &Path) -> Result<Command, String> {
    let exe = tool(&env.dlc, name).ok_or_else(|| {
        format!(
            "{name} not found in {}; check the OE {} install with `abl doctor`",
            env.dlc.join("bin").display(),
            env.version
        )
    })?;
    let mut c = Command::new(exe);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![env.dlc.join("bin")];
    dirs.extend(std::env::split_paths(&path));
    c.current_dir(cwd)
        .env("DLC", &env.dlc)
        .env("PATH", std::env::join_paths(dirs).unwrap_or(path));
    Ok(c)
}

/// Runs a tool, answering its prompts with `input`. Fails with its output
/// when it exits non-zero.
fn run_tool(
    env: &Env,
    name: &str,
    cwd: &Path,
    args: &[String],
    input: Option<&str>,
) -> Result<Output, String> {
    let mut c = command(env, name, cwd)?;
    c.args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("cannot start {name}: {e}"))?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        let _ = stdin.write_all(text.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{name}: {e}"))?;
    Ok(out)
}

/// Runs a tool from `DLC/bin` in `cwd`; fails with its output when it exits non-zero.
pub fn run_checked(env: &Env, name: &str, cwd: &Path, args: &[String]) -> Result<String, String> {
    let out = checked(name, run_tool(env, name, cwd, args, None)?)?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn checked(name: &str, out: Output) -> Result<Output, String> {
    if out.status.success() {
        return Ok(out);
    }
    let mut msg = format!("{name} failed ({})", out.status);
    for s in [&out.stdout, &out.stderr] {
        let s = String::from_utf8_lossy(s);
        if !s.trim().is_empty() {
            msg.push('\n');
            msg.push_str(s.trim_end());
        }
    }
    Err(msg)
}

fn arg(p: &Path) -> String {
    p.display().to_string()
}

fn folder(db: &Path) -> &Path {
    db.parent().unwrap_or(Path::new("."))
}

/// Who has the database open, from `proutil -C holder`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    None,
    SingleUser,
    Server,
}

pub fn holder(env: &Env, db: &Path) -> Result<Holder, String> {
    let out = run_tool(
        env,
        "proutil",
        folder(db),
        &[arg(db), "-C".into(), "holder".into()],
        None,
    )?;
    // proutil reports through its exit code: 14 single-user, 16 multi-user.
    match out.status.code() {
        Some(0) => Ok(Holder::None),
        Some(14) => Ok(Holder::SingleUser),
        Some(16) => Ok(Holder::Server),
        _ => Err(checked("proutil -C holder", out).err().unwrap_or_default()),
    }
}

/// An empty database at `db` (a `.db` path), from `DLC/empty`.
pub fn create(env: &Env, db: &Path) -> Result<(), String> {
    fs::create_dir_all(folder(db)).map_err(|e| format!("{}: {e}", folder(db).display()))?;
    let out = run_tool(
        env,
        "prodb",
        folder(db),
        &[arg(&db.with_extension("")), "empty".into()],
        None,
    )?;
    checked("prodb", out).map(drop)
}

/// Loads a .df into `db`, connected single-user on its own.
pub fn load_df(env: &Env, name: &str, db: &Path, df: &Path) -> Result<(), String> {
    let mut e = env.clone();
    e.params = vec![
        "-db".into(),
        arg(db),
        "-ld".into(),
        name.into(),
        "-1".into(),
    ];
    e.single_user = true;
    let out = session::data_job(
        &e,
        "abl_load_df.p",
        driver::LOAD_DF,
        &[("df", arg(df))],
        None,
    )?;
    out.done().map(drop)
}

pub fn serve(env: &Env, db: &Path, port: Option<u16>) -> Result<String, String> {
    let mut args = vec![arg(db)];
    if let Some(p) = port {
        args.extend(["-S".into(), p.to_string()]);
    }
    let out = checked(
        "proserve",
        run_tool(env, "proserve", folder(db), &args, None)?,
    )?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn shutdown(env: &Env, db: &Path) -> Result<(), String> {
    let out = run_tool(env, "proshut", folder(db), &[arg(db), "-by".into()], None)?;
    checked("proshut", out).map(drop)
}

/// A backup of `db` into `file`; online when a server has it open.
pub fn backup(env: &Env, db: &Path, file: &Path, online: bool) -> Result<(), String> {
    let mut args = Vec::new();
    if online {
        args.push("online".to_string());
    }
    args.extend([arg(db), arg(file)]);
    let out = run_tool(env, "probkup", folder(db), &args, None)?;
    checked("probkup", out).map(drop)
}

/// Restores `file` over `db`; prorest asks before overwriting, abl says yes.
pub fn restore(env: &Env, db: &Path, file: &Path) -> Result<(), String> {
    fs::create_dir_all(folder(db)).map_err(|e| format!("{}: {e}", folder(db).display()))?;
    let out = run_tool(
        env,
        "prorest",
        folder(db),
        &[arg(db), arg(file)],
        Some("y\n"),
    )?;
    checked("prorest", out).map(drop)
}

/// Deletes `db` and its extents; prodel asks first, abl says yes.
pub fn delete(env: &Env, db: &Path) -> Result<(), String> {
    let out = run_tool(env, "prodel", folder(db), &[arg(db)], Some("y\n"))?;
    checked("prodel", out).map(drop)
}

// ---- data files ---------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// The ABL data type: `character`, `integer`, `decimal`, `logical`, `date`...
    pub data_type: String,
    /// 0 for a field without an extent.
    pub extent: u32,
}

/// Rows of one table. Each row has one value per column, or `extent` values
/// for a column with an extent; `None` is the unknown value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rows {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Option<String>>>,
    /// The query stopped at its row limit.
    pub more: bool,
}

impl Rows {
    /// Values per row: the sum of the columns' widths.
    pub fn width(&self) -> usize {
        self.columns.iter().map(|c| c.extent.max(1) as usize).sum()
    }
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 1);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    out
}

fn value_text(v: &Option<String>) -> String {
    match v {
        None => "?".into(),
        Some(s) => format!("={}", escape(s)),
    }
}

fn parse_value(s: &str) -> Option<String> {
    s.strip_prefix('=').map(unescape)
}

/// Reads the query driver's data file.
pub fn parse_rows(text: &str) -> Result<Rows, String> {
    let mut r = Rows::default();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f[0] {
            "field" if f.len() >= 4 => r.columns.push(Column {
                name: f[1].to_string(),
                data_type: f[2].to_ascii_lowercase(),
                extent: f[3].trim().parse().unwrap_or(0),
            }),
            "row" => {
                let row: Vec<Option<String>> = f[1..].iter().map(|v| parse_value(v)).collect();
                if row.len() != r.width() {
                    return Err(format!(
                        "data file row {} has {} values, expected {}",
                        r.rows.len() + 1,
                        row.len(),
                        r.width()
                    ));
                }
                r.rows.push(row);
            }
            _ => {}
        }
    }
    Ok(r)
}

/// Writes rows for the import driver: one `field` line per value position.
pub fn import_text(rows: &Rows) -> String {
    let mut out = String::new();
    for c in &rows.columns {
        if c.extent == 0 {
            out.push_str(&format!("field\t{}\t0\n", c.name));
        } else {
            for i in 1..=c.extent {
                out.push_str(&format!("field\t{}\t{i}\n", c.name));
            }
        }
    }
    for row in &rows.rows {
        out.push_str("row");
        for v in row {
            out.push('\t');
            out.push_str(&value_text(v));
        }
        out.push('\n');
    }
    out
}

/// The query driver's data file for `rows` (the fake `_progres` writes these).
pub fn query_text(rows: &Rows) -> String {
    let mut out = String::new();
    for c in &rows.columns {
        out.push_str(&format!(
            "field\t{}\t{}\t{}\n",
            c.name, c.data_type, c.extent
        ));
    }
    for row in &rows.rows {
        out.push_str("row");
        for v in row {
            out.push('\t');
            out.push_str(&value_text(v));
        }
        out.push('\n');
    }
    out
}

pub struct Query<'a> {
    pub table: &'a str,
    /// WHERE / BY text after the table name.
    pub phrase: &'a str,
    /// 0 for no limit.
    pub max: u64,
    pub fields: &'a [String],
}

pub fn query(env: &Env, q: &Query) -> Result<Rows, String> {
    let out = session::data_job(
        env,
        "abl_query.p",
        driver::QUERY,
        &[
            ("table", q.table.to_string()),
            ("phrase", q.phrase.to_string()),
            ("max", q.max.to_string()),
            ("fields", q.fields.join(",")),
        ],
        None,
    )?;
    let (_, more) = out.done()?;
    let mut rows = parse_rows(out.data.as_deref().unwrap_or(""))?;
    rows.more = more == 1;
    Ok(rows)
}

/// Creates `rows` in `table`; with `replace`, deletes its rows first. All or
/// nothing. Returns (rows created, rows deleted).
pub fn import(env: &Env, table: &str, rows: &Rows, replace: bool) -> Result<(u64, u64), String> {
    let out = session::data_job(
        env,
        "abl_import.p",
        driver::IMPORT,
        &[
            ("table", table.to_string()),
            ("replace", if replace { "yes" } else { "no" }.to_string()),
        ],
        Some(&import_text(rows)),
    )?;
    out.done().map_err(|e| match out.error_at() {
        Some(row) if row > 0 => format!("row {row}: {e}\nnothing was imported"),
        _ => format!("{e}\nnothing was imported"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_round_trips() {
        for s in [
            "plain",
            "a\tb",
            "line\nbreak\r",
            "back\\slash",
            "\\t literal",
            "",
        ] {
            assert_eq!(unescape(&escape(s)), s);
        }
        assert_eq!(escape("a\\tb\t"), "a\\\\tb\\t");
    }

    #[test]
    fn data_files() {
        let rows = Rows {
            columns: vec![
                Column {
                    name: "Id".into(),
                    data_type: "integer".into(),
                    extent: 0,
                },
                Column {
                    name: "Tag".into(),
                    data_type: "character".into(),
                    extent: 2,
                },
            ],
            rows: vec![vec![Some("1".into()), Some("a\tb".into()), None]],
            more: false,
        };
        assert_eq!(parse_rows(&query_text(&rows)).unwrap(), rows);
        assert_eq!(
            import_text(&rows),
            "field\tId\t0\nfield\tTag\t1\nfield\tTag\t2\nrow\t=1\t=a\\tb\t?\n"
        );
        assert!(parse_rows("field\tId\tinteger\t0\nrow\t=1\t=2\n").is_err());
    }
}

//! The fake database, for testing `abl db`, `query`, `export` and `import`
//! without OpenEdge. Not a database engine.
//!
//! A database is its `.db` file (a marker) plus `<name>.fake.json` with the
//! loaded schema (.df text) and the rows, each a JSON object of field values
//! in their text form (`null` is the unknown value, an array is an extent).
//! A served database has a `.lk` file, as a real one does.
//!
//! The fake tools are this binary under another name: prodb, proserve,
//! proshut, proutil (-C holder), probkup, prorest, prodel and prolib.
//!
//! Queries understand `WHERE` with `=`, `<>`, `<`, `>`, `<=`, `>=` (and EQ, NE,
//! LT, GT, LE, GE), `BEGINS`, `MATCHES`, `AND`, `OR`, `NOT` and parentheses,
//! comparing a field with a literal; then `BY field [DESCENDING]`.

use std::collections::BTreeMap;
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use abl_core::df;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const TOOLS: [&str; 8] = [
    "prodb", "proserve", "proshut", "proutil", "probkup", "prorest", "prodel", "prolib",
];

#[derive(Default, Serialize, Deserialize)]
struct Store {
    schema: String,
    tables: BTreeMap<String, Vec<Map<String, Value>>>,
}

fn db_arg(s: &str) -> PathBuf {
    let p = PathBuf::from(s);
    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("db")) {
        p
    } else {
        PathBuf::from(format!("{s}.db"))
    }
}

fn store_path(db: &Path) -> PathBuf {
    db.with_extension("fake.json")
}

fn load(db: &Path) -> Result<Store, String> {
    if !db.is_file() {
        return Err(format!(
            "** The database {} does not exist. (fake)",
            db.display()
        ));
    }
    let text = fs::read_to_string(store_path(db)).unwrap_or_default();
    if text.trim().is_empty() {
        return Ok(Store::default());
    }
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", store_path(db).display()))
}

fn save(db: &Path, s: &Store) -> Result<(), String> {
    let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    fs::write(store_path(db), text).map_err(|e| e.to_string())
}

/// Appends a line to the database's log, in the shape of a real .lg line.
fn log(db: &Path, num: u32, text: &str) {
    use std::io::Write as _;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (h, m, s) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    if let Ok(mut f) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(db.with_extension("lg"))
    {
        let _ = writeln!(
            f,
            "[fake@{h:02}:{m:02}:{s:02}.000+0000] P-{} T-1 I BROKER  0: ({num})  {text} ({num})",
            std::process::id()
        );
    }
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(1)
}

/// Reads one answer from stdin; true for "y".
fn confirmed(question: &str) -> bool {
    println!("{question} (y/n)");
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().eq_ignore_ascii_case("y")
}

pub fn tool(name: &str, args: &[String]) -> ExitCode {
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    match name {
        "prodb" => {
            let Some(target) = pos.first() else {
                return fail("usage: prodb <new db> empty");
            };
            let db = db_arg(target);
            if db.is_file() {
                return fail(format!("** {} already exists. (fake)", db.display()));
            }
            if fs::write(&db, "fake OpenEdge database\n").is_err() {
                return fail(format!("** Cannot create {}. (fake)", db.display()));
            }
            if let Err(e) = save(&db, &Store::default()) {
                return fail(e);
            }
            log(&db, 0, "Database created from empty (fake).");
            println!("Created {} from empty (fake)", db.display());
            ExitCode::SUCCESS
        }
        "proserve" => {
            let Some(db) = pos.first().map(|s| db_arg(s)) else {
                return fail("usage: proserve <db> [-S port]");
            };
            if !db.is_file() {
                return fail(format!(
                    "** The database {} does not exist. (fake)",
                    db.display()
                ));
            }
            if db.with_extension("lk").is_file() {
                return fail(format!(
                    "** The database {} is in use. (fake)",
                    db.display()
                ));
            }
            let _ = fs::write(db.with_extension("lk"), "multi-user\n");
            log(&db, 333, "Multi-user session begin.");
            println!("OpenEdge server started for {} (fake)", db.display());
            ExitCode::SUCCESS
        }
        "proshut" => {
            let Some(db) = pos.first().map(|s| db_arg(s)) else {
                return fail("usage: proshut <db> -by");
            };
            if fs::remove_file(db.with_extension("lk")).is_err() {
                return fail(format!(
                    "** There is no server for database {}. (fake)",
                    db.display()
                ));
            }
            log(&db, 334, "Multi-user session end.");
            println!("Shutdown complete (fake)");
            ExitCode::SUCCESS
        }
        "proutil" => {
            let Some(db) = pos.first().map(|s| db_arg(s)) else {
                return fail("usage: proutil <db> -C holder");
            };
            match fs::read_to_string(db.with_extension("lk")) {
                Ok(t) if t.contains("multi-user") => ExitCode::from(16),
                Ok(_) => ExitCode::from(14),
                Err(_) => ExitCode::SUCCESS,
            }
        }
        "probkup" => {
            let online = args.first().is_some_and(|a| a == "online");
            let pos: Vec<&&String> = pos.iter().filter(|a| a.as_str() != "online").collect();
            let (Some(db), Some(file)) = (pos.first().map(|s| db_arg(s)), pos.get(1)) else {
                return fail("usage: probkup [online] <db> <file>");
            };
            if db.with_extension("lk").is_file() && !online {
                return fail("** The database is in use; use probkup online. (fake)");
            }
            let store = match load(&db) {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let text = serde_json::to_string(&store).unwrap_or_default();
            if fs::write(file.as_str(), format!("fake backup\n{text}")).is_err() {
                return fail(format!("** Cannot write {file}. (fake)"));
            }
            println!("Backup complete (fake)");
            ExitCode::SUCCESS
        }
        "prorest" => {
            let (Some(db), Some(file)) = (pos.first().map(|s| db_arg(s)), pos.get(1)) else {
                return fail("usage: prorest <db> <file>");
            };
            if db.with_extension("lk").is_file() {
                return fail("** The database is in use. (fake)");
            }
            let Some(text) = fs::read_to_string(file.as_str())
                .ok()
                .and_then(|t| t.strip_prefix("fake backup\n").map(str::to_string))
            else {
                return fail(format!("** {file} is not a backup. (fake)"));
            };
            if db.is_file() && !confirmed("Do you want to over write it?") {
                return fail("Restore cancelled (fake)");
            }
            let store: Store = serde_json::from_str(&text).unwrap_or_default();
            let _ = fs::write(&db, "fake OpenEdge database\n");
            if let Err(e) = save(&db, &store) {
                return fail(e);
            }
            println!("Restore complete (fake)");
            ExitCode::SUCCESS
        }
        // A library is a text file: a marker line, then one line per member.
        "prolib" => {
            let Some(lib) = args.first() else {
                return fail("usage: prolib <library> -create | -add <files> | -list");
            };
            let lib = PathBuf::from(lib);
            match args.get(1).map(String::as_str) {
                Some("-create") => {
                    if lib.exists() {
                        return fail(format!(
                            "** Library {} already exists. (fake)",
                            lib.display()
                        ));
                    }
                    if fs::write(&lib, "fake procedure library\n").is_err() {
                        return fail(format!("** Cannot create {}. (fake)", lib.display()));
                    }
                    ExitCode::SUCCESS
                }
                Some("-add") => {
                    let Ok(mut text) = fs::read_to_string(&lib) else {
                        return fail(format!(
                            "** Library {} does not exist. (fake)",
                            lib.display()
                        ));
                    };
                    for f in &args[2..] {
                        let Ok(meta) = fs::metadata(f) else {
                            return fail(format!("** Cannot find {f}. (fake)"));
                        };
                        text.push_str(&format!("{}\t{}\n", f.replace('\\', "/"), meta.len()));
                    }
                    if fs::write(&lib, text).is_err() {
                        return fail(format!("** Cannot write {}. (fake)", lib.display()));
                    }
                    ExitCode::SUCCESS
                }
                Some("-list") => match fs::read_to_string(&lib) {
                    Ok(t) => {
                        for l in t.lines().skip(1) {
                            println!("{}", l.split('\t').next().unwrap_or(""));
                        }
                        ExitCode::SUCCESS
                    }
                    Err(_) => fail(format!(
                        "** Library {} does not exist. (fake)",
                        lib.display()
                    )),
                },
                _ => fail("fake prolib: only -create, -add and -list"),
            }
        }
        "prodel" => {
            let Some(db) = pos.first().map(|s| db_arg(s)) else {
                return fail("usage: prodel <db>");
            };
            if db.with_extension("lk").is_file() {
                return fail("** The database is in use. (fake)");
            }
            if !db.is_file() {
                return fail(format!(
                    "** The database {} does not exist. (fake)",
                    db.display()
                ));
            }
            if !confirmed("Do you want to delete the database?") {
                return fail("Delete cancelled (fake)");
            }
            let _ = fs::remove_file(store_path(&db));
            let _ = fs::remove_file(db.with_extension("lg"));
            let _ = fs::remove_file(&db);
            ExitCode::SUCCESS
        }
        _ => fail(format!("fake {name}: not a tool")),
    }
}

// ---- sessions connected to fake databases -----------------------------------

pub struct Conn {
    db: PathBuf,
    ld: String,
}

/// The `-db path [-ld name]` connections on a command line.
pub fn connections(args: &[String]) -> Vec<Conn> {
    let mut out: Vec<Conn> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-db" if i + 1 < args.len() => {
                let db = db_arg(&args[i + 1]);
                let ld = db
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                out.push(Conn { db, ld });
                i += 1;
            }
            "-ld" if i + 1 < args.len() => {
                if let Some(c) = out.last_mut() {
                    c.ld = args[i + 1].clone();
                }
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

fn get<'a>(job: &'a [(String, String)], key: &str) -> &'a str {
    job.iter()
        .find(|(k, _)| k == key)
        .map_or("", |(_, v)| v.as_str())
}

/// Writes the result file and exits like the drivers do (QUIT: status 0).
fn finish(job: &[(String, String)], result: &str) -> ExitCode {
    match fs::write(get(job, "result"), result) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(2),
    }
}

fn error(job: &[(String, String)], at: u64, msg: &str) -> ExitCode {
    finish(job, &format!("error\t{at}\t{msg}\n"))
}

pub fn load_df(conns: &[Conn], job: &[(String, String)]) -> ExitCode {
    let Some(c) = conns.first() else {
        return error(job, 1006, "** No database connected. (1006)");
    };
    let mut store = match load(&c.db) {
        Ok(s) => s,
        Err(e) => return error(job, 0, &e),
    };
    let Ok(text) = fs::read_to_string(get(job, "df")) else {
        return error(
            job,
            0,
            &format!("** Cannot read {}. (fake)", get(job, "df")),
        );
    };
    let parsed = df::parse(&text);
    if let Some(t) = parsed
        .tables
        .iter()
        .find(|t| df::parse(&store.schema).table(&t.name).is_some())
    {
        return error(
            job,
            0,
            &format!("Table {} already exists in {}. (fake)", t.name, c.ld),
        );
    }
    store.schema.push_str(&text);
    store.schema.push('\n');
    if let Err(e) = save(&c.db, &store) {
        return error(job, 0, &e);
    }
    finish(job, "done\t0\t0\n")
}

/// The connection and schema table for `name` (`table` or `ld.table`).
fn find_table(conns: &[Conn], name: &str) -> Result<(usize, df::Table), String> {
    if conns.is_empty() {
        return Err("** No database connected. (1006)".into());
    }
    let (ld, table) = match name.split_once('.') {
        Some((d, t)) => (Some(d), t),
        None => (None, name),
    };
    for (i, c) in conns.iter().enumerate() {
        if ld.is_some_and(|d| !d.eq_ignore_ascii_case(&c.ld)) {
            continue;
        }
        let store = load(&c.db)?;
        if let Some(t) = df::parse(&store.schema).table(table) {
            return Ok((i, t.clone()));
        }
    }
    Err(format!(
        "** Unable to find table {name} in the connected databases. (fake)"
    ))
}

fn extent(f: &df::Field) -> u32 {
    f.attrs
        .iter()
        .find(|a| a.key == "EXTENT")
        .and_then(|a| a.value.trim().parse().ok())
        .unwrap_or(0)
}

fn data_type(f: &df::Field) -> String {
    f.data_type.to_ascii_lowercase()
}

fn initial(f: &df::Field) -> Value {
    let one = match data_type(f).as_str() {
        "integer" | "int64" | "decimal" => Value::String("0".into()),
        "logical" => Value::String("no".into()),
        "character" | "longchar" => Value::String(String::new()),
        _ => Value::Null,
    };
    match extent(f) {
        0 => one,
        n => Value::Array(vec![one; n as usize]),
    }
}

fn text_of(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_string)
}

// ---- WHERE / BY ------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Num(String),
    Op(String),
    LParen,
    RParen,
}

fn tokens(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '"' || ch == '\'' {
            let mut v = String::new();
            i += 1;
            while i < c.len() && c[i] != ch {
                if c[i] == '~' && i + 1 < c.len() {
                    i += 1;
                }
                v.push(c[i]);
                i += 1;
            }
            i += 1;
            out.push(Tok::Str(v));
        } else if ch == '(' {
            out.push(Tok::LParen);
            i += 1;
        } else if ch == ')' {
            out.push(Tok::RParen);
            i += 1;
        } else if "=<>".contains(ch) {
            let mut op = ch.to_string();
            if i + 1 < c.len() && "=>".contains(c[i + 1]) && ch != '=' {
                op.push(c[i + 1]);
                i += 1;
            }
            out.push(Tok::Op(op));
            i += 1;
        } else if ch.is_ascii_digit()
            || (ch == '-' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            let mut v = String::from(ch);
            i += 1;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                v.push(c[i]);
                i += 1;
            }
            out.push(Tok::Num(v));
        } else if ch == '?' {
            out.push(Tok::Word("?".into()));
            i += 1;
        } else if ch.is_alphanumeric() || ch == '_' {
            let mut v = String::new();
            while i < c.len() && (c[i].is_alphanumeric() || "_-.#$%&".contains(c[i])) {
                v.push(c[i]);
                i += 1;
            }
            out.push(Tok::Word(v));
        } else {
            return Err(format!("** fake query: cannot read {ch:?} in {s:?}"));
        }
    }
    Ok(out)
}

#[derive(Debug)]
enum Expr {
    Or(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Cmp(String, String, Option<String>),
}

struct Parser {
    toks: Vec<Tok>,
    i: usize,
}

fn kw(t: Option<&Tok>, k: &str) -> bool {
    matches!(t, Some(Tok::Word(w)) if w.eq_ignore_ascii_case(k))
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i)
    }
    fn or(&mut self) -> Result<Expr, String> {
        let mut e = self.and()?;
        while kw(self.peek(), "OR") {
            self.i += 1;
            e = Expr::Or(Box::new(e), Box::new(self.and()?));
        }
        Ok(e)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut e = self.not()?;
        while kw(self.peek(), "AND") {
            self.i += 1;
            e = Expr::And(Box::new(e), Box::new(self.not()?));
        }
        Ok(e)
    }
    fn not(&mut self) -> Result<Expr, String> {
        if kw(self.peek(), "NOT") {
            self.i += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        if self.peek() == Some(&Tok::LParen) {
            self.i += 1;
            let e = self.or()?;
            if self.peek() != Some(&Tok::RParen) {
                return Err("** fake query: missing )".into());
            }
            self.i += 1;
            return Ok(e);
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let Some(Tok::Word(field)) = self.peek().cloned() else {
            return Err(format!(
                "** fake query: expected a field at {:?}",
                self.peek()
            ));
        };
        self.i += 1;
        let op = match self.peek().cloned() {
            Some(Tok::Op(o)) => o,
            Some(Tok::Word(w)) => match w.to_ascii_uppercase().as_str() {
                "EQ" => "=".into(),
                "NE" => "<>".into(),
                "LT" => "<".into(),
                "GT" => ">".into(),
                "LE" => "<=".into(),
                "GE" => ">=".into(),
                "BEGINS" | "MATCHES" => w.to_ascii_uppercase(),
                _ => return Err(format!("** fake query: expected an operator after {field}")),
            },
            t => {
                return Err(format!(
                    "** fake query: expected an operator after {field}, got {t:?}"
                ))
            }
        };
        self.i += 1;
        let value = match self.peek().cloned() {
            Some(Tok::Str(s) | Tok::Num(s)) => Some(s),
            Some(Tok::Word(w)) if w == "?" => None,
            Some(Tok::Word(w)) if ["TRUE", "YES"].contains(&w.to_ascii_uppercase().as_str()) => {
                Some("yes".into())
            }
            Some(Tok::Word(w)) if ["FALSE", "NO"].contains(&w.to_ascii_uppercase().as_str()) => {
                Some("no".into())
            }
            t => {
                return Err(format!(
                    "** fake query: the fake compares fields with literals only, got {t:?}"
                ))
            }
        };
        self.i += 1;
        Ok(Expr::Cmp(field, op, value))
    }
}

struct Phrase {
    filter: Option<Expr>,
    by: Vec<(String, bool)>,
}

fn phrase(s: &str) -> Result<Phrase, String> {
    let toks = tokens(s)?;
    let mut p = Parser { toks, i: 0 };
    let mut filter = None;
    if kw(p.peek(), "WHERE") {
        p.i += 1;
        filter = Some(p.or()?);
    }
    let mut by = Vec::new();
    while kw(p.peek(), "BY") {
        p.i += 1;
        let Some(Tok::Word(f)) = p.peek().cloned() else {
            return Err("** fake query: BY needs a field".into());
        };
        p.i += 1;
        let desc = kw(p.peek(), "DESCENDING") || kw(p.peek(), "DESC");
        if desc {
            p.i += 1;
        }
        by.push((f, desc));
    }
    if let Some(t) = p.peek() {
        return Err(format!("** fake query: cannot understand {t:?}"));
    }
    Ok(Phrase { filter, by })
}

fn field_of<'t>(t: &'t df::Table, name: &str) -> Result<&'t df::Field, String> {
    let name = name.rsplit('.').next().unwrap_or(name);
    t.field(name)
        .ok_or_else(|| format!("** Unknown field {name} in table {}. (fake)", t.name))
}

fn compare(ty: &str, a: &str, b: &str) -> std::cmp::Ordering {
    match ty {
        "integer" | "int64" | "decimal" => {
            let (x, y) = (
                a.parse::<f64>().unwrap_or(0.0),
                b.parse::<f64>().unwrap_or(0.0),
            );
            x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
        }
        // Character comparisons ignore case and trailing blanks.
        _ => a
            .trim_end()
            .to_lowercase()
            .cmp(&b.trim_end().to_lowercase()),
    }
}

fn matches_pattern(text: &str, pat: &str) -> bool {
    fn go(t: &[char], p: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => (0..=t.len()).any(|i| go(&t[i..], &p[1..])),
            Some('.') => !t.is_empty() && go(&t[1..], &p[1..]),
            Some(c) => t.first().is_some_and(|x| x.eq_ignore_ascii_case(c)) && go(&t[1..], &p[1..]),
        }
    }
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pat.chars().collect();
    go(&t, &p)
}

fn eval(e: &Expr, t: &df::Table, row: &Map<String, Value>) -> Result<bool, String> {
    Ok(match e {
        Expr::Or(a, b) => eval(a, t, row)? || eval(b, t, row)?,
        Expr::And(a, b) => eval(a, t, row)? && eval(b, t, row)?,
        Expr::Not(a) => !eval(a, t, row)?,
        Expr::Cmp(field, op, lit) => {
            let f = field_of(t, field)?;
            let v = text_of(row.get(&f.name));
            let ty = data_type(f);
            match (v, lit) {
                (None, None) => op == "=",
                (Some(_), None) => op == "<>",
                (None, Some(_)) => op == "<>",
                (Some(v), Some(l)) => {
                    use std::cmp::Ordering::*;
                    match op.as_str() {
                        "BEGINS" => v.to_lowercase().starts_with(&l.to_lowercase()),
                        "MATCHES" => matches_pattern(&v, l),
                        "=" => compare(&ty, &v, l) == Equal,
                        "<>" => compare(&ty, &v, l) != Equal,
                        "<" => compare(&ty, &v, l) == Less,
                        ">" => compare(&ty, &v, l) == Greater,
                        "<=" => compare(&ty, &v, l) != Greater,
                        ">=" => compare(&ty, &v, l) != Less,
                        o => return Err(format!("** fake query: unknown operator {o}")),
                    }
                }
            }
        }
    })
}

pub fn query(conns: &[Conn], job: &[(String, String)]) -> ExitCode {
    let run = || -> Result<(String, String), String> {
        let (ci, t) = find_table(conns, get(job, "table"))?;
        let store = load(&conns[ci].db)?;
        let p = phrase(get(job, "phrase"))?;
        let wanted: Vec<&str> = get(job, "fields")
            .split(',')
            .filter(|s| !s.is_empty())
            .collect();
        let fields: Vec<&df::Field> = t
            .fields
            .iter()
            .filter(|f| !matches!(data_type(f).as_str(), "blob" | "clob" | "raw"))
            .filter(|f| wanted.is_empty() || wanted.iter().any(|w| w.eq_ignore_ascii_case(&f.name)))
            .collect();
        let empty = Vec::new();
        let rows = store.tables.get(&t.name.to_lowercase()).unwrap_or(&empty);
        let mut hits: Vec<&Map<String, Value>> = Vec::new();
        for r in rows {
            if p.filter.as_ref().map_or(Ok(true), |e| eval(e, &t, r))? {
                hits.push(r);
            }
        }
        for (f, desc) in p.by.iter().rev() {
            let f = field_of(&t, f)?;
            let ty = data_type(f);
            hits.sort_by(|a, b| {
                let (x, y) = (
                    text_of(a.get(&f.name)).unwrap_or_default(),
                    text_of(b.get(&f.name)).unwrap_or_default(),
                );
                let o = compare(&ty, &x, &y);
                if *desc {
                    o.reverse()
                } else {
                    o
                }
            });
        }
        let max: usize = get(job, "max").parse().unwrap_or(0);
        let more = max > 0 && hits.len() > max;
        if more {
            hits.truncate(max);
        }
        let mut out = String::new();
        for f in &fields {
            out.push_str(&format!(
                "field\t{}\t{}\t{}\n",
                f.name,
                data_type(f),
                extent(f)
            ));
        }
        for r in &hits {
            out.push_str("row");
            for f in &fields {
                let v = r.get(&f.name).cloned().unwrap_or_else(|| initial(f));
                let vals = match v {
                    Value::Array(a) => a,
                    v => vec![v],
                };
                for v in vals {
                    out.push('\t');
                    match v.as_str() {
                        Some(s) => out.push_str(&format!("={}", abl_oe::db::escape(s))),
                        None => out.push('?'),
                    }
                }
            }
            out.push('\n');
        }
        Ok((out, format!("done\t{}\t{}\n", hits.len(), u8::from(more))))
    };
    match run() {
        Ok((data, result)) => {
            if fs::write(get(job, "out"), data).is_err() {
                return error(job, 0, "** Cannot write the data file. (fake)");
            }
            finish(job, &result)
        }
        Err(e) => error(job, 0, &e),
    }
}

/// A value converted to the field's type, in its text form.
fn convert(ty: &str, v: &str) -> Result<String, String> {
    match ty {
        "integer" | "int64" => v
            .trim()
            .parse::<i64>()
            .map(|n| n.to_string())
            .map_err(|_| format!("** Invalid character in numeric input {v}. (76)")),
        "decimal" => {
            v.trim()
                .parse::<f64>()
                .map_err(|_| format!("** Invalid character in numeric input {v}. (76)"))?;
            let t = v.trim();
            Ok(if t.contains('.') {
                t.trim_end_matches('0').trim_end_matches('.').to_string()
            } else {
                t.to_string()
            })
        }
        "logical" => match v.trim().to_ascii_lowercase().as_str() {
            "yes" | "true" => Ok("yes".into()),
            "no" | "false" => Ok("no".into()),
            _ => Err(format!("** Invalid value {v} for a logical. (fake)")),
        },
        "date" => {
            let ok = v.len() == 10
                && v.chars().enumerate().all(|(i, c)| {
                    if i == 4 || i == 7 {
                        c == '-'
                    } else {
                        c.is_ascii_digit()
                    }
                });
            if ok {
                Ok(v.to_string())
            } else {
                Err(format!("** Invalid date input {v}. (85)"))
            }
        }
        _ => Ok(v.to_string()),
    }
}

pub fn import(conns: &[Conn], job: &[(String, String)]) -> ExitCode {
    let mut row_no = 0u64;
    let mut run = || -> Result<(u64, u64), String> {
        let (ci, t) = find_table(conns, get(job, "table"))?;
        let mut store = load(&conns[ci].db)?;
        let text = fs::read_to_string(get(job, "data")).map_err(|e| e.to_string())?;
        let key = t.name.to_lowercase();
        let mut rows = store.tables.get(&key).cloned().unwrap_or_default();
        let deleted = if get(job, "replace") == "yes" {
            let n = rows.len() as u64;
            rows.clear();
            n
        } else {
            0
        };
        let mut cols: Vec<(String, u32)> = Vec::new();
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f[0] {
                "field" => cols.push((
                    f[1].to_string(),
                    f.get(2).and_then(|s| s.parse().ok()).unwrap_or(0),
                )),
                "row" => {
                    row_no += 1;
                    let mut rec = Map::new();
                    for fld in &t.fields {
                        rec.insert(fld.name.clone(), initial(fld));
                    }
                    for (k, (name, sub)) in cols.iter().enumerate() {
                        let fld = field_of(&t, name)?;
                        let raw = f.get(k + 1).copied().unwrap_or("?");
                        let v = match raw.strip_prefix('=') {
                            Some(s) => {
                                Value::String(convert(&data_type(fld), &abl_oe::db::unescape(s))?)
                            }
                            None => Value::Null,
                        };
                        let slot = rec.entry(fld.name.clone()).or_insert(Value::Null);
                        match (slot, *sub) {
                            (Value::Array(a), i) if i >= 1 && (i as usize) <= a.len() => {
                                a[i as usize - 1] = v
                            }
                            (s, 0) => *s = v,
                            _ => {
                                return Err(format!(
                                    "** Array subscript {sub} is out of range for {name}. (26)"
                                ))
                            }
                        }
                    }
                    // Unique indexes.
                    for ix in t.indexes.iter().filter(|i| i.has("UNIQUE")) {
                        let key_of = |r: &Map<String, Value>| -> Vec<String> {
                            ix.fields
                                .iter()
                                .map(|f| {
                                    text_of(
                                        r.get(
                                            &field_of(&t, &f.name)
                                                .map(|x| x.name.clone())
                                                .unwrap_or_default(),
                                        ),
                                    )
                                    .unwrap_or_default()
                                    .to_lowercase()
                                })
                                .collect()
                        };
                        let k = key_of(&rec);
                        if rows.iter().any(|r| key_of(r) == k) {
                            let desc: Vec<String> = ix
                                .fields
                                .iter()
                                .zip(&k)
                                .map(|(f, v)| format!("{} {v}", f.name))
                                .collect();
                            return Err(format!(
                                "** {} already exists with {}. (132)",
                                t.name,
                                desc.join(" ")
                            ));
                        }
                    }
                    rows.push(rec);
                }
                _ => {}
            }
        }
        store.tables.insert(key, rows);
        save(&conns[ci].db, &store)?;
        Ok((row_no, deleted))
    };
    match run() {
        Ok((n, d)) => finish(job, &format!("done\t{n}\t{d}\n")),
        Err(e) => error(job, row_no, &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases() {
        let p = phrase("WHERE Balance > 100 AND (Name BEGINS \"a\" OR NOT City = ?) BY Name DESC")
            .unwrap();
        assert!(p.filter.is_some());
        assert_eq!(p.by, [("Name".to_string(), true)]);
        assert!(phrase("WHERE x").is_err());
        assert!(matches_pattern("Hello", "h*o"));
        assert!(!matches_pattern("Hello", "h.o"));
    }
}

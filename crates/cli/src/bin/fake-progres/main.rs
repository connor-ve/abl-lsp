//! A stand-in for OpenEdge's `_progres`, for testing abl without an OpenEdge
//! install. It is not an ABL runtime.
//!
//! It understands what abl sends: `-b -p <program> [-param <text>]` with
//! `PROPATH` and `ABL_JOB` in the environment, and the job and result files
//! described in `abl_oe::driver`.
//!
//! - Compile driver: each file is checked with abl's own parser. Syntax errors
//!   come back as compiler errors; a file that passes gets a placeholder `.r`
//!   under the SAVE INTO folder, at the same relative path.
//! - Run driver, or any other `-p` program: the procedure is "run" by printing
//!   the string of each `MESSAGE "..."` statement, `MESSAGE SESSION:PARAMETER.`
//!   prints -param, `RUN x.p.` runs another file, `RETURN ERROR "..."` fails
//!   and `QUIT.` ends the session.
//!
//! - Test driver: see `faketest.rs`.
//! - Load-df, query and import drivers: see `fakedb.rs`, which also plays the
//!   database tools when this binary is copied under their names.
//!
//! Markers for tests: `/* fake-warning */` in a file adds a compiler warning,
//! and `/* fake-crash */` makes the session exit with status 3 at that file.

mod fakedb;
mod faketest;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use abl_core::analysis::Severity;
use abl_core::workspace::{read_text, Workspace};
use abl_oe::driver;

fn propath() -> Vec<PathBuf> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    env::var("PROPATH")
        .unwrap_or_default()
        .split(sep)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn find(name: &str, propath: &[PathBuf]) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() {
        return p.is_file().then(|| p.to_path_buf());
    }
    let mut dirs = propath.to_vec();
    dirs.extend(env::current_dir().ok());
    for d in dirs {
        let c = d.join(name);
        if c.is_file() {
            return Some(c);
        }
        // RUN x.r / RUN x run the source when there is no r-code.
        let stem = name.strip_suffix(".r").unwrap_or(name);
        for ext in ["p", "w"] {
            let c = d.join(format!("{stem}.{ext}"));
            if !stem.contains('.') && c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

fn job() -> Vec<(String, String)> {
    let path = env::var("ABL_JOB").unwrap_or_default();
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn get<'a>(job: &'a [(String, String)], key: &str) -> Option<&'a str> {
    job.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn compile(job: &[(String, String)]) -> ExitCode {
    let propath = propath();
    let mut ws = Workspace::new();
    ws.propath = propath.clone();
    let mut out = String::new();
    for (_, name) in job.iter().filter(|(k, _)| k == "file") {
        let Some(path) = find(name, &propath) else {
            out.push_str(&format!(
                "msg\t{name}\t\t0\t0\t293\terror\t** \"{name}\" was not found. (293)\n"
            ));
            out.push_str(&format!("done\t{name}\terror\n"));
            continue;
        };
        let text = read_text(&path).unwrap_or_default();
        if text.contains("/* fake-crash */") {
            if let Some(r) = get(job, "result") {
                let _ = fs::write(r, &out);
            }
            eprintln!("fake _progres: crashed on {name}");
            return ExitCode::from(3);
        }
        let ana = ws.analyze_file(&path, text.clone(), None);
        let mut ok = true;
        for d in ana.diags.iter().filter(|d| d.severity == Severity::Error) {
            ok = false;
            let pos = ana.lines.position(&ana.text, d.start);
            out.push_str(&format!(
                "msg\t{name}\t{}\t{}\t{}\t0\terror\t** {} (fake compiler)\n",
                path.display(),
                pos.line + 1,
                pos.character + 1,
                d.message
            ));
        }
        if let Some(i) = text.find("/* fake-warning */") {
            let line = text[..i].matches('\n').count() + 1;
            out.push_str(&format!(
                "msg\t{name}\t{}\t{line}\t1\t0\twarning\tfake warning\n",
                path.display()
            ));
        }
        if ok {
            if let Some(dir) = get(job, "output") {
                let rel = Path::new(name).with_extension("r");
                let rel = if rel.is_absolute() {
                    PathBuf::from(rel.file_name().unwrap_or_default())
                } else {
                    rel
                };
                let target = Path::new(dir).join(rel);
                if let Some(parent) = target.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::write(&target, format!("fake r-code for {name}\n"));
            }
        }
        out.push_str(&format!(
            "done\t{name}\t{}\n",
            if ok { "ok" } else { "error" }
        ));
    }
    match get(job, "result") {
        Some(r) if fs::write(r, out).is_ok() => ExitCode::SUCCESS,
        _ => ExitCode::from(2),
    }
}

enum Outcome {
    Ok,
    Error,
    Quit,
}

thread_local! {
    /// Lines "executed": (file as named on the PROPATH, 1-based line, seconds), for profiling.
    static EXECUTED: std::cell::RefCell<Vec<(String, u32, f64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn propath_name(file: &Path, propath: &[PathBuf]) -> String {
    propath
        .iter()
        .find_map(|d| file.strip_prefix(d).ok())
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Prints the MESSAGE strings of `file`, following RUN statements.
fn run_file(file: &Path, param: &str, propath: &[PathBuf], depth: usize) -> Outcome {
    let text = read_text(file).unwrap_or_default();
    let name = propath_name(file, propath);
    // One statement per line is all the fake understands.
    for (i, raw) in text.lines().enumerate() {
        let stmt = raw.trim().trim_end_matches('.').trim_end();
        if !stmt.is_empty() && !stmt.starts_with("/*") {
            // A line marked /* fake-slow */ "takes" half a second.
            let t = if raw.contains("/* fake-slow */") {
                0.5
            } else {
                0.001
            };
            EXECUTED.with(|e| e.borrow_mut().push((name.clone(), i as u32 + 1, t)));
        }
        let upper = stmt.to_ascii_uppercase();
        if let Some(rest) = upper.strip_prefix("MESSAGE ") {
            if rest.trim() == "SESSION:PARAMETER" {
                println!("{param}");
            } else if let Some(s) = quoted(stmt) {
                println!("{s}");
            }
        } else if upper.starts_with("RETURN ERROR") {
            if let Some(s) = quoted(stmt) {
                println!("{s}");
            }
            return Outcome::Error;
        } else if upper == "QUIT" {
            return Outcome::Quit;
        } else if let Some(target) = upper.strip_prefix("RUN ") {
            let name = stmt[4..4 + target.trim().len()].trim();
            match find(name, propath) {
                Some(p) if depth < 20 => match run_file(&p, param, propath, depth + 1) {
                    Outcome::Ok => {}
                    other => return other,
                },
                _ => {
                    println!("** \"{name}\" was not found. (293)");
                    return Outcome::Error;
                }
            }
        }
    }
    Outcome::Ok
}

fn quoted(s: &str) -> Option<&str> {
    let a = s.find('"')?;
    let b = s[a + 1..].find('"')?;
    Some(&s[a + 1..a + 1 + b])
}

fn run(target: &str, param: &str, result: Option<&str>) -> ExitCode {
    let propath = propath();
    let outcome = match find(target, &propath) {
        Some(p) => run_file(&p, param, &propath, 0),
        None => {
            println!("** \"{target}\" was not found. (293)");
            Outcome::Error
        }
    };
    let code = match outcome {
        Outcome::Quit => return ExitCode::SUCCESS, // never returns to the driver
        Outcome::Ok => 0,
        Outcome::Error => 1,
    };
    if let Some(r) = result {
        let _ = fs::write(r, format!("exit\t{code}\n"));
    }
    ExitCode::SUCCESS
}

/// The REPL driver: connect to abl, run each message, reply ok/error/stop.
fn repl(job: &[(String, String)]) -> ExitCode {
    use std::io::{Read, Write};
    let port = get(job, "port").unwrap_or_default().to_string();
    let dir = PathBuf::from(get(job, "dir").unwrap_or_default());
    let Ok(mut s) = std::net::TcpStream::connect(format!("127.0.0.1:{port}")) else {
        eprintln!("fake repl: cannot connect to port {port}");
        return ExitCode::from(1);
    };
    let propath = propath();
    let mut seq = 0;
    loop {
        let mut head = [0u8; 8];
        if s.read_exact(&mut head).is_err() {
            break;
        }
        let len: usize = std::str::from_utf8(&head)
            .ok()
            .and_then(|h| h.parse().ok())
            .unwrap_or(0);
        if len == 0 {
            break;
        }
        let mut body = vec![0u8; len];
        if s.read_exact(&mut body).is_err() {
            break;
        }
        seq += 1;
        let file = dir.join(format!("abl_repl_{seq}.p"));
        let _ = fs::write(&file, &body);
        // A STOP statement in the code plays a STOP condition.
        let text = String::from_utf8_lossy(&body).to_ascii_uppercase();
        let reply = if text
            .lines()
            .any(|l| l.trim().trim_end_matches('.') == "STOP")
        {
            "stop".to_string()
        } else {
            match run_file(&file, "", &propath, 0) {
                Outcome::Error => "error\n** fake error. (fake)".to_string(),
                _ => "ok".to_string(),
            }
        };
        let _ = std::io::stdout().flush();
        let _ = fs::remove_file(&file);
        if s.write_all(format!("{:08}{reply}", reply.len()).as_bytes())
            .is_err()
        {
            break;
        }
    }
    ExitCode::SUCCESS
}

/// Writes what run_file recorded in the profiler's output format.
fn write_profile(out: &str, target: &str) {
    let executed = EXECUTED.with(|e| e.borrow().clone());
    let mut modules: Vec<String> = Vec::new();
    let mut lines: Vec<(usize, u32, u64, f64)> = Vec::new();
    for (file, line, t) in executed {
        let m = match modules.iter().position(|x| *x == file) {
            Some(m) => m,
            None => {
                modules.push(file);
                modules.len() - 1
            }
        };
        match lines.iter_mut().find(|l| l.0 == m && l.1 == line) {
            Some(l) => {
                l.2 += 1;
                l.3 += t;
            }
            None => lines.push((m, line, 1, t)),
        }
    }
    let mut text = format!("3 01/01/2026 \"abl profile {target}\" 00:00:00 \"fake\"\n.\n");
    for (i, m) in modules.iter().enumerate() {
        text.push_str(&format!("{} \"{m}\" \"\" 0\n", i + 1));
    }
    text.push_str(".\n.\n");
    for (m, line, n, t) in &lines {
        text.push_str(&format!("{} {line} {n} {t:.6} {t:.6}\n", m + 1));
    }
    text.push_str(".\n.\n");
    let _ = fs::write(out, text);
}

/// The first ABL string literal on a line, unescaped: `RUN VALUE("a~"b")` -> `a"b`.
fn abl_string_arg(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let mut out = String::new();
    let mut chars = line[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '~' => out.push(chars.next()?),
            '"' => return Some(out),
            c => out.push(c),
        }
    }
    None
}

/// Plays abl_startup.p: each procedure must be found (absolute, on the
/// PROPATH, or in the working folder); prints `fake startup: <mode> <path>`.
/// A missing one is written to the error file, as the real one does, and
/// the session ends (false).
fn startup(file: &str) -> bool {
    let Ok(text) = fs::read_to_string(file) else {
        return true;
    };
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let err = lines
        .iter()
        .find(|l| l.starts_with("OUTPUT STREAM sErr TO VALUE("))
        .and_then(|l| abl_string_arg(l));
    let sep = if cfg!(windows) { ';' } else { ':' };
    let propath: Vec<PathBuf> = env::var("PROPATH")
        .unwrap_or_default()
        .split(sep)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    for (i, l) in lines.iter().enumerate() {
        if !l.starts_with("RUN VALUE(") {
            continue;
        }
        let Some(path) = abl_string_arg(l) else {
            continue;
        };
        let mode = if lines
            .get(i + 1)
            .is_some_and(|n| n.starts_with("SESSION:ADD-SUPER-PROCEDURE"))
        {
            "super"
        } else if l.contains("PERSISTENT") {
            "persistent"
        } else {
            "run"
        };
        let p = Path::new(&path);
        let found = p.is_absolute() && p.is_file()
            || propath.iter().any(|d| d.join(&path).is_file())
            || Path::new(".").join(&path).is_file();
        if !found {
            if let Some(e) = &err {
                let _ = fs::write(e, format!("{path}\t** \"{path}\" was not found. (293)\n"));
            }
            return false;
        }
        println!("fake startup: {mode} {path}");
    }
    true
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    // Copied under a tool's name (prodb, proserve...), it plays that tool.
    let me = env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    // As `code`: VS Code's command line, for `abl i @code`.
    if me == "code" {
        println!("fake code: {}", args.join(" "));
        return ExitCode::SUCCESS;
    }
    if fakedb::TOOLS.contains(&me.as_str()) {
        return fakedb::tool(&me, &args);
    }
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let Some(program) = value("-p") else {
        eprintln!("fake _progres: only batch sessions (-b -p <program>) are supported");
        return ExitCode::from(2);
    };
    let param = value("-param").unwrap_or_default();
    let first_line = fs::read_to_string(&program)
        .ok()
        .and_then(|t| t.lines().next().map(str::to_string))
        .unwrap_or_default();
    let job = job();
    // A driver whose second line runs abl_startup.p: play the startup procedures.
    if let Some(start) = fs::read_to_string(&program)
        .ok()
        .and_then(|t| t.lines().nth(1).and_then(abl_string_arg))
        .filter(|p| p.ends_with("abl_startup.p"))
    {
        if !startup(&start) {
            return ExitCode::SUCCESS;
        }
    }
    if first_line == driver::MARK_COMPILE {
        compile(&job)
    } else if first_line == driver::MARK_LOAD_DF {
        fakedb::load_df(&fakedb::connections(&args), &job)
    } else if first_line == driver::MARK_QUERY {
        fakedb::query(&fakedb::connections(&args), &job)
    } else if first_line == driver::MARK_IMPORT {
        fakedb::import(&fakedb::connections(&args), &job)
    } else if first_line == driver::MARK_RUN {
        let target = get(&job, "target").unwrap_or_default().to_string();
        run(&target, &param, get(&job, "result"))
    } else if first_line == driver::MARK_TEST {
        faketest::run(&job)
    } else if first_line == driver::MARK_REPL {
        repl(&job)
    } else if first_line == driver::MARK_PROFILE {
        let target = get(&job, "target").unwrap_or_default().to_string();
        let code = run(&target, &param, get(&job, "result"));
        write_profile(get(&job, "out").unwrap_or_default(), &target);
        code
    } else {
        run(&program, &param, None)
    }
}

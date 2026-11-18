//! The test driver, faked. Each listed method or internal procedure is found
//! in its file and its body "run" one statement per line:
//!
//! - `MESSAGE "x".` prints x
//! - `Assert:Equals(a, b).` fails unless a and b are written the same (strings unquoted)
//! - `Assert:IsTrue(x).` fails unless x is TRUE
//! - `RETURN ERROR "x".` raises a Progress.Lang.AppError
//! - `UNDO, THROW NEW Some.Error("x").` raises Some.Error
//! - `STOP.` raises STOP, `QUIT.` ends the session at once
//!
//! A file the parser finds errors in fails to load, like a compile error.

use std::fs;
use std::process::ExitCode;

use abl_core::analysis::{analyze, Severity};
use abl_core::workspace::read_text;

use super::{find, propath};

enum Raised {
    /// (error class, message, line)
    Error(String, String, usize),
    Stop,
    Quit,
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn quoted(s: &str) -> Option<&str> {
    let a = s.find('"')?;
    let b = s[a + 1..].find('"')?;
    Some(&s[a + 1..a + 1 + b])
}

/// The arguments inside the first parentheses, split at top-level commas.
fn args(s: &str) -> Vec<String> {
    let (Some(a), Some(b)) = (s.find('('), s.rfind(')')) else {
        return Vec::new();
    };
    let mut out = vec![String::new()];
    let (mut depth, mut in_str) = (0, false);
    for c in s[a + 1..b].chars() {
        match c {
            '"' => in_str = !in_str,
            '(' if !in_str => depth += 1,
            ')' if !in_str => depth -= 1,
            ',' if !in_str && depth == 0 => {
                out.push(String::new());
                continue;
            }
            _ => {}
        }
        out.last_mut().unwrap().push(c);
    }
    out.iter()
        .map(|x| x.trim().trim_matches('"').to_string())
        .collect()
}

/// The body of a method or internal procedure: (1-based line, text).
fn body(text: &str, name: &str, class: bool) -> Option<Vec<(usize, String)>> {
    let lines: Vec<&str> = text.lines().collect();
    let name_up = name.to_ascii_uppercase();
    let start = lines.iter().position(|l| {
        let u = l.trim().to_ascii_uppercase();
        if class {
            u.starts_with("METHOD ") && u.contains(&format!(" {name_up}("))
        } else {
            u.starts_with("PROCEDURE ")
                && u["PROCEDURE ".len()..].trim_end_matches(':').trim() == name_up
        }
    })?;
    let end_kw = if class { "END METHOD" } else { "END PROCEDURE" };
    Some(
        lines[start + 1..]
            .iter()
            .enumerate()
            .take_while(|(_, l)| !l.trim().to_ascii_uppercase().starts_with(end_kw))
            .map(|(i, l)| {
                (
                    start + i + 2,
                    l.trim().trim_end_matches('.').trim_end().to_string(),
                )
            })
            .collect(),
    )
}

fn exec(lines: &[(usize, String)]) -> Result<(), Raised> {
    for (n, stmt) in lines {
        let up = stmt.to_ascii_uppercase();
        if up.starts_with("MESSAGE ") {
            println!("{}", quoted(stmt).unwrap_or(stmt[8..].trim()));
        } else if up.starts_with("ASSERT:EQUALS(") {
            let a = args(stmt);
            if a.len() == 2 && a[0] != a[1] {
                return Err(Raised::Error(
                    "OpenEdge.Core.AssertionFailedError".into(),
                    format!("Expected: {} but was: {}", a[0], a[1]),
                    *n,
                ));
            }
        } else if up.starts_with("ASSERT:ISTRUE(") {
            let a = args(stmt);
            if a.first().map(|x| x.to_ascii_uppercase()) != Some("TRUE".into()) {
                return Err(Raised::Error(
                    "OpenEdge.Core.AssertionFailedError".into(),
                    "Expected: TRUE but was: FALSE".into(),
                    *n,
                ));
            }
        } else if up.starts_with("RETURN ERROR") {
            return Err(Raised::Error(
                "Progress.Lang.AppError".into(),
                quoted(stmt).unwrap_or("").into(),
                *n,
            ));
        } else if let Some(rest) = up.strip_prefix("UNDO, THROW NEW ") {
            let class = stmt[stmt.len() - rest.len()..]
                .split('(')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            return Err(Raised::Error(class, quoted(stmt).unwrap_or("").into(), *n));
        } else if up == "STOP" {
            return Err(Raised::Stop);
        } else if up == "QUIT" {
            return Err(Raised::Quit);
        }
    }
    Ok(())
}

fn is_a(class: &str, expected: &str) -> bool {
    class.eq_ignore_ascii_case(expected)
        || expected.eq_ignore_ascii_case("Progress.Lang.Error")
        || (expected.eq_ignore_ascii_case("Progress.Lang.AppError")
            && !class.starts_with("Progress.Lang.SysError"))
}

struct Suite {
    id: String,
    class: bool,
    name: String,
    calls: Vec<(String, String, String, String)>, // role, method, id, expected
}

pub fn run(job: &[(String, String)]) -> ExitCode {
    let Some(result) = job
        .iter()
        .find(|(k, _)| k == "result")
        .map(|(_, v)| v.clone())
    else {
        return ExitCode::from(2);
    };
    let mut suites: Vec<Suite> = Vec::new();
    for (k, v) in job {
        let f: Vec<&str> = v.split('\t').collect();
        match k.as_str() {
            "suite" if f.len() >= 3 => suites.push(Suite {
                id: f[0].into(),
                class: f[1] == "cls",
                name: f[2].into(),
                calls: Vec::new(),
            }),
            "before" | "after" | "setup" | "teardown" | "test" => {
                if let Some(s) = suites.last_mut() {
                    let g = |i: usize| f.get(i).copied().unwrap_or("").to_string();
                    s.calls.push((k.clone(), g(0), g(1), g(2)));
                }
            }
            _ => {}
        }
    }
    let propath = propath();
    let mut out = String::new();
    let flush = |out: &str| {
        let _ = fs::write(&result, out);
    };
    for s in &suites {
        let file_name = if s.class {
            format!("{}.cls", s.name.replace('.', "/"))
        } else {
            s.name.clone()
        };
        let text = find(&file_name, &propath).and_then(|p| read_text(&p));
        let load_err = match &text {
            None => Some(format!("Could not find {} (fake)", s.name)),
            Some(t) => analyze(t.clone(), false)
                .diags
                .iter()
                .find(|d| d.severity == Severity::Error)
                .map(|d| format!("** {} (fake compiler)", d.message)),
        };
        let text = text.unwrap_or_default();
        let call = |method: &str| -> Result<(), Raised> {
            match body(&text, method, s.class) {
                Some(b) => exec(&b),
                None => Err(Raised::Error(
                    "Progress.Lang.SysError".into(),
                    format!("Could not find {method} in {} (fake)", s.name),
                    0,
                )),
            }
        };
        let stack = |method: &str, class: &str, line: usize| {
            let me = format!("{method} {} at line {line}  ({file_name})", s.name);
            if class.contains("AssertionFailedError") {
                format!("Equals OpenEdge.Core.Assert at line 50  (OpenEdge/Core/Assert.cls)\n{me}")
            } else {
                me
            }
        };
        let role = |r: &str| -> Result<Option<String>, ()> {
            for (_, m, _, _) in s.calls.iter().filter(|c| c.0 == r) {
                match call(m) {
                    Ok(()) => {}
                    Err(Raised::Quit) => return Err(()),
                    Err(Raised::Stop) => {
                        return Ok(Some(format!("{r} {m}: a STOP condition ended it")))
                    }
                    Err(Raised::Error(_, msg, _)) => return Ok(Some(format!("{r} {m}: {msg}"))),
                }
            }
            Ok(None)
        };
        let mut suite_err = load_err;
        if suite_err.is_none() {
            match role("before") {
                Ok(e) => suite_err = e,
                Err(()) => {
                    flush(&out);
                    return ExitCode::SUCCESS;
                }
            }
        }
        for (_, m, id, expected) in s.calls.iter().filter(|c| c.0 == "test") {
            if let Some(e) = &suite_err {
                out.push_str(&format!("case\t{id}\terror\t0\t{}\t\n", esc(e)));
                continue;
            }
            let (mut status, mut msg, mut st) = match role("setup") {
                Err(()) => {
                    flush(&out);
                    return ExitCode::SUCCESS;
                }
                Ok(Some(e)) => ("error", e, String::new()),
                Ok(None) => match call(m) {
                    Ok(()) if expected.is_empty() => ("pass", String::new(), String::new()),
                    Ok(()) => (
                        "fail",
                        format!("expected {expected} to be raised"),
                        String::new(),
                    ),
                    Err(Raised::Quit) => {
                        flush(&out);
                        return ExitCode::SUCCESS;
                    }
                    Err(Raised::Stop) => {
                        ("error", "a STOP condition ended it".into(), String::new())
                    }
                    Err(Raised::Error(class, text, line)) => {
                        if !expected.is_empty() && is_a(&class, expected) {
                            ("pass", String::new(), String::new())
                        } else {
                            let kind = if class.contains("AssertionFailedError") {
                                "fail"
                            } else {
                                "error"
                            };
                            let text = if text.is_empty() { class.clone() } else { text };
                            (kind, text, stack(m, &class, line))
                        }
                    }
                },
            };
            match role("teardown") {
                Err(()) => {
                    flush(&out);
                    return ExitCode::SUCCESS;
                }
                Ok(Some(e)) if status == "pass" => {
                    status = "error";
                    msg = e;
                    st = String::new();
                }
                _ => {}
            }
            out.push_str(&format!(
                "case\t{id}\t{status}\t1\t{}\t{}\n",
                esc(&msg),
                esc(&st)
            ));
            flush(&out);
        }
        if suite_err.is_none() {
            match role("after") {
                Err(()) => {
                    flush(&out);
                    return ExitCode::SUCCESS;
                }
                Ok(Some(e)) => out.push_str(&format!("suite\t{}\t{}\n", s.id, esc(&e))),
                Ok(None) => {}
            }
        }
    }
    flush(&out);
    ExitCode::SUCCESS
}

//! `abl lint`: syntax diagnostics and lint findings for a project or files, for
//! people (text), tools (JSON) and CI (GitHub annotations). `--fix` applies the
//! same quick fixes the editor offers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use abl_core::features;
use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Position};
use serde_json::{json, Value};

use crate::source::{self, Project};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Output {
    Text,
    Json,
    /// GitHub Actions workflow commands (`::warning file=...::message`).
    Github,
}

pub struct Options {
    pub output: Output,
    /// Fail when there are more warnings than this.
    pub max_warnings: Option<usize>,
    pub fix: bool,
    /// Also report hints (by default only errors, warnings and infos).
    pub hints: bool,
}

struct Finding {
    file: String,
    diag: Diagnostic,
}

fn severity_name(s: Option<DiagnosticSeverity>) -> &'static str {
    match s {
        Some(DiagnosticSeverity::ERROR) => "error",
        Some(DiagnosticSeverity::WARNING) => "warning",
        Some(DiagnosticSeverity::INFORMATION) => "info",
        _ => "hint",
    }
}

fn rule(d: &Diagnostic) -> Option<&str> {
    match &d.code {
        Some(NumberOrString::String(s)) => Some(s),
        _ => None,
    }
}

/// The edit a diagnostic's quick fix makes: (start, end, text) as positions.
fn fix_of(d: &Diagnostic) -> Option<(Position, Position, String)> {
    let f = d.data.as_ref()?.get("fix")?;
    let pos = |v: &Value| {
        Some(Position::new(
            u32::try_from(v.get("line")?.as_u64()?).ok()?,
            u32::try_from(v.get("character")?.as_u64()?).ok()?,
        ))
    };
    let start = pos(f)?;
    let end = f.get("end").and_then(pos).unwrap_or(start);
    Some((start, end, f.get("text")?.as_str()?.to_string()))
}

/// Applies every quick fix that doesn't overlap another, repeating until none
/// are left (a fix can uncover another). Returns how many were applied.
fn fix_file(p: &Project, path: &Path) -> Result<usize, String> {
    let src = source::read(path)?;
    let mut text = src.text.clone();
    let mut applied = 0;
    for _ in 0..10 {
        let ana = Arc::new(p.analyze(path, text.clone()));
        let mut edits: Vec<(usize, usize, String)> = features::diagnostics(&p.ws, path, &ana)
            .iter()
            .filter_map(fix_of)
            .map(|(s, e, t)| {
                (
                    ana.lines.offset(&ana.text, s),
                    ana.lines.offset(&ana.text, e),
                    t,
                )
            })
            .collect();
        if edits.is_empty() {
            break;
        }
        // Apply from the end so earlier offsets stay valid; skip overlaps.
        edits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
        edits.dedup();
        let mut limit = usize::MAX;
        let mut n = 0;
        for (s, e, t) in edits {
            if e > limit {
                continue;
            }
            text.replace_range(s..e, &t);
            limit = s;
            n += 1;
        }
        applied += n;
    }
    if text != src.text {
        source::write(path, &text, src.encoding)?;
    }
    Ok(applied)
}

pub fn run(cwd: &Path, paths: &[PathBuf], opts: &Options) -> Result<u8, String> {
    let project = Project::open(cwd)?;
    let files = project.select(paths, cwd)?;
    let mut fixed = 0;
    if opts.fix {
        for f in &files {
            fixed += fix_file(&project, f)?;
        }
    }
    let mut findings = Vec::new();
    for f in &files {
        let src = source::read(f)?;
        let ana = project.analyze(f, src.text);
        let shown = project.show(f, cwd);
        for d in features::diagnostics(&project.ws, f, &ana) {
            if !opts.hints && d.severity == Some(DiagnosticSeverity::HINT) {
                continue;
            }
            findings.push(Finding {
                file: shown.clone(),
                diag: d,
            });
        }
    }
    findings.sort_by(|a, b| {
        (
            &a.file,
            a.diag.range.start.line,
            a.diag.range.start.character,
        )
            .cmp(&(
                &b.file,
                b.diag.range.start.line,
                b.diag.range.start.character,
            ))
    });
    let count = |s| {
        findings
            .iter()
            .filter(|f| f.diag.severity == Some(s))
            .count()
    };
    let errors = count(DiagnosticSeverity::ERROR);
    let warnings = count(DiagnosticSeverity::WARNING);
    match opts.output {
        Output::Text => print_text(&findings, files.len(), errors, warnings, fixed, opts.fix),
        Output::Json => print_json(&findings, files.len(), errors, warnings, fixed),
        Output::Github => print_github(&findings),
    }
    let too_many_warnings = opts.max_warnings.is_some_and(|m| warnings > m);
    if too_many_warnings && opts.output == Output::Text {
        eprintln!(
            "abl: {warnings} warnings, more than --max-warnings {}",
            opts.max_warnings.unwrap_or(0)
        );
    }
    Ok(u8::from(errors > 0 || too_many_warnings))
}

fn print_text(
    findings: &[Finding],
    files: usize,
    errors: usize,
    warnings: usize,
    fixed: usize,
    fix: bool,
) {
    for f in findings {
        let d = &f.diag;
        let code = rule(d).map(|r| format!(" [{r}]")).unwrap_or_default();
        println!(
            "{}:{}:{}: {}{code}: {}",
            f.file,
            d.range.start.line + 1,
            d.range.start.character + 1,
            severity_name(d.severity),
            d.message
        );
    }
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    let mut summary = format!(
        "{}, {} in {}",
        plural(errors, "error"),
        plural(warnings, "warning"),
        plural(files, "file")
    );
    if fix {
        summary.push_str(&format!(" ({fixed} fixed)"));
    }
    if !findings.is_empty() {
        println!();
    }
    println!("{summary}");
}

fn print_json(findings: &[Finding], files: usize, errors: usize, warnings: usize, fixed: usize) {
    let items: Vec<Value> = findings
        .iter()
        .map(|f| {
            let r = f.diag.range;
            json!({
                "file": f.file,
                "line": r.start.line + 1,
                "column": r.start.character + 1,
                "endLine": r.end.line + 1,
                "endColumn": r.end.character + 1,
                "severity": severity_name(f.diag.severity),
                "rule": rule(&f.diag),
                "message": f.diag.message,
                "fixable": fix_of(&f.diag).is_some(),
            })
        })
        .collect();
    let out = json!({
        "files": files,
        "errors": errors,
        "warnings": warnings,
        "fixed": fixed,
        "findings": items,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

/// Escapes for GitHub workflow commands: data and property values differ.
pub fn gh_escape(s: &str, property: bool) -> String {
    let s = s
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A");
    if property {
        s.replace(':', "%3A").replace(',', "%2C")
    } else {
        s
    }
}

fn print_github(findings: &[Finding]) {
    for f in findings {
        let d = &f.diag;
        let level = match d.severity {
            Some(DiagnosticSeverity::ERROR) => "error",
            Some(DiagnosticSeverity::WARNING) => "warning",
            _ => "notice",
        };
        let title = match rule(d) {
            Some(r) => format!("abl {r}"),
            None => "abl".to_string(),
        };
        println!(
            "::{level} file={},line={},col={},endLine={},endColumn={},title={}::{}",
            gh_escape(&f.file, true),
            d.range.start.line + 1,
            d.range.start.character + 1,
            d.range.end.line + 1,
            d.range.end.character + 1,
            gh_escape(&title, true),
            gh_escape(&d.message, false)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_escaping() {
        assert_eq!(gh_escape("a:b,c%\n", true), "a%3Ab%2Cc%25%0A");
        assert_eq!(gh_escape("a:b,c%\n", false), "a:b,c%25%0A");
    }
}

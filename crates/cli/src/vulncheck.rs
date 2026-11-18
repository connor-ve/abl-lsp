//! `abl vulncheck`: checks a project against advisories you maintain.
//!
//! Advisories are TOML files, each with `[[advisory]]` entries. An advisory
//! matches one of three things:
//!
//! ```toml
//! [[advisory]]
//! id = "ACME-2026-001"
//! title = "What is wrong"
//! severity = "high"                 # low, medium, high or critical
//! description = "Why it matters."   # optional, like url and fix
//! fix = "What to do."
//!
//! [advisory.oe]                     # the OpenEdge release in use
//! affected = ["11.7.0 - 11.7.15", "< 12.2.12", "12.0"]
//!
//! [advisory.package]                # an installed package (abl.lock)
//! name = "abl-json"                 # or git = "github.com/someone/abl-json"
//! affected = ["< 1.4.2"]            # versions from the package's tag
//! commits = ["3f2a9c1"]             # or exact commits (prefixes)
//!
//! [advisory.code]                   # statements in the code, packages' too
//! statement = "OS-COMMAND"          # the statement's first keyword(s); abbreviations work
//! contains = ["VALUE("]             # all must appear (case and spaces ignored)
//! not_contains = ["SILENT"]         # none may appear
//! ```
//!
//! A version range is `a - b` (inclusive), `< a`, `<= a`, `> a`, `>= a`, or a
//! version alone, which matches it and everything under it (`12.2` is any
//! 12.2.x). A `vulncheck-ignore <id>` comment on a statement's line, or the
//! line above, silences that advisory there; `[vulncheck] ignore` in abl.toml
//! silences it everywhere.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

use abl_core::analysis::Analysis;
use abl_core::keywords::keyword;
use abl_core::workspace::is_abl_file;
use abl_oe::config::{self, Lock};
use abl_oe::session::Env;
use serde::Deserialize;
use serde_json::json;

use crate::lint::gh_escape;
use crate::source::{self, Project};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    fn word(self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Text,
    Json,
    Github,
}

#[derive(Debug, Deserialize)]
struct AdvisoryFile {
    #[serde(default, rename = "advisory")]
    advisories: Vec<Advisory>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Advisory {
    pub id: String,
    pub title: String,
    pub severity: Severity,
    #[serde(default)]
    pub description: String,
    pub url: Option<String>,
    pub fix: Option<String>,
    pub oe: Option<OeRule>,
    pub package: Option<PackageRule>,
    pub code: Option<CodeRule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OeRule {
    pub affected: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRule {
    pub name: Option<String>,
    pub git: Option<String>,
    #[serde(default)]
    pub affected: Vec<String>,
    #[serde(default)]
    pub commits: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeRule {
    pub statement: Option<String>,
    #[serde(default)]
    pub contains: Vec<String>,
    #[serde(default)]
    pub not_contains: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub advisory: Advisory,
    /// What matched: a file and line, a package or the OE release.
    pub file: Option<PathBuf>,
    pub line: usize,
    pub what: String,
}

// ---- versions ----

fn parse_version(v: &str) -> Option<Vec<u64>> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let parts: Vec<u64> = v
        .split(['.', '-', '+'])
        .map_while(|p| p.parse().ok())
        .collect();
    (!parts.is_empty()).then_some(parts)
}

fn cmp(a: &[u64], b: &[u64]) -> Ordering {
    for i in 0..a.len().max(b.len()) {
        match a.get(i).unwrap_or(&0).cmp(b.get(i).unwrap_or(&0)) {
            Ordering::Equal => {}
            o => return o,
        }
    }
    Ordering::Equal
}

/// Does `version` fall in `range`? None when the range can't be read.
pub fn in_range(version: &str, range: &str) -> Option<bool> {
    let v = parse_version(version)?;
    let r = range.trim();
    if let Some((lo, hi)) = r.split_once(" - ") {
        let (lo, hi) = (parse_version(lo)?, parse_version(hi)?);
        return Some(cmp(&v, &lo) != Ordering::Less && cmp(&v, &hi) != Ordering::Greater);
    }
    for (op, test) in [
        ("<=", (|o| o != Ordering::Greater) as fn(Ordering) -> bool),
        (">=", |o| o != Ordering::Less),
        ("<", |o| o == Ordering::Less),
        (">", |o| o == Ordering::Greater),
        ("=", |o| o == Ordering::Equal),
    ] {
        if let Some(rest) = r.strip_prefix(op) {
            return Some(test(cmp(&v, &parse_version(rest)?)));
        }
    }
    // A version alone: it and everything under it.
    let p = parse_version(r)?;
    Some(v.len() >= p.len() && v[..p.len()] == p[..] || cmp(&v, &p) == Ordering::Equal)
}

// ---- loading ----

/// The advisory files: `--db`, abl.toml's `[vulncheck] db`, or ~/.abl/advisories.
pub fn db_path(project: &Project, flag: Option<&Path>, cwd: &Path) -> Result<PathBuf, String> {
    if let Some(f) = flag {
        return Ok(if f.is_absolute() {
            f.to_path_buf()
        } else {
            cwd.join(f)
        });
    }
    if let Some(l) = &project.loaded {
        if let Some(db) = &l.project.vulncheck.db {
            return Ok(l.path(db));
        }
    }
    Ok(config::home()?.join("advisories"))
}

pub fn load(db: &Path) -> Result<(Vec<Advisory>, usize), String> {
    let mut files: Vec<PathBuf> = if db.is_file() {
        vec![db.to_path_buf()]
    } else if db.is_dir() {
        walkdir::WalkDir::new(db)
            .into_iter()
            .filter_entry(|e| e.file_name() != ".git")
            .flatten()
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
            })
            .collect()
    } else {
        return Err(format!(
            "no advisories at {}: `abl vulncheck init` writes examples there, `abl vulncheck fetch <git url>` clones yours",
            db.display()
        ));
    };
    files.sort();
    let mut all: Vec<Advisory> = Vec::new();
    for f in &files {
        let text = fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let parsed: AdvisoryFile =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", f.display()))?;
        for a in parsed.advisories {
            let rules = [a.oe.is_some(), a.package.is_some(), a.code.is_some()];
            if rules.iter().filter(|r| **r).count() != 1 {
                return Err(format!(
                    "{}: advisory {} needs exactly one of [advisory.oe], [advisory.package], [advisory.code]",
                    f.display(),
                    a.id
                ));
            }
            if all.iter().any(|x| x.id == a.id) {
                return Err(format!(
                    "{}: advisory {} is defined twice",
                    f.display(),
                    a.id
                ));
            }
            all.push(a);
        }
    }
    Ok((all, files.len()))
}

// ---- matching code ----

fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_uppercase()
}

/// The statement's first keywords match `statement` (full or abbreviated).
fn starts_with_keywords(ana: &Analysis, toks: &[usize], statement: &str) -> bool {
    let want: Vec<&str> = statement.split_whitespace().collect();
    want.len() <= toks.len()
        && want.iter().zip(toks).all(|(w, &t)| {
            let full = keyword(w).unwrap_or(w);
            ana.kw(t).is_some_and(|k| k.eq_ignore_ascii_case(full))
                || ana.tok_text(t).eq_ignore_ascii_case(w)
        })
}

fn ignored_here(text: &str, line: usize, id: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    [line.checked_sub(1), Some(line)]
        .into_iter()
        .flatten()
        .filter_map(|l| lines.get(l))
        .any(|l| {
            l.find("vulncheck-ignore").is_some_and(|i| {
                l[i..]
                    .split_whitespace()
                    .any(|w| w.trim_end_matches("*/") == id)
            })
        })
}

pub fn code_findings(ana: &Analysis, path: &Path, rules: &[&Advisory]) -> Vec<Finding> {
    let mut out = Vec::new();
    for st in &ana.statements {
        let toks: Vec<usize> = ana.stmt_tokens(st).collect();
        if toks.is_empty() {
            continue;
        }
        let start = ana.tokens[st.first].start;
        let end = st
            .term
            .map_or(ana.tokens[st.last].end, |t| ana.tokens[t].end);
        let text: String = toks
            .iter()
            .map(|&t| ana.tok_text(t))
            .collect::<Vec<_>>()
            .join(" ");
        let flat = squash(&text);
        for a in rules {
            let Some(rule) = &a.code else { continue };
            if rule
                .statement
                .as_deref()
                .is_some_and(|s| !starts_with_keywords(ana, &toks, s))
            {
                continue;
            }
            if !rule.contains.iter().all(|c| flat.contains(&squash(c)))
                || rule.not_contains.iter().any(|c| flat.contains(&squash(c)))
            {
                continue;
            }
            let line = ana.lines.position(&ana.text, start).line as usize;
            if ignored_here(&ana.text, line, &a.id) {
                continue;
            }
            let src = ana.text[start..end]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let src = if src.chars().count() > 70 {
                format!("{}...", src.chars().take(67).collect::<String>())
            } else {
                src
            };
            out.push(Finding {
                advisory: (*a).clone(),
                file: Some(path.to_path_buf()),
                line: line + 1,
                what: src,
            });
        }
    }
    out
}

fn same_git(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.trim_end_matches(['/', '\\']);
        let s = s.strip_suffix(".git").unwrap_or(s);
        let s = s.split("://").last().unwrap_or(s);
        s.replace('\\', "/").to_lowercase()
    };
    norm(a) == norm(b)
}

// ---- the command ----

pub struct Options {
    pub db: Option<PathBuf>,
    pub format: Format,
    pub fail_on: Severity,
}

pub fn run(cwd: &Path, flag: Option<&str>, opts: &Options) -> Result<u8, String> {
    let project = Project::open(cwd)?;
    let db = db_path(&project, opts.db.as_deref(), cwd)?;
    let (advisories, nfiles) = load(&db)?;
    let ignore: Vec<String> = project
        .loaded
        .as_ref()
        .map(|l| l.project.vulncheck.ignore.clone())
        .unwrap_or_default();
    let active: Vec<&Advisory> = advisories
        .iter()
        .filter(|a| !ignore.contains(&a.id))
        .collect();
    let mut findings: Vec<Finding> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    // The OpenEdge release.
    let oe_rules: Vec<&&Advisory> = active.iter().filter(|a| a.oe.is_some()).collect();
    if !oe_rules.is_empty() {
        match Env::new(project.loaded.as_ref(), flag, cwd) {
            Ok(env) => {
                let release = config::install_release(&env.dlc).unwrap_or(env.version.clone());
                if release.split('.').count() < 3 {
                    notes.push(format!(
                        "OE {release}: the install doesn't say its patch level, so ranges are matched on {release}"
                    ));
                }
                for a in oe_rules {
                    let rule = a.oe.as_ref().unwrap();
                    if rule
                        .affected
                        .iter()
                        .any(|r| in_range(&release, r) == Some(true))
                    {
                        findings.push(Finding {
                            advisory: (**a).clone(),
                            file: None,
                            line: 0,
                            what: format!("OpenEdge {release}"),
                        });
                    }
                }
            }
            Err(e) => notes.push(format!("OpenEdge advisories skipped: {e}")),
        }
    }

    // Installed packages.
    let pkg_rules: Vec<&&Advisory> = active.iter().filter(|a| a.package.is_some()).collect();
    if let Some(l) = project.loaded.as_ref().filter(|_| !pkg_rules.is_empty()) {
        for p in Lock::load(&l.root)?.packages {
            for a in &pkg_rules {
                let rule = a.package.as_ref().unwrap();
                let named = rule
                    .name
                    .as_ref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(&p.name))
                    || rule.git.as_ref().is_some_and(|g| same_git(g, &p.git));
                if !named {
                    continue;
                }
                let by_commit = rule.commits.iter().any(|c| {
                    c.len() >= 7 && p.commit.to_lowercase().starts_with(&c.to_lowercase())
                });
                let version = p
                    .reference
                    .as_deref()
                    .filter(|r| parse_version(r).is_some());
                let by_version = version
                    .is_some_and(|v| rule.affected.iter().any(|r| in_range(v, r) == Some(true)));
                if by_commit || by_version {
                    findings.push(Finding {
                        advisory: (**a).clone(),
                        file: None,
                        line: 0,
                        what: format!(
                            "package {} {} ({})",
                            p.name,
                            p.reference.as_deref().unwrap_or("default branch"),
                            &p.commit[..p.commit.len().min(7)]
                        ),
                    });
                } else if version.is_none() && !rule.affected.is_empty() {
                    notes.push(format!(
                        "package {}: {} names versions, but it is installed from {} (not a version tag); check by hand",
                        p.name,
                        a.id,
                        p.reference.as_deref().unwrap_or("the default branch")
                    ));
                }
            }
        }
    }

    // Code, the project's and the installed packages'.
    let code_rules: Vec<&Advisory> = active
        .iter()
        .copied()
        .filter(|a| a.code.is_some())
        .collect();
    if !code_rules.is_empty() {
        let mut files = project.files.clone();
        if let Some(l) = &project.loaded {
            let vendor = l.root.join(config::VENDOR_DIR);
            if vendor.is_dir() {
                files.extend(
                    walkdir::WalkDir::new(&vendor)
                        .into_iter()
                        .filter_entry(|e| e.file_name() != ".git")
                        .flatten()
                        .filter(|e| e.file_type().is_file() && is_abl_file(e.path()))
                        .map(|e| e.into_path()),
                );
            }
        }
        for f in &files {
            let Ok(src) = source::read(f) else { continue };
            let ana = project.analyze(f, src.text);
            findings.extend(code_findings(&ana, f, &code_rules));
        }
    }

    findings.sort_by(|a, b| {
        b.advisory
            .severity
            .cmp(&a.advisory.severity)
            .then(a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });
    let failing = findings
        .iter()
        .filter(|f| f.advisory.severity >= opts.fail_on)
        .count();
    let show = |p: &Path| project.show(p, cwd);
    match opts.format {
        Format::Text => {
            for f in &findings {
                let a = &f.advisory;
                println!(
                    "{:<8} {}  {}",
                    a.severity.word().to_uppercase(),
                    a.id,
                    a.title
                );
                match &f.file {
                    Some(p) => println!("         {}:{}  {}", show(p), f.line, f.what),
                    None => println!("         {}", f.what),
                }
                if let Some(fix) = &a.fix {
                    println!("         fix: {fix}");
                }
                if let Some(u) = &a.url {
                    println!("         {u}");
                }
            }
            for n in &notes {
                println!("note: {n}");
            }
            let mut counts = Vec::new();
            for s in [
                Severity::Critical,
                Severity::High,
                Severity::Medium,
                Severity::Low,
            ] {
                let n = findings.iter().filter(|f| f.advisory.severity == s).count();
                if n > 0 {
                    counts.push(format!("{n} {}", s.word()));
                }
            }
            if !findings.is_empty() || !notes.is_empty() {
                println!();
            }
            println!(
                "{} finding{}{} ({} advisories in {} file{}, {})",
                findings.len(),
                if findings.len() == 1 { "" } else { "s" },
                if counts.is_empty() {
                    String::new()
                } else {
                    format!(": {}", counts.join(", "))
                },
                active.len(),
                nfiles,
                if nfiles == 1 { "" } else { "s" },
                db.display()
            );
        }
        Format::Json => {
            let list: Vec<_> = findings
                .iter()
                .map(|f| {
                    json!({
                        "id": f.advisory.id,
                        "title": f.advisory.title,
                        "description": f.advisory.description,
                        "severity": f.advisory.severity.word(),
                        "file": f.file.as_deref().map(show),
                        "line": f.line,
                        "what": f.what,
                        "fix": f.advisory.fix,
                        "url": f.advisory.url,
                    })
                })
                .collect();
            let doc = json!({"findings": list, "notes": notes, "advisories": active.len()});
            println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        }
        Format::Github => {
            for f in &findings {
                let a = &f.advisory;
                let level = if a.severity >= Severity::High {
                    "error"
                } else {
                    "warning"
                };
                let title = gh_escape(&format!("{} {}", a.id, a.title), true);
                let msg = gh_escape(
                    &format!(
                        "{}{}",
                        f.what,
                        a.fix
                            .as_ref()
                            .map_or(String::new(), |x| format!(" Fix: {x}"))
                    ),
                    false,
                );
                match &f.file {
                    Some(p) => println!(
                        "::{level} file={},line={},title={title}::{msg}",
                        gh_escape(&show(p), true),
                        f.line
                    ),
                    None => println!("::{level} title={title}::{msg}"),
                }
            }
        }
    }
    Ok(u8::from(failing > 0))
}

// ---- init and fetch ----

const EXAMPLES: &str = r#"# Advisories for `abl vulncheck`. Each [[advisory]] has an id, a title, a
# severity (low, medium, high, critical) and one rule: [advisory.oe],
# [advisory.package] or [advisory.code]. Edit these, add your own, or keep
# them in a git repository and use `abl vulncheck fetch <url>`.
#
# These examples are generic code patterns, not published vulnerabilities.

[[advisory]]
id = "EXAMPLE-CODE-1"
title = "Shell command built at runtime"
severity = "high"
description = "A command computed from data can run anything the data says."
fix = "Use a fixed command, or check every part of it against a list of allowed values."

[advisory.code]
statement = "OS-COMMAND"
contains = ["VALUE("]

[[advisory]]
id = "EXAMPLE-CODE-2"
title = "Password written in a CONNECT"
severity = "medium"
fix = "Read credentials from the environment or a protected file at runtime."

[advisory.code]
statement = "CONNECT"
contains = ["-P"]

[[advisory]]
id = "EXAMPLE-CODE-3"
title = "Query text built by concatenation"
severity = "medium"
description = "Concatenated user input in QUERY-PREPARE can change the query."
fix = "Quote values with QUOTER(), or check them before building the WHERE."

[advisory.code]
contains = ["QUERY-PREPARE(", "+"]

[[advisory]]
id = "EXAMPLE-CODE-4"
title = "Procedure name taken from data"
severity = "low"
fix = "Map allowed names to procedures instead of running what the data names."

[advisory.code]
statement = "RUN"
contains = ["VALUE("]

# A rule for the OpenEdge release in use (fill in a real advisory's ranges):
#
# [[advisory]]
# id = "MY-OE-1"
# title = "..."
# severity = "critical"
# fix = "Upgrade OpenEdge."
#
# [advisory.oe]
# affected = ["11.7.0 - 11.7.15"]

# A rule for a package installed with `abl install`:
#
# [[advisory]]
# id = "MY-PKG-1"
# title = "..."
# severity = "high"
#
# [advisory.package]
# name = "abl-json"
# affected = ["< 1.4.2"]
"#;

pub fn init(cwd: &Path, dir: Option<&Path>) -> Result<u8, String> {
    let project = Project::open(cwd)?;
    let dir = db_path(&project, dir, cwd)?;
    let file = if dir
        .extension()
        .is_some_and(|x| x.eq_ignore_ascii_case("toml"))
    {
        dir
    } else {
        dir.join("examples.toml")
    };
    if file.exists() {
        return Err(format!("{} exists already", file.display()));
    }
    if let Some(d) = file.parent() {
        fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    fs::write(&file, EXAMPLES).map_err(|e| format!("{}: {e}", file.display()))?;
    println!("Wrote example advisories to {}", file.display());
    Ok(0)
}

/// Clones an advisory repository into the default folder, or pulls it.
pub fn fetch(cwd: &Path, url: Option<&str>) -> Result<u8, String> {
    let project = Project::open(cwd)?;
    let dir = db_path(&project, None, cwd)?;
    let git = |args: &[&str], at: &Path| -> Result<(), String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(at)
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    };
    if dir.join(".git").exists() {
        if url.is_some() {
            return Err(format!(
                "{} is already a clone; run `abl vulncheck fetch` to update it",
                dir.display()
            ));
        }
        git(&["pull", "--quiet", "--ff-only"], &dir)?;
        println!("Updated {}", dir.display());
    } else {
        let url = url.ok_or_else(|| {
            format!(
                "{} isn't a git clone; give the repository: abl vulncheck fetch <url>",
                dir.display()
            )
        })?;
        if dir.exists() && fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some()) {
            return Err(format!(
                "{} has files already; move them away first",
                dir.display()
            ));
        }
        let parent = dir.parent().unwrap_or(&dir);
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        git(
            &["clone", "--quiet", url, &dir.display().to_string()],
            parent,
        )?;
        println!("Cloned {url} into {}", dir.display());
    }
    let (a, n) = load(&dir)?;
    println!(
        "{} advisories in {n} file{}",
        a.len(),
        if n == 1 { "" } else { "s" }
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use abl_core::analysis::analyze;

    #[test]
    fn ranges() {
        assert_eq!(in_range("11.7.4", "11.7.0 - 11.7.15"), Some(true));
        assert_eq!(in_range("11.7.16", "11.7.0 - 11.7.15"), Some(false));
        assert_eq!(in_range("12.2.11", "< 12.2.12"), Some(true));
        assert_eq!(in_range("v1.4.2", "< 1.4.2"), Some(false));
        assert_eq!(in_range("1.4.2", "<= 1.4.2"), Some(true));
        assert_eq!(in_range("12.2.3", "12.2"), Some(true));
        assert_eq!(in_range("12.20", "12.2"), Some(false));
        assert_eq!(in_range("12.8", ">= 12.2"), Some(true));
        assert_eq!(in_range("main", "< 1.0"), None);
    }

    #[test]
    fn code_rules_and_ignores() {
        let file: AdvisoryFile = toml::from_str(EXAMPLES).unwrap();
        let rules: Vec<&Advisory> = file.advisories.iter().collect();
        let src = "OS-COMMAND SILENT VALUE(cCmd).\nOS-COMMAND \"dir\".\n\
                   CONNECT -db sports -U admin -P \"secret\".\n\
                   /* vulncheck-ignore EXAMPLE-CODE-4 */\nRUN VALUE(cProc).\n\
                   hQ:QUERY-PREPARE(\"FOR EACH c WHERE c.name = '\" + cName + \"'\").\n\
                   RUN VALUE(cOther).\n";
        let ana = analyze(src.into(), false);
        let f = code_findings(&ana, Path::new("a.p"), &rules);
        let got: Vec<(&str, usize)> = f.iter().map(|x| (x.advisory.id.as_str(), x.line)).collect();
        assert_eq!(
            got,
            [
                ("EXAMPLE-CODE-1", 1),
                ("EXAMPLE-CODE-2", 3),
                ("EXAMPLE-CODE-3", 6),
                ("EXAMPLE-CODE-4", 7)
            ]
        );
        assert!(
            f[0].what.starts_with("OS-COMMAND SILENT VALUE(cCmd)"),
            "{}",
            f[0].what
        );
    }
}

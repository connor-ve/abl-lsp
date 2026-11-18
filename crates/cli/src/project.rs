//! Project commands: `abl status` (the default), `abl env`, `abl doctor`, `abl init`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::config::{self, Global, Loaded, Source};

const SOURCE_EXTS: &[&str] = &["p", "w", "cls", "i", "t"];

/// Version, where it came from, and its DLC folder if this machine has it.
struct Oe {
    version: String,
    source: Source,
    dlc: Result<PathBuf, String>,
}

fn resolve_oe(flag: Option<&str>, loaded: Option<&Loaded>, global: &Global) -> Option<Oe> {
    let (version, source) = config::resolve_version(flag, loaded.map(|l| &l.project), global)?;
    let dlc = config::dlc_for(global, &version).map(Path::to_path_buf);
    Some(Oe {
        version,
        source,
        dlc,
    })
}

/// The project's own PROPATH entries as absolute paths (just the project root
/// if none are set), without installed packages: for counting its sources.
fn propath(loaded: &Loaded) -> Vec<PathBuf> {
    if loaded.project.propath.is_empty() {
        return vec![loaded.root.clone()];
    }
    loaded
        .project
        .propath
        .iter()
        .map(|p| loaded.path(p))
        .collect()
}

fn count_sources(dirs: &[PathBuf]) -> usize {
    let mut seen = BTreeSet::new();
    for d in dirs {
        for e in WalkDir::new(d).into_iter().filter_map(Result::ok) {
            let is_src = e.file_type().is_file()
                && (abl_core::workspace::is_abl_file(e.path())
                    || e.path()
                        .extension()
                        .and_then(|x| x.to_str())
                        .is_some_and(|x| SOURCE_EXTS.contains(&x.to_ascii_lowercase().as_str())));
            if is_src {
                seen.insert(e.into_path());
            }
        }
    }
    seen.len()
}

/// `abl status`. Returns false when there is no project (the caller shows help).
pub fn status(cwd: &Path, flag: Option<&str>) -> Result<bool, String> {
    let Some(loaded) = config::load_project(cwd)? else {
        return Ok(false);
    };
    let global = config::load_global()?;
    let p = &loaded.project;
    let mut out = String::new();
    let row = |out: &mut String, k: &str, v: &str| {
        let _ = writeln!(out, "{k:<9}{v}");
    };
    row(&mut out, "project", &loaded.root.display().to_string());
    let oe = match resolve_oe(flag, Some(&loaded), &global) {
        None => "not set (set `oe` in abl.toml or run `abl oe use <version>`)".to_string(),
        Some(Oe {
            version,
            source,
            dlc: Ok(dlc),
        }) => format!("{version} (from {source}) -> {}", dlc.display()),
        Some(Oe {
            version, source, ..
        }) => format!("{version} (from {source}), not installed here"),
    };
    row(&mut out, "oe", &oe);
    let pp = if p.propath.is_empty() {
        ". (propath not set)".to_string()
    } else {
        p.propath.join(", ")
    };
    row(&mut out, "propath", &pp);
    if let Some(o) = &p.output {
        row(&mut out, "output", o);
    }
    if let Some(e) = &p.entry {
        row(&mut out, "entry", e);
    }
    for (name, db) in &p.db {
        let mut detail = match (&db.schema, &db.connect) {
            (Some(s), _) => format!("{name} ({s})"),
            (None, Some(c)) => format!("{name} ({c})"),
            (None, None) => name.clone(),
        };
        if db.connect.is_none() {
            let state = crate::db::state(&loaded.db_path(name, db));
            detail.push_str(&format!(", {state}"));
        }
        row(&mut out, "db", &detail);
    }
    let n = count_sources(&propath(&loaded));
    row(&mut out, "sources", &format!("{n} files"));
    // Needs a usable OpenEdge install: the record of the last compile is tied to it.
    if let Ok(proj) = crate::source::Project::open(cwd) {
        if let Some((stale, total)) = crate::compile::stale(&proj, flag, cwd) {
            let build = if stale == 0 {
                format!("up to date ({total} files)")
            } else {
                format!("{stale} of {total} files need compiling (abl compile)")
            };
            row(&mut out, "build", &build);
        }
    }
    print!("{out}");
    Ok(true)
}

/// `abl env`: the environment abl will hand to OpenEdge, as KEY=value lines.
pub fn env(cwd: &Path, flag: Option<&str>) -> Result<(), String> {
    let loaded = config::load_project(cwd)?;
    let global = config::load_global()?;
    let sep = if cfg!(windows) { ";" } else { ":" };
    println!("ABL_CONFIG={}", config::global_path()?.display());
    match &loaded {
        Some(l) => println!("ABL_PROJECT={}", l.file.display()),
        None => println!("ABL_PROJECT="),
    }
    match resolve_oe(flag, loaded.as_ref(), &global) {
        Some(oe) => {
            println!("ABL_OE={}", oe.version);
            println!("ABL_OE_SOURCE={}", oe.source);
            match &oe.dlc {
                Ok(d) => println!("DLC={}", d.display()),
                Err(_) => println!("DLC="),
            }
        }
        None => {
            println!("ABL_OE=");
            println!("DLC=");
        }
    }
    if let Some(l) = &loaded {
        let pp: Vec<String> = l
            .propath()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        println!("PROPATH={}", pp.join(sep));
    }
    // What every session gets: database connections, then `startup`.
    if let Ok(e) = abl_oe::session::Env::new(loaded.as_ref(), flag, cwd) {
        let params: Vec<String> = e
            .params
            .iter()
            .map(|p| {
                if p.is_empty() || p.contains(char::is_whitespace) {
                    format!("\"{p}\"")
                } else {
                    p.clone()
                }
            })
            .collect();
        println!("ABL_STARTUP={}", params.join(" "));
        let procs: Vec<String> = e
            .startup
            .iter()
            .map(|(m, p)| format!("{}:{p}", m.word()))
            .collect();
        println!("ABL_PROCEDURES={}", procs.join(" "));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Ok,
    Warn,
    Fail,
}

struct Report {
    worst: Level,
}

impl Report {
    fn line(&mut self, level: Level, msg: impl AsRef<str>) {
        let tag = match level {
            Level::Ok => "  ok  ",
            Level::Warn => " warn ",
            Level::Fail => " FAIL ",
        };
        println!("[{tag}] {}", msg.as_ref());
        self.worst = self.worst.max(level);
    }
}

/// `abl doctor`: checks the configuration and says what to fix. Returns the
/// process exit code (1 if anything failed).
pub fn doctor(cwd: &Path, flag: Option<&str>) -> i32 {
    let mut r = Report { worst: Level::Ok };
    let global = match (config::global_path(), config::load_global()) {
        (Ok(path), Ok(g)) => {
            let what = if path.exists() {
                "global config"
            } else {
                "global config (not created yet)"
            };
            r.line(Level::Ok, format!("{what}: {}", path.display()));
            g
        }
        (_, Err(e)) | (Err(e), _) => {
            r.line(Level::Fail, format!("global config: {e}"));
            Global::default()
        }
    };
    let loaded = match config::load_project(cwd) {
        Ok(Some(l)) => {
            r.line(Level::Ok, format!("project: {}", l.file.display()));
            Some(l)
        }
        Ok(None) => {
            r.line(Level::Warn, "no abl.toml here or above (run `abl init`)");
            None
        }
        Err(e) => {
            r.line(Level::Fail, format!("project: {e}"));
            None
        }
    };
    match resolve_oe(flag, loaded.as_ref(), &global) {
        None => r.line(
            Level::Fail,
            "no OpenEdge version set (set `oe` in abl.toml or run `abl oe use <version>`)",
        ),
        Some(oe) => {
            r.line(Level::Ok, format!("OE {} (from {})", oe.version, oe.source));
            check_install(&mut r, &oe);
        }
    }
    if let Some(l) = &loaded {
        check_project(&mut r, l);
        check_dotnet(&mut r, cwd);
    }
    match r.worst {
        Level::Ok => println!("\nAll good."),
        Level::Warn => println!("\nUsable, with warnings."),
        Level::Fail => println!("\nProblems found."),
    }
    i32::from(r.worst == Level::Fail)
}

/// .NET namespaces the code uses that assemblies.xml doesn't seem to cover.
fn check_dotnet(r: &mut Report, cwd: &Path) {
    let Ok(project) = crate::source::Project::open(cwd) else {
        return;
    };
    let Ok(u) = crate::dotnet::usage(&project) else {
        return;
    };
    if u.used.is_empty() {
        return;
    }
    let missing = u.missing();
    if missing.is_empty() {
        r.line(
            Level::Ok,
            format!(
                ".NET: {} namespaces, all built in or in assemblies.xml",
                u.used.len()
            ),
        );
    } else {
        r.line(
            Level::Warn,
            format!(
                ".NET namespaces not in assemblies.xml: {} (see `abl dotnet`)",
                missing.join(", ")
            ),
        );
    }
}

fn check_install(r: &mut Report, oe: &Oe) {
    let dlc = match &oe.dlc {
        Ok(d) => d,
        Err(e) => return r.line(Level::Fail, e),
    };
    if !dlc.is_dir() {
        return r.line(
            Level::Fail,
            format!("DLC folder missing: {}", dlc.display()),
        );
    }
    r.line(Level::Ok, format!("DLC: {}", dlc.display()));
    let exe = config::progres(dlc);
    if exe.is_file() {
        r.line(Level::Ok, format!("found {}", exe.display()));
    } else {
        r.line(Level::Fail, format!("missing {}", exe.display()));
    }
    match config::install_version(dlc) {
        Some(v) if v == oe.version => r.line(Level::Ok, format!("version file says {v}")),
        Some(v) => r.line(
            Level::Warn,
            format!(
                "version file says {v}, but this install is registered as {}",
                oe.version
            ),
        ),
        None => r.line(
            Level::Warn,
            format!("no readable version file in {}", dlc.display()),
        ),
    }
}

/// Where RUN would find `name`: a full path, or on the PROPATH (source or r-code).
fn find_procedure(l: &Loaded, name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    let mut names = vec![p.to_path_buf()];
    if p.extension().is_some() {
        names.push(p.with_extension("r"));
    }
    if p.is_absolute() {
        return names.into_iter().find(|n| n.is_file());
    }
    l.propath()
        .iter()
        .chain(std::iter::once(&l.root))
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|c| c.is_file())
}

fn check_procedures(r: &mut Report, l: &Loaded) {
    let procs = &l.project.procedures;
    if procs.is_empty() {
        return;
    }
    let missing: Vec<&str> = procs
        .iter()
        .filter(|p| find_procedure(l, &p.path).is_none())
        .map(|p| p.path.as_str())
        .collect();
    if missing.is_empty() {
        r.line(
            Level::Ok,
            format!(
                "startup procedures: {}",
                procs
                    .iter()
                    .map(|p| format!("{} ({})", p.path, p.mode.word()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    } else {
        r.line(
            Level::Fail,
            format!(
                "startup procedures not found on the PROPATH: {} (every session would stop)",
                missing.join(", ")
            ),
        );
    }
}

fn check_project(r: &mut Report, l: &Loaded) {
    check_procedures(r, l);
    let p = &l.project;
    if p.propath.is_empty() {
        r.line(Level::Warn, "propath not set; using the project folder");
    }
    if !p.lint.is_empty() {
        let mut cfg = abl_core::lint::LintConfig::default();
        let unknown = serde_json::to_value(&p.lint)
            .map(|v| cfg.apply(&v))
            .unwrap_or_default();
        if unknown.is_empty() {
            r.line(Level::Ok, format!("[lint]: {} rules set", p.lint.len()));
        } else {
            r.line(
                Level::Fail,
                format!("[lint]: unknown entries {}", unknown.join(", ")),
            );
        }
    }
    for entry in &p.propath {
        if l.path(entry).is_dir() {
            r.line(Level::Ok, format!("propath {entry}"));
        } else {
            r.line(Level::Warn, format!("propath {entry}: folder not found"));
        }
    }
    if let Some(e) = &p.entry {
        if l.path(e).is_file() {
            r.line(Level::Ok, format!("entry {e}"));
        } else {
            r.line(Level::Warn, format!("entry {e}: file not found"));
        }
    }
    for (name, db) in &p.db {
        let Some(schema) = &db.schema else {
            if db.connect.is_none() {
                r.line(
                    Level::Warn,
                    format!("db {name}: neither schema nor connect is set"),
                );
            }
            continue;
        };
        let path = l.path(schema);
        let Ok(text) = fs::read_to_string(&path) else {
            r.line(Level::Warn, format!("db {name}: cannot read {schema}"));
            continue;
        };
        let a = abl_core::schema::analyze_df(text, name);
        let errors = a
            .diags
            .iter()
            .filter(|d| d.severity == abl_core::analysis::Severity::Error)
            .count();
        let tables = a
            .symbols
            .iter()
            .filter(|s| s.kind == abl_core::analysis::SymKind::Table)
            .count();
        if errors == 0 {
            r.line(Level::Ok, format!("db {name}: {schema}, {tables} tables"));
        } else {
            r.line(
                Level::Warn,
                format!("db {name}: {schema} has {errors} errors"),
            );
        }
    }
}

/// `abl init`: writes an abl.toml in `cwd`, guessing PROPATH and databases from
/// the folder layout.
pub fn init(cwd: &Path, flag: Option<&str>, force: bool) -> Result<(), String> {
    let file = cwd.join(config::PROJECT_FILE);
    if file.exists() && !force {
        return Err(format!(
            "{} already exists (use --force to overwrite)",
            file.display()
        ));
    }
    let global = config::load_global()?;
    let oe = flag
        .map(String::from)
        .or_else(|| {
            std::env::var(config::OE_ENV)
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .or_else(|| global.default.clone());
    let propath = guess_propath(cwd);
    let dbs = find_schemas(cwd);
    let text = render_init(oe.as_deref(), &propath, &dbs);
    fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    println!("Wrote {}", file.display());
    if oe.is_none() {
        println!("No OpenEdge version set yet: run `abl oe use <version> --project` once one is installed.");
    }
    println!("Run `abl doctor` to check it.");
    Ok(())
}

fn guess_propath(root: &Path) -> Vec<String> {
    let mut pp: Vec<String> = ["src", "include", "inc", "src/include", "src/inc"]
        .iter()
        .filter(|d| root.join(d).is_dir())
        .map(|d| d.to_string())
        .collect();
    if pp.is_empty() {
        pp.push(".".into());
    }
    pp
}

/// `.df` files up to three folders deep, as (db name, path relative to root).
fn find_schemas(root: &Path) -> Vec<(String, String)> {
    let skip = ["target", "node_modules", "build", ".git"];
    let mut out: Vec<(String, String)> = WalkDir::new(root)
        .max_depth(3)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0 || !skip.contains(&e.file_name().to_string_lossy().as_ref())
        })
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().is_file()
                && e.path()
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("df"))
        })
        .filter_map(|e| {
            let name = e.path().file_stem()?.to_string_lossy().to_string();
            let rel = e.path().strip_prefix(root).ok()?;
            let rel = rel.to_string_lossy().replace('\\', "/");
            Some((name, rel))
        })
        .collect();
    out.sort();
    out.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
    out
}

fn render_init(oe: Option<&str>, propath: &[String], dbs: &[(String, String)]) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut t = String::new();
    t.push_str("# abl project file. Check it with `abl doctor`; see it with `abl status`.\n\n");
    t.push_str("# OpenEdge version. Each machine maps it to an install with `abl oe add`.\n");
    match oe {
        Some(v) => {
            let _ = writeln!(t, "oe = {}", quote(v));
        }
        None => t.push_str("# oe = \"12.8\"\n"),
    }
    t.push_str("\n# Source folders, searched in order like PROPATH.\n");
    let pp: Vec<String> = propath.iter().map(|p| quote(p)).collect();
    let _ = writeln!(t, "propath = [{}]", pp.join(", "));
    t.push_str("\n# Where compiled r-code goes.\noutput = \"build/rcode\"\n");
    t.push_str(
        "\n# The procedure `abl run` starts when no file is given.\n# entry = \"src/main.p\"\n",
    );
    for (name, rel) in dbs {
        let _ = writeln!(t, "\n[db.{name}]\nschema = {}", quote(rel));
        t.push_str("# connect = \"-db <path or name> -1\"\n");
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_output_parses_as_project() {
        let text = render_init(
            Some("11.7"),
            &["src".into(), "src/include".into()],
            &[("sports".into(), "db/sports.df".into())],
        );
        let p: config::Project = toml::from_str(&text).unwrap();
        assert_eq!(p.oe.as_deref(), Some("11.7"));
        assert_eq!(p.propath, ["src", "src/include"]);
        assert_eq!(p.output.as_deref(), Some("build/rcode"));
        assert_eq!(p.db["sports"].schema.as_deref(), Some("db/sports.df"));

        let bare: config::Project = toml::from_str(&render_init(None, &[".".into()], &[])).unwrap();
        assert_eq!(bare.oe, None);
    }
}

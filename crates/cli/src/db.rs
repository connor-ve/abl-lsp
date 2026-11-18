//! `abl db`: the project's databases. A `[db.x]` entry without `connect` is
//! managed by abl: created from its `.df`, served, snapshotted and deleted
//! with the OpenEdge tools. Entries with `connect` are someone else's.

use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use abl_oe::config::{self, Db, Loaded};
use abl_oe::db;
use abl_oe::session::Env;

pub enum Action {
    List,
    Create { no_schema: bool },
    Start,
    Stop,
    Delete { yes: bool },
    Snapshot { label: Option<String>, list: bool },
    Restore { label: String },
}

fn project(cwd: &Path) -> Result<Loaded, String> {
    config::load_project(cwd)?
        .ok_or_else(|| "no abl.toml here or above; run `abl init`".to_string())
}

/// The managed database named `name`, or the only one.
fn pick<'a>(l: &'a Loaded, name: Option<&str>) -> Result<(&'a str, &'a Db, PathBuf), String> {
    let managed: Vec<(&String, &Db)> = l
        .project
        .db
        .iter()
        .filter(|(_, d)| d.connect.is_none())
        .collect();
    let found = match name {
        Some(n) => {
            let (k, d) = l
                .project
                .db
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(n))
                .ok_or_else(|| {
                    let known: Vec<&str> = l.project.db.keys().map(String::as_str).collect();
                    if known.is_empty() {
                        format!("no database {n} in abl.toml (add a [db.{n}] table)")
                    } else {
                        format!("no database {n} in abl.toml (known: {})", known.join(", "))
                    }
                })?;
            if d.connect.is_some() {
                return Err(format!(
                    "{k} has `connect` in abl.toml, so it isn't managed by abl; remove `connect` to let abl create and serve it"
                ));
            }
            (k, d)
        }
        None => match managed.as_slice() {
            [one] => *one,
            [] => {
                return Err(
                    "no database for abl to manage: add one to abl.toml, e.g.\n\n[db.sports]\nschema = \"db/sports.df\"".into(),
                )
            }
            many => {
                let names: Vec<&str> = many.iter().map(|(k, _)| k.as_str()).collect();
                return Err(format!(
                    "abl.toml has several databases ({}); name one",
                    names.join(", ")
                ));
            }
        },
    };
    Ok((found.0.as_str(), found.1, l.db_path(found.0, found.1)))
}

fn rel(l: &Loaded, p: &Path) -> String {
    p.strip_prefix(&l.root)
        .unwrap_or(p)
        .display()
        .to_string()
        .replace('\\', "/")
}

pub fn state(p: &Path) -> &'static str {
    if !p.is_file() {
        "not created"
    } else if db::in_use(p) {
        "running"
    } else {
        "stopped"
    }
}

pub fn run(
    cwd: &Path,
    name: Option<&str>,
    action: Action,
    flag: Option<&str>,
) -> Result<u8, String> {
    let l = project(cwd)?;
    if let Action::List = action {
        return list(&l);
    }
    let (name, d, path) = pick(&l, name)?;
    let env = Env::new(Some(&l), flag, cwd)?;
    match action {
        Action::List => unreachable!(),
        Action::Create { no_schema } => {
            if path.is_file() {
                return Err(format!(
                    "{name} already exists at {} (delete it with `abl db delete {name}`)",
                    rel(&l, &path)
                ));
            }
            db::create(&env, &path)?;
            println!("Created {name} at {}", rel(&l, &path));
            if let (Some(s), false) = (&d.schema, no_schema) {
                let df = l.path(s);
                if !df.is_file() {
                    return Err(format!("{}: schema file not found", rel(&l, &df)));
                }
                db::load_df(&env, name, &path, &df)?;
                println!("Loaded {}", rel(&l, &df));
            }
            Ok(0)
        }
        Action::Start => {
            ensure_created(name, &path)?;
            if db::in_use(&path) {
                println!("{name} is already running");
                return Ok(0);
            }
            db::serve(&env, &path, d.port)?;
            match d.port {
                Some(p) => println!("Started {name} on port {p}"),
                None => println!("Started {name} (shared memory; set `port` for TCP connections)"),
            }
            Ok(0)
        }
        Action::Stop => {
            ensure_created(name, &path)?;
            if !db::in_use(&path) {
                println!("{name} is not running");
                return Ok(0);
            }
            db::shutdown(&env, &path)?;
            println!("Stopped {name}");
            Ok(0)
        }
        Action::Delete { yes } => {
            ensure_created(name, &path)?;
            ensure_stopped(name, &path)?;
            if !yes && !confirm(&format!("Delete {name} at {}?", rel(&l, &path)))? {
                println!("Nothing deleted");
                return Ok(1);
            }
            db::delete(&env, &path)?;
            println!("Deleted {name}");
            Ok(0)
        }
        Action::Snapshot { label, list } => {
            let dir = l.root.join(".abl").join("snapshots").join(name);
            if list {
                return list_snapshots(name, &dir);
            }
            ensure_created(name, &path)?;
            let label = label.unwrap_or_else(timestamp);
            check_label(&label)?;
            let file = dir.join(format!("{label}.bck"));
            if file.exists() {
                return Err(format!("snapshot {label} of {name} already exists"));
            }
            fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            db::backup(&env, &path, &file, db::in_use(&path))?;
            println!("Saved snapshot {label} of {name}");
            Ok(0)
        }
        Action::Restore { label } => {
            check_label(&label)?;
            let file = l
                .root
                .join(".abl")
                .join("snapshots")
                .join(name)
                .join(format!("{label}.bck"));
            if !file.is_file() {
                return Err(format!(
                    "no snapshot {label} of {name} (see `abl db snapshot --list`)"
                ));
            }
            ensure_stopped(name, &path)?;
            db::restore(&env, &path, &file)?;
            println!("Restored {name} from snapshot {label}");
            Ok(0)
        }
    }
}

fn ensure_created(name: &str, path: &Path) -> Result<(), String> {
    if path.is_file() {
        Ok(())
    } else {
        Err(format!("{name} isn't created yet (abl db create {name})"))
    }
}

fn ensure_stopped(name: &str, path: &Path) -> Result<(), String> {
    if db::in_use(path) {
        Err(format!(
            "{name} is in use; stop it first (abl db stop {name})"
        ))
    } else {
        Ok(())
    }
}

fn check_label(label: &str) -> Result<(), String> {
    let ok = !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        && !label.starts_with('.');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{label:?}: use letters, digits, '-', '_' and '.' in a snapshot name"
        ))
    }
}

fn confirm(question: &str) -> Result<bool, String> {
    if !std::io::stdin().is_terminal() {
        return Err(format!("{question} Pass --yes to confirm."));
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

fn list(l: &Loaded) -> Result<u8, String> {
    if l.project.db.is_empty() {
        println!("No databases in abl.toml");
        return Ok(0);
    }
    let w = l.project.db.keys().map(String::len).max().unwrap_or(0);
    for (name, d) in &l.project.db {
        let line = match &d.connect {
            Some(c) => format!("connect {c}"),
            None => {
                let p = l.db_path(name, d);
                format!("{:<11} {}", state(&p), rel(l, &p))
            }
        };
        println!("{name:<w$}  {line}");
    }
    Ok(0)
}

fn list_snapshots(name: &str, dir: &Path) -> Result<u8, String> {
    let mut snaps: Vec<(String, u64)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let label = p.file_name()?.to_str()?.strip_suffix(".bck")?.to_string();
            Some((label, e.metadata().ok()?.len()))
        })
        .collect();
    snaps.sort();
    if snaps.is_empty() {
        println!("No snapshots of {name}");
    }
    for (label, size) in snaps {
        println!("{label}  {}", human(size));
    }
    Ok(0)
}

fn human(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{n} bytes"),
    }
}

/// `20261118-142530` (UTC), the default snapshot name.
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert!(check_label("before-test_1.2").is_ok());
        assert!(check_label("../x").is_err());
        assert!(check_label("a b").is_err());
        assert_eq!(timestamp().len(), 15);
    }
}

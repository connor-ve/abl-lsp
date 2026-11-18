//! `abl oe`: manage the OpenEdge installs this machine knows about.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{self, Global};

pub fn list(cwd: &Path, flag: Option<&str>) -> Result<(), String> {
    let global = config::load_global()?;
    let project = config::load_project(cwd)?;
    if global.oe.is_empty() {
        println!("No OpenEdge installs known.");
        println!(
            "Add one with `abl oe add <version> <path>`, or find them with `abl oe add --detect`."
        );
        return Ok(());
    }
    let active = config::resolve_version(flag, project.as_ref().map(|l| &l.project), &global);
    let width = global.oe.keys().map(String::len).max().unwrap_or(0);
    for (version, dlc) in &global.oe {
        let mut notes = Vec::new();
        if global.default.as_deref() == Some(version.as_str()) {
            notes.push("default".to_string());
        }
        if let Some((v, src)) = &active {
            if v == version {
                notes.push(format!("active, from {src}"));
            }
        }
        if !dlc.is_dir() {
            notes.push("missing".to_string());
        }
        let mark = if active.as_ref().is_some_and(|(v, _)| v == version) {
            '*'
        } else {
            ' '
        };
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!("  ({})", notes.join(", "))
        };
        println!("{mark} {version:<width$}  {}{notes}", dlc.display());
    }
    if let Some((v, src)) = &active {
        if !global.oe.contains_key(v) {
            println!("\nActive version {v} (from {src}) is not installed here.");
        }
    }
    Ok(())
}

pub fn add(version: &str, path: &Path, force: bool) -> Result<(), String> {
    let dlc = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !dlc.is_dir() {
        return Err(format!("{} is not a folder", dlc.display()));
    }
    match config::install_version(&dlc) {
        // Registering an install under the wrong version would compile with the
        // wrong OpenEdge without anyone noticing, so refuse unless forced.
        Some(found) if found != version && !force => {
            return Err(format!(
                "{} says this is OE {found}, not {version}. Run `abl oe add {found} {}`, or add --force",
                dlc.join("version").display(),
                path.display()
            ))
        }
        Some(found) if found != version => eprintln!(
            "warning: {} says it is OE {found}, not {version}",
            dlc.join("version").display()
        ),
        None => eprintln!("warning: no readable version file in {}", dlc.display()),
        _ => {}
    }
    let global = config::load_global()?;
    if let Some(old) = global.oe.get(version) {
        if *old != dlc {
            println!("Replacing OE {version} (was {})", old.display());
        }
    }
    if !config::progres(&dlc).is_file() {
        eprintln!("warning: {} not found", config::progres(&dlc).display());
    }
    config::add_install(version, &dlc)?;
    println!("Added OE {version} -> {}", dlc.display());
    if global.default.is_none() && global.oe.is_empty() {
        config::set_default(version)?;
        println!("OE {version} is now the global default.");
    }
    Ok(())
}

pub fn detect() -> Result<(), String> {
    let found = detect_in(&default_roots());
    if found.is_empty() {
        println!("No OpenEdge installs found. Add one with `abl oe add <version> <path>`.");
        return Ok(());
    }
    let global = config::load_global()?;
    let mut added = 0;
    for (version, dlc) in found {
        match global.oe.get(&version) {
            Some(old) if *old == dlc => println!("  {version}  {}  (already known)", dlc.display()),
            Some(old) => println!(
                "  {version}  {}  (skipped: {version} already maps to {})",
                dlc.display(),
                old.display()
            ),
            None => {
                config::add_install(&version, &dlc)?;
                println!("+ {version}  {}", dlc.display());
                added += 1;
            }
        }
    }
    if added > 0 && global.default.is_none() {
        let first = config::load_global()?.oe.into_keys().next();
        if let Some(v) = first {
            config::set_default(&v)?;
            println!("OE {v} is now the global default.");
        }
    }
    Ok(())
}

/// Folders that commonly hold OpenEdge installs. Each one, and each folder
/// directly inside it, is checked.
pub fn default_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = env::var_os("DLC").map(PathBuf::from).into_iter().collect();
    let common: &[&str] = if cfg!(windows) {
        &["C:/Progress", "C:/OpenEdge", "D:/Progress", "D:/OpenEdge"]
    } else {
        &[
            "/usr/dlc",
            "/usr/oe",
            "/opt/progress",
            "/usr/progress",
            "/opt/oe",
        ]
    };
    roots.extend(common.iter().map(PathBuf::from));
    roots
}

/// Finds installs (folders with a `version` file naming an OpenEdge release) in
/// `roots` and one level below them. The first install of each version wins.
pub fn detect_in(roots: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    let consider = |dir: PathBuf, found: &mut Vec<(String, PathBuf)>| {
        if let Some(v) = config::install_version(&dir) {
            if !found.iter().any(|(fv, fd)| *fv == v || *fd == dir) {
                found.push((v, dir));
            }
        }
    };
    for root in roots {
        consider(root.clone(), &mut found);
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for d in dirs {
            consider(d, &mut found);
        }
    }
    found.sort();
    found
}

pub fn remove(version: &str) -> Result<(), String> {
    let global = config::load_global()?;
    if !global.oe.contains_key(version) {
        return Err(not_known(&global, version));
    }
    let was_default = config::remove_install(version)?;
    println!("Removed OE {version}.");
    if was_default {
        println!("It was the global default; set a new one with `abl oe use <version>`.");
    }
    Ok(())
}

pub fn use_version(cwd: &Path, version: &str, project: bool) -> Result<(), String> {
    let global = config::load_global()?;
    if project {
        let Some(file) = config::find_project(cwd) else {
            return Err(format!(
                "no {} here or above; run `abl init` first",
                config::PROJECT_FILE
            ));
        };
        config::set_project_oe(&file, version)?;
        println!("Pinned OE {version} in {}", file.display());
        if !global.oe.contains_key(version) {
            eprintln!("warning: OE {version} is not installed here; add it with `abl oe add {version} <path>`");
        }
        return Ok(());
    }
    if !global.oe.contains_key(version) {
        return Err(not_known(&global, version));
    }
    config::set_default(version)?;
    println!("OE {version} is now the global default.");
    if let Some(loaded) = config::load_project(cwd)? {
        if let Some(pinned) = loaded.project.oe.as_deref().filter(|p| *p != version) {
            println!(
                "This project still uses OE {pinned} (from {}).",
                config::PROJECT_FILE
            );
        }
    }
    Ok(())
}

fn not_known(global: &Global, version: &str) -> String {
    config::dlc_for(global, version).unwrap_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install(dir: &Path, release: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("version"),
            format!("OpenEdge Release {release} as of today\n"),
        )
        .unwrap();
    }

    #[test]
    fn detects_installs_one_level_down() {
        let tmp = std::env::temp_dir().join(format!("abl-detect-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        install(&tmp.join("OpenEdge117"), "11.7.4");
        install(&tmp.join("OpenEdge128"), "12.8.1");
        install(&tmp.join("OpenEdge128b"), "12.8.3"); // same version: first wins
        fs::create_dir_all(tmp.join("NotOE")).unwrap();
        let found = detect_in(&[tmp.clone(), tmp.join("does-not-exist")]);
        let names: Vec<(String, String)> = found
            .iter()
            .map(|(v, d)| (v.clone(), d.file_name().unwrap().to_string_lossy().into()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("11.7".into(), "OpenEdge117".into()),
                ("12.8".into(), "OpenEdge128".into())
            ]
        );
        fs::remove_dir_all(&tmp).unwrap();
    }
}

//! `abl install` (`abl i`), `abl update` and `abl remove`: packages are git
//! repositories, cloned into `vendor/<name>` and put on the PROPATH (the
//! package's own `propath` from its abl.toml, else its root). abl.toml's
//! `[deps]` says what the project wants; `abl.lock` records the commit of
//! every package, the ones packages ask for included, so a fresh clone of
//! the project installs exactly the same code.
//!
//! `abl i @code` installs the VS Code extension instead.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use abl_oe::config::{self, Dep, Lock, Locked, VENDOR_DIR};

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(["-c", "advice.detachedHead=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("cannot run git (is it installed and on the PATH?): {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// A local folder (as opposed to a URL or `user@host:path`).
fn is_local(spec: &str) -> bool {
    let url = spec.contains("://");
    let scp = spec.contains('@') && spec.contains(':') && !Path::new(spec).is_absolute();
    !url && !scp
}

/// `github.com/a/b` -> `https://github.com/a/b`; `url@v1` -> (url, v1).
pub fn parse_spec(spec: &str) -> (String, Option<String>) {
    let (mut url, mut reference) = (spec.to_string(), None);
    if let Some((left, right)) = spec.rsplit_once('@') {
        // `git@host:path` has a ':' after the '@'; a ref has neither ':' nor '/'.
        if !left.is_empty()
            && !right.is_empty()
            && !right.contains([':', '/'])
            && !left.ends_with(':')
        {
            url = left.to_string();
            reference = Some(right.to_string());
        }
    }
    let first = url.split('/').next().unwrap_or("");
    if !url.contains("://")
        && !url.contains('@')
        && first.contains('.')
        && !Path::new(&url).exists()
    {
        url = format!("https://{url}");
    }
    (url, reference)
}

/// The package name a repository gets by default: its last path segment.
pub fn default_name(url: &str) -> String {
    let last = url
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\', ':'])
        .next()
        .unwrap_or(url);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && !n.starts_with('.')
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The same repository, however it was written.
fn same_git(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.trim_end_matches(['/', '\\']);
        s.strip_suffix(".git")
            .unwrap_or(s)
            .replace('\\', "/")
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// A relative local path in a package's abl.toml is relative to that package.
fn absolute_git(spec: &str, base: &Path) -> String {
    if is_local(spec) && Path::new(spec).is_relative() {
        base.join(spec).display().to_string()
    } else {
        spec.to_string()
    }
}

/// `[deps]` of a package's abl.toml, read leniently.
fn nested_deps(dir: &Path) -> Vec<(String, Dep)> {
    let Some(table) = fs::read_to_string(dir.join(config::PROJECT_FILE))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
    else {
        return Vec::new();
    };
    let Some(deps) = table.get("deps").and_then(|d| d.as_table()) else {
        return Vec::new();
    };
    deps.iter()
        .filter_map(|(n, v)| {
            let t = v.as_table()?;
            Some((
                n.clone(),
                Dep {
                    git: t.get("git")?.as_str()?.to_string(),
                    reference: t.get("ref").and_then(|r| r.as_str()).map(String::from),
                },
            ))
        })
        .collect()
}

/// Makes `vendor/<name>` a clone of `git` and returns whether it was cloned now.
fn ensure_clone(dir: &Path, url: &str) -> Result<bool, String> {
    if dir.join(".git").exists() {
        let origin = git(dir, &["remote", "get-url", "origin"]).unwrap_or_default();
        if same_git(&origin, url) {
            return Ok(false);
        }
        fs::remove_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    } else if dir.exists() {
        return Err(format!(
            "{} exists and isn't a git clone; move it away and run abl install again",
            dir.display()
        ));
    }
    let parent = dir.parent().unwrap_or(dir);
    fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    git(
        parent,
        &["clone", "--quiet", url, &dir.display().to_string()],
    )?;
    Ok(true)
}

/// The commit a ref names: a branch (its newest commit), a tag, or a commit.
fn resolve(dir: &Path, reference: Option<&str>) -> Result<String, String> {
    let Some(r) = reference else {
        let _ = git(dir, &["remote", "set-head", "origin", "--auto"]);
        return git(dir, &["rev-parse", "origin/HEAD^{commit}"]);
    };
    for cand in [format!("origin/{r}^{{commit}}"), format!("{r}^{{commit}}")] {
        if let Ok(c) = git(dir, &["rev-parse", "--verify", "--quiet", &cand]) {
            return Ok(c);
        }
    }
    Err(format!(
        "{}: no branch, tag or commit named {r}",
        dir.display()
    ))
}

fn short(c: &str) -> &str {
    &c[..c.len().min(7)]
}

/// Brings vendor/ and abl.lock in line with abl.toml. Packages named in
/// `refresh` (or all, with `all`) move to the newest commit of their ref;
/// the rest stay at their locked commit.
fn sync(
    root: &Path,
    deps: &BTreeMap<String, Dep>,
    refresh: &BTreeSet<String>,
    all: bool,
) -> Result<u8, String> {
    let old = Lock::load(root)?;
    let vendor = root.join(VENDOR_DIR);
    let mut queue: VecDeque<(String, Dep, Option<String>)> = deps
        .iter()
        .map(|(n, d)| {
            let d = Dep {
                git: absolute_git(&d.git, root),
                reference: d.reference.clone(),
            };
            (n.clone(), d, None)
        })
        .collect();
    let mut done: Vec<Locked> = Vec::new();
    let mut fetched: BTreeSet<String> = BTreeSet::new();
    while let Some((name, dep, via)) = queue.pop_front() {
        if !valid_name(&name) {
            return Err(format!(
                "{name:?} isn't a usable package name (letters, digits, - _ .)"
            ));
        }
        if let Some(prev) = done.iter().find(|p| p.name == name) {
            if !same_git(&prev.git, &dep.git) || prev.reference != dep.reference {
                let who = |v: &Option<String>| {
                    v.clone()
                        .map_or("abl.toml".to_string(), |p| format!("package {p}"))
                };
                return Err(format!(
                    "{name} is asked for twice, differently:\n  {} wants {}{}\n  {} wants {}{}\nPin one version in abl.toml's [deps].",
                    who(&prev.via),
                    prev.git,
                    prev.reference.as_ref().map_or(String::new(), |r| format!(" @ {r}")),
                    who(&via),
                    dep.git,
                    dep.reference.as_ref().map_or(String::new(), |r| format!(" @ {r}")),
                ));
            }
            continue;
        }
        let dir = vendor.join(&name);
        let cloned = ensure_clone(&dir, &dep.git)?;
        let locked = old.packages.iter().find(|p| {
            p.name == name
                && same_git(&absolute_git(&p.git, root), &dep.git)
                && p.reference == dep.reference
        });
        let fresh = all || refresh.contains(&name);
        let commit = match locked {
            Some(l) if !fresh => {
                if git(
                    &dir,
                    &["cat-file", "-e", &format!("{}^{{commit}}", l.commit)],
                )
                .is_err()
                {
                    git(&dir, &["fetch", "--quiet", "--tags", "origin"])?;
                }
                l.commit.clone()
            }
            _ => {
                if !cloned && fetched.insert(name.clone()) {
                    git(&dir, &["fetch", "--quiet", "--tags", "--force", "origin"])?;
                }
                resolve(&dir, dep.reference.as_deref())?
            }
        };
        let before = git(&dir, &["rev-parse", "HEAD"]).unwrap_or_default();
        git(&dir, &["checkout", "--quiet", "--detach", &commit])?;
        let what = dep.reference.as_deref().unwrap_or("default branch");
        let state = if cloned {
            "installed"
        } else if before != commit {
            "updated"
        } else {
            "up to date"
        };
        println!("  {name:<16} {what} @ {}  {state}", short(&commit));
        for (n, d) in nested_deps(&dir) {
            let d = Dep {
                git: absolute_git(&d.git, &dir),
                reference: d.reference,
            };
            queue.push_back((n, d, Some(name.clone())));
        }
        done.push(Locked {
            name,
            git: dep.git,
            reference: dep.reference,
            commit,
            via,
        });
    }
    // Packages nothing asks for any more. Only folders abl put there go.
    for p in &old.packages {
        if !done.iter().any(|d| d.name == p.name) {
            let dir = vendor.join(&p.name);
            if dir.join(".git").exists() {
                fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                println!("  {:<16} removed", p.name);
            }
        }
    }
    // Keep the root's own entries as written in abl.toml.
    for p in &mut done {
        if p.via.is_none() {
            if let Some(d) = deps.get(&p.name) {
                p.git = d.git.clone();
            }
        }
    }
    let n = done.len();
    Lock { packages: done }.save(root)?;
    if n == 0 {
        println!("No packages.");
    } else {
        println!(
            "{n} package{} in {VENDOR_DIR}/, on the PROPATH",
            if n == 1 { "" } else { "s" }
        );
        let ignore = fs::read_to_string(root.join(".gitignore")).unwrap_or_default();
        if root.join(".git").exists()
            && !ignore
                .lines()
                .any(|l| l.trim().trim_matches('/') == VENDOR_DIR)
        {
            println!("Add {VENDOR_DIR}/ to .gitignore; abl.lock is what to commit.");
        }
    }
    Ok(0)
}

fn project(cwd: &Path) -> Result<config::Loaded, String> {
    config::load_project(cwd)?
        .ok_or_else(|| "no abl.toml here or above; run `abl init` first".to_string())
}

pub fn install(
    cwd: &Path,
    spec: Option<&str>,
    name: Option<&str>,
    reference: Option<&str>,
    vsix: Option<&Path>,
) -> Result<u8, String> {
    if spec == Some("@code") {
        return install_code(vsix);
    }
    let l = match project(cwd) {
        Ok(l) => l,
        // PowerShell reads an unquoted @code as splatting and passes nothing.
        Err(e) if spec.is_none() => {
            return Err(format!(
                "{e}\n(for the VS Code extension in PowerShell, quote it: abl i '@code')"
            ))
        }
        Err(e) => return Err(e),
    };
    let Some(spec) = spec else {
        return sync(&l.root, &l.project.deps, &BTreeSet::new(), false);
    };
    let (url, at) = parse_spec(spec);
    let reference = reference.map(String::from).or(at);
    let name = name.map_or_else(|| default_name(&url), String::from);
    if !valid_name(&name) {
        return Err(format!(
            "{name:?} isn't a usable package name; give one with --name"
        ));
    }
    let dep = Dep {
        git: url,
        reference,
    };
    let mut deps = l.project.deps.clone();
    let changed = deps.get(&name) != Some(&dep);
    deps.insert(name.clone(), dep.clone());
    let refresh = BTreeSet::from([name.clone()]);
    // Fetch first: abl.toml only changes when the package installs.
    let code = sync(&l.root, &deps, &refresh, false)?;
    if changed {
        config::set_dep(&l.file, &name, &dep)?;
    }
    Ok(code)
}

pub fn update(cwd: &Path, name: Option<&str>) -> Result<u8, String> {
    let l = project(cwd)?;
    if let Some(n) = name {
        let lock = Lock::load(&l.root)?;
        if !l.project.deps.contains_key(n) && !lock.packages.iter().any(|p| p.name == n) {
            return Err(format!("no package {n} in abl.toml or abl.lock"));
        }
    }
    let refresh: BTreeSet<String> = name.map(String::from).into_iter().collect();
    sync(&l.root, &l.project.deps, &refresh, name.is_none())
}

pub fn remove(cwd: &Path, name: &str) -> Result<u8, String> {
    let l = project(cwd)?;
    let mut deps = l.project.deps.clone();
    if deps.remove(name).is_none() {
        return Err(format!("no package {name} in abl.toml's [deps]"));
    }
    config::remove_dep(&l.file, name)?;
    sync(&l.root, &deps, &BTreeSet::new(), false)
}

// ---- abl i @code ----

/// The VS Code command line: `ABL_CODE`, else `code` on the PATH.
fn code_command() -> Option<PathBuf> {
    if let Some(c) = std::env::var_os("ABL_CODE") {
        return Some(PathBuf::from(c));
    }
    let names: &[&str] = if cfg!(windows) {
        &["code.cmd", "code.exe"]
    } else {
        &["code"]
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

/// The extension package shipped next to abl (the release zip has one).
fn bundled_vsix() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    let dir = exe.parent()?;
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("vsix"))
        })
        .collect();
    found.sort();
    found.pop()
}

fn install_code(vsix: Option<&Path>) -> Result<u8, String> {
    let vsix = match vsix {
        Some(v) => v.to_path_buf(),
        None => bundled_vsix().ok_or(
            "no .vsix next to abl; install abl from a release (winget), or pass --vsix <file>",
        )?,
    };
    if !vsix.is_file() {
        return Err(format!("{}: no such file", vsix.display()));
    }
    let code = code_command()
        .ok_or("VS Code's `code` command isn't on the PATH (in VS Code: Shell Command: Install 'code' command in PATH)")?;
    let out = Command::new(&code)
        .arg("--install-extension")
        .arg(&vsix)
        .arg("--force")
        .output()
        .map_err(|e| format!("cannot run {}: {e}", code.display()))?;
    print!("{}", String::from_utf8_lossy(&out.stdout));
    if !out.status.success() {
        return Err(format!(
            "installing the extension failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    println!("Installed the ABL extension in VS Code. Reload open windows to use it.");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs() {
        assert_eq!(
            parse_spec("github.com/a/abl-json@v1.2"),
            ("https://github.com/a/abl-json".into(), Some("v1.2".into()))
        );
        assert_eq!(
            parse_spec("git@github.com:a/b.git"),
            ("git@github.com:a/b.git".into(), None)
        );
        assert_eq!(
            parse_spec("git@github.com:a/b.git@main"),
            ("git@github.com:a/b.git".into(), Some("main".into()))
        );
        assert_eq!(
            parse_spec("https://x.org/a/b"),
            ("https://x.org/a/b".into(), None)
        );
        assert_eq!(
            default_name("https://github.com/a/abl-json.git"),
            "abl-json"
        );
        assert_eq!(default_name("git@github.com:a/b.git"), "b");
        assert_eq!(default_name("C:\\repos\\util\\"), "util");
        assert!(same_git(
            "https://github.com/A/b.git",
            "https://github.com/a/b/"
        ));
        assert!(!valid_name("../x") && valid_name("abl-json"));
    }
}

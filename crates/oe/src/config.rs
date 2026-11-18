//! Configuration in two layers.
//!
//! - Global (`~/.abl/config.toml`, or `$ABL_HOME/config.toml`): which OpenEdge
//!   installs exist on this machine, by version, plus a default version.
//! - Project (`abl.toml`, found by walking up from the current folder): the
//!   version the project pins, its PROPATH, output folder, entry point and databases.
//!
//! A project names a version, never a path, so the same `abl.toml` works on any
//! machine that maps that version to a local install.

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml_edit::{value, DocumentMut, Item, Table};

pub const PROJECT_FILE: &str = "abl.toml";
pub const LOCK_FILE: &str = "abl.lock";
/// Where `abl install` puts packages, under the project root.
pub const VENDOR_DIR: &str = "vendor";
pub const OE_ENV: &str = "ABL_OE";
const HOME_ENV: &str = "ABL_HOME";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Global {
    /// Version used when neither a flag, `ABL_OE` nor the project names one.
    pub default: Option<String>,
    /// Version -> DLC folder.
    #[serde(default)]
    pub oe: BTreeMap<String, PathBuf>,
    /// Your own formatting defaults; a project's abl.toml wins over them.
    #[serde(default)]
    pub format: Format,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub oe: Option<String>,
    #[serde(default)]
    pub propath: Vec<String>,
    pub output: Option<String>,
    pub entry: Option<String>,
    /// Extra OpenEdge startup parameters for every session: `-cpinternal UTF-8`.
    pub startup: Option<String>,
    /// The procedure library `abl build` writes, relative to the root.
    pub library: Option<String>,
    /// The folder with assemblies.xml (the .NET assemblies sessions load),
    /// relative to the root. Default: the root.
    pub assemblies: Option<String>,
    #[serde(default)]
    pub db: BTreeMap<String, Db>,
    /// Packages `abl install` fetches into `vendor/<name>`.
    #[serde(default)]
    pub deps: BTreeMap<String, Dep>,
    #[serde(default)]
    pub vulncheck: Vulncheck,
    /// Procedures every session runs before its work, in order.
    #[serde(default)]
    pub procedures: Vec<StartupProcedure>,
    /// File extensions besides .p .w .cls .t (compiled) and .i (included).
    #[serde(default)]
    pub extensions: Extensions,
    /// Folders (relative to the root) that lint, fmt and deps skip.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Lint rule levels, the same as the `abl.lint` editor setting:
    /// `unused-variable = "off"`.
    #[serde(default)]
    pub lint: toml::Table,
    #[serde(default)]
    pub format: Format,
    /// Names defined before every file, like `abl.preprocessor`.
    #[serde(default)]
    pub preprocessor: toml::Table,
    /// The project's local PASOE instance (`abl pas`).
    #[serde(default)]
    pub pas: Pas,
}

/// `[pas]`: a local PASOE instance for the project. Everything has a default.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Pas {
    /// Instance name, also its ABL application's. Default: from the project folder's name.
    pub name: Option<String>,
    /// The instance folder, relative to the root. Default: `.abl/pas/<name>`.
    pub path: Option<String>,
    /// HTTP port. Default 8810; HTTPS is the next port and shutdown the one after.
    pub port: Option<u16>,
    pub https_port: Option<u16>,
    pub shutdown_port: Option<u16>,
    /// Extra startup parameters for the agents, after the database connections.
    pub startup: Option<String>,
}

impl Pas {
    pub fn http(&self) -> u16 {
        self.port.unwrap_or(8810)
    }
    pub fn https(&self) -> u16 {
        self.https_port.unwrap_or(self.http().wrapping_add(1))
    }
    pub fn shutdown(&self) -> u16 {
        self.shutdown_port.unwrap_or(self.http().wrapping_add(2))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Format {
    /// `preserve` (default), `upper` or `lower`.
    pub keyword_case: Option<String>,
    /// Spaces per block level (default 4).
    pub indent: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Db {
    /// Connection parameters, as given to `CONNECT` or on the command line.
    /// When set, the database is someone else's: `abl db` doesn't manage it.
    pub connect: Option<String>,
    /// The schema as a `.df` file, relative to the project root.
    pub schema: Option<String>,
    /// Where `abl db` keeps the database (the `.db` file), relative to the
    /// project root. Default: `.abl/db/<name>/<name>.db`.
    pub path: Option<String>,
    /// TCP port for `abl db start` (`proserve -S`). Without it the server only
    /// takes shared-memory connections from this machine.
    pub port: Option<u16>,
}

/// `[[procedures]]`: a procedure a session runs before its own work.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupProcedure {
    /// As given to RUN: a name on the PROPATH or a full path.
    pub path: String,
    #[serde(default)]
    pub mode: StartupMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StartupMode {
    /// RUN it, and go on when it returns.
    #[default]
    Run,
    /// RUN it PERSISTENT: its internal procedures stay callable.
    Persistent,
    /// RUN it PERSISTENT and add it as a session super procedure.
    Super,
}

impl StartupMode {
    pub fn word(self) -> &'static str {
        match self {
            StartupMode::Run => "run",
            StartupMode::Persistent => "persistent",
            StartupMode::Super => "super",
        }
    }
}

/// `[extensions]`: more file extensions for ABL source, without the dot.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extensions {
    /// Compiled like .p: `["x", "bp"]`.
    #[serde(default)]
    pub main: Vec<String>,
    /// Included like .i: `["f", "v"]`.
    #[serde(default)]
    pub include: Vec<String>,
}

impl Extensions {
    /// Lower-cased, without a leading dot, empty ones dropped.
    pub fn normalized(&self) -> (Vec<String>, Vec<String>) {
        let norm = |v: &[String]| -> Vec<String> {
            v.iter()
                .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|e| !e.is_empty())
                .collect()
        };
        (norm(&self.main), norm(&self.include))
    }
}

/// `[vulncheck]`: where the advisories are and which to skip.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vulncheck {
    /// An advisory file or folder, relative to the root. Default: ~/.abl/advisories.
    pub db: Option<String>,
    /// Advisory ids that don't apply to this project.
    #[serde(default)]
    pub ignore: Vec<String>,
}

/// A package from a git repository.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dep {
    /// Anything `git clone` takes: a URL or a local path.
    pub git: String,
    /// A tag, branch or commit. Default: the repository's default branch.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

/// `abl.lock`: the exact commit of every package, nested ones included.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Lock {
    #[serde(default, rename = "package")]
    pub packages: Vec<Locked>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Locked {
    pub name: String,
    pub git: String,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub commit: String,
    /// The package that asked for it, when it isn't in abl.toml itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

impl Lock {
    pub fn load(root: &Path) -> Result<Lock, String> {
        let path = root.join(LOCK_FILE);
        if !path.exists() {
            return Ok(Lock::default());
        }
        toml::from_str(&read(&path)?).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let path = root.join(LOCK_FILE);
        if self.packages.is_empty() {
            let _ = fs::remove_file(&path);
            return Ok(());
        }
        let body = toml::to_string(self).map_err(|e| e.to_string())?;
        fs::write(
            &path,
            format!("# Written by `abl install`. Commit it, so every machine gets the same code.\n\n{body}"),
        )
        .map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The PROPATH a package adds, relative to its own root: its abl.toml's
/// `propath`, else `src` when it has one (where ABL repositories usually keep
/// their code, as `abl init` assumes too), else the root itself. Read
/// leniently, so a package written for a newer abl still works.
pub fn package_propath(dir: &Path) -> Vec<String> {
    let entries: Vec<String> = fs::read_to_string(dir.join(PROJECT_FILE))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .and_then(|t| t.get("propath").and_then(|p| p.as_array()).cloned())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if entries.is_empty() {
        if dir.join("src").is_dir() {
            vec!["src".into()]
        } else {
            vec![".".into()]
        }
    } else {
        entries
    }
}

/// A loaded `abl.toml`.
pub struct Loaded {
    /// The folder that holds `abl.toml`. Relative paths in it resolve from here.
    pub root: PathBuf,
    pub file: PathBuf,
    pub project: Project,
}

impl Loaded {
    /// A path from abl.toml, resolved against the project root. Either slash
    /// works in the file; the result uses the platform's separator.
    pub fn path(&self, rel: &str) -> PathBuf {
        let rel: PathBuf = rel
            .split(['/', '\\'])
            .filter(|c| !c.is_empty() && *c != ".")
            .collect();
        if rel.as_os_str().is_empty() {
            return self.root.clone();
        }
        self.root.join(rel)
    }

    /// The project PROPATH as absolute folders (the root alone if none is
    /// set), then the installed packages'.
    pub fn propath(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = if self.project.propath.is_empty() {
            vec![self.root.clone()]
        } else {
            self.project.propath.iter().map(|p| self.path(p)).collect()
        };
        out.extend(self.package_propath().iter().map(|p| self.path(p)));
        out
    }

    /// The installed packages' PROPATH entries, relative to the root, in
    /// abl.lock order (abl.toml order when there is no lock yet).
    pub fn package_propath(&self) -> Vec<String> {
        let names: Vec<String> = match Lock::load(&self.root) {
            Ok(l) if !l.packages.is_empty() => l.packages.into_iter().map(|p| p.name).collect(),
            _ => self.project.deps.keys().cloned().collect(),
        };
        let mut out = Vec::new();
        for n in names {
            let dir = self.root.join(VENDOR_DIR).join(&n);
            if !dir.is_dir() {
                continue;
            }
            for e in package_propath(&dir) {
                let e = e.trim_start_matches("./").trim_end_matches('/');
                out.push(if e == "." || e.is_empty() {
                    format!("{VENDOR_DIR}/{n}")
                } else {
                    format!("{VENDOR_DIR}/{n}/{e}")
                });
            }
        }
        out
    }

    /// The folder that holds (or would hold) assemblies.xml.
    pub fn assemblies_dir(&self) -> PathBuf {
        self.path(self.project.assemblies.as_deref().unwrap_or("."))
    }

    /// Where r-code goes (`output`, default `build/rcode`).
    pub fn output(&self) -> PathBuf {
        self.path(self.project.output.as_deref().unwrap_or("build/rcode"))
    }

    /// The `.db` file of a database `abl db` manages.
    pub fn db_path(&self, name: &str, db: &Db) -> PathBuf {
        match &db.path {
            Some(p) => {
                let p = self.path(p);
                if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("db")) {
                    p
                } else {
                    p.with_extension("db")
                }
            }
            None => self
                .root
                .join(".abl")
                .join("db")
                .join(name)
                .join(format!("{name}.db")),
        }
    }
}

/// The folder that holds global configuration.
pub fn home() -> Result<PathBuf, String> {
    if let Some(h) = env::var_os(HOME_ENV).filter(|h| !h.is_empty()) {
        return Ok(PathBuf::from(h));
    }
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env::var_os(var)
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".abl"))
        .ok_or_else(|| format!("cannot find the home folder; set {HOME_ENV}"))
}

pub fn global_path() -> Result<PathBuf, String> {
    Ok(home()?.join("config.toml"))
}

/// Reads the global config. A missing file is an empty config.
pub fn load_global() -> Result<Global, String> {
    let path = global_path()?;
    if !path.exists() {
        return Ok(Global::default());
    }
    let text = read(&path)?;
    toml::from_str(&text).map_err(|e| format!("{}: {}", path.display(), e.message()))
}

/// The nearest `abl.toml` at or above `start`.
pub fn find_project(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|d| d.join(PROJECT_FILE))
        .find(|f| f.is_file())
}

pub fn load_project(start: &Path) -> Result<Option<Loaded>, String> {
    let Some(file) = find_project(start) else {
        return Ok(None);
    };
    let text = read(&file)?;
    let project: Project =
        toml::from_str(&text).map_err(|e| format!("{}: {}", file.display(), e.message()))?;
    let root = file.parent().unwrap_or(Path::new(".")).to_path_buf();
    Ok(Some(Loaded {
        root,
        file,
        project,
    }))
}

/// Where the chosen OpenEdge version came from, highest priority first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Flag,
    Env,
    Project,
    Global,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::Flag => "--oe",
            Source::Env => OE_ENV,
            Source::Project => PROJECT_FILE,
            Source::Global => "global default",
        })
    }
}

/// Picks the OpenEdge version: `--oe` flag, then `ABL_OE`, then `abl.toml`, then
/// the global default.
pub fn resolve_version(
    flag: Option<&str>,
    project: Option<&Project>,
    global: &Global,
) -> Option<(String, Source)> {
    let env = env::var(OE_ENV).ok();
    pick_version(flag, env.as_deref(), project, global)
}

fn pick_version(
    flag: Option<&str>,
    env: Option<&str>,
    project: Option<&Project>,
    global: &Global,
) -> Option<(String, Source)> {
    let nonempty = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    nonempty(flag)
        .map(|v| (v, Source::Flag))
        .or_else(|| nonempty(env).map(|v| (v, Source::Env)))
        .or_else(|| nonempty(project.and_then(|p| p.oe.as_deref())).map(|v| (v, Source::Project)))
        .or_else(|| nonempty(global.default.as_deref()).map(|v| (v, Source::Global)))
}

/// The DLC folder for `version`, or an error that lists the known versions.
pub fn dlc_for<'g>(global: &'g Global, version: &str) -> Result<&'g Path, String> {
    if let Some(p) = global.oe.get(version) {
        return Ok(p);
    }
    let known: Vec<&str> = global.oe.keys().map(String::as_str).collect();
    let known = if known.is_empty() {
        "none".to_string()
    } else {
        known.join(", ")
    };
    Err(format!(
        "OE {version} is not installed here (known: {known}). Run `abl oe add {version} <path>`"
    ))
}

/// The `_progres` executable inside a DLC folder.
pub fn progres(dlc: &Path) -> PathBuf {
    dlc.join("bin").join(if cfg!(windows) {
        "_progres.exe"
    } else {
        "_progres"
    })
}

/// Reads `DLC/version` ("OpenEdge Release 12.8.3 as of ...") and returns "12.8".
/// The full release of an install, `12.8.3` from "OpenEdge Release 12.8.3 as of ...".
pub fn install_release(dlc: &Path) -> Option<String> {
    let text = fs::read_to_string(dlc.join("version")).ok()?;
    let mut words = text.split_whitespace();
    words.find(|w| w.eq_ignore_ascii_case("release"))?;
    let full = words.next()?;
    full.starts_with(|c: char| c.is_ascii_digit()).then(|| {
        full.trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
            .to_string()
    })
}

pub fn install_version(dlc: &Path) -> Option<String> {
    let text = fs::read_to_string(dlc.join("version")).ok()?;
    parse_version(&text)
}

fn parse_version(text: &str) -> Option<String> {
    let mut words = text.split_whitespace();
    words.find(|w| w.eq_ignore_ascii_case("release"))?;
    let full = words.next()?;
    let mut parts = full.split('.');
    let major = parts
        .next()
        .filter(|p| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty())?;
    let minor: String = parts
        .next()
        .map(|p| p.chars().take_while(char::is_ascii_digit).collect())
        .unwrap_or_default();
    Some(if minor.is_empty() {
        major.to_string()
    } else {
        format!("{major}.{minor}")
    })
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

// ---- Writing. toml_edit keeps the user's comments and layout. ----

fn load_doc(path: &Path) -> Result<DocumentMut, String> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    read(path)?
        .parse::<DocumentMut>()
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn save_doc(path: &Path, doc: &mut DocumentMut) -> Result<(), String> {
    // Top-level values print before all tables. Keep one blank line between
    // them and the first table; anything the user put there (comments) stays.
    let has_values = doc.iter().any(|(_, item)| !item.is_table());
    let first_table = doc
        .iter_mut()
        .filter_map(|(_, item)| item.as_table_mut())
        .min_by_key(|t| t.position().unwrap_or(usize::MAX));
    if let (true, Some(t)) = (has_values, first_table) {
        let bare = t
            .decor()
            .prefix()
            .and_then(|p| p.as_str())
            .is_none_or(str::is_empty);
        if bare {
            t.decor_mut().set_prefix("\n");
        }
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(path, doc.to_string()).map_err(|e| format!("{}: {e}", path.display()))
}

fn oe_table(doc: &mut DocumentMut) -> Result<&mut Table, String> {
    doc.entry("oe")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| "`oe` in the global config is not a table".to_string())
}

pub fn add_install(version: &str, dlc: &Path) -> Result<(), String> {
    let path = global_path()?;
    let mut doc = load_doc(&path)?;
    oe_table(&mut doc)?.insert(version, value(dlc.display().to_string()));
    save_doc(&path, &mut doc)
}

/// Removes an install. Returns true if it was also the global default (which is
/// then cleared).
pub fn remove_install(version: &str) -> Result<bool, String> {
    let path = global_path()?;
    let mut doc = load_doc(&path)?;
    oe_table(&mut doc)?.remove(version);
    let was_default = doc.get("default").and_then(Item::as_str) == Some(version);
    if was_default {
        doc.remove("default");
    }
    save_doc(&path, &mut doc)?;
    Ok(was_default)
}

pub fn set_default(version: &str) -> Result<(), String> {
    let path = global_path()?;
    let mut doc = load_doc(&path)?;
    doc.insert("default", value(version));
    save_doc(&path, &mut doc)
}

/// Adds or replaces `[deps] name = { git = ..., ref = ... }` in abl.toml.
pub fn set_dep(file: &Path, name: &str, dep: &Dep) -> Result<(), String> {
    let mut doc = load_doc(file)?;
    let deps = doc
        .entry("deps")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or("`deps` in abl.toml is not a table")?;
    let mut t = toml_edit::InlineTable::new();
    t.insert("git", dep.git.as_str().into());
    if let Some(r) = &dep.reference {
        t.insert("ref", r.as_str().into());
    }
    deps.insert(name, value(t));
    save_doc(file, &mut doc)
}

/// Removes a package from `[deps]`; false if it wasn't there.
pub fn remove_dep(file: &Path, name: &str) -> Result<bool, String> {
    let mut doc = load_doc(file)?;
    let Some(deps) = doc.get_mut("deps").and_then(Item::as_table_mut) else {
        return Ok(false);
    };
    let had = deps.remove(name).is_some();
    if deps.is_empty() {
        doc.remove("deps");
    }
    save_doc(file, &mut doc)?;
    Ok(had)
}

pub fn set_project_oe(file: &Path, version: &str) -> Result<(), String> {
    let mut doc = load_doc(file)?;
    doc.insert("oe", value(version));
    save_doc(file, &mut doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn global(default: Option<&str>) -> Global {
        Global {
            default: default.map(String::from),
            oe: BTreeMap::new(),
            format: Format::default(),
        }
    }

    #[test]
    fn package_propath_defaults() {
        let dir = std::env::temp_dir().join(format!("abl-pkgpp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Nothing to go on: the root.
        assert_eq!(package_propath(&dir), ["."]);
        // A src folder: that.
        fs::create_dir_all(dir.join("src")).unwrap();
        assert_eq!(package_propath(&dir), ["src"]);
        // The package's own abl.toml wins.
        fs::write(dir.join(PROJECT_FILE), "propath = [\"lib\", \"src\"]\n").unwrap();
        assert_eq!(package_propath(&dir), ["lib", "src"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_precedence() {
        let proj = Project {
            oe: Some("11.7".into()),
            ..Default::default()
        };
        let g = global(Some("12.8"));
        let pick = |f, e, p| pick_version(f, e, p, &g);
        assert_eq!(
            pick(Some("12.2"), Some("12.6"), Some(&proj)),
            Some(("12.2".into(), Source::Flag))
        );
        assert_eq!(
            pick(None, Some("12.6"), Some(&proj)),
            Some(("12.6".into(), Source::Env))
        );
        assert_eq!(
            pick(None, Some(" "), Some(&proj)),
            Some(("11.7".into(), Source::Project))
        );
        assert_eq!(
            pick(None, None, None),
            Some(("12.8".into(), Source::Global))
        );
        assert_eq!(pick_version(None, None, None, &global(None)), None);
    }

    #[test]
    fn version_file() {
        assert_eq!(
            parse_version("OpenEdge Release 11.7.4 as of Thu Sep 13 19:01:24 EDT 2018\n"),
            Some("11.7".into())
        );
        assert_eq!(
            parse_version("OpenEdge Release 12.8 as of ..."),
            Some("12.8".into())
        );
        assert_eq!(
            parse_version("OpenEdge Release 12.2.0.1 as of ..."),
            Some("12.2".into())
        );
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn unknown_version_lists_known() {
        let mut g = global(None);
        g.oe.insert("11.7".into(), PathBuf::from("C:/dlc117"));
        g.oe.insert("12.8".into(), PathBuf::from("C:/dlc128"));
        let err = dlc_for(&g, "12.2").unwrap_err();
        assert!(err.contains("known: 11.7, 12.8"), "{err}");
        assert_eq!(dlc_for(&g, "11.7").unwrap(), Path::new("C:/dlc117"));
    }

    #[test]
    fn project_rejects_typos() {
        let err = toml::from_str::<Project>("propth = [\"src\"]").unwrap_err();
        assert!(err.message().contains("propth"), "{}", err.message());
    }
}

//! `abl compile`: compile the project with OpenEdge, incrementally.
//!
//! A unit (.p, .w, .cls, .t) is recompiled when its source, or any file it
//! depends on at compile time (includes and classes, followed through), has
//! changed since its last successful compile, or its .r is missing. Changing
//! the OpenEdge version, PROPATH, startup parameters or a .df schema recompiles
//! everything. The record lives in `<output>/.abl-build`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use abl_core::deps::{edges, EdgeKind};
use abl_core::workspace::key;
use abl_oe::session::{self, CompileMessage, Env};
use serde_json::{json, Value};

use crate::lint::Output;
use crate::source::Project;

const STATE_FILE: &str = ".abl-build";
const STATE_VERSION: &str = "abl-build 1";

pub fn is_unit(p: &Path) -> bool {
    abl_core::workspace::is_compile_unit(p)
}

/// FNV-1a: stable across Rust versions, unlike `DefaultHasher`.
#[derive(Clone, Copy)]
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
    fn add_raw(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
    /// Adds a field: the bytes, then a separator so ("ab", "c") and ("a", "bc") differ.
    fn add(&mut self, bytes: &[u8]) {
        self.add_raw(bytes);
        self.add_raw(&[0xff]);
    }
    fn hex(self) -> String {
        format!("{:016x}", self.0)
    }
}

/// The record of what was compiled, and from which inputs.
#[derive(Default)]
struct State {
    env: String,
    /// compile name -> input hash
    units: BTreeMap<String, String>,
}

impl State {
    fn load(dir: &Path) -> State {
        let Ok(text) = fs::read_to_string(dir.join(STATE_FILE)) else {
            return State::default();
        };
        let mut lines = text.lines();
        if lines.next() != Some(STATE_VERSION) {
            return State::default();
        }
        let mut s = State::default();
        for l in lines {
            match l.split_once('\t') {
                Some(("env", h)) => s.env = h.to_string(),
                Some((h, name)) => {
                    s.units.insert(name.to_string(), h.to_string());
                }
                None => {}
            }
        }
        s
    }

    fn save(&self, dir: &Path) -> Result<(), String> {
        let mut text = format!("{STATE_VERSION}\nenv\t{}\n", self.env);
        for (name, h) in &self.units {
            text.push_str(&format!("{h}\t{name}\n"));
        }
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let p = dir.join(STATE_FILE);
        fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))
    }
}

fn env_hash(env: &Env, project: &Project) -> String {
    let mut h = Fnv::new();
    h.add(env.version.as_bytes());
    h.add(env.dlc.to_string_lossy().as_bytes());
    h.add(env.propath_string().as_bytes());
    // `-1` comes and goes as a database server starts and stops; it doesn't
    // change what compiles.
    for p in env.params.iter().filter(|p| *p != "-1") {
        h.add(p.as_bytes());
    }
    for s in project.ws.schema() {
        h.add(s.ana.text.as_bytes());
    }
    h.hex()
}

/// Hashes a unit with every file it depends on at compile time.
struct Hasher<'a> {
    project: &'a Project,
    files: HashMap<String, Vec<u8>>,
}

impl Hasher<'_> {
    fn bytes(&mut self, p: &Path) -> &[u8] {
        self.files
            .entry(key(p))
            .or_insert_with(|| fs::read(p).unwrap_or_default())
    }

    fn deps(&self, unit: &Path) -> BTreeSet<(String, PathBuf)> {
        let mut seen: BTreeSet<(String, PathBuf)> = BTreeSet::new();
        let mut stack = vec![unit.to_path_buf()];
        while let Some(f) = stack.pop() {
            let Some(ana) = self.project.ws.get(&f) else {
                continue;
            };
            for e in edges(&self.project.ws, &f, &ana) {
                if e.kind == EdgeKind::Run {
                    continue;
                }
                if let Some(r) = e.resolved {
                    if key(&r) != key(unit) && seen.insert((key(&r), r.clone())) {
                        stack.push(r);
                    }
                }
            }
        }
        seen
    }

    fn unit(&mut self, unit: &Path) -> String {
        let mut h = Fnv::new();
        h.add(self.bytes(unit));
        for (k, p) in self.deps(unit) {
            h.add(k.as_bytes());
            let b = self.bytes(&p).to_vec();
            h.add(&b);
        }
        h.hex()
    }
}

/// What a compile would do.
pub struct Plan {
    pub env: Env,
    pub output: PathBuf,
    /// (unit, compile name, input hash), in the order to compile.
    pub todo: Vec<(PathBuf, String, String)>,
    pub up_to_date: usize,
    state: State,
    env_hash: String,
}

fn rcode_path(output: &Path, name: &str) -> PathBuf {
    let p = Path::new(name);
    let rel = if p.is_absolute() {
        PathBuf::from(p.file_name().unwrap_or_default())
    } else {
        p.to_path_buf()
    };
    output.join(rel.with_extension("r"))
}

/// The project's r-code folder.
pub fn default_output(project: &Project, env: &Env) -> PathBuf {
    project
        .loaded
        .as_ref()
        .map_or_else(|| env.cwd.join("build").join("rcode"), |l| l.output())
}

pub fn plan(
    project: &Project,
    env: Env,
    units: &[PathBuf],
    all: bool,
    output: Option<&Path>,
) -> Plan {
    let output = output.map_or_else(|| default_output(project, &env), Path::to_path_buf);
    let state = State::load(&output);
    let env_hash = env_hash(&env, project);
    let fresh_env = state.env == env_hash;
    let mut hasher = Hasher {
        project,
        files: HashMap::new(),
    };
    let mut todo = Vec::new();
    let mut up_to_date = 0;
    for u in units {
        let name = env.compile_name(u);
        let h = hasher.unit(u);
        let current = !all
            && fresh_env
            && state.units.get(&name) == Some(&h)
            && rcode_path(&output, &name).is_file();
        if current {
            up_to_date += 1;
        } else {
            todo.push((u.clone(), name, h));
        }
    }
    Plan {
        env,
        output,
        todo,
        up_to_date,
        state,
        env_hash,
    }
}

pub struct Options {
    /// Only check: compile without saving r-code, and record nothing.
    pub check: bool,
    pub force: bool,
    pub jobs: usize,
    pub output: Output,
    /// R-code goes here instead of the project's `output`.
    pub output_dir: Option<PathBuf>,
}

pub fn run(
    cwd: &Path,
    paths: &[PathBuf],
    flag: Option<&str>,
    opts: &Options,
) -> Result<u8, String> {
    let project = Project::open(cwd)?;
    let env = Env::new(project.loaded.as_ref(), flag, cwd)?;
    let units: Vec<PathBuf> = project
        .select(paths, cwd)?
        .into_iter()
        .filter(|p| is_unit(p))
        .collect();
    let plan = plan(
        &project,
        env,
        &units,
        opts.check || opts.force,
        opts.output_dir.as_deref(),
    );
    let result = execute(&plan, opts.check, opts.jobs)?;
    let shown = |m: &CompileMessage| message_file(&project, &plan, m, cwd);
    match opts.output {
        Output::Text => {
            for m in &result.messages {
                println!(
                    "{}:{}:{}: {}: {}",
                    shown(m),
                    m.line.max(1),
                    m.column.max(1),
                    if m.error { "error" } else { "warning" },
                    m.text
                );
            }
            if !result.messages.is_empty() {
                println!();
            }
            let failed = result.failed.len();
            if plan.todo.is_empty() {
                println!("Nothing to compile: {} files up to date", plan.up_to_date);
            } else {
                let verb = if opts.check { "Checked" } else { "Compiled" };
                let mut line = format!(
                    "{verb} {} of {} files with OE {}",
                    plan.todo.len(),
                    units.len(),
                    plan.env.version
                );
                if plan.up_to_date > 0 {
                    line.push_str(&format!(" ({} up to date)", plan.up_to_date));
                }
                if failed > 0 {
                    line.push_str(&format!(", {failed} failed"));
                }
                println!("{line}");
            }
        }
        Output::Json => {
            let items: Vec<Value> = result
                .messages
                .iter()
                .map(|m| {
                    json!({
                        "file": shown(m),
                        "unit": m.unit,
                        "line": m.line,
                        "column": m.column,
                        "number": m.number,
                        "severity": if m.error { "error" } else { "warning" },
                        "message": m.text,
                    })
                })
                .collect();
            let out = json!({
                "oe": plan.env.version,
                "compiled": plan.todo.len(),
                "upToDate": plan.up_to_date,
                "failed": result.failed,
                "messages": items,
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        }
        Output::Github => {
            for m in &result.messages {
                println!(
                    "::{} file={},line={},col={},title=abl compile::{}",
                    if m.error { "error" } else { "warning" },
                    shown(m),
                    m.line.max(1),
                    m.column.max(1),
                    m.text.replace('%', "%25").replace('\n', "%0A")
                );
            }
        }
    }
    Ok(u8::from(!result.failed.is_empty()))
}

pub struct Outcome {
    pub messages: Vec<CompileMessage>,
    /// Compile names of the units that failed.
    pub failed: Vec<String>,
}

/// Compiles the plan's units in `jobs` parallel sessions and records the
/// successful ones (unless `check`).
pub fn execute(plan: &Plan, check: bool, jobs: usize) -> Result<Outcome, String> {
    let names: Vec<String> = plan.todo.iter().map(|t| t.1.clone()).collect();
    if !check {
        for n in &names {
            if let Some(dir) = rcode_path(&plan.output, n).parent() {
                fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
        }
    }
    let save = (!check).then_some(plan.output.as_path());
    // A database open single-user (no server running) takes one session at a time.
    let jobs = if plan.env.single_user { 1 } else { jobs };
    let jobs = jobs.clamp(1, names.len().max(1));
    // Round-robin so each session gets a similar mix.
    let mut chunks: Vec<Vec<String>> = vec![Vec::new(); jobs];
    for (i, n) in names.iter().enumerate() {
        chunks[i % jobs].push(n.clone());
    }
    let results: Vec<Result<session::CompileOutcome, String>> = std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .iter()
            .filter(|c| !c.is_empty())
            .map(|c| s.spawn(|| session::compile(&plan.env, c, save)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("compile thread panicked".into()))
            })
            .collect()
    });
    let mut messages = Vec::new();
    let mut done: Vec<(String, bool)> = Vec::new();
    let mut errors = Vec::new();
    for r in results {
        match r {
            Ok(o) => {
                messages.extend(o.messages);
                done.extend(o.done);
            }
            Err(e) => errors.push(e),
        }
    }
    // Keep the project's file order in the report.
    let order: HashMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    messages.sort_by_key(|m| {
        (
            order.get(m.unit.as_str()).copied().unwrap_or(usize::MAX),
            m.line,
            m.column,
        )
    });
    let failed: Vec<String> = done
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| n.clone())
        .collect();
    if !check {
        let mut state = State {
            env: plan.env_hash.clone(),
            units: if plan.state.env == plan.env_hash {
                plan.state.units.clone()
            } else {
                BTreeMap::new()
            },
        };
        let hashes: HashMap<&str, &str> = plan
            .todo
            .iter()
            .map(|t| (t.1.as_str(), t.2.as_str()))
            .collect();
        for (n, ok) in &done {
            if *ok {
                if let Some(h) = hashes.get(n.as_str()) {
                    state.units.insert(n.clone(), (*h).to_string());
                }
            } else {
                state.units.remove(n);
            }
        }
        state.save(&plan.output)?;
    }
    if let Some(e) = errors.into_iter().next() {
        return Err(e);
    }
    Ok(Outcome { messages, failed })
}

/// Where a compiler message points, for display: the compiler reports names
/// relative to the PROPATH (or absolute); show them relative to `cwd`.
pub fn message_file(project: &Project, plan: &Plan, m: &CompileMessage, cwd: &Path) -> String {
    let file = if m.file.is_empty() { &m.unit } else { &m.file };
    let p = Path::new(file);
    let resolved = if p.is_absolute() {
        Some(p.to_path_buf())
    } else {
        plan.env
            .propath
            .iter()
            .map(|d| d.join(p))
            .find(|c| c.is_file())
    };
    match resolved {
        Some(r) => project.show(&r, cwd),
        None => file.replace('\\', "/"),
    }
}

/// `abl compile --versions 11.7,12.8`: one compile per version, each into
/// `<output>/<version>` with its own record. All versions are checked to be
/// installed before any compile starts.
pub fn run_versions(
    cwd: &Path,
    paths: &[PathBuf],
    versions: &[String],
    opts: &Options,
) -> Result<u8, String> {
    let project = Project::open(cwd)?;
    for v in versions {
        Env::new(project.loaded.as_ref(), Some(v), cwd)?;
    }
    let base = default_output(
        &project,
        &Env::new(project.loaded.as_ref(), Some(&versions[0]), cwd)?,
    );
    let mut worst = 0;
    for (i, v) in versions.iter().enumerate() {
        if opts.output == Output::Text {
            if i > 0 {
                println!();
            }
            println!("== OE {v} ==");
        }
        let o = Options {
            check: opts.check,
            force: opts.force,
            jobs: opts.jobs,
            output: opts.output,
            output_dir: Some(base.join(v)),
        };
        worst = worst.max(run(cwd, paths, Some(v), &o)?);
    }
    Ok(worst)
}

/// For `abl status`: (units needing a compile, all units), or None when there
/// is no usable OpenEdge install.
pub fn stale(project: &Project, flag: Option<&str>, cwd: &Path) -> Option<(usize, usize)> {
    let env = Env::new(project.loaded.as_ref(), flag, cwd).ok()?;
    let units: Vec<PathBuf> = project
        .files
        .iter()
        .filter(|p| is_unit(p))
        .cloned()
        .collect();
    let p = plan(project, env, &units, false, None);
    Some((p.todo.len(), units.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_is_stable() {
        // The published FNV-1a 64-bit test vector for "a".
        let mut raw = Fnv::new();
        raw.add_raw(b"a");
        assert_eq!(raw.hex(), "af63dc4c8601ec8c");
        // With the field separator. Pinned: a change here means every
        // project recompiles everything once.
        let mut h = Fnv::new();
        h.add(b"abc");
        assert_eq!(h.hex(), "fc182483ee0806dc");
        let mut a = Fnv::new();
        a.add(b"ab");
        a.add(b"c");
        assert_ne!(a.hex(), h.hex());
    }

    #[test]
    fn rcode_paths() {
        let out = Path::new("out");
        assert_eq!(
            rcode_path(out, "app/Order.cls"),
            Path::new("out/app/Order.r")
        );
        assert_eq!(rcode_path(out, "a.p"), Path::new("out/a.r"));
    }
}

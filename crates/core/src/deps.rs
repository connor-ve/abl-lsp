//! The dependency graph between source files: includes, `RUN` targets and classes.
//!
//! Includes and class references are compile-time dependencies: changing them
//! means the files that use them must be recompiled. `RUN` targets are runtime
//! dependencies only.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::analysis::{Analysis, SymKind};
use crate::workspace::{is_include_file, key, Workspace};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EdgeKind {
    /// `{file.i}`
    Include,
    /// `RUN file.p`
    Run,
    /// `INHERITS`
    Inherits,
    /// `IMPLEMENTS`
    Implements,
    /// A variable, parameter or property of a class type (`AS Foo`).
    Uses,
}

impl EdgeKind {
    pub fn name(self) -> &'static str {
        match self {
            EdgeKind::Include => "include",
            EdgeKind::Run => "run",
            EdgeKind::Inherits => "inherits",
            EdgeKind::Implements => "implements",
            EdgeKind::Uses => "uses",
        }
    }

    /// Does a change to the target mean the source has to be recompiled?
    pub fn compile_time(self) -> bool {
        self != EdgeKind::Run
    }
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub kind: EdgeKind,
    /// The target as written in the source.
    pub target: String,
    /// The file it resolves to on the PROPATH, if found.
    pub resolved: Option<PathBuf>,
    /// 0-based line of the reference.
    pub line: u32,
}

/// Direct dependencies of one file, in source order. Built-in classes
/// (Progress.Lang.*) are left out.
pub fn edges(ws: &Workspace, path: &Path, ana: &Analysis) -> Vec<Edge> {
    let line = |off: usize| ana.lines.position(&ana.text, off).line;
    let mut out: Vec<(usize, Edge)> = Vec::new();
    for r in &ana.includes {
        out.push((
            r.start,
            Edge {
                kind: EdgeKind::Include,
                target: r.target.clone(),
                resolved: ws.resolve_file(&r.target, Some(path)),
                line: line(r.start),
            },
        ));
    }
    for r in ana.runs.iter().filter(|r| r.is_file) {
        // RUN VALUE(expr) has no static target.
        if r.target.contains(['(', ')']) {
            continue;
        }
        out.push((
            r.start,
            Edge {
                kind: EdgeKind::Run,
                target: r.target.clone(),
                resolved: ws.resolve_run(&r.target, Some(path)),
                line: line(r.start),
            },
        ));
    }
    let mut classes: Vec<(EdgeKind, String, usize)> = Vec::new();
    if let Some(i) = &ana.inherits {
        classes.push((EdgeKind::Inherits, i.clone(), class_offset(ana, i)));
    }
    for i in &ana.implements {
        classes.push((EdgeKind::Implements, i.clone(), class_offset(ana, i)));
    }
    let mut seen: HashSet<String> = classes.iter().map(|c| c.1.to_ascii_lowercase()).collect();
    for s in &ana.symbols {
        let typed = matches!(
            s.kind,
            SymKind::Variable | SymKind::Parameter | SymKind::Property
        );
        let Some(t) = s.type_name.as_ref().filter(|_| typed) else {
            continue;
        };
        if seen.insert(t.to_ascii_lowercase()) {
            classes.push((EdgeKind::Uses, t.clone(), s.start));
        }
    }
    for (kind, name, off) in classes {
        let resolved = ws.resolve_class(&name, &ana.usings);
        if resolved.as_deref().is_some_and(crate::builtins::is_builtin) {
            continue;
        }
        // A type name that isn't a class (a table for LIKE, a buffer's table)
        // just doesn't resolve; only report unresolved names for INHERITS/IMPLEMENTS.
        if resolved.is_none() && kind == EdgeKind::Uses {
            continue;
        }
        out.push((
            off,
            Edge {
                kind,
                target: name,
                resolved,
                line: line(off),
            },
        ));
    }
    out.sort_by_key(|(off, _)| *off);
    out.into_iter().map(|(_, e)| e).collect()
}

fn class_offset(ana: &Analysis, name: &str) -> usize {
    ana.text
        .to_ascii_lowercase()
        .find(&name.to_ascii_lowercase())
        .unwrap_or(0)
}

/// The project-wide graph, keyed by [`key`] of each path.
pub struct Graph {
    pub files: BTreeMap<String, PathBuf>,
    pub out: BTreeMap<String, Vec<Edge>>,
}

/// Something that depends on the file being looked at.
#[derive(Clone, Debug)]
pub struct Dependent {
    pub path: PathBuf,
    /// How it depends on the target directly or through the first hop of `via`.
    pub kind: EdgeKind,
    /// Files in between (for example the include that includes the target).
    pub via: Vec<PathBuf>,
}

impl Graph {
    /// Reads every file in `files` and records its edges.
    pub fn build(ws: &Workspace, files: &[PathBuf]) -> Graph {
        let mut g = Graph {
            files: BTreeMap::new(),
            out: BTreeMap::new(),
        };
        for p in files {
            let Some(a) = ws.get(p) else { continue };
            let k = key(p);
            g.out.insert(k.clone(), edges(ws, p, &a));
            g.files.insert(k, p.clone());
        }
        g
    }

    /// Everything that depends on `target`, nearest first. Includes are followed
    /// through: a file that includes an include that includes `target` is a
    /// dependent with that include in `via`.
    pub fn dependents(&self, target: &Path) -> Vec<Dependent> {
        let mut rev: BTreeMap<String, Vec<(String, EdgeKind)>> = BTreeMap::new();
        for (from, es) in &self.out {
            for e in es {
                if let Some(r) = &e.resolved {
                    rev.entry(key(r)).or_default().push((from.clone(), e.kind));
                }
            }
        }
        let mut out = Vec::new();
        let start = key(target);
        let mut seen: BTreeSet<String> = BTreeSet::from([start.clone()]);
        // (file, first edge kind, path of files between it and the target)
        let mut queue: VecDeque<(String, Vec<PathBuf>)> = VecDeque::from([(start, Vec::new())]);
        while let Some((k, via)) = queue.pop_front() {
            let Some(users) = rev.get(&k) else { continue };
            for (from, kind) in users {
                if !seen.insert(from.clone()) {
                    continue;
                }
                let Some(path) = self.files.get(from) else {
                    continue;
                };
                out.push(Dependent {
                    path: path.clone(),
                    kind: *kind,
                    via: via.clone(),
                });
                // Keep walking through compile-time links: whoever includes an
                // include, or extends a class, depends on what it depends on.
                if kind.compile_time() {
                    let mut v = via.clone();
                    v.insert(0, path.clone());
                    queue.push_back((from.clone(), v));
                }
            }
        }
        out
    }

    /// The compile units (`.p`, `.w`, `.cls`) to recompile after `target` changes.
    pub fn recompile_set(&self, target: &Path) -> Vec<PathBuf> {
        let mut set: Vec<PathBuf> = self
            .dependents(target)
            .into_iter()
            // A file that only RUNs something affected is not recompiled.
            .filter(|d| d.kind.compile_time())
            .filter(|d| !is_include_file(&d.path))
            .map(|d| d.path)
            .collect();
        if !is_include_file(target) && !set.iter().any(|p| key(p) == key(target)) {
            set.insert(0, target.to_path_buf());
        }
        set
    }
}

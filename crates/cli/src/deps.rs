//! `abl deps`: what a file depends on (includes, RUN targets, classes), or with
//! `--reverse`, what depends on it and what to recompile after changing it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use abl_core::deps::{edges, EdgeKind, Graph};
use abl_core::workspace::key;
use serde_json::{json, Value};

use crate::source::Project;

pub struct Options {
    pub reverse: bool,
    /// Follow RUN targets and classes too, not only includes.
    pub all: bool,
    pub json: bool,
}

pub fn run(cwd: &Path, file: &Path, opts: &Options) -> Result<u8, String> {
    let project = Project::open(cwd)?;
    let target = project
        .select(std::slice::from_ref(&file.to_path_buf()), cwd)?
        .into_iter()
        .next()
        .ok_or_else(|| format!("{}: not an ABL source file", file.display()))?;
    if opts.reverse {
        reverse(&project, cwd, &target, opts.json);
    } else {
        forward(&project, cwd, &target, opts);
    }
    Ok(0)
}

struct Node {
    kind: EdgeKind,
    target: String,
    path: Option<PathBuf>,
    line: u32,
    /// Already shown higher up (or a cycle): not expanded again.
    repeat: bool,
    children: Vec<Node>,
}

fn tree(
    p: &Project,
    file: &Path,
    all: bool,
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> Vec<Node> {
    let Some(ana) = p.ws.get(file) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in edges(&p.ws, file, &ana) {
        let mut n = Node {
            kind: e.kind,
            target: e.target,
            path: e.resolved.clone(),
            line: e.line,
            repeat: false,
            children: Vec::new(),
        };
        // Includes are always expanded: they are part of the file when it compiles.
        let expand = e.kind == EdgeKind::Include || all;
        if let (Some(r), true) = (&e.resolved, expand) {
            if depth >= 32 || !seen.insert(key(r)) {
                n.repeat = true;
            } else {
                n.children = tree(p, r, all, seen, depth + 1);
            }
        }
        out.push(n);
    }
    out
}

fn forward(p: &Project, cwd: &Path, target: &Path, opts: &Options) {
    let mut seen = BTreeSet::from([key(target)]);
    let nodes = tree(p, target, opts.all, &mut seen, 0);
    if opts.json {
        fn to_json(p: &Project, cwd: &Path, nodes: &[Node]) -> Vec<Value> {
            nodes
                .iter()
                .map(|n| {
                    json!({
                        "kind": n.kind.name(),
                        "target": n.target,
                        "file": n.path.as_ref().map(|x| p.show(x, cwd)),
                        "line": n.line + 1,
                        "repeat": n.repeat,
                        "children": to_json(p, cwd, &n.children),
                    })
                })
                .collect()
        }
        let out = json!({ "file": p.show(target, cwd), "dependencies": to_json(p, cwd, &nodes) });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return;
    }
    println!("{}", p.show(target, cwd));
    if nodes.is_empty() {
        println!("  (no dependencies)");
    }
    print_tree(p, cwd, &nodes, "");
    let missing = count_missing(&nodes);
    if missing > 0 {
        println!("\n{missing} not found on the PROPATH");
    }
}

fn print_tree(p: &Project, cwd: &Path, nodes: &[Node], prefix: &str) {
    for (i, n) in nodes.iter().enumerate() {
        let last = i + 1 == nodes.len();
        let (branch, next) = if last {
            ("└─ ", "   ")
        } else {
            ("├─ ", "│  ")
        };
        let what = match &n.path {
            Some(path) => p.show(path, cwd),
            None => format!("{}  (not found)", n.target),
        };
        let repeat = if n.repeat { "  (see above)" } else { "" };
        println!(
            "{prefix}{branch}{:<10} {what}  line {}{repeat}",
            n.kind.name(),
            n.line + 1
        );
        print_tree(p, cwd, &n.children, &format!("{prefix}{next}"));
    }
}

fn count_missing(nodes: &[Node]) -> usize {
    nodes
        .iter()
        .map(|n| usize::from(n.path.is_none()) + count_missing(&n.children))
        .sum()
}

fn reverse(p: &Project, cwd: &Path, target: &Path, json: bool) {
    let graph = Graph::build(&p.ws, &p.files);
    let deps = graph.dependents(target);
    let recompile = graph.recompile_set(target);
    if json {
        let items: Vec<Value> = deps
            .iter()
            .map(|d| {
                json!({
                    "file": p.show(&d.path, cwd),
                    "kind": d.kind.name(),
                    "via": d.via.iter().map(|v| p.show(v, cwd)).collect::<Vec<_>>(),
                })
            })
            .collect();
        let out = json!({
            "file": p.show(target, cwd),
            "dependents": items,
            "recompile": recompile.iter().map(|r| p.show(r, cwd)).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return;
    }
    let shown = p.show(target, cwd);
    if deps.is_empty() {
        println!("Nothing in the project depends on {shown}.");
    } else {
        println!("Used by:");
        let width = deps
            .iter()
            .map(|d| p.show(&d.path, cwd).len())
            .max()
            .unwrap_or(0);
        for d in &deps {
            let via = if d.via.is_empty() {
                String::new()
            } else {
                let chain: Vec<String> = d.via.iter().map(|v| p.show(v, cwd)).collect();
                format!(" (via {})", chain.join(" -> "))
            };
            println!("  {:<width$}  {}{via}", p.show(&d.path, cwd), d.kind.name());
        }
    }
    if !recompile.is_empty() {
        println!("\nRecompile after changing {shown}:");
        for r in &recompile {
            println!("  {}", p.show(r, cwd));
        }
    }
}

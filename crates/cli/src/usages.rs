//! `abl usages <table>[.field]`: where a database table or field is read,
//! created, updated or deleted.

use std::path::Path;
use std::sync::Arc;

use abl_core::usage::{self, Access, Target};
use serde_json::{json, Value};

use crate::source::Project;

pub struct Options {
    /// Only creates, updates and deletes.
    pub writes: bool,
    pub json: bool,
}

pub fn run(cwd: &Path, name: &str, opts: &Options) -> Result<u8, String> {
    let project = Project::open(cwd)?;
    let target = Target::parse(&project.ws, name)?;
    let files: Vec<_> = project
        .files
        .iter()
        .filter_map(|p| {
            let text = crate::source::read(p).ok()?.text;
            Some((p.clone(), Arc::new(project.analyze(p, text))))
        })
        .collect();
    let mut uses = usage::find(&project.ws, &files, &target);
    if opts.writes {
        uses.retain(|u| matches!(u.access, Access::Create | Access::Update | Access::Delete));
    }

    if opts.json {
        let items: Vec<Value> = uses
            .iter()
            .map(|u| {
                let (line, col) = u.line_col();
                json!({
                    "file": project.show(&u.path, cwd),
                    "line": line,
                    "column": col,
                    "access": u.access.name(),
                    "note": u.note,
                    "exclusiveLock": u.exclusive,
                    "text": u.line_text(),
                })
            })
            .collect();
        let v = json!({ "target": target.display(), "uses": items });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return Ok(0);
    }

    let mut nfiles: Vec<String> = uses
        .iter()
        .map(|u| abl_core::workspace::key(&u.path))
        .collect();
    nfiles.sort();
    nfiles.dedup();
    let what = if opts.writes { "writes" } else { "uses" };
    if uses.is_empty() {
        println!("{}: no {what} found", target.display());
        return Ok(0);
    }
    println!(
        "{}: {} {what} in {} file{}",
        target.display(),
        uses.len(),
        nfiles.len(),
        if nfiles.len() == 1 { "" } else { "s" }
    );
    let mut current: Option<Access> = None;
    for u in &uses {
        if current != Some(u.access) {
            let n = uses.iter().filter(|x| x.access == u.access).count();
            println!("\n{} ({n})", u.access.name());
            current = Some(u.access);
        }
        let (line, col) = u.line_col();
        let mut extra = Vec::new();
        if let Some(n) = &u.note {
            extra.push(n.clone());
        }
        if u.exclusive {
            extra.push("exclusive-lock".into());
        }
        let extra = if extra.is_empty() {
            String::new()
        } else {
            format!("  [{}]", extra.join(", "))
        };
        println!(
            "  {}:{line}:{col}  {}{extra}",
            project.show(&u.path, cwd),
            u.line_text()
        );
    }
    Ok(0)
}

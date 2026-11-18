//! `abl schema diff`: compare two .df files and optionally write the delta .df
//! that turns the first into the second.

use std::fs;
use std::path::Path;

use abl_core::df::{self, AttrChange, FieldChange, IndexChange, SequenceChange};

use crate::source;

fn read_df(p: &Path) -> Result<df::Schema, String> {
    let s = df::parse(&source::read(p)?.text);
    for w in &s.warnings {
        eprintln!("warning: {}: {w}", p.display());
    }
    Ok(s)
}

fn one_line(s: &str) -> String {
    let flat = s.replace('\n', " ");
    if flat.chars().count() > 60 {
        format!("{}...", flat.chars().take(57).collect::<String>())
    } else {
        flat
    }
}

fn describe(c: &AttrChange) -> String {
    match c {
        AttrChange::Added(a) if a.value.is_empty() => format!("+{}", a.key),
        AttrChange::Added(a) => format!("{} {}", a.key, one_line(&a.value)),
        AttrChange::Removed(a) => format!("-{}", a.key),
        AttrChange::Changed { from, to } => {
            format!(
                "{} {} -> {}",
                to.key,
                one_line(&from.value),
                one_line(&to.value)
            )
        }
    }
}

fn index_fields(i: &df::Index) -> String {
    i.fields
        .iter()
        .map(|f| f.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_summary(d: &df::Diff) {
    for t in &d.changed_tables {
        println!("~ table {}", t.name);
        for c in &t.attrs {
            println!("    ~ {}", describe(c));
        }
        for c in &t.fields {
            match c {
                FieldChange::Added(f) => println!("    + field {} ({})", f.name, f.data_type),
                FieldChange::Removed(f) => println!("    - field {} ({})", f.name, f.data_type),
                FieldChange::Updated { name, changes } => {
                    let list: Vec<String> = changes.iter().map(describe).collect();
                    println!("    ~ field {name}: {}", list.join("; "));
                }
                FieldChange::Recreated { old, new } => println!(
                    "    ! field {}: {} -> {} (dropped and re-added)",
                    new.name, old.data_type, new.data_type
                ),
            }
        }
        for c in &t.indexes {
            match c {
                IndexChange::Added(i) => println!("    + index {} ({})", i.name, index_fields(i)),
                IndexChange::Removed(i) => println!("    - index {} ({})", i.name, index_fields(i)),
                IndexChange::Recreated { old, new } => println!(
                    "    ! index {}: ({}) -> ({}) (dropped and re-added)",
                    new.name,
                    index_fields(old),
                    index_fields(new)
                ),
            }
        }
        if let Some(p) = &t.new_primary {
            println!("    ~ primary index -> {p}");
        }
    }
    for t in &d.added_tables {
        println!(
            "+ table {} ({}, {})",
            t.name,
            plural(t.fields.len(), "field", "fields"),
            plural(t.indexes.len(), "index", "indexes")
        );
    }
    for t in &d.removed_tables {
        println!("- table {}", t.name);
    }
    for c in &d.sequences {
        match c {
            SequenceChange::Added(s) => println!("+ sequence {}", s.name),
            SequenceChange::Removed(s) => println!("- sequence {}", s.name),
            SequenceChange::Updated { name, changes } => {
                let list: Vec<String> = changes.iter().map(describe).collect();
                println!("~ sequence {name}: {}", list.join("; "));
            }
        }
    }
}

pub fn diff(old: &Path, new: &Path, delta: Option<&Path>, exit_code: bool) -> Result<u8, String> {
    let d = df::diff(&read_df(old)?, &read_df(new)?);
    if d.is_empty() {
        println!("No schema differences.");
        return Ok(0);
    }
    print_summary(&d);
    if !d.warnings.is_empty() {
        println!();
        for w in &d.warnings {
            println!("warning: {w}");
        }
    }
    if let Some(out) = delta {
        let (text, notes) = df::delta(&d);
        fs::write(out, text).map_err(|e| format!("{}: {e}", out.display()))?;
        println!(
            "\nWrote {}. Load it with Data Administration > Load Data Definitions.",
            out.display()
        );
        for n in notes {
            println!("note: {n}");
        }
    }
    Ok(u8::from(exit_code))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

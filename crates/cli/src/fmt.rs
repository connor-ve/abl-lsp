//! `abl fmt`: format files in place, check that they are formatted (`--check`),
//! or print the result (`--stdout`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use abl_core::features;
use lsp_types::FormattingOptions;

use crate::source::{self, Project};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Write,
    Check,
    Stdout,
}

fn formatted(p: &Project, path: &Path, text: &str) -> String {
    let indent = p
        .loaded
        .as_ref()
        .and_then(|l| l.project.format.indent)
        .unwrap_or(4);
    let opts = FormattingOptions {
        tab_size: indent,
        insert_spaces: true,
        ..Default::default()
    };
    let ana = Arc::new(p.analyze(path, text.to_string()));
    let mut edits: Vec<(usize, usize, String)> = features::format(&p.ws, path, &ana, &opts, None)
        .into_iter()
        .map(|e| {
            (
                ana.lines.offset(&ana.text, e.range.start),
                ana.lines.offset(&ana.text, e.range.end),
                e.new_text,
            )
        })
        .collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = ana.text.clone();
    for (s, e, t) in edits {
        out.replace_range(s..e, &t);
    }
    out
}

/// First 1-based line where `a` and `b` differ.
fn first_difference(a: &str, b: &str) -> usize {
    a.lines()
        .zip(b.lines())
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| a.lines().count().min(b.lines().count()))
        + 1
}

pub fn run(
    cwd: &Path,
    paths: &[PathBuf],
    mode: Mode,
    case: Option<abl_core::format::KeywordCase>,
) -> Result<u8, String> {
    let mut project = Project::open(cwd)?;
    if let Some(c) = case {
        project.ws.keyword_case = c;
    }
    let files = project.select(paths, cwd)?;
    let mut changed = 0;
    for f in &files {
        let src = source::read(f)?;
        let out = formatted(&project, f, &src.text);
        let shown = project.show(f, cwd);
        match mode {
            Mode::Stdout => print!("{out}"),
            _ if out == src.text => {}
            Mode::Check => {
                println!(
                    "{shown}: not formatted (first difference on line {})",
                    first_difference(&src.text, &out)
                );
                changed += 1;
            }
            Mode::Write => {
                source::write(f, &out, src.encoding)?;
                println!("formatted {shown}");
                changed += 1;
            }
        }
    }
    match mode {
        Mode::Stdout => Ok(0),
        Mode::Check if changed > 0 => {
            eprintln!(
                "abl: {changed} of {} files need formatting; run `abl fmt`",
                files.len()
            );
            Ok(1)
        }
        Mode::Check => {
            println!("{} files formatted correctly", files.len());
            Ok(0)
        }
        Mode::Write => {
            if changed == 0 {
                println!("{} files already formatted", files.len());
            }
            Ok(0)
        }
    }
}

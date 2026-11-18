//! `abl build`: compile what changed, then package all the r-code into a
//! procedure library with `prolib`. The library is rebuilt from scratch each
//! time, so it never holds r-code of deleted sources.

use std::fs;
use std::path::{Path, PathBuf};

use abl_oe::db;
use abl_oe::session::Env;
use walkdir::WalkDir;

use crate::compile;
use crate::lint::Output;
use crate::source::Project;

/// Files per `prolib -add`, so the command line stays short (batch files on
/// Windows allow about 8000 characters).
const CHUNK: usize = 40;

fn library_path(project: &Project, cwd: &Path, output: Option<&Path>) -> PathBuf {
    if let Some(o) = output {
        return if o.is_absolute() {
            o.to_path_buf()
        } else {
            cwd.join(o)
        };
    }
    match &project.loaded {
        Some(l) => match &l.project.library {
            Some(lib) => l.path(lib),
            None => {
                let name = l
                    .root
                    .file_name()
                    .map_or("app".into(), |n| n.to_string_lossy().to_string());
                l.root.join("build").join(format!("{name}.pl"))
            }
        },
        None => cwd.join("build").join("app.pl"),
    }
}

pub fn run(
    cwd: &Path,
    output: Option<&Path>,
    jobs: usize,
    flag: Option<&str>,
) -> Result<u8, String> {
    let opts = compile::Options {
        check: false,
        force: false,
        jobs,
        output: Output::Text,
        output_dir: None,
    };
    if compile::run(cwd, &[], flag, &opts)? != 0 {
        return Err("not building the library: the compile failed".into());
    }
    let project = Project::open(cwd)?;
    let env = Env::new(project.loaded.as_ref(), flag, cwd)?;
    let rcode = compile::default_output(&project, &env);
    let mut files: Vec<String> = WalkDir::new(&rcode)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("r"))
        })
        .filter_map(|e| {
            let rel = e.path().strip_prefix(&rcode).ok()?;
            Some(rel.to_string_lossy().replace('\\', "/"))
        })
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(format!("no r-code in {} to package", rcode.display()));
    }

    let lib = library_path(&project, cwd, output);
    if let Some(dir) = lib.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if lib.exists() {
        fs::remove_file(&lib).map_err(|e| format!("{}: {e}", lib.display()))?;
    }
    let lib_arg = lib.display().to_string();
    // Run from the r-code folder so members keep their relative paths.
    db::run_checked(&env, "prolib", &rcode, &[lib_arg.clone(), "-create".into()])?;
    for chunk in files.chunks(CHUNK) {
        let mut args = vec![lib_arg.clone(), "-add".into()];
        args.extend(chunk.iter().cloned());
        if let Err(e) = db::run_checked(&env, "prolib", &rcode, &args) {
            let _ = fs::remove_file(&lib);
            return Err(e);
        }
    }
    let size = fs::metadata(&lib).map_or(0, |m| m.len());
    let shown = lib
        .strip_prefix(cwd)
        .unwrap_or(&lib)
        .display()
        .to_string()
        .replace('\\', "/");
    println!(
        "Built {shown}: {} file{}, {} KB",
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        size.div_ceil(1024)
    );
    Ok(0)
}

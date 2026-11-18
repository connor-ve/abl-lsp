//! `abl run`, `abl exec` and `abl watch`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use abl_core::workspace::{is_abl_file, is_schema_file};
use abl_oe::config;
use abl_oe::session::{self, Env};

use crate::compile;
use crate::source::Project;

/// `abl run [FILE] [-- ARGS]`: runs a procedure (default: `entry` from
/// abl.toml). ARGS are joined with spaces and passed as -param.
pub fn run(
    cwd: &Path,
    file: Option<&Path>,
    args: &[String],
    flag: Option<&str>,
) -> Result<u8, String> {
    let loaded = config::load_project(cwd)?;
    let env = Env::new(loaded.as_ref(), flag, cwd)?;
    let entry = loaded
        .as_ref()
        .and_then(|l| l.project.entry.as_ref().map(|e| l.path(e)));
    let target = match (file, entry) {
        (Some(f), _) => target_name(&env, cwd, f),
        (None, Some(e)) => target_name(&env, cwd, &e),
        (None, None) => {
            return Err("no procedure given and no `entry` in abl.toml: `abl run <file>`".into())
        }
    };
    let param = (!args.is_empty()).then(|| args.join(" "));
    let code = session::run(&env, &target, param.as_deref())?;
    Ok(u8::try_from(code).unwrap_or(1))
}

/// A file that exists is run by its PROPATH-relative name (so a compiled .r
/// is used when there is one), or by its full path. Anything else is passed
/// on as written, for OpenEdge to find on the PROPATH.
pub fn target_name(env: &Env, cwd: &Path, f: &Path) -> String {
    let abs = if f.is_absolute() {
        f.to_path_buf()
    } else {
        cwd.join(f)
    };
    if abs.is_file() {
        env.compile_name(&abs)
    } else {
        f.to_string_lossy().replace('\\', "/")
    }
}

/// `abl exec CODE`: runs a few statements without making a file.
pub fn exec(cwd: &Path, code: &[String], flag: Option<&str>) -> Result<u8, String> {
    let loaded = config::load_project(cwd)?;
    let env = Env::new(loaded.as_ref(), flag, cwd)?;
    let mut text = code.join(" ");
    if !text.trim_end().ends_with(['.', ':']) {
        text.push('.');
    }
    text.push('\n');
    let dir = std::env::temp_dir().join(format!("abl-exec-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join("abl_exec.p");
    fs::write(&file, &text).map_err(|e| format!("{}: {e}", file.display()))?;
    let result = session::run(&env, &file.display().to_string(), None);
    let _ = fs::remove_dir_all(&dir);
    Ok(u8::try_from(result?).unwrap_or(1))
}

/// Modification times of everything a compile depends on.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, SystemTime> {
    let skip = ["target", "node_modules", ".git", "build"];
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0 || !skip.contains(&e.file_name().to_string_lossy().as_ref())
        })
        .filter_map(Result::ok)
        .filter(|e| {
            let p = e.path();
            e.file_type().is_file()
                && (is_abl_file(p)
                    || is_schema_file(p)
                    || p.file_name().is_some_and(|n| n == config::PROJECT_FILE))
        })
        .filter_map(|e| Some((e.path().to_path_buf(), e.metadata().ok()?.modified().ok()?)))
        .collect()
}

/// `abl watch`: compiles what changed whenever a source file is saved. Stops
/// after `cycles` compiles when given (used by tests).
pub fn watch(
    cwd: &Path,
    flag: Option<&str>,
    jobs: usize,
    cycles: Option<usize>,
) -> Result<u8, String> {
    let root = config::find_project(cwd)
        .and_then(|f| f.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| cwd.to_path_buf());
    let mut seen = snapshot(&root);
    let mut runs = 0;
    loop {
        match compile_once(cwd, flag, jobs) {
            Ok(line) => println!("{line}"),
            Err(e) => println!("abl: {e}"),
        }
        runs += 1;
        if cycles.is_some_and(|c| runs >= c) {
            return Ok(0);
        }
        println!(
            "Watching {} for changes (Ctrl+C to stop)...",
            root.display()
        );
        // Wait for a change, then for writes to settle.
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let now = snapshot(&root);
            if now != seen {
                std::thread::sleep(Duration::from_millis(200));
                seen = snapshot(&root);
                break;
            }
        }
    }
}

fn compile_once(cwd: &Path, flag: Option<&str>, jobs: usize) -> Result<String, String> {
    let project = Project::open(cwd)?;
    let env = Env::new(project.loaded.as_ref(), flag, cwd)?;
    let units: Vec<PathBuf> = project
        .files
        .iter()
        .filter(|p| compile::is_unit(p))
        .cloned()
        .collect();
    let plan = compile::plan(&project, env, &units, false, None);
    if plan.todo.is_empty() {
        return Ok(format!("Up to date ({} files)", units.len()));
    }
    let names: Vec<String> = plan.todo.iter().map(|t| project.show(&t.0, cwd)).collect();
    let outcome = compile::execute(&plan, false, jobs)?;
    for m in &outcome.messages {
        println!(
            "{}:{}:{}: {}: {}",
            compile::message_file(&project, &plan, m, cwd),
            m.line.max(1),
            m.column.max(1),
            if m.error { "error" } else { "warning" },
            m.text
        );
    }
    let list = if names.len() <= 3 {
        names.join(", ")
    } else {
        format!("{}, ... ({} files)", names[..3].join(", "), names.len())
    };
    Ok(if outcome.failed.is_empty() {
        format!("Compiled {list}")
    } else {
        format!("Compiled {list}: {} failed", outcome.failed.len())
    })
}

/// `abl dev fake-dlc DIR`: makes a fake OpenEdge install for trying abl
/// without OpenEdge, using the fake `_progres` built next to abl.
/// The database tools the fake install has, besides `_progres`.
const FAKE_TOOLS: [&str; 8] = [
    "prodb", "proserve", "proshut", "proutil", "probkup", "prorest", "prodel", "prolib",
];

pub fn fake_dlc(dir: &Path, release: &str) -> Result<u8, String> {
    let exe_name = if cfg!(windows) {
        "fake-progres.exe"
    } else {
        "fake-progres"
    };
    let here = std::env::current_exe().map_err(|e| e.to_string())?;
    let fake = here.with_file_name(exe_name);
    if !fake.is_file() {
        return Err(format!(
            "{} not found; build it with `cargo build --bin fake-progres`",
            fake.display()
        ));
    }
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let target = config::progres(dir);
    fs::copy(&fake, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    // The same binary plays the database tools, chosen by its file name.
    for tool in FAKE_TOOLS {
        let t = bin.join(format!("{tool}{}", std::env::consts::EXE_SUFFIX));
        fs::copy(&fake, &t).map_err(|e| format!("{}: {e}", t.display()))?;
    }
    fs::write(
        dir.join("version"),
        format!("OpenEdge Release {release} as of (fake install made by abl)\n"),
    )
    .map_err(|e| format!("{}: {e}", dir.display()))?;
    let version: String = release.split('.').take(2).collect::<Vec<_>>().join(".");
    println!("Made a fake OE {release} install in {}", dir.display());
    println!("Register it with: abl oe add {version} {}", dir.display());
    Ok(0)
}

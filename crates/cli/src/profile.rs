//! `abl profile`: runs a procedure with the profiler on and summarizes the
//! profiler's output: the lines that took the most time, and time per
//! procedure.
//!
//! The output file has sections separated by lines holding only `.`:
//!
//! ```text
//! 1. description:  <version> <date> "<description>" <time> "<user>"
//! 2. modules:      <id> "<name>" "<listing>" <crc> ...
//! 3. call graph:   <caller id> <caller line> <callee id> <calls>
//! 4. line summary: <module id> <line> <executions> <actual time> <cumulative time>
//! ```
//!
//! A module name is a file (`src/a.p`), or an internal procedure, function
//! or method followed by its file (`calc src/a.p`). Later sections (tracing,
//! coverage) are ignored.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use abl_oe::config;
use abl_oe::session::{self, Env};

#[derive(Debug, Default)]
pub struct Profile {
    pub modules: HashMap<u32, String>,
    /// (module id, line, executions, actual seconds)
    pub lines: Vec<(u32, u32, u64, f64)>,
}

/// The first double-quoted string in `s`.
fn first_quoted(s: &str) -> Option<&str> {
    let a = s.find('"')?;
    let b = s[a + 1..].find('"')?;
    Some(&s[a + 1..a + 1 + b])
}

pub fn parse(text: &str) -> Result<Profile, String> {
    let mut sections: Vec<Vec<&str>> = vec![Vec::new()];
    for line in text.lines() {
        if line.trim() == "." {
            sections.push(Vec::new());
        } else if !line.trim().is_empty() {
            sections.last_mut().unwrap().push(line);
        }
    }
    if sections.len() < 4 {
        return Err("the profiler output is incomplete (fewer than four sections)".into());
    }
    let mut p = Profile::default();
    for l in &sections[1] {
        let id = l.split_whitespace().next().and_then(|s| s.parse().ok());
        if let (Some(id), Some(name)) = (id, first_quoted(l)) {
            p.modules.insert(id, name.to_string());
        }
    }
    for l in &sections[3] {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        let (Ok(m), Ok(line), Ok(n), Ok(t)) = (
            f[0].parse::<u32>(),
            f[1].parse::<u32>(),
            f[2].parse::<u64>(),
            f[3].parse::<f64>(),
        ) else {
            continue;
        };
        p.lines.push((m, line, n, t));
    }
    Ok(p)
}

/// The file part of a module name: its last word.
fn module_file(name: &str) -> &str {
    name.rsplit(' ').next().unwrap_or(name)
}

fn source_line(env: &Env, file: &str, line: u32) -> String {
    let p = Path::new(file);
    let found = if p.is_absolute() {
        Some(p.to_path_buf())
    } else {
        env.propath.iter().map(|d| d.join(p)).find(|c| c.is_file())
    };
    let text = found
        .and_then(|f| fs::read(f).ok())
        .map(|b| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_default();
    let l = text
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .unwrap_or("")
        .trim();
    if l.chars().count() > 60 {
        format!("{}...", l.chars().take(57).collect::<String>())
    } else {
        l.to_string()
    }
}

pub fn report(p: &Profile, env: &Env, top: usize) -> String {
    let total: f64 = p.lines.iter().map(|l| l.3).sum();
    let pct = |t: f64| if total > 0.0 { t * 100.0 / total } else { 0.0 };
    let name = |m: u32| p.modules.get(&m).map_or("?", String::as_str);
    let mut out = String::new();

    // Line 0 is the module's own overhead, not a source line.
    let mut lines: Vec<&(u32, u32, u64, f64)> = p.lines.iter().filter(|l| l.1 > 0).collect();
    lines.sort_by(|a, b| b.3.total_cmp(&a.3).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
    out.push_str("Lines by time\n      time      %    count  where\n");
    for (m, line, n, t) in lines.iter().take(top).copied() {
        let file = module_file(name(*m));
        let place = format!("{file}:{line}");
        out.push_str(&format!(
            "{t:10.4} {:5.1}% {n:8}  {place:<28} {}\n",
            pct(*t),
            source_line(env, file, *line)
        ));
    }

    let mut per: HashMap<u32, f64> = HashMap::new();
    for l in &p.lines {
        *per.entry(l.0).or_default() += l.3;
    }
    let mut per: Vec<(u32, f64)> = per.into_iter().collect();
    per.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    out.push_str("\nBy procedure\n      time      %  name\n");
    for (m, t) in per.iter().take(top) {
        out.push_str(&format!("{t:10.4} {:5.1}%  {}\n", pct(*t), name(*m)));
    }
    out
}

pub fn run(
    cwd: &Path,
    file: Option<&Path>,
    args: &[String],
    top: usize,
    save: Option<&Path>,
    flag: Option<&str>,
) -> Result<u8, String> {
    let loaded = config::load_project(cwd)?;
    let env = Env::new(loaded.as_ref(), flag, cwd)?;
    let entry = loaded
        .as_ref()
        .and_then(|l| l.project.entry.as_ref().map(|e| l.path(e)));
    let target = match (file, entry) {
        (Some(f), _) => crate::run::target_name(&env, cwd, f),
        (None, Some(e)) => crate::run::target_name(&env, cwd, &e),
        (None, None) => {
            return Err(
                "no procedure given and no `entry` in abl.toml: `abl profile <file>`".into(),
            )
        }
    };
    let out = match save {
        Some(s) if s.is_absolute() => s.to_path_buf(),
        Some(s) => cwd.join(s),
        None => std::env::temp_dir().join(format!("abl-profile-{}.prof", std::process::id())),
    };
    let _ = fs::remove_file(&out);
    let param = (!args.is_empty()).then(|| args.join(" "));
    let code = session::profile(&env, &target, param.as_deref(), &out)?;
    let text = fs::read_to_string(&out);
    if save.is_none() {
        let _ = fs::remove_file(&out);
    }
    let text = text.map_err(|_| "the profiler wrote no data".to_string())?;
    let p = parse(&text)?;
    let total: f64 = p.lines.iter().map(|l| l.3).sum();
    let n = p.lines.iter().filter(|l| l.1 > 0).count();
    println!(
        "\nProfiled {target}: {total:.4} s over {n} line{}{}\n",
        if n == 1 { "" } else { "s" },
        if code == 0 {
            String::new()
        } else {
            format!(" (ended with exit code {code})")
        }
    );
    print!("{}", report(&p, &env, top));
    if let Some(s) = save {
        println!("\nProfiler data saved in {}", s.display());
    }
    Ok(u8::try_from(code).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "1 11/18/2026 \"abl profile a.p\" 10:00:00 \"me\"\n.\n\
        1 \"a.p\" \"\" 123\n2 \"calc a.p\" \"\" 0\n.\n\
        0 0 1 1\n1 4 2 3\n.\n\
        1 0 1 0.0001 0.5\n1 3 1 0.2000 0.2\n1 4 3 0.0500 0.3\n2 8 3 0.2500 0.25\n.\n\
        .\n";

    #[test]
    fn parses_sections() {
        let p = parse(SAMPLE).unwrap();
        assert_eq!(p.modules[&2], "calc a.p");
        assert_eq!(p.lines.len(), 4);
        assert_eq!(p.lines[3], (2, 8, 3, 0.25));
        assert!(parse("1 x\n.\n").is_err());
        assert_eq!(module_file("calc src/a.p"), "src/a.p");
    }
}

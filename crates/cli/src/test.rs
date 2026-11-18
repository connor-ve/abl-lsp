//! `abl test`: finds unit tests by their annotations (ABLUnit style, see
//! `abl_core::unittest`), runs them in one OpenEdge session and reports each
//! outcome as text, JSON, JUnit XML or GitHub annotations.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use abl_core::deps::Graph;
use abl_core::unittest::{self, TestKind};
use abl_core::workspace::{is_abl_file, key};
use abl_oe::db::unescape;
use abl_oe::session::{self, Env, TestSuite};
use serde_json::json;

use crate::lint::gh_escape;
use crate::source::{self, Project};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Text,
    Json,
    Junit,
    Github,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    Error,
    Skip,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Error => "error",
            Status::Skip => "skip",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Case {
    pub file: PathBuf,
    /// Class type name, or the procedure's PROPATH name.
    pub suite: String,
    pub name: String,
    /// Line of the routine (1-based).
    pub decl_line: usize,
    pub status: Status,
    pub ms: u64,
    pub message: String,
    pub stack: String,
}

impl Case {
    /// Where a failure happened: the first stack entry in the test's own file,
    /// else the routine itself.
    pub fn location(&self, file_name: &str) -> usize {
        for l in self.stack.lines() {
            let (Some(at), Some(open)) = (l.find(" at line "), l.rfind('(')) else {
                continue;
            };
            let file = l[open + 1..].trim_end().trim_end_matches(')');
            if file.replace('\\', "/").eq_ignore_ascii_case(file_name) {
                let n: String = l[at + 9..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                if let Ok(n) = n.parse() {
                    return n;
                }
            }
        }
        self.decl_line
    }
}

pub struct Options {
    pub format: Format,
    pub filter: Option<String>,
    pub changed: bool,
    pub verbose: bool,
}

/// Files changed in git: modified, staged or untracked, under `root`.
fn git_changed(root: &Path) -> Result<Vec<PathBuf>, String> {
    let run = |args: &[&str]| -> Result<String, String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .map_err(|e| format!("--changed needs git: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "--changed needs a git repository: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    };
    // git names files from the repository's top folder, which it may spell
    // differently from `root` (a long name for a Windows 8.3 short one, a
    // junction's target). So take the paths relative to `root` (the prefix is
    // where root sits in the repository) and join them onto root as given.
    let prefix = run(&["rev-parse", "--show-prefix"])?.trim().to_string();
    let status = run(&["status", "--porcelain", "--untracked-files=all"])?;
    Ok(status
        .lines()
        .filter(|l| l.len() > 3)
        .filter_map(|l| {
            let p = &l[3..];
            // A rename is "old -> new".
            let p = p.rsplit(" -> ").next().unwrap_or(p).trim_matches('"');
            p.strip_prefix(prefix.as_str()).map(|rel| root.join(rel))
        })
        .collect())
}

/// The changed files and everything that depends on them, by any edge (RUN too).
fn affected(project: &Project, changed: &[PathBuf]) -> BTreeSet<String> {
    let graph = Graph::build(&project.ws, &project.files);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<PathBuf> = changed.iter().cloned().collect();
    while let Some(p) = queue.pop_front() {
        if !seen.insert(key(&p)) {
            continue;
        }
        for d in graph.dependents(&p) {
            queue.push_back(d.path);
        }
    }
    seen
}

fn matches_filter(filter: Option<&str>, suite: &str, name: &str) -> bool {
    filter.is_none_or(|f| {
        let f = f.to_lowercase();
        format!("{suite}:{name}").to_lowercase().contains(&f)
    })
}

pub fn run(
    cwd: &Path,
    paths: &[PathBuf],
    flag: Option<&str>,
    opts: &Options,
) -> Result<u8, String> {
    let project = Project::open(cwd)?;
    let env = Env::new(project.loaded.as_ref(), flag, cwd)?;
    let mut files: Vec<PathBuf> = project
        .select(paths, cwd)?
        .into_iter()
        .filter(|p| {
            is_abl_file(p)
                && p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("cls") || x.eq_ignore_ascii_case("p"))
        })
        .collect();
    if opts.changed {
        let root = project
            .loaded
            .as_ref()
            .map_or(cwd.to_path_buf(), |l| l.root.clone());
        let hit = affected(&project, &git_changed(&root)?);
        files.retain(|f| hit.contains(&key(f)));
    }

    // Discover.
    let mut suites: Vec<TestSuite> = Vec::new();
    let mut cases: Vec<Case> = Vec::new();
    let mut files_of: Vec<(PathBuf, String)> = Vec::new(); // per suite: file, PROPATH name
    for f in &files {
        let Ok(src) = source::read(f) else { continue };
        let ana = project.analyze(f, src.text);
        let Some(tf) = unittest::find(&ana) else {
            continue;
        };
        let pname = env.compile_name(f);
        let suite_name = match tf.kind {
            TestKind::Class => ana
                .class_name
                .clone()
                .unwrap_or_else(|| pname.trim_end_matches(".cls").replace('/', ".")),
            TestKind::Procedure => pname.clone(),
        };
        let mut suite = TestSuite {
            id: suites.len().to_string(),
            name: suite_name.clone(),
            is_class: tf.kind == TestKind::Class,
            before: tf.before,
            after: tf.after,
            setup: tf.setup,
            teardown: tf.teardown,
            tests: Vec::new(),
        };
        for t in tf.tests {
            if !matches_filter(opts.filter.as_deref(), &suite_name, &t.name) {
                continue;
            }
            let id = cases.len().to_string();
            cases.push(Case {
                file: f.clone(),
                suite: suite_name.clone(),
                name: t.name.clone(),
                decl_line: ana.lines.position(&ana.text, t.start).line as usize + 1,
                status: if t.ignored {
                    Status::Skip
                } else {
                    Status::Error
                },
                ms: 0,
                message: if t.ignored {
                    String::new()
                } else {
                    "not run: the session ended before this test (QUIT, or OpenEdge stopped)".into()
                },
                stack: String::new(),
            });
            if !t.ignored {
                suite
                    .tests
                    .push((id, t.name, t.expected.unwrap_or_default()));
            }
        }
        if !suite.tests.is_empty() || cases.iter().any(|c| c.suite == suite_name) {
            files_of.push((f.clone(), pname));
            suites.push(suite);
        }
    }
    if cases.is_empty() {
        if opts.format == Format::Text {
            println!("no tests found");
        }
        return Ok(0);
    }

    // Run.
    let started = Instant::now();
    let runnable: Vec<TestSuite> = suites
        .iter()
        .filter(|s| !s.tests.is_empty())
        .cloned()
        .collect();
    let mut suite_errors: Vec<(String, String)> = Vec::new();
    let mut output = String::new();
    if !runnable.is_empty() {
        let outcome = session::test(&env, &runnable)?;
        output = outcome.output;
        for l in outcome.results.lines() {
            let f: Vec<&str> = l.split('\t').collect();
            match f.first().copied() {
                Some("case") if f.len() >= 5 => {
                    let Some(c) = f[1].parse::<usize>().ok().and_then(|i| cases.get_mut(i)) else {
                        continue;
                    };
                    c.status = match f[2] {
                        "pass" => Status::Pass,
                        "fail" => Status::Fail,
                        _ => Status::Error,
                    };
                    c.ms = f[3].trim().parse().unwrap_or(0);
                    c.message = unescape(f[4]);
                    c.stack = f.get(5).map(|s| unescape(s)).unwrap_or_default();
                }
                Some("suite") if f.len() >= 3 => {
                    let name = f[1]
                        .parse::<usize>()
                        .ok()
                        .and_then(|i| runnable.iter().find(|s| s.id == i.to_string()))
                        .map_or(String::new(), |s| s.name.clone());
                    suite_errors.push((name, unescape(f[2])));
                }
                _ => {}
            }
        }
    }
    let secs = started.elapsed().as_secs_f64();
    let names: Vec<String> = cases
        .iter()
        .map(|c| {
            files_of
                .iter()
                .find(|(f, _)| f == &c.file)
                .map_or(String::new(), |(_, n)| n.clone())
        })
        .collect();

    let count = |s: Status| cases.iter().filter(|c| c.status == s).count();
    let (pass, fail, error, skip) = (
        count(Status::Pass),
        count(Status::Fail),
        count(Status::Error) + suite_errors.len(),
        count(Status::Skip),
    );
    match opts.format {
        Format::Text => print_text(
            &project,
            cwd,
            &cases,
            &names,
            &suite_errors,
            &output,
            opts,
            secs,
        ),
        Format::Json => {
            let tests: Vec<_> = cases
                .iter()
                .zip(&names)
                .map(|(c, n)| {
                    json!({
                        "file": project.show(&c.file, cwd),
                        "suite": c.suite,
                        "name": c.name,
                        "status": c.status.word(),
                        "ms": c.ms,
                        "line": c.location(n),
                        "message": c.message,
                        "stack": c.stack,
                    })
                })
                .collect();
            let errors: Vec<_> = suite_errors
                .iter()
                .map(|(s, m)| json!({"suite": s, "message": m}))
                .collect();
            let doc = json!({
                "tests": tests,
                "suiteErrors": errors,
                "summary": {"total": cases.len(), "passed": pass, "failed": fail, "errors": error, "skipped": skip, "seconds": secs},
                "output": output,
            });
            println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
        }
        Format::Junit => print!("{}", junit(&cases, &suite_errors, &output, secs)),
        Format::Github => {
            for (c, n) in cases.iter().zip(&names) {
                if matches!(c.status, Status::Fail | Status::Error) {
                    println!(
                        "::error file={},line={},title={}::{}",
                        gh_escape(&project.show(&c.file, cwd), true),
                        c.location(n),
                        gh_escape(&format!("{} {}", c.status.word(), c.name), true),
                        gh_escape(&c.message, false)
                    );
                }
            }
            for (s, m) in &suite_errors {
                println!(
                    "::error title={}::{}",
                    gh_escape(s, true),
                    gh_escape(m, false)
                );
            }
            println!(
                "{} tests: {pass} passed, {fail} failed, {error} {}, {skip} skipped",
                cases.len(),
                if error == 1 { "error" } else { "errors" }
            );
        }
    }
    Ok(u8::from(fail + error > 0))
}

#[allow(clippy::too_many_arguments)]
fn print_text(
    project: &Project,
    cwd: &Path,
    cases: &[Case],
    names: &[String],
    suite_errors: &[(String, String)],
    output: &str,
    opts: &Options,
    secs: f64,
) {
    let mut last: Option<&Path> = None;
    for (c, n) in cases.iter().zip(names) {
        if last != Some(c.file.as_path()) {
            if last.is_some() {
                println!();
            }
            println!("{}", project.show(&c.file, cwd));
            last = Some(&c.file);
        }
        let word = match c.status {
            Status::Pass => "pass ",
            Status::Fail => "FAIL ",
            Status::Error => "ERROR",
            Status::Skip => "skip ",
        };
        let time = if c.status == Status::Skip {
            String::new()
        } else {
            format!(" ({} ms)", c.ms)
        };
        println!("  {word} {}{time}", c.name);
        if matches!(c.status, Status::Fail | Status::Error) {
            for l in c.message.lines() {
                println!("        {l}");
            }
            println!(
                "        at {}:{}",
                project.show(&c.file, cwd),
                c.location(n)
            );
        }
    }
    for (s, m) in suite_errors {
        println!("\nERROR in {s}: {m}");
    }
    let count = |s: Status| cases.iter().filter(|c| c.status == s).count();
    let (fail, error) = (
        count(Status::Fail),
        count(Status::Error) + suite_errors.len(),
    );
    if !output.trim().is_empty() && (opts.verbose || fail + error > 0) {
        println!("\nSession output:");
        for l in output.lines() {
            println!("  {l}");
        }
    }
    let mut parts = vec![format!("{} passed", count(Status::Pass))];
    let errors = if error == 1 { "error" } else { "errors" };
    for (n, w) in [
        (fail, "failed"),
        (error, errors),
        (count(Status::Skip), "skipped"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {w}"));
        }
    }
    println!(
        "\n{} test{}: {} ({secs:.2} s)",
        cases.len(),
        if cases.len() == 1 { "" } else { "s" },
        parts.join(", ")
    );
}

fn xml(s: &str) -> String {
    s.chars()
        .filter(|&c| c == '\t' || c == '\n' || c == '\r' || c >= ' ')
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// JUnit XML, one `<testsuite>` per test file.
pub fn junit(cases: &[Case], suite_errors: &[(String, String)], output: &str, secs: f64) -> String {
    let mut suites: Vec<&str> = Vec::new();
    for c in cases {
        if !suites.contains(&c.suite.as_str()) {
            suites.push(&c.suite);
        }
    }
    let n = |list: &[&Case], s: Status| list.iter().filter(|c| c.status == s).count();
    let all: Vec<&Case> = cases.iter().collect();
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites name=\"abl test\" tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\" time=\"{secs:.3}\">\n",
        cases.len(),
        n(&all, Status::Fail),
        n(&all, Status::Error) + suite_errors.len(),
        n(&all, Status::Skip)
    );
    for s in suites {
        let list: Vec<&Case> = cases.iter().filter(|c| c.suite == s).collect();
        let extra = suite_errors.iter().filter(|(x, _)| x == s).count();
        let time: u64 = list.iter().map(|c| c.ms).sum();
        out.push_str(&format!(
            "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\" time=\"{:.3}\">\n",
            xml(s),
            list.len(),
            n(&list, Status::Fail),
            n(&list, Status::Error) + extra,
            n(&list, Status::Skip),
            time as f64 / 1000.0
        ));
        for c in &list {
            out.push_str(&format!(
                "    <testcase name=\"{}\" classname=\"{}\" time=\"{:.3}\"",
                xml(&c.name),
                xml(s),
                c.ms as f64 / 1000.0
            ));
            let body = |tag: &str| {
                format!(
                    ">\n      <{tag} message=\"{}\">{}</{tag}>\n    </testcase>\n",
                    xml(c.message.lines().next().unwrap_or("")),
                    xml(format!("{}\n{}", c.message, c.stack).trim())
                )
            };
            out.push_str(&match c.status {
                Status::Pass => "/>\n".to_string(),
                Status::Skip => ">\n      <skipped/>\n    </testcase>\n".to_string(),
                Status::Fail => body("failure"),
                Status::Error => body("error"),
            });
        }
        for (_, m) in suite_errors.iter().filter(|(x, _)| x == s) {
            out.push_str(&format!(
                "    <testcase name=\"@After\" classname=\"{}\" time=\"0\">\n      <error message=\"{}\"/>\n    </testcase>\n",
                xml(s),
                xml(m)
            ));
        }
        out.push_str("  </testsuite>\n");
    }
    if !output.trim().is_empty() {
        out.push_str(&format!("  <system-out>{}</system-out>\n", xml(output)));
    }
    out.push_str("</testsuites>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(status: Status, stack: &str) -> Case {
        Case {
            file: PathBuf::from("t/CalcTest.cls"),
            suite: "t.CalcTest".into(),
            name: "Adds".into(),
            decl_line: 7,
            status,
            ms: 12,
            message: "Expected: 2 but was: 3".into(),
            stack: stack.into(),
        }
    }

    #[test]
    fn failure_line_comes_from_the_stack() {
        let c = case(
            Status::Fail,
            "Equals OpenEdge.Core.Assert at line 50  (OpenEdge/Core/Assert.cls)\nAdds t.CalcTest at line 9  (t/CalcTest.cls)",
        );
        assert_eq!(c.location("t/CalcTest.cls"), 9);
        assert_eq!(case(Status::Fail, "").location("t/CalcTest.cls"), 7);
    }

    #[test]
    fn junit_xml() {
        let x = junit(
            &[case(Status::Fail, "s <1>"), case(Status::Pass, "")],
            &[],
            "",
            0.5,
        );
        assert!(
            x.contains("<testsuites name=\"abl test\" tests=\"2\" failures=\"1\""),
            "{x}"
        );
        assert!(x.contains("<failure message=\"Expected: 2 but was: 3\">Expected: 2 but was: 3\ns &lt;1&gt;</failure>"), "{x}");
        assert!(x.contains("time=\"0.012\"/>"), "{x}");
    }
}

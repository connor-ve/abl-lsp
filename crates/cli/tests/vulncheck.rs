//! End-to-end tests for `abl vulncheck`: OE release, package and code rules,
//! ignores, formats, init and fetch.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Proj {
    dir: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

const ADVISORIES: &str = r#"
[[advisory]]
id = "T-OE-1"
title = "Old OpenEdge"
severity = "critical"
fix = "Upgrade."
[advisory.oe]
affected = ["11.7.0 - 11.7.15"]

[[advisory]]
id = "T-OE-2"
title = "Newer OpenEdge only"
severity = "high"
[advisory.oe]
affected = [">= 12.0"]

[[advisory]]
id = "T-PKG-1"
title = "Old util"
severity = "high"
[advisory.package]
name = "util"
affected = ["< 1.4"]

[[advisory]]
id = "T-CODE-1"
title = "Computed shell command"
severity = "medium"
fix = "Use a fixed command."
[advisory.code]
statement = "OS-COMMAND"
contains = ["VALUE("]

[[advisory]]
id = "T-CODE-2"
title = "Computed RUN"
severity = "low"
[advisory.code]
statement = "RUN"
contains = ["VALUE("]
"#;

impl Proj {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("abl-vuln-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("proj/src")).unwrap();
        let p = Proj { dir };
        let dlc = p.dir.join("dlc");
        p.ok(&["dev", "fake-dlc", dlc.to_str().unwrap()]);
        fs::write(
            dlc.join("version"),
            "OpenEdge Release 11.7.4 as of (fake)\n",
        )
        .unwrap();
        p.ok(&["oe", "add", "11.7", dlc.to_str().unwrap()]);
        p.write(
            "abl.toml",
            "oe = \"11.7\"\npropath = [\"src\"]\n\n[vulncheck]\ndb = \"security\"\n",
        );
        p.write("security/test.toml", ADVISORIES);
        p.write(
            "src/tool.p",
            "OS-COMMAND SILENT VALUE(cCmd).\nOS-COMMAND \"dir\".\n/* vulncheck-ignore T-CODE-2 */\nRUN VALUE(cProc).\n",
        );
        p
    }

    fn root(&self) -> PathBuf {
        self.dir.join("proj")
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_abl"))
            .args(args)
            .current_dir(self.root())
            .env("ABL_HOME", self.dir.join("home"))
            .env_remove("ABL_OE")
            .stdin(Stdio::null())
            .output()
            .expect("run abl");
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, out) = self.run(args);
        assert_eq!(code, 0, "abl {args:?}:\n{out}");
        out
    }

    fn fails(&self, args: &[&str]) -> String {
        let (code, out) = self.run(args);
        assert_eq!(code, 1, "abl {args:?} should fail:\n{out}");
        out
    }

    /// A package "util" tagged v1.2 with a computed RUN in it, installed.
    fn install_util(&self) {
        let repo = self.dir.join("util");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("u.p"), "RUN VALUE(cWhat).\n").unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "c"]);
        git(&repo, &["tag", "v1.2"]);
        self.ok(&["i", &format!("{}@v1.2", repo.display())]);
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn finds_release_package_and_code() {
    let p = Proj::new("all");
    p.install_util();
    let out = p.fails(&["vulncheck"]);
    for want in [
        "CRITICAL T-OE-1  Old OpenEdge\n         OpenEdge 11.7.4\n         fix: Upgrade.",
        "HIGH     T-PKG-1  Old util\n         package util v1.2 (",
        "MEDIUM   T-CODE-1  Computed shell command\n         src/tool.p:1  OS-COMMAND SILENT VALUE(cCmd).",
        "LOW      T-CODE-2  Computed RUN\n         vendor/util/u.p:1  RUN VALUE(cWhat).",
        "4 findings: 1 critical, 1 high, 1 medium, 1 low (5 advisories in 1 file,",
    ] {
        assert!(out.replace('\\', "/").contains(want), "{want:?} missing in:\n{out}");
    }
    assert!(!out.contains("T-OE-2"), "{out}");
    assert!(!out.contains("tool.p:4"), "ignored by comment: {out}");

    // Severity threshold, formats, and ignores from abl.toml.
    assert!(p
        .fails(&["vulncheck", "--fail-on", "critical"])
        .contains("4 findings"));
    let json = p.fails(&["vulncheck", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["findings"].as_array().unwrap().len(), 4);
    assert_eq!(v["findings"][0]["id"], "T-OE-1");
    let gh = p.fails(&["vulncheck", "--format", "github"]);
    assert!(
        gh.contains("::warning file=src/tool.p,line=1,title=T-CODE-1 Computed shell command::"),
        "{gh}"
    );
    assert!(
        gh.contains("::error title=T-OE-1 Old OpenEdge::OpenEdge 11.7.4 Fix: Upgrade."),
        "{gh}"
    );

    p.write(
        "abl.toml",
        "oe = \"11.7\"\npropath = [\"src\"]\n\n[vulncheck]\ndb = \"security\"\nignore = [\"T-OE-1\", \"T-PKG-1\"]\n",
    );
    let out = p.fails(&["vulncheck"]);
    assert!(
        out.contains("2 findings: 1 medium, 1 low (3 advisories"),
        "{out}"
    );
    let out = p.ok(&["vulncheck", "--fail-on", "high"]);
    assert!(out.contains("2 findings"), "below the threshold: {out}");
}

#[test]
fn init_fetch_and_bad_files() {
    let p = Proj::new("init");
    p.write("abl.toml", "oe = \"11.7\"\npropath = [\"src\"]\n");
    let out = p.fails(&["vulncheck"]);
    assert!(
        out.contains("no advisories at") && out.contains("abl vulncheck init"),
        "{out}"
    );
    let out = p.ok(&["vulncheck", "init"]);
    assert!(out.contains("Wrote example advisories"), "{out}");
    assert!(p.dir.join("home/advisories/examples.toml").is_file());
    let out = p.fails(&["vulncheck"]);
    assert!(
        out.contains("EXAMPLE-CODE-1") && out.contains("src/tool.p:1"),
        "{out}"
    );
    assert!(p.fails(&["vulncheck", "init"]).contains("exists already"));

    // fetch: the default folder isn't a clone and has files.
    assert!(p
        .fails(&["vulncheck", "fetch"])
        .contains("isn't a git clone"));
    fs::remove_dir_all(p.dir.join("home/advisories")).unwrap();
    let repo = p.dir.join("advisories-repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("a.toml"), ADVISORIES).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "c"]);
    let out = p.ok(&["vulncheck", "fetch", repo.to_str().unwrap()]);
    assert!(out.contains("5 advisories in 1 file"), "{out}");
    fs::write(repo.join("b.toml"), "[[advisory]]\nid = \"T-NEW\"\ntitle = \"x\"\nseverity = \"low\"\n[advisory.code]\ncontains = [\"NOPE\"]\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "d"]);
    let out = p.ok(&["vulncheck", "fetch"]);
    assert!(
        out.contains("Updated") && out.contains("6 advisories in 2 files"),
        "{out}"
    );

    p.write(
        "bad.toml",
        "[[advisory]]\nid = \"X\"\ntitle = \"x\"\nseverity = \"low\"\n",
    );
    let out = p.fails(&["vulncheck", "--db", "bad.toml"]);
    assert!(out.contains("needs exactly one of"), "{out}");
}

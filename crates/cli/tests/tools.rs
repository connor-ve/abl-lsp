//! End-to-end tests for `abl build`, `profile`, `logs`, `repl` and
//! `compile --versions`, against the fake install from `abl dev fake-dlc`.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Proj {
    dir: PathBuf,
}

impl Proj {
    fn new(name: &str, toml: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("abl-tools-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("proj/src")).unwrap();
        let p = Proj { dir };
        for v in ["11.7", "12.8"] {
            let dlc = p.dir.join(format!("dlc{v}"));
            p.ok(&["dev", "fake-dlc", dlc.to_str().unwrap()]);
            fs::write(
                dlc.join("version"),
                format!("OpenEdge Release {v}.0 as of (fake install made by abl)\n"),
            )
            .unwrap();
            p.ok(&["oe", "add", v, dlc.to_str().unwrap()]);
        }
        p.write("abl.toml", toml);
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

    fn run_with(&self, args: &[&str], input: Option<&str>) -> (i32, String, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_abl"))
            .args(args)
            .current_dir(self.root())
            .env("ABL_HOME", self.dir.join("home"))
            .env_remove("ABL_OE")
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run abl");
        if let Some(i) = input {
            child.stdin.take().unwrap().write_all(i.as_bytes()).unwrap();
        }
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run_with(args, None);
        assert_eq!(code, 0, "abl {args:?}:\n{out}{err}");
        out
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const TOML: &str = "oe = \"12.8\"\npropath = [\"src\"]\nentry = \"src/main.p\"\n";

#[test]
fn build_packages_rcode() {
    let p = Proj::new("build", TOML);
    p.write("src/main.p", "MESSAGE \"hi\".\n");
    p.write("src/lib/util.p", "MESSAGE \"util\".\n");
    let out = p.ok(&["build"]);
    assert!(out.contains("Built build/proj.pl: 2 files"), "{out}");
    let lib = fs::read_to_string(p.root().join("build/proj.pl")).unwrap();
    assert!(
        lib.contains("lib/util.r\t") && lib.contains("main.r\t"),
        "{lib}"
    );

    // Rebuilt from scratch, and --output wins over the default.
    fs::remove_file(p.root().join("src/lib/util.p")).unwrap();
    let out = p.ok(&["build", "--output", "dist/app.pl"]);
    assert!(out.contains("Built dist/app.pl:"), "{out}");

    // A failed compile builds nothing.
    p.write("src/bad.p", "DO:\n");
    let (code, out, err) = p.run_with(&["build", "--output", "dist/x.pl"], None);
    assert_ne!(code, 0, "{out}{err}");
    assert!(!p.root().join("dist/x.pl").exists());
}

#[test]
fn compile_for_several_versions() {
    let p = Proj::new("versions", TOML);
    p.write("src/main.p", "MESSAGE \"hi\".\n");
    let out = p.ok(&["compile", "--versions", "11.7,12.8"]);
    assert!(
        out.contains("== OE 11.7 ==") && out.contains("== OE 12.8 =="),
        "{out}"
    );
    let (code, out, err) = p.run_with(&["compile", "--versions", "11.7,9.1"], None);
    assert_ne!(code, 0);
    assert!(
        !out.contains("== OE"),
        "nothing compiles when a version is missing: {out}{err}"
    );
}

#[test]
fn profile_shows_slow_lines() {
    let p = Proj::new("profile", TOML);
    p.write(
        "src/main.p",
        "MESSAGE \"start\".\nRUN src/slow.p.\nMESSAGE \"end\".\n",
    );
    p.write(
        "src/slow.p",
        "/* a slow one */\nMESSAGE \"slow\". /* fake-slow */\n",
    );
    let out = p.ok(&["profile", "--save", "run.prof"]);
    assert!(out.contains("start") && out.contains("slow"), "{out}");
    let first = out
        .lines()
        .skip_while(|l| !l.starts_with("Lines by time"))
        .nth(2)
        .unwrap_or("");
    assert!(
        first.contains("slow.p:2") && first.contains("MESSAGE \"slow\""),
        "{out}"
    );
    assert!(out.contains("By procedure"), "{out}");
    assert!(p.root().join("run.prof").is_file());
}

#[test]
fn logs_of_managed_databases() {
    let df = "ADD TABLE \"T\"\n\nADD FIELD \"F\" OF \"T\" AS integer\n  POSITION 2\n\n.\n";
    let p = Proj::new(
        "logs",
        &format!("{TOML}\n[db.shop]\nschema = \"db/shop.df\"\n"),
    );
    p.write("db/shop.df", df);
    assert!(p.ok(&["logs"]).contains("no log yet"));
    p.ok(&["db", "create"]);
    p.ok(&["db", "start"]);
    p.ok(&["db", "stop"]);
    let out = p.ok(&["logs"]);
    assert!(out.contains("(333)") && out.contains("(334)"), "{out}");
    let out = p.ok(&["logs", "--grep", "END", "-n", "5"]);
    assert_eq!(out.lines().count(), 1, "{out}");
    let out = p.ok(&["logs", "shop", "-n", "1"]);
    assert!(out.contains("session end"), "{out}");
    let (code, _, err) = p.run_with(&["logs", "nope"], None);
    assert_eq!(code, 1);
    assert!(err.contains("no database nope"), "{err}");
}

#[test]
fn repl_runs_entries_in_one_session() {
    let p = Proj::new("repl", TOML);
    let input = "\
MESSAGE \"one\".
DO:
  MESSAGE \"two\".
END.
:defs
DEFINE VARIABLE i AS INTEGER NO-UNDO.
:defs
RETURN ERROR \"bad\".
STOP.
:nope
= \"three\"
:quit
MESSAGE \"never\".
";
    let (code, out, err) = p.run_with(&["repl"], Some(input));
    assert_eq!(code, 0, "{out}{err}");
    let want = [
        "one",
        "two",
        "(no definitions)",
        "DEFINE NEW GLOBAL SHARED VARIABLE i AS INTEGER NO-UNDO.",
        "bad",
        "** fake error. (fake)",
        "(stopped)",
        "unknown command :nope",
        "three",
    ];
    let mut rest = out.as_str();
    for w in want {
        let at = rest
            .find(w)
            .unwrap_or_else(|| panic!("{w:?} missing or out of order in:\n{out}"));
        rest = &rest[at + w.len()..];
    }
    assert!(!out.contains("never"), "{out}");
}

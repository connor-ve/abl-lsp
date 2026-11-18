//! End-to-end tests for `[[procedures]]` (run before each session's work) and
//! `[extensions]` (more file extensions for ABL source).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Proj {
    dir: PathBuf,
}

const TOML: &str = r#"oe = "12.8"
propath = ["src"]

[[procedures]]
path = "init.p"

[[procedures]]
path = "lib.p"
mode = "persistent"

[[procedures]]
path = "sup.p"
mode = "super"

[extensions]
main = ["x"]
include = [".F"]
"#;

impl Proj {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("abl-startup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("proj/src")).unwrap();
        let p = Proj { dir };
        let dlc = p.dir.join("dlc");
        p.ok(&["dev", "fake-dlc", dlc.to_str().unwrap()]);
        p.ok(&["oe", "add", "12.8", dlc.to_str().unwrap()]);
        p.write("abl.toml", TOML);
        for f in ["init", "lib", "sup", "hello"] {
            p.write(&format!("src/{f}.p"), "MESSAGE \"hi\".\n");
        }
        p.write("src/a.x", "{b.f}\nMESSAGE i.\n");
        p.write("src/b.f", "DEFINE VARIABLE i AS INTEGER NO-UNDO.\n");
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
        assert_ne!(code, 0, "abl {args:?} should fail:\n{out}");
        out
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn startup_procedures_run_first() {
    let p = Proj::new("procs");
    let out = p.ok(&["run", "hello.p"]);
    assert!(
        out.contains(
            "fake startup: run init.p\nfake startup: persistent lib.p\nfake startup: super sup.p\nhi"
        ),
        "{out}"
    );
    let doc = p.ok(&["doctor"]);
    assert!(
        doc.contains(
            "[  ok  ] startup procedures: init.p (run), lib.p (persistent), sup.p (super)"
        ),
        "{doc}"
    );
    assert!(p
        .ok(&["env"])
        .contains("ABL_PROCEDURES=run:init.p persistent:lib.p super:sup.p"));

    // A missing one stops every kind of session, with the procedure named.
    p.write("abl.toml", &TOML.replace("\"sup.p\"", "\"nope.p\""));
    let want = "startup procedure nope.p failed: ** \"nope.p\" was not found. (293)";
    let out = p.fails(&["run", "hello.p"]);
    assert!(out.contains(want), "{out}");
    let out = p.fails(&["compile", "--force"]);
    assert!(out.contains(want), "{out}");
    let doc = p.fails(&["doctor"]);
    assert!(
        doc.contains("[ FAIL ] startup procedures not found on the PROPATH: nope.p"),
        "{doc}"
    );
}

#[test]
fn extra_extensions_are_abl_source() {
    let p = Proj::new("ext");
    let out = p.ok(&["compile"]);
    assert!(out.contains("Compiled 5 of 5 files"), "{out}");
    let deps = p.ok(&["deps", "src/a.x"]).replace('\\', "/");
    assert!(deps.contains("include    src/b.f  line 1"), "{deps}");
    // The include isn't compiled on its own, and nothing is reported in it.
    let lint = p.ok(&["lint"]);
    assert!(!lint.contains("b.f:"), "{lint}");
}

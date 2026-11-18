//! End-to-end tests for `abl test` against the fake install.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Proj {
    dir: PathBuf,
}

const TOML: &str = "oe = \"12.8\"\npropath = [\"src\", \"test\"]\n";

const CALC_TEST: &str = "\
USING OpenEdge.Core.Assert.

CLASS CalcTest:
    @Before.
    METHOD PUBLIC VOID BeforeAll():
        MESSAGE \"before all\".
    END METHOD.

    @Setup.
    METHOD PUBLIC VOID Each():
        MESSAGE \"setup\".
    END METHOD.

    @Test.
    METHOD PUBLIC VOID Adds():
        Assert:Equals(2, 2).
    END METHOD.

    @Test.
    METHOD PUBLIC VOID Subtracts():
        MESSAGE \"in subtracts\".
        Assert:Equals(1, 3).
    END METHOD.

    @Test (expected=\"Progress.Lang.AppError\").
    METHOD PUBLIC VOID Throws():
        RETURN ERROR \"wanted\".
    END METHOD.

    @Test.
    METHOD PUBLIC VOID Boom():
        UNDO, THROW NEW Progress.Lang.AppError(\"boom\").
    END METHOD.

    @Test.
    @Ignore.
    METHOD PUBLIC VOID Later():
    END METHOD.

    @After.
    METHOD PUBLIC VOID AfterAll():
        MESSAGE \"after all\".
    END METHOD.
END CLASS.
";

const PROC_TEST: &str = "\
@Test.
PROCEDURE testTrue:
    Assert:IsTrue(TRUE).
END PROCEDURE.

@Test.
PROCEDURE testFalse:
    Assert:IsTrue(FALSE).
END PROCEDURE.
";

impl Proj {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("abl-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("proj/src")).unwrap();
        let p = Proj { dir };
        let dlc = p.dir.join("dlc");
        p.ok(&["dev", "fake-dlc", dlc.to_str().unwrap()]);
        p.ok(&["oe", "add", "12.8", dlc.to_str().unwrap()]);
        p.write("abl.toml", TOML);
        p.write("src/calc.p", "MESSAGE \"calc\".\n");
        p.write("test/CalcTest.cls", CALC_TEST);
        p.write("test/proc_test.p", PROC_TEST);
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

    fn run(&self, args: &[&str]) -> (i32, String, String) {
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
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run(args);
        assert_eq!(code, 0, "abl {args:?}:\n{out}{err}");
        out
    }

    fn fails(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run(args);
        assert_eq!(code, 1, "abl {args:?} should fail:\n{out}{err}");
        out
    }

    fn git(&self, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(self.root())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn runs_class_and_procedure_tests() {
    let p = Proj::new("all");
    let out = p.fails(&["test"]);
    for want in [
        "test/CalcTest.cls",
        "  pass  Adds (",
        "  FAIL  Subtracts (",
        "        Expected: 1 but was: 3\n        at test/CalcTest.cls:22",
        "  pass  Throws (",
        "  ERROR Boom (",
        "        boom\n        at test/CalcTest.cls:32",
        "  skip  Later\n",
        "test/proc_test.p",
        "  pass  testTrue (",
        "  FAIL  testFalse (",
        "at test/proc_test.p:8",
        "Session output:",
        "7 tests: 3 passed, 2 failed, 1 error, 1 skipped",
    ] {
        assert!(out.contains(want), "{want:?} missing in:\n{out}");
    }
    // Lifecycle order: once before, setup before each test, once after.
    let seq: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| ["before all", "setup", "in subtracts", "after all"].contains(l))
        .collect();
    assert_eq!(
        seq,
        [
            "before all",
            "setup",
            "setup",
            "in subtracts",
            "setup",
            "setup",
            "after all"
        ],
        "{out}"
    );
    assert!(!out.contains("calc.p"), "{out}");
}

#[test]
fn filters_and_passing_runs() {
    let p = Proj::new("filter");
    let out = p.ok(&["test", "--filter", "calctest:adds"]);
    assert!(out.contains("1 test: 1 passed"), "{out}");
    assert!(
        !out.contains("Session output"),
        "quiet when everything passes: {out}"
    );
    let out = p.ok(&["test", "test/CalcTest.cls", "--filter", "adds", "-v"]);
    assert!(out.contains("Session output:\n  before all"), "{out}");
    let out = p.ok(&["test", "src"]);
    assert!(out.contains("no tests found"), "{out}");
}

#[test]
fn machine_formats() {
    let p = Proj::new("formats");
    let out = p.fails(&["test", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["summary"]["total"], 7);
    assert_eq!(v["summary"]["failed"], 2);
    let sub = v["tests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Subtracts")
        .unwrap();
    assert_eq!(sub["status"], "fail");
    assert_eq!(sub["line"], 22);

    let out = p.fails(&["test", "--format", "junit"]);
    assert!(out.starts_with("<?xml"), "{out}");
    assert!(
        out.contains(
            "<testsuite name=\"CalcTest\" tests=\"5\" failures=\"1\" errors=\"1\" skipped=\"1\""
        ),
        "{out}"
    );
    assert!(
        out.contains("<failure message=\"Expected: 1 but was: 3\">"),
        "{out}"
    );

    let out = p.fails(&["test", "--format", "github"]);
    assert!(
        out.contains(
            "::error file=test/CalcTest.cls,line=22,title=fail Subtracts::Expected: 1 but was: 3"
        ),
        "{out}"
    );
}

#[test]
fn broken_files_and_quit() {
    let p = Proj::new("broken");
    p.write(
        "test/BadTest.cls",
        "CLASS BadTest:\n    @Test.\n    METHOD PUBLIC VOID A():\n        DO:\n    END METHOD.\nEND CLASS.\n",
    );
    let out = p.fails(&["test", "test/BadTest.cls"]);
    assert!(
        out.contains("ERROR A (0 ms)\n        ** "),
        "a file that doesn't compile errors each test: {out}"
    );

    p.write(
        "test/QuitTest.cls",
        "CLASS QuitTest:\n    @Test.\n    METHOD PUBLIC VOID A():\n        QUIT.\n    END METHOD.\n    @Test.\n    METHOD PUBLIC VOID B():\n    END METHOD.\nEND CLASS.\n",
    );
    let out = p.fails(&["test", "test/QuitTest.cls"]);
    assert!(
        out.contains("ERROR B (0 ms)\n        not run: the session ended"),
        "{out}"
    );
}

#[test]
fn changed_runs_affected_tests() {
    let p = Proj::new("changed");
    p.write(
        "test/calc_test.p",
        "@Test.\nPROCEDURE testCalc:\n    RUN calc.p.\nEND PROCEDURE.\n",
    );
    p.git(&["init", "-q"]);
    p.git(&["add", "."]);
    p.git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "x",
    ]);
    let out = p.ok(&["test", "--changed"]);
    assert!(out.contains("no tests found"), "{out}");
    p.write("src/calc.p", "MESSAGE \"calc 2\".\n");
    let out = p.ok(&["test", "--changed"]);
    assert!(
        out.contains("calc_test.p") && out.contains("1 test: 1 passed"),
        "{out}"
    );
    p.write("test/proc_test.p", &PROC_TEST.replace("FALSE", "TRUE"));
    let out = p.ok(&["test", "--changed"]);
    assert!(out.contains("3 tests: 3 passed"), "{out}");
}

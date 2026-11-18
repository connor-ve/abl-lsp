//! End-to-end tests for the commands that start OpenEdge, against the fake
//! `_progres` (src/bin/fake-progres.rs) in a fake install.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

struct Proj {
    dir: PathBuf,
}

const TOML: &str = "oe = \"12.8\"\npropath = [\"src\"]\noutput = \"build/rcode\"\n";

impl Proj {
    /// A project with a fake OE 12.8 install registered in its own ABL_HOME.
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("abl-oe-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let dlc = dir.join("dlc");
        fs::create_dir_all(dlc.join("bin")).unwrap();
        fs::create_dir_all(dir.join("proj/src")).unwrap();
        let exe = if cfg!(windows) {
            "_progres.exe"
        } else {
            "_progres"
        };
        fs::copy(
            env!("CARGO_BIN_EXE_fake-progres"),
            dlc.join("bin").join(exe),
        )
        .unwrap();
        fs::write(dlc.join("version"), "OpenEdge Release 12.8.0 as of test\n").unwrap();
        let p = Proj { dir };
        p.ok(&["oe", "add", "12.8", dlc.to_str().unwrap()]);
        p.write("abl.toml", TOML);
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

    fn rcode(&self, rel: &str) -> PathBuf {
        self.root().join("build/rcode").join(rel)
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_abl"));
        c.args(args)
            .current_dir(self.root())
            .env("ABL_HOME", self.dir.join("home"))
            .env_remove("ABL_OE");
        c
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out: Output = self.cmd(args).output().expect("run abl");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap_or(-1), text)
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, text) = self.run(args);
        assert_eq!(code, 0, "abl {args:?}:\n{text}");
        text
    }

    fn code(&self, args: &[&str], expected: i32) -> String {
        let (code, text) = self.run(args);
        assert_eq!(code, expected, "abl {args:?}:\n{text}");
        text
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn compile_is_incremental() {
    let p = Proj::new("incr");
    p.write("src/a.p", "MESSAGE \"a\".\n");
    p.write("src/inc/x.i", "DEFINE VARIABLE iX AS INTEGER NO-UNDO.\n");
    p.write("src/b.p", "{inc/x.i}\nMESSAGE iX.\n");
    p.write("src/app/Thing.cls", "CLASS app.Thing:\nEND CLASS.\n");
    p.write(
        "src/c.p",
        "USING app.*.\nDEFINE VARIABLE o AS Thing NO-UNDO.\n",
    );

    let out = p.ok(&["compile"]);
    assert!(out.contains("Compiled 4 of 4 files with OE 12.8"), "{out}");
    for r in ["a.r", "b.r", "c.r", "app/Thing.r"] {
        assert!(p.rcode(r).is_file(), "{r} missing");
    }
    assert!(p.ok(&["status"]).contains("up to date (4 files)"));
    assert!(p
        .ok(&["compile"])
        .contains("Nothing to compile: 4 files up to date"));

    // An include changes: only its user recompiles.
    p.write(
        "src/inc/x.i",
        "DEFINE VARIABLE iX AS INTEGER NO-UNDO INITIAL 1.\n",
    );
    assert!(p.ok(&["status"]).contains("1 of 4 files need compiling"));
    let out = p.ok(&["compile"]);
    assert!(
        out.contains("Compiled 1 of 4 files with OE 12.8 (3 up to date)"),
        "{out}"
    );

    // A class changes: its users recompile too.
    p.write(
        "src/app/Thing.cls",
        "CLASS app.Thing:\n  /* changed */\nEND CLASS.\n",
    );
    assert!(p.ok(&["compile"]).contains("Compiled 2 of 4"));

    // Missing r-code is rebuilt.
    fs::remove_file(p.rcode("a.r")).unwrap();
    assert!(p.ok(&["compile"]).contains("Compiled 1 of 4"));

    // The environment changes: everything recompiles.
    p.write(
        "abl.toml",
        &format!("{TOML}startup = \"-cpinternal UTF-8\"\n"),
    );
    assert!(p.ok(&["compile"]).contains("Compiled 4 of 4"));
    assert!(p.ok(&["compile", "--force"]).contains("Compiled 4 of 4"));
    // Naming files compiles only those.
    assert!(p
        .ok(&["compile", "src/a.p", "--force"])
        .contains("Compiled 1 of 1"));
}

#[test]
fn compile_errors_and_formats() {
    let p = Proj::new("errors");
    p.write("src/good.p", "MESSAGE \"ok\".\n/* fake-warning */\n");
    p.write("src/bad.p", "DO WHILE TRUE:\n    MESSAGE \"x\".\n");
    let out = p.code(&["compile"], 1);
    assert!(out.contains("src/bad.p:1:1: error:"), "{out}");
    assert!(
        out.contains("src/good.p:2:1: warning: fake warning"),
        "{out}"
    );
    assert!(out.contains("2 files with OE 12.8, 1 failed"), "{out}");
    assert!(p.rcode("good.r").is_file());
    assert!(!p.rcode("bad.r").exists());
    // A failed unit is not recorded as compiled.
    assert!(p.code(&["compile"], 1).contains("Compiled 1 of 2"));

    let json: serde_json::Value =
        serde_json::from_str(&p.code(&["compile", "--format", "json"], 1)).unwrap();
    assert_eq!(json["failed"][0], "bad.p");
    assert_eq!(json["messages"][0]["severity"], "error");
    let gh = p.code(&["compile", "--format", "github"], 1);
    assert!(gh.contains("::error file=src/bad.p,line=1,col=1,"), "{gh}");

    p.write("src/bad.p", "MESSAGE \"fixed\".\n");
    assert!(p.ok(&["compile"]).contains("Compiled 1 of 2"));
}

#[test]
fn check_saves_nothing() {
    let p = Proj::new("check");
    p.write("src/a.p", "MESSAGE \"a\".\n");
    assert!(p.ok(&["compile", "--check"]).contains("Checked 1 of 1"));
    assert!(!p.root().join("build").exists());
    // Checking again checks again: nothing was recorded.
    assert!(p.ok(&["compile", "--check"]).contains("Checked 1 of 1"));
}

#[test]
fn parallel_sessions() {
    let p = Proj::new("jobs");
    for i in 0..12 {
        p.write(&format!("src/f{i:02}.p"), &format!("MESSAGE \"{i}\".\n"));
    }
    p.write("src/zz_bad.p", "DO:\n");
    let out = p.code(&["compile", "-j", "3"], 1);
    assert!(
        out.contains("Compiled 13 of 13 files with OE 12.8, 1 failed"),
        "{out}"
    );
    for i in 0..12 {
        assert!(p.rcode(&format!("f{i:02}.r")).is_file());
    }
}

#[test]
fn crashed_session_is_reported() {
    let p = Proj::new("crash");
    p.write("src/a.p", "MESSAGE \"a\".\n");
    p.write("src/b.p", "/* fake-crash */\n");
    let out = p.code(&["compile", "-j", "1"], 1);
    assert!(out.contains("stopped after 1 of 2 files"), "{out}");
    assert!(out.contains("crashed on b.p"), "{out}");
}

#[test]
fn no_openedge_is_a_clear_error() {
    let p = Proj::new("nooe");
    p.write("abl.toml", "oe = \"11.7\"\npropath = [\"src\"]\n");
    p.write("src/a.p", "MESSAGE \"a\".\n");
    let out = p.code(&["compile"], 1);
    assert!(
        out.contains("OE 11.7 is not installed here (known: 12.8)"),
        "{out}"
    );
    // status still works, without a build line.
    assert!(!p.ok(&[]).contains("build"));
}

#[test]
fn run_and_exec() {
    let p = Proj::new("run");
    p.write(
        "src/main.p",
        "MESSAGE \"main\".\nMESSAGE SESSION:PARAMETER.\nRUN util/helper.p.\n",
    );
    p.write("src/util/helper.p", "MESSAGE \"helper\".\n");
    p.write(
        "src/fail.p",
        "MESSAGE \"before\".\nRETURN ERROR \"it failed\".\n",
    );
    p.write("src/quits.p", "MESSAGE \"bye\".\nQUIT.\n");

    let out = p.ok(&["run", "src/main.p", "--", "a", "b"]);
    assert_eq!(out, "main\na b\nhelper\n");
    let out = p.code(&["run", "src/fail.p"], 1);
    assert!(out.contains("before\nit failed"), "{out}");
    assert_eq!(p.ok(&["run", "src/quits.p"]), "bye\n");
    // A PROPATH name works too; a missing one is an error.
    assert!(p.ok(&["run", "util/helper.p"]).contains("helper"));
    assert!(p.code(&["run", "nope.p"], 1).contains("was not found"));

    // Without a file, `entry` from abl.toml runs.
    assert!(p.code(&["run"], 1).contains("no `entry` in abl.toml"));
    p.write(
        "abl.toml",
        &format!("{TOML}entry = \"src/util/helper.p\"\n"),
    );
    assert!(p.ok(&["run"]).contains("helper"));

    assert_eq!(p.ok(&["exec", "MESSAGE \"from exec\""]), "from exec\n");
}

#[test]
fn watch_recompiles_on_change() {
    let p = Proj::new("watch");
    p.write("src/a.p", "MESSAGE \"a\".\n");
    p.write("src/b.p", "MESSAGE \"b\".\n");
    let mut child = p
        .cmd(&["watch", "--cycles", "2"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let wait_for = |needle: &str| -> Vec<String> {
        let mut seen = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(20)) {
                Ok(l) => {
                    let hit = l.contains(needle);
                    seen.push(l);
                    if hit {
                        return seen;
                    }
                }
                Err(_) => panic!("timed out waiting for {needle:?}; saw {seen:?}"),
            }
        }
    };
    let first = wait_for("Watching");
    assert!(
        first
            .iter()
            .any(|l| l.contains("Compiled src/a.p, src/b.p")),
        "{first:?}"
    );
    // Give the watcher a moment so the change lands after its snapshot.
    std::thread::sleep(Duration::from_millis(500));
    p.write("src/b.p", "MESSAGE \"b changed\".\n");
    let second = wait_for("Compiled");
    assert!(
        second.last().unwrap().contains("Compiled src/b.p"),
        "{second:?}"
    );
    let status = child.wait().unwrap();
    assert!(status.success());
}

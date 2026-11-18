//! End-to-end tests for the `abl` CLI's project and install commands. Each test
//! gets its own ABL_HOME, fake OpenEdge installs and project folder, so nothing
//! touches the real home folder.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("abl-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("home")).unwrap();
        fs::create_dir_all(dir.join("proj/src")).unwrap();
        Sandbox { dir }
    }

    /// A fake install: a version file and an empty `_progres`.
    fn install(&self, folder: &str, release: &str) -> PathBuf {
        let dlc = self.dir.join(folder);
        fs::create_dir_all(dlc.join("bin")).unwrap();
        fs::write(
            dlc.join("version"),
            format!("OpenEdge Release {release} as of today\n"),
        )
        .unwrap();
        let exe = if cfg!(windows) {
            "_progres.exe"
        } else {
            "_progres"
        };
        fs::write(dlc.join("bin").join(exe), "").unwrap();
        dlc
    }

    fn proj(&self) -> PathBuf {
        self.dir.join("proj")
    }

    fn abl(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_abl"))
            .args(args)
            .current_dir(self.proj())
            .env("ABL_HOME", self.dir.join("home"))
            .env_remove("ABL_OE")
            .output()
            .expect("run abl")
    }

    /// Runs abl, asserts the exit status, and returns stdout + stderr.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.abl(args);
        let text = text(&out);
        assert!(out.status.success(), "abl {args:?} failed:\n{text}");
        text
    }

    fn fails(&self, args: &[&str]) -> String {
        let out = self.abl(args);
        let text = text(&out);
        assert!(!out.status.success(), "abl {args:?} should fail:\n{text}");
        text
    }

    fn global_config(&self) -> String {
        fs::read_to_string(self.dir.join("home/config.toml")).unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn no_project_shows_help_and_status_fails() {
    let sb = Sandbox::new("noproj");
    assert!(sb.ok(&[]).contains("Usage: abl"));
    assert!(sb.fails(&["status"]).contains("abl init"));
}

#[test]
fn install_registry_and_version_precedence() {
    let sb = Sandbox::new("oe");
    let oe117 = sb.install("OpenEdge117", "11.7.4");
    let oe128 = sb.install("OpenEdge128", "12.8.1");

    // The first install becomes the global default.
    assert!(sb
        .ok(&["oe", "add", "11.7", s(&oe117)])
        .contains("now the global default"));
    // A version file that disagrees is refused unless forced.
    assert!(sb
        .fails(&["oe", "add", "12.8", s(&oe117)])
        .contains("not 12.8"));
    sb.ok(&["oe", "add", "12.8", s(&oe128)]);
    let cfg = sb.global_config();
    assert!(cfg.starts_with("default = \"11.7\"\n\n[oe]\n"), "{cfg}");

    sb.ok(&["init"]);
    let status = sb.ok(&[]);
    assert!(status.contains("11.7 (from abl.toml)"), "{status}");

    // Project pin beats the global default; ABL_OE beats the pin; --oe beats both.
    sb.ok(&["oe", "use", "12.8", "--project"]);
    assert!(sb.ok(&["status"]).contains("12.8 (from abl.toml)"));
    let env_out = Command::new(env!("CARGO_BIN_EXE_abl"))
        .current_dir(sb.proj())
        .env("ABL_HOME", sb.dir.join("home"))
        .env("ABL_OE", "11.7")
        .output()
        .unwrap();
    assert!(text(&env_out).contains("11.7 (from ABL_OE)"));
    let flag = sb.ok(&["--oe", "12.2", "status"]);
    assert!(
        flag.contains("12.2 (from --oe), not installed here"),
        "{flag}"
    );

    let list = sb.ok(&["oe", "list"]);
    assert!(list.contains("* 12.8"), "{list}");
    assert!(list.contains("(default)"), "{list}");

    // Removing the default clears it.
    assert!(sb
        .ok(&["oe", "remove", "11.7"])
        .contains("was the global default"));
    assert!(!sb.global_config().contains("default"));
    assert!(sb.fails(&["oe", "remove", "11.7"]).contains("known: 12.8"));
    assert!(sb.fails(&["oe", "use", "9.1"]).contains("not installed"));
}

#[test]
fn init_guesses_layout_and_doctor_checks_it() {
    let sb = Sandbox::new("doctor");
    let dlc = sb.install("OpenEdge128", "12.8.0");
    fs::create_dir_all(sb.proj().join("src/inc")).unwrap();
    fs::create_dir_all(sb.proj().join("db")).unwrap();
    fs::write(sb.proj().join("src/hello.p"), "MESSAGE \"hi\".\n").unwrap();
    fs::write(
        sb.proj().join("db/shop.df"),
        "ADD TABLE \"item\"\n  AREA \"Data\"\n\nADD FIELD \"name\" OF \"item\" AS character\n  FORMAT \"x(20)\"\n\n.\n",
    )
    .unwrap();

    // No OE configured yet: doctor fails and says why.
    sb.ok(&["init"]);
    assert!(sb.fails(&["doctor"]).contains("no OpenEdge version set"));
    assert!(sb.fails(&["init"]).contains("already exists"));

    sb.ok(&["oe", "add", "12.8", s(&dlc)]);
    sb.ok(&["--oe", "12.8", "init", "--force"]);
    let toml = fs::read_to_string(sb.proj().join("abl.toml")).unwrap();
    assert!(toml.contains("oe = \"12.8\""), "{toml}");
    assert!(toml.contains("propath = [\"src\", \"src/inc\"]"), "{toml}");
    assert!(
        toml.contains("[db.shop]\nschema = \"db/shop.df\""),
        "{toml}"
    );

    let doctor = sb.ok(&["doctor"]);
    assert!(doctor.contains("db shop: db/shop.df, 1 tables"), "{doctor}");
    assert!(doctor.contains("All good."), "{doctor}");

    let status = sb.ok(&[]);
    assert!(status.contains("sources  1 files"), "{status}");
    // Where the PROPATH is set, then its folders, absolute, in search order.
    let sep = std::path::MAIN_SEPARATOR;
    let file = sb.proj().join("abl.toml").display().to_string();
    let line = toml.lines().position(|l| l.starts_with("propath")).unwrap() + 1;
    assert!(
        status.contains(&format!("propath  set in {file}:{line}\n")),
        "{status}"
    );
    let src = sb.proj().join("src").display().to_string();
    assert!(status.contains(&format!("         {src}\n")), "{status}");
    assert!(
        status.contains(&format!("         {src}{sep}inc\n")),
        "{status}"
    );
    fs::write(
        sb.proj().join("abl.toml"),
        toml.replace("\"src/inc\"]", "\"src/inc\", \"lib\"]"),
    )
    .unwrap();
    let status = sb.ok(&[]);
    assert!(status.contains("lib  (folder not found)"), "{status}");
    fs::write(sb.proj().join("abl.toml"), &toml).unwrap();

    let env = sb.ok(&["env"]);
    let sep = if cfg!(windows) { ";" } else { ":" };
    let pp = env
        .lines()
        .find_map(|l| l.strip_prefix("PROPATH="))
        .unwrap();
    let entries: Vec<&str> = pp.split(sep).collect();
    assert_eq!(entries.len(), 2, "{env}");
    assert!(
        entries[1].ends_with(&format!("src{}inc", std::path::MAIN_SEPARATOR)),
        "{env}"
    );

    // A broken install is a failure, with the missing file named.
    fs::remove_file(dlc.join("bin").join(if cfg!(windows) {
        "_progres.exe"
    } else {
        "_progres"
    }))
    .unwrap();
    assert!(sb.fails(&["doctor"]).contains("missing"));
}

#[test]
fn bad_project_file_is_reported() {
    let sb = Sandbox::new("badtoml");
    fs::write(sb.proj().join("abl.toml"), "propth = [\"src\"]\n").unwrap();
    let err = sb.fails(&["status"]);
    assert!(err.contains("propth"), "{err}");
}

#[test]
fn doctor_fix_repairs_what_it_can() {
    let sb = Sandbox::new("doctorfix");
    let dlc = sb.install("OpenEdge128", "12.8.2");
    fs::create_dir_all(sb.proj().join("db/extra")).unwrap();
    let df = "ADD TABLE \"item\"\n  AREA \"Data\"\n\nADD FIELD \"name\" OF \"item\" AS character\n  FORMAT \"x(20)\"\n\n.\n";
    fs::write(sb.proj().join("db/shop.df"), df).unwrap();
    fs::write(sb.proj().join("db/extra/stock.df"), df).unwrap();
    fs::write(
        sb.proj().join("abl.toml"),
        "oe = \"12.8\"\npropath = [\"src\", \"lib\"]\n\n[db.shop]\nschema = \"db/shop.df\"\n",
    )
    .unwrap();

    // Not registered, a propath folder missing, a schema not listed.
    let before = sb.fails(&["doctor"]);
    assert!(before.contains("propath lib: folder not found"), "{before}");

    // The install is found where installs usually are ($DLC among them).
    let out = Command::new(env!("CARGO_BIN_EXE_abl"))
        .args(["doctor", "--fix"])
        .current_dir(sb.proj())
        .env("ABL_HOME", sb.dir.join("home"))
        .env("DLC", &dlc)
        .env_remove("ABL_OE")
        .output()
        .unwrap();
    let fixed = text(&out);
    assert!(out.status.success(), "{fixed}");
    assert!(fixed.contains("[ fix  ] registered OE 12.8 at"), "{fixed}");
    assert!(
        fixed.contains("[ fix  ] created propath folder lib"),
        "{fixed}"
    );
    assert!(
        fixed.contains("[ fix  ] added [db.stock] for db/extra/stock.df"),
        "{fixed}"
    );
    assert!(fixed.contains("All good."), "{fixed}");
    assert!(sb.proj().join("lib").is_dir());
    let toml = fs::read_to_string(sb.proj().join("abl.toml")).unwrap();
    assert!(
        toml.ends_with("[db.stock]\nschema = \"db/extra/stock.df\"\n"),
        "{toml}"
    );
    assert!(sb.global_config().contains("12.8"));

    // Run again: nothing left to do, and abl.toml is left as it is.
    let again = sb.ok(&["doctor", "--fix"]);
    assert!(again.contains("Nothing to fix automatically."), "{again}");
    assert_eq!(
        fs::read_to_string(sb.proj().join("abl.toml")).unwrap(),
        toml
    );
}

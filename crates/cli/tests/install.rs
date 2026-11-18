//! End-to-end tests for `abl install`, `update` and `remove`, with local git
//! repositories as packages, and `abl i @code` with a stand-in `code`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct T {
    dir: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl T {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("abl-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("proj/src")).unwrap();
        let t = T { dir };
        t.write("proj/abl.toml", "propath = [\"src\"]\n");
        t.write("proj/src/main.p", "RUN util/strings.p.\n");
        // util: two tagged versions on main.
        t.repo(
            "util",
            &[
                ("abl.toml", "propath = [\"src\"]\n"),
                ("src/util/strings.p", "MESSAGE \"v1\".\n"),
            ],
        );
        git(&t.path("util"), &["tag", "v1"]);
        t.commit("util", &[("src/util/strings.p", "MESSAGE \"v2\".\n")]);
        git(&t.path("util"), &["tag", "v2"]);
        t
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path(rel)).unwrap_or_default()
    }

    fn repo(&self, name: &str, files: &[(&str, &str)]) {
        fs::create_dir_all(self.path(name)).unwrap();
        git(&self.path(name), &["init", "-q"]);
        self.commit(name, files);
    }

    fn commit(&self, name: &str, files: &[(&str, &str)]) -> String {
        for (f, text) in files {
            self.write(&format!("{name}/{f}"), text);
        }
        let d = self.path(name);
        git(&d, &["add", "."]);
        git(&d, &["commit", "-qm", "c"]);
        git(&d, &["rev-parse", "HEAD"])
    }

    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_abl"))
            .args(args)
            .current_dir(self.path("proj"))
            .env("ABL_HOME", self.path("home"))
            .env("ABL_CODE", self.path("bin/code.exe"))
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

    fn head(&self, rel: &str) -> String {
        git(&self.path(rel), &["rev-parse", "HEAD"])
    }
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn install_lock_and_reinstall() {
    let t = T::new("basic");
    let util = t.path("util").display().to_string();
    let out = t.ok(&["i", &format!("{util}@v1")]);
    assert!(
        out.contains("util             v1 @") && out.contains("installed"),
        "{out}"
    );
    assert!(out.contains("1 package in vendor/"), "{out}");
    let toml = t.read("proj/abl.toml");
    assert!(
        toml.contains("[deps]") && toml.contains("ref = \"v1\""),
        "{toml}"
    );
    let v1 = git(&t.path("util"), &["rev-parse", "v1^{commit}"]);
    assert!(t
        .read("proj/abl.lock")
        .contains(&format!("commit = \"{v1}\"")));
    // (git may check it out with CRLF line ends)
    assert_eq!(
        t.read("proj/vendor/util/src/util/strings.p").trim_end(),
        "MESSAGE \"v1\"."
    );

    // On the PROPATH, resolvable, and not linted.
    let env = t.ok(&["env"]);
    assert!(env.replace('\\', "/").contains("vendor/util/src"), "{env}");
    let deps = t.ok(&["deps", "src/main.p"]);
    assert!(
        deps.replace('\\', "/")
            .contains("vendor/util/src/util/strings.p"),
        "{deps}"
    );
    assert!(t.ok(&["lint"]).contains("in 1 file"), "vendor/ is skipped");

    // A fresh checkout: the lock decides, not the newest commit.
    fs::remove_dir_all(t.path("proj/vendor")).unwrap();
    let out = t.ok(&["install"]);
    assert!(out.contains("installed"), "{out}");
    assert_eq!(t.head("proj/vendor/util"), v1);
    assert!(t.ok(&["i"]).contains("up to date"));
}

#[test]
fn update_moves_branches_and_remove_cleans_up() {
    let t = T::new("update");
    let util = t.path("util").display().to_string();
    t.ok(&["i", &util, "--ref", "main", "--name", "u"]);
    let c2 = t.head("util");
    assert_eq!(t.head("proj/vendor/u"), c2);
    let c3 = t.commit("util", &[("src/util/strings.p", "MESSAGE \"v3\".\n")]);
    assert!(
        t.ok(&["i"]).contains("up to date"),
        "install keeps the locked commit"
    );
    assert_eq!(t.head("proj/vendor/u"), c2);
    assert!(t.fails(&["update", "nope"]).contains("no package nope"));
    let out = t.ok(&["update", "u"]);
    assert!(out.contains("updated"), "{out}");
    assert_eq!(t.head("proj/vendor/u"), c3);
    assert!(t.read("proj/abl.lock").contains(&c3));

    let out = t.ok(&["remove", "u"]);
    assert!(out.contains("u                removed"), "{out}");
    assert!(!t.path("proj/vendor/u").exists());
    assert!(!t.path("proj/abl.lock").exists());
    assert!(!t.read("proj/abl.toml").contains("deps"));
}

#[test]
fn nested_packages_and_conflicts() {
    let t = T::new("nested");
    let util = t.path("util").display().to_string().replace('\\', "/");
    t.repo(
        "json",
        &[
            (
                "abl.toml",
                &format!("[deps]\nutil = {{ git = \"{util}\", ref = \"v1\" }}\n"),
            ),
            ("json/parse.p", "RUN util/strings.p.\n"),
        ],
    );
    t.repo(
        "other",
        &[(
            "abl.toml",
            &format!("[deps]\nutil = {{ git = \"{util}\", ref = \"v2\" }}\n"),
        )],
    );
    let out = t.ok(&["i", &t.path("json").display().to_string()]);
    assert!(
        out.contains("json") && out.contains("util") && out.contains("2 packages"),
        "{out}"
    );
    let lock = t.read("proj/abl.lock");
    assert!(lock.contains("via = \"json\""), "{lock}");
    assert!(t.path("proj/vendor/util/src/util/strings.p").is_file());
    let env = t.ok(&["env"]).replace('\\', "/");
    assert!(
        env.contains("vendor/json") && env.contains("vendor/util/src"),
        "{env}"
    );

    let before = t.read("proj/abl.toml");
    let out = t.fails(&["i", &t.path("other").display().to_string()]);
    assert!(out.contains("util is asked for twice"), "{out}");
    assert_eq!(
        t.read("proj/abl.toml"),
        before,
        "abl.toml unchanged on failure"
    );
}

#[test]
fn errors_and_code() {
    let t = T::new("code");
    assert!(t
        .fails(&["i", &t.path("missing").display().to_string()])
        .contains("git clone"));
    assert!(t
        .fails(&["i", "x", "--name", "../bad"])
        .contains("isn't a usable package name"));

    fs::create_dir_all(t.path("bin")).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_fake-progres"), t.path("bin/code.exe")).unwrap();
    t.write("abl-lsp-1.0.0.vsix", "not really");
    let vsix = t.path("abl-lsp-1.0.0.vsix").display().to_string();
    let out = t.ok(&["i", "@code", "--vsix", &vsix]);
    assert!(
        out.contains(&format!("fake code: --install-extension {vsix} --force")),
        "{out}"
    );
    assert!(out.contains("Installed the ABL extension"), "{out}");
    assert!(t.fails(&["i", "@code"]).contains("no .vsix next to abl"));
}

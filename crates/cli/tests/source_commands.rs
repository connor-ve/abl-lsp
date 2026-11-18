//! End-to-end tests for the commands that work on source and schema files
//! without OpenEdge: lint, fmt, deps, explain and schema diff. Each test builds
//! a small project in its own temp folder.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Proj {
    dir: PathBuf,
}

impl Proj {
    fn new(name: &str, abl_toml: Option<&str>) -> Self {
        let dir = std::env::temp_dir().join(format!("abl-src-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("home")).unwrap();
        fs::create_dir_all(dir.join("proj/src")).unwrap();
        let p = Proj { dir };
        if let Some(t) = abl_toml {
            p.write("abl.toml", t);
        }
        p
    }

    fn root(&self) -> PathBuf {
        self.dir.join("proj")
    }

    fn write(&self, rel: &str, text: &str) {
        self.write_bytes(rel, text.as_bytes());
    }

    fn write_bytes(&self, rel: &str, bytes: &[u8]) {
        let p = self.root().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root().join(rel)).unwrap()
    }

    fn abl(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_abl"))
            .args(args)
            .current_dir(self.root())
            .env("ABL_HOME", self.dir.join("home"))
            .env_remove("ABL_OE")
            .output()
            .expect("run abl")
    }

    /// Runs abl and returns (exit code, stdout + stderr).
    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = self.abl(args);
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

    fn fails(&self, args: &[&str]) -> String {
        let (code, text) = self.run(args);
        assert_eq!(code, 1, "abl {args:?} should exit 1:\n{text}");
        text
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const TOML: &str = "propath = [\"src\"]\n";

#[test]
fn lint_reports_fixes_and_respects_config() {
    let p = Proj::new("lint", Some(TOML));
    p.write(
        "src/a.p",
        "DEFINE VARIABLE cName AS CHARACTER.\nDEFINE VARIABLE iUnused AS INTEGER NO-UNDO.\ncName = \"x\".\nMESSAGE cName.\n",
    );
    p.write("src/ok.p", "MESSAGE \"fine\".\n");

    // Warnings only: exit 0 unless --max-warnings is exceeded.
    let out = p.ok(&["lint"]);
    assert!(
        out.contains("src/a.p:1:17: warning [missing-no-undo]"),
        "{out}"
    );
    assert!(
        out.contains("src/a.p:2:17: warning [unused-variable]"),
        "{out}"
    );
    assert!(out.contains("0 errors, 2 warnings in 2 files"), "{out}");
    p.fails(&["lint", "--max-warnings", "1"]);
    p.ok(&["lint", "--max-warnings", "2"]);

    let json: serde_json::Value =
        serde_json::from_str(&p.ok(&["lint", "src/a.p", "--format", "json"])).unwrap();
    assert_eq!(json["warnings"], 2);
    assert_eq!(json["findings"][0]["rule"], "missing-no-undo");
    assert_eq!(json["findings"][0]["fixable"], true);

    let gh = p.ok(&["lint", "--format", "github"]);
    assert!(gh.contains("::warning file=src/a.p,line=1,col=17,"), "{gh}");

    // --fix applies the quick fix and leaves the rest.
    let fixed = p.ok(&["lint", "--fix"]);
    assert!(fixed.contains("(1 fixed)"), "{fixed}");
    assert!(p
        .read("src/a.p")
        .starts_with("DEFINE VARIABLE cName AS CHARACTER NO-UNDO.\n"));

    // [lint] levels from abl.toml.
    p.write(
        "abl.toml",
        &format!("{TOML}\n[lint]\nunused-variable = \"error\"\n"),
    );
    let out = p.fails(&["lint"]);
    assert!(out.contains("error [unused-variable]"), "{out}");
    p.write(
        "abl.toml",
        &format!("{TOML}\n[lint]\nunused-variable = \"off\"\n"),
    );
    assert!(p.ok(&["lint"]).contains("0 errors, 0 warnings"));
    p.write(
        "abl.toml",
        &format!("{TOML}\n[lint]\nno-such-rule = \"off\"\n"),
    );
    assert!(p
        .fails(&["lint"])
        .contains("unknown [lint] entries: no-such-rule"));
    assert!(p
        .run(&["doctor"])
        .1
        .contains("[lint]: unknown entries no-such-rule"));

    // Syntax errors fail the run.
    p.write("abl.toml", TOML);
    p.write("src/bad.p", "DO WHILE TRUE:\n  MESSAGE \"x\".\n");
    assert!(p.fails(&["lint", "src/bad.p"]).contains("error"));
    p.fails(&["lint", "src/missing.p"]);
}

#[test]
fn includes_are_checked_with_their_arguments() {
    let p = Proj::new("incargs", Some(TOML));
    p.write(
        "src/inc/tt.i",
        "DEFINE TEMP-TABLE {1} NO-UNDO\n    FIELD {&key} AS INTEGER.\n",
    );
    p.write(
        "src/main.p",
        "{inc/tt.i ttOrder &key=OrderNum}\nFIND FIRST ttOrder NO-ERROR.\nIF AVAILABLE ttOrder THEN MESSAGE ttOrder.OrderNum.\n",
    );
    let out = p.ok(&["lint"]);
    assert!(out.contains("0 errors"), "{out}");
}

#[test]
fn fmt_check_write_and_encoding() {
    let p = Proj::new("fmt", Some(TOML));
    let messy = "DO:\nMESSAGE \"x\".   \nEND.\n";
    p.write("src/a.p", messy);
    // Latin-1 (not UTF-8) file: é is the single byte 0xE9.
    p.write_bytes(
        "src/l1.p",
        b"IF TRUE THEN DO:\nMESSAGE \"caf\xe9\".\nEND.\n",
    );

    let out = p.fails(&["fmt", "--check"]);
    assert!(
        out.contains("src/a.p: not formatted (first difference on line 2)"),
        "{out}"
    );
    assert_eq!(p.read("src/a.p"), messy, "--check must not write");

    assert!(p
        .ok(&["fmt", "src/a.p", "--stdout"])
        .contains("    MESSAGE \"x\".\n"));
    assert_eq!(p.read("src/a.p"), messy, "--stdout must not write");

    p.ok(&["fmt"]);
    assert_eq!(p.read("src/a.p"), "DO:\n    MESSAGE \"x\".\nEND.\n");
    let l1 = fs::read(p.root().join("src/l1.p")).unwrap();
    assert_eq!(
        l1, b"IF TRUE THEN DO:\n    MESSAGE \"caf\xe9\".\nEND.\n",
        "stays Latin-1"
    );
    assert!(p
        .ok(&["fmt", "--check"])
        .contains("2 files formatted correctly"));

    // [format] indent.
    p.write("abl.toml", &format!("{TOML}\n[format]\nindent = 2\n"));
    p.ok(&["fmt", "src/a.p"]);
    assert_eq!(p.read("src/a.p"), "DO:\n  MESSAGE \"x\".\nEND.\n");
}

#[test]
fn deps_forward_and_reverse() {
    let p = Proj::new("deps", Some(TOML));
    p.write("src/inc/a.i", "{inc/b.i}\n");
    p.write("src/inc/b.i", "DEFINE VARIABLE iB AS INTEGER NO-UNDO.\n");
    p.write(
        "src/main.p",
        "{inc/a.i}\nRUN other.p.\nRUN util/missing.p.\n",
    );
    p.write("src/other.p", "{inc/b.i}\n");
    p.write("src/caller.p", "RUN main.p.\n");
    p.write("src/shapes/Shape.cls", "CLASS shapes.Shape:\nEND CLASS.\n");
    p.write(
        "src/shapes/Square.cls",
        "USING shapes.*.\nCLASS shapes.Square INHERITS Shape:\nEND CLASS.\n",
    );
    p.write(
        "src/use.p",
        "USING shapes.*.\nDEFINE VARIABLE o AS Square NO-UNDO.\n",
    );

    let out = p.ok(&["deps", "src/main.p"]);
    assert!(out.contains("include    src/inc/a.i"), "{out}");
    assert!(out.contains("include    src/inc/b.i"), "{out}");
    assert!(out.contains("util/missing.p  (not found)"), "{out}");
    assert!(out.contains("1 not found on the PROPATH"), "{out}");

    // b.i is used by both includers and by what includes a.i; RUN callers are
    // dependents but are not recompiled.
    let out = p.ok(&["deps", "--reverse", "src/inc/b.i", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let recompile: Vec<&str> = v["recompile"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(recompile, ["src/other.p", "src/main.p"], "{out}");
    let via_a = v["dependents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["file"] == "src/main.p")
        .unwrap();
    assert_eq!(via_a["via"][0], "src/inc/a.i");

    let out = p.ok(&["deps", "-r", "src/main.p"]);
    assert!(out.contains("src/caller.p  run"), "{out}");
    assert!(
        out.contains("Recompile after changing src/main.p:\n  src/main.p\n"),
        "{out}"
    );

    let out = p.ok(&["deps", "-r", "src/shapes/Shape.cls"]);
    assert!(out.contains("src/shapes/Square.cls  inherits"), "{out}");
    // use.p uses Square, which inherits Shape: it has to be recompiled too.
    assert!(out.contains("  src/use.p\n"), "{out}");
}

#[test]
fn editor_settings_are_used_without_abl_toml() {
    let p = Proj::new("vscode", None);
    p.write(
        ".vscode/settings.json",
        "{\n  // where the sources are\n  \"abl.propath\": [\"src\"],\n  \"abl.lint\": { \"missing-no-undo\": \"off\", }, // trailing\n}\n",
    );
    p.write(
        "src/lib/x.i",
        "DEFINE VARIABLE iX AS INTEGER.\nMESSAGE iX.\n",
    );
    p.write("src/main.p", "{lib/x.i}\n");
    let out = p.ok(&["lint"]);
    assert!(out.contains("0 errors, 0 warnings"), "{out}");
    assert!(!out.contains("not found on the PROPATH"), "{out}");
}

#[test]
fn explain_messages() {
    let p = Proj::new("explain", None);
    let out = p.ok(&[
        "explain", "**", "Customer", "record", "not", "on", "file.", "(138)",
    ]);
    assert!(out.contains("record not on file"), "{out}");
    assert!(out.contains("find-without-no-error"), "{out}");
    assert!(p.ok(&["explain", "--list"]).contains("3135"));
    assert!(p
        .ok(&["explain", "invalid", "handle"])
        .contains("VALID-HANDLE"));
    assert!(p
        .fails(&["explain", "99999"])
        .contains("not in abl's table"));
}

#[test]
fn schema_diff_writes_a_delta() {
    let p = Proj::new("schema", None);
    let old = "ADD TABLE \"Item\"\n  AREA \"Data\"\n\nADD FIELD \"Name\" OF \"Item\" AS character\n  FORMAT \"x(20)\"\n  POSITION 2\n\nADD INDEX \"Name\" ON \"Item\"\n  PRIMARY\n  INDEX-FIELD \"Name\" ASCENDING\n\n.\nPSC\ncpstream=UTF-8\n.\n0000000200\n";
    let new = old.replace("x(20)", "x(40)").replace(
        "  POSITION 2\n",
        "  POSITION 2\n\nADD FIELD \"Price\" OF \"Item\" AS decimal\n  DECIMALS 2\n  POSITION 3\n",
    );
    p.write("old.df", old);
    p.write("new.df", &new);

    assert!(p
        .ok(&["schema", "diff", "old.df", "old.df", "--exit-code"])
        .contains("No schema differences"));
    let out = p.ok(&["schema", "diff", "old.df", "new.df", "--delta", "delta.df"]);
    assert!(
        out.contains("~ field Name: FORMAT \"x(20)\" -> \"x(40)\""),
        "{out}"
    );
    assert!(out.contains("+ field Price (decimal)"), "{out}");
    let delta = p.read("delta.df");
    assert_eq!(
        delta,
        "UPDATE FIELD \"Name\" OF \"Item\"\n  FORMAT \"x(40)\"\n\nADD FIELD \"Price\" OF \"Item\" AS decimal\n  DECIMALS 2\n  POSITION 3\n\n.\n"
    );
    p.fails(&["schema", "diff", "old.df", "new.df", "--exit-code"]);
}

const SHOP_DF: &str = r#"ADD TABLE "Customer"
  DUMP-NAME "customer"

ADD FIELD "CustNum" OF "Customer" AS integer
  FORMAT ">>>>9"
  POSITION 2

ADD FIELD "Name" OF "Customer" AS character
  FORMAT "x(30)"
  POSITION 3

ADD FIELD "Balance" OF "Customer" AS decimal
  FORMAT "->>,>>9.99"
  POSITION 4

ADD TABLE "Order"
  DUMP-NAME "order"

ADD FIELD "CustNum" OF "Order" AS integer
  FORMAT ">>>>9"
  POSITION 2

.
"#;

#[test]
fn usages_of_tables_and_fields() {
    let p = Proj::new(
        "usages",
        Some("propath = [\"src\"]\n\n[db.shop]\nschema = \"db/shop.df\"\n"),
    );
    p.write("db/shop.df", SHOP_DF);
    p.write(
        "src/new.p",
        "CREATE Customer.\nASSIGN Customer.CustNum = 1\n       Customer.Name = \"x\".\n",
    );
    p.write(
        "src/upd.p",
        "DEFINE BUFFER bCust FOR Customer.\n\
         FIND FIRST bCust EXCLUSIVE-LOCK WHERE bCust.Name = \"x\" NO-ERROR.\n\
         IF AVAILABLE bCust THEN bCust.Balance = bCust.Balance + 1.\n\
         FOR EACH Customer WHERE Customer.Balance > 0:\n    Balance = 0.\nEND.\n\
         FIND FIRST Customer NO-ERROR.\nDELETE Customer.\n",
    );
    p.write(
        "src/other.p",
        "FOR EACH Order NO-LOCK:\n    DISPLAY Order.CustNum.\nEND.\n",
    );

    let out = p.ok(&["usages", "customer"]);
    assert!(out.starts_with("Customer: "), "{out}");
    assert!(
        out.contains("create (1)\n  src/new.p:1:8  CREATE Customer."),
        "{out}"
    );
    assert!(
        out.contains("src/new.p:2:8  ASSIGN Customer.CustNum = 1  [CustNum]"),
        "{out}"
    );
    assert!(
        out.contains("src/new.p:3:8  Customer.Name = \"x\".  [Name]"),
        "{out}"
    );
    assert!(
        out.contains(
            "src/upd.p:3:25  IF AVAILABLE bCust THEN bCust.Balance = bCust.Balance + 1.  [Balance]"
        ),
        "{out}"
    );
    assert!(
        out.contains("src/upd.p:5:5  Balance = 0.  [Balance]"),
        "{out}"
    );
    assert!(
        out.contains("delete (1)\n  src/upd.p:8:8  DELETE Customer."),
        "{out}"
    );
    assert!(
        out.contains("src/upd.p:2:12  FIND FIRST bCust EXCLUSIVE-LOCK"),
        "{out}"
    );
    assert!(out.contains("[via bCust, exclusive-lock]"), "{out}");
    assert!(
        out.contains("define (1)\n  src/upd.p:1:25  DEFINE BUFFER bCust FOR Customer."),
        "{out}"
    );
    assert!(!out.contains("other.p"), "{out}");

    // A field: reads too, and only that field (Order.CustNum is a different field).
    let out = p.ok(&["usages", "Customer.Balance"]);
    assert!(
        out.starts_with("Customer.Balance: 4 uses in 1 file"),
        "{out}"
    );
    assert!(out.contains("update (2)"), "{out}");
    assert!(out.contains("read (2)"), "{out}");
    assert!(!p.ok(&["usages", "customer.custnum"]).contains("other.p"));
    assert!(p
        .ok(&["usages", "order.custnum"])
        .contains("src/other.p:2:13"));

    let out = p.ok(&["usages", "shop.customer", "--writes"]);
    assert!(!out.contains("read ("), "{out}");
    let v: serde_json::Value =
        serde_json::from_str(&p.ok(&["usages", "customer", "--writes", "--json"])).unwrap();
    assert_eq!(v["target"], "Customer");
    assert_eq!(v["uses"][0]["access"], "create");

    assert!(p
        .fails(&["usages", "nosuch"])
        .contains("no table nosuch in the schema (shop)"));
    assert!(p
        .fails(&["usages", "customer.nope"])
        .contains("table Customer has no field nope"));
}

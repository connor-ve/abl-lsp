//! End-to-end tests for `abl db`, `query`, `export` and `import` against the
//! fake install `abl dev fake-dlc` makes (fake `_progres` and database tools).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Proj {
    dir: PathBuf,
}

const TOML: &str = "oe = \"12.8\"\npropath = [\"src\"]\n\n[db.shop]\nschema = \"db/shop.df\"\n";

const SHOP_DF: &str = r#"ADD TABLE "Customer"
  DUMP-NAME "customer"

ADD FIELD "CustNum" OF "Customer" AS integer
  FORMAT ">>>>9"
  INITIAL "0"
  POSITION 2

ADD FIELD "Name" OF "Customer" AS character
  FORMAT "x(30)"
  INITIAL ""
  POSITION 3

ADD FIELD "Balance" OF "Customer" AS decimal
  FORMAT "->>,>>9.99"
  INITIAL "0"
  POSITION 4

ADD FIELD "Active" OF "Customer" AS logical
  FORMAT "yes/no"
  INITIAL "yes"
  POSITION 5

ADD FIELD "Phone" OF "Customer" AS character
  FORMAT "x(12)"
  INITIAL ""
  POSITION 6
  EXTENT 2

ADD INDEX "CustNum" ON "Customer"
  UNIQUE
  PRIMARY
  INDEX-FIELD "CustNum" ASCENDING

.
"#;

impl Proj {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("abl-data-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("proj/src")).unwrap();
        let p = Proj { dir };
        let dlc = p.dir.join("dlc");
        p.ok(&["dev", "fake-dlc", dlc.to_str().unwrap()]);
        p.ok(&["oe", "add", "12.8", dlc.to_str().unwrap()]);
        p.write("abl.toml", TOML);
        p.write("db/shop.df", SHOP_DF);
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

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root().join(rel)).unwrap()
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
        format!("{out}{err}")
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const CSV: &str = "CustNum,Name,Balance,Active,Phone[1],Phone[2]\n\
                   1,Lift Line,1200.50,yes,555-1,?\n\
                   2,\"Urpon, Inc\",0,no,,555-9\n\
                   3,Hoops,4000,yes,?,?\n";

#[test]
fn database_lifecycle() {
    let p = Proj::new("life");
    assert!(p
        .ok(&["db"])
        .contains("shop  not created .abl/db/shop/shop.db"));
    assert!(p.ok(&["status"]).contains("shop (db/shop.df), not created"));
    assert!(p
        .fails(&["query", "customer"])
        .contains("shop isn't created yet (abl db create)"));

    let out = p.ok(&["db", "create"]);
    assert!(
        out.contains("Created shop at .abl/db/shop/shop.db\nLoaded db/shop.df"),
        "{out}"
    );
    assert!(p.fails(&["db", "create"]).contains("shop already exists"));
    assert!(p.ok(&["db", "list"]).contains("shop  stopped"));

    assert!(p
        .ok(&["db", "start"])
        .contains("Started shop (shared memory"));
    assert!(p.ok(&["db", "start"]).contains("already running"));
    assert!(p.ok(&["status"]).contains("shop (db/shop.df), running"));
    assert!(p
        .fails(&["db", "delete", "--yes"])
        .contains("stop it first"));
    assert!(p.ok(&["db", "stop"]).contains("Stopped shop"));
    assert!(p.ok(&["db", "stop"]).contains("not running"));

    // Without a terminal, deleting needs --yes.
    assert!(p.fails(&["db", "delete"]).contains("Pass --yes to confirm"));
    assert!(p
        .ok(&["db", "delete", "shop", "--yes"])
        .contains("Deleted shop"));
    assert!(p.ok(&["db"]).contains("not created"));

    // Several databases: name one. One with `connect` isn't abl's.
    p.write(
        "abl.toml",
        &format!("{TOML}\n[db.other]\nconnect = \"-db other -H host -S 5000\"\n[db.third]\npath = \"data/third\"\n"),
    );
    assert!(p
        .fails(&["db", "create"])
        .contains("several databases (shop, third); name one"));
    assert!(p
        .fails(&["db", "start", "other"])
        .contains("other has `connect`"));
    assert!(p
        .fails(&["db", "start", "nope"])
        .contains("no database nope in abl.toml (known: other, shop, third)"));
    let out = p.ok(&["db", "create", "third"]);
    assert!(out.contains("Created third at data/third.db"), "{out}");
    assert!(p
        .ok(&["db"])
        .contains("other  connect -db other -H host -S 5000"));
}

#[test]
fn import_query_export() {
    let p = Proj::new("rows");
    p.ok(&["db", "create"]);
    p.write("cust.csv", CSV);
    assert_eq!(
        p.ok(&["import", "customer", "cust.csv"]),
        "Imported 3 rows into customer\n"
    );

    let table = p.ok(&["query", "FOR EACH customer WHERE balance > 100 BY name."]);
    assert_eq!(
        table,
        "CustNum  Name       Balance  Active  Phone[1]  Phone[2]\n\
         -------  ---------  -------  ------  --------  --------\n      \
               3  Hoops         4000  yes     ?         ?\n      \
               1  Lift Line   1200.5  yes     555-1     ?\n\
         (2 rows)\n"
    );
    let out = p.ok(&[
        "query",
        "customer",
        "--fields",
        "custnum,name",
        "--max",
        "2",
    ]);
    assert!(out.contains("CustNum  Name\n"), "{out}");
    assert!(
        out.ends_with("(2 rows shown; there are more: --max 0 shows all)\n"),
        "{out}"
    );
    let out = p.ok(&[
        "query",
        "customer where name begins \"u\" or custnum = 3",
        "--format",
        "csv",
    ]);
    assert_eq!(
        out,
        "CustNum,Name,Balance,Active,Phone[1],Phone[2]\n2,\"Urpon, Inc\",0,no,,555-9\n3,Hoops,4000,yes,?,?\n"
    );

    let json: serde_json::Value =
        serde_json::from_str(&p.ok(&["query", "shop.customer", "--format", "json"])).unwrap();
    assert_eq!(json[0]["CustNum"], 1);
    assert_eq!(json[0]["Balance"], 1200.5);
    assert_eq!(json[0]["Active"], true);
    assert_eq!(json[0]["Phone"], serde_json::json!(["555-1", null]));
    assert_eq!(json[2]["Balance"], 4000);
    // Fields keep the table's order.
    let keys: Vec<&String> = json[0].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["CustNum", "Name", "Balance", "Active", "Phone"]);

    assert!(p
        .fails(&["query", "customer balance > 1"])
        .contains("expected WHERE or BY"));
    assert!(p
        .fails(&["query", "nosuch"])
        .contains("Unable to find table nosuch"));

    // A duplicate key: nothing is imported, and the row is named.
    p.write(
        "more.json",
        r#"[{"CustNum": 4, "Name": "New"}, {"CustNum": 1, "Name": "Again"}]"#,
    );
    let err = p.fails(&["import", "customer", "more.json"]);
    assert!(
        err.contains("row 2: ** Customer already exists with CustNum 1. (132)"),
        "{err}"
    );
    assert!(err.contains("nothing was imported"), "{err}");
    assert!(p.ok(&["query", "customer"]).contains("(3 rows)"));
    p.write("bad.csv", "CustNum,Name\nabc,x\n");
    assert!(p
        .fails(&["import", "customer", "bad.csv"])
        .contains("row 1: ** Invalid character in numeric input abc. (76)"));

    // Export, and import the export back with --replace.
    let (code, _, err) = p.run(&[
        "export",
        "customer",
        "--where",
        "balance >= 1000",
        "-o",
        "big.csv",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(err, "Exported 2 rows of customer to big.csv\n");
    assert!(p
        .read("big.csv")
        .starts_with("CustNum,Name,Balance,Active,Phone[1],Phone[2]\n1,Lift Line,1200.5"));
    let out = p.ok(&["export", "customer"]);
    let all: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 3);
    p.write("all.json", &out);
    assert_eq!(
        p.ok(&["import", "customer", "big.csv", "--replace"]),
        "Replaced 3 rows of customer with 2\n"
    );
    assert!(p.ok(&["query", "customer"]).contains("(2 rows)"));
    p.ok(&["import", "customer", "all.json", "--replace"]);
    assert_eq!(
        p.ok(&["export", "customer"]),
        out,
        "a JSON export imports back unchanged"
    );
    assert!(p
        .fails(&["import", "customer", "cust.txt"])
        .contains("cust.txt"));
}

#[test]
fn snapshots() {
    let p = Proj::new("snap");
    p.ok(&["db", "create"]);
    p.write("cust.csv", CSV);
    p.ok(&["import", "customer", "cust.csv"]);
    assert!(p
        .ok(&["db", "snapshot", "--list"])
        .contains("No snapshots of shop"));
    assert_eq!(
        p.ok(&["db", "snapshot", "three"]),
        "Saved snapshot three of shop\n"
    );
    assert!(p
        .fails(&["db", "snapshot", "three"])
        .contains("already exists"));
    assert!(p
        .fails(&["db", "snapshot", "../x"])
        .contains("snapshot name"));

    p.write("one.csv", "CustNum,Name\n9,Only\n");
    p.ok(&["import", "customer", "one.csv", "--replace"]);
    assert!(p.ok(&["query", "customer"]).contains("(1 row)"));

    // A running database is backed up online, but restored only when stopped.
    p.ok(&["db", "start"]);
    p.ok(&["db", "snapshot", "one"]);
    assert!(p
        .fails(&["db", "restore", "three"])
        .contains("stop it first"));
    p.ok(&["db", "stop"]);
    assert_eq!(
        p.ok(&["db", "restore", "three"]),
        "Restored shop from snapshot three\n"
    );
    assert!(p.ok(&["query", "customer"]).contains("(3 rows)"));
    p.ok(&["db", "restore", "one"]);
    assert!(p.ok(&["query", "customer"]).contains("(1 row)"));

    let list = p.ok(&["db", "snapshot", "--list"]);
    assert!(
        list.starts_with("one  ") && list.contains("\nthree  "),
        "{list}"
    );
    assert!(p
        .fails(&["db", "restore", "nope"])
        .contains("no snapshot nope of shop"));
    // Without a name, the snapshot is named after the date and time.
    let out = p.ok(&["db", "snapshot"]);
    assert!(out.starts_with("Saved snapshot 20"), "{out}");
}

#[test]
fn compile_uses_the_database() {
    let p = Proj::new("compile");
    p.write(
        "src/a.p",
        "FOR EACH Customer NO-LOCK:\n    DISPLAY Customer.Name.\nEND.\n",
    );
    p.ok(&["compile"]);
    // Creating the database changes what the code compiles against.
    p.ok(&["db", "create"]);
    assert!(p.ok(&["compile"]).contains("Compiled 1 of 1"));
    // Starting the server doesn't: still up to date.
    let env = p.ok(&["env"]);
    assert!(
        env.contains("ABL_STARTUP=-db ") && env.contains(" -ld shop -1\n"),
        "{env}"
    );
    p.ok(&["db", "start"]);
    assert!(p.ok(&["compile"]).contains("Nothing to compile"));
    // Served: connected through the server, not single-user.
    assert!(p.ok(&["env"]).contains(" -ld shop\n"));
    p.ok(&["db", "stop"]);
    assert!(p.ok(&["compile"]).contains("Nothing to compile"));
}

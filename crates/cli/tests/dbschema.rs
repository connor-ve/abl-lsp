//! `abl db schema` and the page format of `abl query`, which the VS Code
//! database browser reads.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

fn abl(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_abl"))
        .args(args)
        .current_dir(dir.join("proj"))
        .env("ABL_HOME", dir.join("home"))
        .env_remove("ABL_OE")
        .stdin(Stdio::null())
        .output()
        .expect("run abl");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "abl {args:?}:\n{text}");
    (0, text)
}

#[test]
fn schema_and_page_rows() {
    let dir = std::env::temp_dir().join(format!("abl-dbschema-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("proj/db")).unwrap();
    let dlc = dir.join("dlc");
    abl(&dir, &["dev", "fake-dlc", dlc.to_str().unwrap()]);
    abl(&dir, &["oe", "add", "12.8", dlc.to_str().unwrap()]);
    let df = concat!(env!("CARGO_MANIFEST_DIR"), "/../../samples/db/sports.df");
    fs::copy(df, dir.join("proj/db/sports.df")).unwrap();
    fs::write(
        dir.join("proj/abl.toml"),
        "oe = \"12.8\"\n\n[db.sports]\nschema = \"db/sports.df\"\n\n[db.remote]\nconnect = \"-db x -H h -S 1\"\n",
    )
    .unwrap();

    let (_, text) = abl(&dir, &["db", "schema"]);
    assert!(text.contains("sports (not created): 2 tables"), "{text}");
    assert!(text.contains("remote (external): 0 tables"), "{text}");

    let (_, json) = abl(&dir, &["db", "schema", "sports", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let dbs = v["databases"].as_array().unwrap();
    assert_eq!(dbs.len(), 1);
    assert_eq!(dbs[0]["managed"], true);
    let cust = &dbs[0]["tables"][0];
    assert_eq!(cust["name"], "Customer");
    assert_eq!(cust["fields"][0]["name"], "CustNum");
    assert_eq!(cust["fields"][0]["indexes"][0], "CustNum");
    assert!(cust["line"].as_u64().unwrap() > 0);
    assert_eq!(cust["indexes"][0]["primary"], true);

    abl(&dir, &["db", "create"]);
    let (_, page) = abl(
        &dir,
        &["query", "sports.Customer", "--format", "page", "--max", "5"],
    );
    let v: serde_json::Value = serde_json::from_str(&page).unwrap();
    assert_eq!(v["columns"][0]["name"], "CustNum");
    assert_eq!(v["columns"][0]["type"], "integer");
    assert!(v["rows"].is_array() && v["more"].is_boolean(), "{page}");
    let _ = fs::remove_dir_all(&dir);
}

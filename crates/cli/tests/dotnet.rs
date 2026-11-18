//! End-to-end tests for `abl dotnet` and the -assemblies session parameter.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Proj {
    dir: PathBuf,
}

impl Proj {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("abl-dotnet-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("proj/src")).unwrap();
        let p = Proj { dir };
        let dlc = p.dir.join("dlc");
        p.ok(&["dev", "fake-dlc", dlc.to_str().unwrap()]);
        p.ok(&["oe", "add", "12.8", dlc.to_str().unwrap()]);
        p.write("abl.toml", "oe = \"12.8\"\npropath = [\"src\"]\n");
        p.write(
            "src/feed.p",
            "USING Newtonsoft.Json.* FROM ASSEMBLY.\n\
             DEFINE VARIABLE oDoc AS System.Xml.XmlDocument NO-UNDO.\n\
             DEFINE VARIABLE sb AS System.Text.StringBuilder NO-UNDO.\n\
             MESSAGE System.Environment:MachineName.\n",
        );
        p.write("src/plain.p", "MESSAGE \"no .NET here\".\n");
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
        fs::read_to_string(self.root().join(rel)).unwrap_or_default()
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
fn assemblies_and_namespaces() {
    let p = Proj::new("list");
    let out = p.ok(&["dotnet"]);
    assert!(out.contains("No assemblies.xml"), "{out}");
    for want in [
        "Newtonsoft.Json    1 file   not in assemblies.xml",
        "System             1 file   built in",
        "System.Text        1 file   built in",
        "System.Xml         1 file   not in assemblies.xml",
    ] {
        assert!(out.contains(want), "{want:?} missing in:\n{out}");
    }
    let doc = p.ok(&["doctor"]);
    assert!(
        doc.contains("[ warn ] .NET namespaces not in assemblies.xml: Newtonsoft.Json, System.Xml"),
        "{doc}"
    );
    // No assemblies.xml yet: no -assemblies.
    assert!(!p.ok(&["env"]).contains("-assemblies"));

    p.ok(&["dotnet", "add", "Newtonsoft.Json"]);
    let xml = p.read("assemblies.xml");
    assert!(
        xml.starts_with("<?xml") && xml.contains("<assembly name=\"Newtonsoft.Json\"/>"),
        "{xml}"
    );
    assert!(p
        .fails(&["dotnet", "add", "newtonsoft.json"])
        .contains("already in"));
    let full = "System.Xml, Version=4.0.0.0, Culture=neutral, PublicKeyToken=b77a5c561934e089";
    let out = p.ok(&["dotnet", "add", full]);
    assert!(!out.contains("GAC"), "a strong name needs no hint: {out}");
    assert!(p
        .read("assemblies.xml")
        .trim_end()
        .ends_with("</references>"));

    let out = p.ok(&["dotnet", "list"]);
    assert!(out.contains("assemblies.xml: 2 assemblies"), "{out}");
    assert!(
        out.contains("Newtonsoft.Json    1 file   assemblies.xml: Newtonsoft.Json"),
        "{out}"
    );
    assert!(
        out.contains("System.Xml         1 file   assemblies.xml: System.Xml"),
        "{out}"
    );
    assert!(p.ok(&["doctor"]).contains("[  ok  ] .NET: 4 namespaces"));

    // Every session now gets -assemblies with the folder.
    let env = p.ok(&["env"]);
    assert!(env.contains("-assemblies"), "{env}");

    p.ok(&["dotnet", "remove", "System.Xml"]);
    let xml = p.read("assemblies.xml");
    assert!(
        !xml.contains("System.Xml") && xml.contains("Newtonsoft.Json"),
        "{xml}"
    );
    assert!(p
        .fails(&["dotnet", "remove", "System.Xml"])
        .contains("no assembly System.Xml"));
}

#[test]
fn assemblies_folder_from_abl_toml() {
    let p = Proj::new("folder");
    p.write(
        "abl.toml",
        "oe = \"12.8\"\npropath = [\"src\"]\nassemblies = \"dotnet\"\n",
    );
    p.ok(&["dotnet", "add", "Acme.Widgets"]);
    assert!(p.root().join("dotnet/assemblies.xml").is_file());
    let env = p.ok(&["env"]).replace('\\', "/");
    assert!(
        env.contains("-assemblies") && env.contains("proj/dotnet"),
        "{env}"
    );
}

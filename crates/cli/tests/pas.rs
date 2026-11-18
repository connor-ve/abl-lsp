//! End-to-end test for `abl pas`: create, start, status, stop against the
//! fake install's PASOE (a small HTTP server).

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Proj {
    dir: PathBuf,
    port: u16,
}

impl Proj {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("abl-pas-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("proj/src")).unwrap();
        // A port of its own, away from the usual ones.
        let port = 20000 + (std::process::id() % 15000) as u16 * 3 % 30000;
        let p = Proj { dir, port };
        let dlc = p.dir.join("dlc");
        p.ok(&["dev", "fake-dlc", dlc.to_str().unwrap()]);
        p.ok(&["oe", "add", "12.8", dlc.to_str().unwrap()]);
        fs::write(
            p.root().join("abl.toml"),
            format!(
                "oe = \"12.8\"\npropath = [\"src\"]\n\n[pas]\nname = \"shop\"\nport = {port}\nstartup = \"-cpinternal UTF-8\"\n\n[db.extra]\nconnect = \"-db C:/data/extra.db -H dbhost -S 9999\"\n"
            ),
        )
        .unwrap();
        p
    }

    fn root(&self) -> PathBuf {
        self.dir.join("proj")
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

    fn get(&self) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
        let mut body = String::new();
        let _ = s.read_to_string(&mut body);
        body
    }
}

impl Drop for Proj {
    fn drop(&mut self) {
        // Never leave a server running.
        let _ = self.run(&["pas", "stop", "--wait", "5"]);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn create_start_stop() {
    let p = Proj::new("cycle");
    let port = p.port;
    let inst = p.root().join(".abl/pas/shop");

    assert!(p.ok(&["pas"]).contains("not created (abl pas create)"));
    assert!(p.fails(&["pas", "start"]).contains("abl pas create"));

    let out = p.ok(&["pas", "create"]);
    assert!(out.contains("Created PASOE instance shop"), "{out}");
    assert!(
        out.contains(&format!(
            "http {port}  https {}  shutdown {}",
            port + 1,
            port + 2
        )),
        "{out}"
    );
    let props = fs::read_to_string(inst.join("conf/openedge.properties")).unwrap();
    let rcode = p.root().join("build").join("rcode").display().to_string();
    let src = p.root().join("src").display().to_string();
    assert!(
        props.contains(&format!(
            "    PROPATH={rcode},{src},${{CATALINA_BASE}}/webapps/ROOT/WEB-INF/openedge,"
        )),
        "{props}"
    );
    assert!(
        props.contains("agentStartupParam=-T \"${catalina.base}/temp\" -db C:/data/extra.db -H dbhost -S 9999 -cpinternal UTF-8"),
        "{props}"
    );
    // The instance's other settings are kept.
    assert!(props.contains("numInitialSessions=1"), "{props}");
    assert!(p.fails(&["pas", "create"]).contains("already exists"));

    let out = p.ok(&["pas", "start"]);
    assert!(
        out.contains(&format!("up: http://localhost:{port}/")),
        "{out}"
    );
    assert!(p.get().contains("fake PASOE instance shop"));
    assert!(p.ok(&["pas", "start"]).contains("already running"));
    let status = p.ok(&["pas", "status"]);
    assert!(
        status.contains(&format!("running: http://localhost:{port}/")),
        "{status}"
    );
    assert!(status.contains(&format!("propath  {rcode}\n")), "{status}");
    // Running: not made again over the top.
    assert!(p
        .fails(&["pas", "create", "--force"])
        .contains("abl pas stop"));

    assert!(p.ok(&["pas", "stop"]).contains("Stopped shop."));
    assert!(p.ok(&["pas"]).contains("stopped (abl pas start)"));
    assert!(p.ok(&["pas", "stop"]).contains("not running"));

    // Stopped: --force makes it again.
    assert!(p
        .ok(&["pas", "create", "--force"])
        .contains("Created PASOE instance shop"));
}

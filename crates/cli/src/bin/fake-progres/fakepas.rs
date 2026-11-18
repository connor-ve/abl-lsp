//! PASOE, faked: `pasman create` makes an instance folder with the files abl
//! reads and writes (conf/openedge.properties, conf/catalina.properties) and a
//! `bin/tcman`. `tcman start` runs a small HTTP server on the instance's port
//! in the background; `tcman stop` asks it to quit.

use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

fn fail(msg: impl AsRef<str>) -> ExitCode {
    eprintln!("{}", msg.as_ref());
    ExitCode::from(1)
}

/// `pasman create [-v] [-f] [-p port] [-P port] [-s port] path`.
pub fn pasman(args: &[String]) -> ExitCode {
    if args.first().map(String::as_str) != Some("create") {
        return fail("fake pasman: only `create` is supported");
    }
    let mut http = "8810".to_string();
    let mut https = "8811".to_string();
    let mut shut = "8812".to_string();
    let mut path = None;
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-p" => http = it.next().cloned().unwrap_or_default(),
            "-P" => https = it.next().cloned().unwrap_or_default(),
            "-s" => shut = it.next().cloned().unwrap_or_default(),
            "-N" | "-j" | "-Z" | "-U" | "-G" => {
                it.next();
            }
            a if a.starts_with('-') => {}
            a => path = Some(PathBuf::from(a)),
        }
    }
    let Some(dir) = path else {
        return fail("fake pasman: create needs an instance path");
    };
    if dir.exists() && dir.read_dir().is_ok_and(|mut d| d.next().is_some()) {
        return fail(format!("fake pasman: {} is not empty", dir.display()));
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "oepas1".into());
    let made = (|| -> std::io::Result<()> {
        for sub in [
            "bin",
            "conf",
            "logs",
            "temp",
            "openedge",
            "webapps/ROOT/WEB-INF/openedge",
        ] {
            fs::create_dir_all(dir.join(sub))?;
        }
        let me = env::current_exe()?;
        fs::copy(
            &me,
            dir.join("bin")
                .join(format!("tcman{}", env::consts::EXE_SUFFIX)),
        )?;
        fs::write(
            dir.join("conf/catalina.properties"),
            format!("# fake PASOE instance\npsc.as.http.port={http}\npsc.as.https.port={https}\npsc.as.shut.port={shut}\n"),
        )?;
        fs::write(
            dir.join("conf/openedge.properties"),
            format!(
                "[AppServer.Agent]\n    numInitialSessions=1\n\n[AppServer.Agent.{name}]\n    PROPATH=${{CATALINA_BASE}}/webapps/ROOT/WEB-INF/openedge,${{CATALINA_BASE}}/openedge,${{DLC}}/tty\n\n[AppServer.SessMgr.{name}]\n    agentStartupParam=-T \"${{catalina.base}}/temp\"\n"
            ),
        )?;
        Ok(())
    })();
    if let Err(e) = made {
        return fail(format!("fake pasman: {e}"));
    }
    println!("fake pasman: created instance {name} in {}", dir.display());
    ExitCode::SUCCESS
}

fn instance_dir() -> Option<PathBuf> {
    // bin/tcman(.exe) inside the instance.
    env::current_exe()
        .ok()?
        .parent()?
        .parent()
        .map(Path::to_path_buf)
}

fn http_port(dir: &Path) -> Option<u16> {
    fs::read_to_string(dir.join("conf/catalina.properties"))
        .ok()?
        .lines()
        .find_map(|l| l.trim().strip_prefix("psc.as.http.port="))?
        .trim()
        .parse()
        .ok()
}

/// `tcman start`, `tcman stop`, and the server itself (`tcman --fake-serve`).
pub fn tcman(args: &[String]) -> ExitCode {
    let Some(dir) = instance_dir() else {
        return fail("fake tcman: no instance folder");
    };
    let Some(port) = http_port(&dir) else {
        return fail("fake tcman: no http port in conf/catalina.properties");
    };
    match args.first().map(String::as_str) {
        Some("start") => {
            let Ok(me) = env::current_exe() else {
                return fail("fake tcman: cannot find itself");
            };
            // Detached, with no handles shared: the caller doesn't wait on it.
            match Command::new(me)
                .arg("--fake-serve")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(_) => {
                    println!("fake tcman: starting on port {port}");
                    ExitCode::SUCCESS
                }
                Err(e) => fail(format!("fake tcman: {e}")),
            }
        }
        Some("stop") => match TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut s) => {
                let _ = s.write_all(b"GET /fake-stop HTTP/1.0\r\n\r\n");
                println!("fake tcman: stopping");
                ExitCode::SUCCESS
            }
            Err(_) => fail("fake tcman: not running"),
        },
        Some("--fake-serve") => serve(&dir, port),
        _ => fail("fake tcman: start or stop"),
    }
}

fn serve(dir: &Path, port: u16) -> ExitCode {
    let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) else {
        return ExitCode::from(1);
    };
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let log = dir.join("logs").join("fake-pasoe.log");
    let _ = fs::write(&log, format!("fake PASOE {name} listening on {port}\n"));
    for stream in listener.incoming().flatten() {
        let mut line = String::new();
        let _ = BufReader::new(&stream).read_line(&mut line);
        let stop = line.contains("/fake-stop");
        let body = format!("fake PASOE instance {name}\n");
        let mut s = stream;
        let _ = write!(
            s,
            "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        if stop {
            break;
        }
    }
    ExitCode::SUCCESS
}

//! `abl pas`: a local PASOE instance for the project. `create` makes it with
//! `pasman`, and every `create` and `start` points its ABL application at the
//! project: the PROPATH (r-code first) and the databases in abl.toml go into
//! the instance's `conf/openedge.properties`. `start` and `stop` run the
//! instance's `tcman` and wait for its HTTP port.

use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use abl_oe::config::{self, Loaded};
use abl_oe::db;
use abl_oe::session::{split_params, Env};

/// The instance as abl.toml describes it.
struct Instance {
    name: String,
    dir: PathBuf,
    http: u16,
    https: u16,
    shutdown: u16,
}

fn instance(l: &Loaded) -> Instance {
    let pas = &l.project.pas;
    let name = pas.name.clone().unwrap_or_else(|| default_name(&l.root));
    let dir = match &pas.path {
        Some(p) => l.path(p),
        None => l.root.join(".abl").join("pas").join(&name),
    };
    Instance {
        name,
        dir,
        http: pas.http(),
        https: pas.https(),
        shutdown: pas.shutdown(),
    }
}

/// A PASOE name from the project folder: letters and digits, starting with a
/// letter (`my-app` gives `myapp`).
fn default_name(root: &Path) -> String {
    let base: String = root
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let base = base.trim_start_matches(|c: char| c.is_ascii_digit());
    if base.is_empty() {
        "ablpas".into()
    } else {
        base.chars().take(30).collect()
    }
}

fn project(cwd: &Path) -> Result<Loaded, String> {
    config::load_project(cwd)?
        .ok_or_else(|| "no abl.toml here or above (run `abl init` first)".to_string())
}

/// The instance's script for `name` (`tcman`): `.bat` on Windows, `.sh` elsewhere.
fn script(dir: &Path, name: &str) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        ["bat", "exe", "cmd"]
            .iter()
            .map(|e| format!("{name}.{e}"))
            .collect()
    } else {
        vec![format!("{name}.sh"), name.to_string()]
    };
    names
        .into_iter()
        .map(|n| dir.join("bin").join(n))
        .find(|p| p.is_file())
}

fn up(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

fn wait_for(port: u16, want_up: bool, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        if up(port) == want_up {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The HTTP port the instance really uses (its `conf/catalina.properties`),
/// falling back to abl.toml's.
fn http_port(inst: &Instance) -> u16 {
    fs::read_to_string(inst.dir.join("conf").join("catalina.properties"))
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.trim().strip_prefix("psc.as.http.port="))
                .and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or(inst.http)
}

fn url(port: u16) -> String {
    format!("http://localhost:{port}/")
}

/// Sets `key=value` in `[section]` of a properties file's text, adding the
/// key or the section when missing. Other lines are kept as they are.
fn set_property(text: &str, section: &str, key: &str, value: &str) -> String {
    let header = format!("[{section}]");
    let mut out: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut found_section = false;
    let mut done = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if in_section && !done {
                // After the section's last setting, before its blank lines.
                let blanks = out.iter().rev().take_while(|l| l.trim().is_empty()).count();
                out.insert(out.len() - blanks, format!("{key}={value}"));
                done = true;
            }
            in_section = t.eq_ignore_ascii_case(&header);
            found_section |= in_section;
        } else if in_section && !done {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim().eq_ignore_ascii_case(key) {
                    let indent = &line[..line.len() - line.trim_start().len()];
                    out.push(format!("{indent}{key}={value}"));
                    done = true;
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    if in_section && !done {
        out.push(format!("{key}={value}"));
    } else if !found_section {
        if out.last().is_some_and(|l| !l.trim().is_empty()) {
            out.push(String::new());
        }
        out.push(header);
        out.push(format!("{key}={value}"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// The agents' PROPATH: compiled r-code, the project's PROPATH (packages
/// included), then what PASOE needs itself.
fn agent_propath(l: &Loaded) -> String {
    let mut entries: Vec<String> = vec![l.output().display().to_string()];
    entries.extend(l.propath().iter().map(|p| p.display().to_string()));
    entries.extend(
        [
            "${CATALINA_BASE}/webapps/ROOT/WEB-INF/openedge",
            "${CATALINA_BASE}/openedge",
            "${DLC}/tty",
            "${DLC}/tty/netlib/OpenEdge.Net.pl",
        ]
        .map(String::from),
    );
    entries.join(",")
}

fn quote(s: &str) -> String {
    if s.contains(char::is_whitespace) {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

/// The agents' startup parameters: a temp folder, the databases (multi-user:
/// agents share them), then `[pas] startup`. Also the databases that need a
/// server first.
fn agent_startup(l: &Loaded) -> (String, Vec<String>) {
    let mut params = vec!["-T".to_string(), "\"${catalina.base}/temp\"".to_string()];
    let mut not_served = Vec::new();
    for (name, d) in &l.project.db {
        match &d.connect {
            Some(c) => params.extend(split_params(c).iter().map(|p| quote(p))),
            None => {
                let p = l.db_path(name, d);
                if p.is_file() {
                    params.extend([
                        "-db".into(),
                        quote(&p.display().to_string()),
                        "-ld".into(),
                        name.clone(),
                    ]);
                    if !db::in_use(&p) {
                        not_served.push(name.clone());
                    }
                }
            }
        }
    }
    if let Some(s) = &l.project.pas.startup {
        params.push(s.clone());
    }
    (params.join(" "), not_served)
}

/// Points the instance's ABL application at the project.
fn configure(l: &Loaded, inst: &Instance) -> Result<Vec<String>, String> {
    let file = inst.dir.join("conf").join("openedge.properties");
    let text = fs::read_to_string(&file)
        .map_err(|e| format!("{}: {e} (is this a PASOE instance?)", file.display()))?;
    let (startup, not_served) = agent_startup(l);
    let text = set_property(
        &text,
        &format!("AppServer.Agent.{}", inst.name),
        "PROPATH",
        &agent_propath(l),
    );
    let text = set_property(
        &text,
        &format!("AppServer.SessMgr.{}", inst.name),
        "agentStartupParam",
        &startup,
    );
    fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(not_served)
}

fn warn_not_served(names: &[String]) {
    for n in names {
        eprintln!("warning: database {n} has no server; agents share databases, so run `abl db start {n}` first");
    }
}

fn run(mut c: Command, what: &str) -> Result<(), String> {
    let status = c
        .status()
        .map_err(|e| format!("cannot start {what}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed ({status})"))
    }
}

/// Keeps abl's own stdin/stdout/stderr out of the processes it starts. Windows
/// hands a child every inheritable handle, not just the ones asked for, so
/// the server tcman leaves running would hold abl's output pipe open and
/// whoever reads it (a terminal pipe, VS Code, a test) would wait forever.
/// Handles given to a child explicitly are still passed: Rust duplicates them.
#[cfg(windows)]
fn keep_std_handles_private() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(which: u32) -> isize;
        fn SetHandleInformation(handle: isize, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE
    for which in [-10i32, -11, -12] {
        // SAFETY: plain Win32 calls on this process's own standard handles;
        // a null or invalid handle is skipped.
        unsafe {
            let h = GetStdHandle(which as u32);
            if h != 0 && h != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

#[cfg(not(windows))]
fn keep_std_handles_private() {}

/// Runs the instance's tcman with its output in `logs/abl-tcman.log`, shown
/// afterwards. Not through abl's own stdout: the server tcman leaves running
/// would inherit that handle, and whoever reads abl's output would wait on it.
fn tcman(inst: &Instance, env: &Env, action: &str) -> Result<(), String> {
    keep_std_handles_private();
    let exe = script(&inst.dir, "tcman").ok_or_else(|| {
        format!(
            "no PASOE instance in {}: create it with `abl pas create`",
            inst.dir.display()
        )
    })?;
    let logs = inst.dir.join("logs");
    fs::create_dir_all(&logs).map_err(|e| format!("{}: {e}", logs.display()))?;
    let log = logs.join("abl-tcman.log");
    let file = fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = file.try_clone().map_err(|e| e.to_string())?;
    let mut c = Command::new(exe);
    c.arg(action)
        .current_dir(&inst.dir)
        .env("DLC", &env.dlc)
        .env("CATALINA_BASE", &inst.dir)
        .stdin(std::process::Stdio::null())
        .stdout(file)
        .stderr(err);
    let result = run(c, &format!("tcman {action}"));
    let text = fs::read_to_string(&log).unwrap_or_default();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        println!("  {line}");
    }
    result
}

/// `abl pas create`: a new instance from the install's PASOE, set up for the project.
pub fn create(cwd: &Path, flag: Option<&str>, force: bool) -> Result<(), String> {
    let l = project(cwd)?;
    let env = Env::new(Some(&l), flag, cwd)?;
    let inst = instance(&l);
    if inst.dir.exists() {
        if !force {
            return Err(format!(
                "{} already exists: start it with `abl pas start` (or --force to make it again)",
                inst.dir.display()
            ));
        }
        if up(http_port(&inst)) {
            return Err("the instance is running: `abl pas stop` it first".into());
        }
        fs::remove_dir_all(&inst.dir).map_err(|e| format!("{}: {e}", inst.dir.display()))?;
    }
    for port in [inst.http, inst.https, inst.shutdown] {
        if up(port) {
            return Err(format!(
                "port {port} is in use; set another with `port` under [pas] in abl.toml"
            ));
        }
    }
    let pasman = db::tool(&env.dlc, "pasman").ok_or_else(|| {
        format!(
            "pasman not found in {}: does OE {} include PASOE?",
            env.dlc.join("bin").display(),
            env.version
        )
    })?;
    if let Some(parent) = inst.dir.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let mut c = Command::new(pasman);
    c.args(["create", "-v", "-f", "-p"])
        .arg(inst.http.to_string())
        .arg("-P")
        .arg(inst.https.to_string())
        .arg("-s")
        .arg(inst.shutdown.to_string())
        .arg(&inst.dir)
        .current_dir(&l.root)
        .env("DLC", &env.dlc);
    run(c, "pasman create")?;
    let not_served = configure(&l, &inst)?;
    println!(
        "Created PASOE instance {} in {}",
        inst.name,
        inst.dir.display()
    );
    println!(
        "  http {}  https {}  shutdown {}",
        inst.http, inst.https, inst.shutdown
    );
    println!(
        "  ABL application {}: PROPATH and databases from abl.toml",
        inst.name
    );
    warn_not_served(&not_served);
    println!("Start it with `abl pas start`.");
    Ok(())
}

/// `abl pas start`: refreshes the setup from abl.toml, starts the instance and
/// waits until it answers.
pub fn start(cwd: &Path, flag: Option<&str>, wait: u64) -> Result<(), String> {
    let l = project(cwd)?;
    let env = Env::new(Some(&l), flag, cwd)?;
    let inst = instance(&l);
    if script(&inst.dir, "tcman").is_none() {
        return Err(format!(
            "no PASOE instance in {}: create it with `abl pas create`",
            inst.dir.display()
        ));
    }
    let port = http_port(&inst);
    if up(port) {
        println!("{} is already running: {}", inst.name, url(port));
        return Ok(());
    }
    let not_served = configure(&l, &inst)?;
    warn_not_served(&not_served);
    tcman(&inst, &env, "start")?;
    print!("Starting {}...", inst.name);
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    if wait_for(port, true, Duration::from_secs(wait)) {
        println!(" up: {}", url(port));
        Ok(())
    } else {
        println!();
        Err(format!(
            "{} did not answer on port {port} within {wait} s; see the logs in {}",
            inst.name,
            inst.dir.join("logs").display()
        ))
    }
}

/// `abl pas stop`.
pub fn stop(cwd: &Path, flag: Option<&str>, wait: u64) -> Result<(), String> {
    let l = project(cwd)?;
    let env = Env::new(Some(&l), flag, cwd)?;
    let inst = instance(&l);
    if script(&inst.dir, "tcman").is_none() {
        return Err(format!("no PASOE instance in {}", inst.dir.display()));
    }
    let port = http_port(&inst);
    if !up(port) {
        println!("{} is not running.", inst.name);
        return Ok(());
    }
    tcman(&inst, &env, "stop")?;
    if wait_for(port, false, Duration::from_secs(wait)) {
        println!("Stopped {}.", inst.name);
        Ok(())
    } else {
        Err(format!(
            "{} still answers on port {port} after {wait} s",
            inst.name
        ))
    }
}

/// `abl pas status` (and `abl pas` alone).
pub fn status(cwd: &Path) -> Result<(), String> {
    let l = project(cwd)?;
    let inst = instance(&l);
    let row = |k: &str, v: &str| println!("{k:<9}{v}");
    row("name", &inst.name);
    row("path", &inst.dir.display().to_string());
    if script(&inst.dir, "tcman").is_none() {
        row("state", "not created (abl pas create)");
        return Ok(());
    }
    let port = http_port(&inst);
    if up(port) {
        row("state", &format!("running: {}", url(port)));
    } else {
        row("state", "stopped (abl pas start)");
    }
    row(
        "ports",
        &format!(
            "http {port}  https {}  shutdown {}",
            inst.https, inst.shutdown
        ),
    );
    let props =
        fs::read_to_string(inst.dir.join("conf").join("openedge.properties")).unwrap_or_default();
    let section = format!("[AppServer.Agent.{}]", inst.name);
    let propath = props
        .lines()
        .skip_while(|line| !line.trim().eq_ignore_ascii_case(&section))
        .skip(1)
        .take_while(|line| !line.trim().starts_with('['))
        .find_map(|line| line.trim().strip_prefix("PROPATH="));
    if let Some(pp) = propath {
        for (i, e) in pp.split(',').enumerate() {
            row(if i == 0 { "propath" } else { "" }, e);
        }
    }
    row("logs", &inst.dir.join("logs").display().to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_come_from_the_folder() {
        assert_eq!(default_name(Path::new("C:/work/My-App_2")), "myapp2");
        assert_eq!(default_name(Path::new("/x/42crm")), "crm");
        assert_eq!(default_name(Path::new("/x/---")), "ablpas");
    }

    #[test]
    fn properties_are_set_in_their_section() {
        let text = "[AppServer.Agent]\nPROPATH=x\n\n[AppServer.Agent.app]\n    PROPATH=old\nnumInitialSessions=1\n\n[Other]\nPROPATH=keep\n";
        let out = set_property(text, "AppServer.Agent.app", "PROPATH", "new");
        assert_eq!(
            out,
            "[AppServer.Agent]\nPROPATH=x\n\n[AppServer.Agent.app]\n    PROPATH=new\nnumInitialSessions=1\n\n[Other]\nPROPATH=keep\n"
        );
        // A key the section lacks goes at its end; a missing section at the file's end.
        let out = set_property(text, "AppServer.Agent.app", "agentStartupParam", "-T x");
        assert!(
            out.contains("numInitialSessions=1\nagentStartupParam=-T x\n\n[Other]"),
            "{out}"
        );
        let out = set_property("[A]\nk=v\n", "B", "k", "w");
        assert_eq!(out, "[A]\nk=v\n\n[B]\nk=w\n");
    }
}

//! Starting OpenEdge sessions: the environment they get, and the compile and
//! run jobs that go through the drivers in [`crate::driver`].

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::config::{self, Loaded, Source, StartupMode};
use crate::driver;

/// Everything needed to start `_progres` for a project.
#[derive(Clone, Debug)]
pub struct Env {
    pub version: String,
    pub source: Source,
    pub dlc: PathBuf,
    pub propath: Vec<PathBuf>,
    /// Startup parameters: database connections, then `startup` from abl.toml.
    pub params: Vec<String>,
    /// The session's working folder (the project root).
    pub cwd: PathBuf,
    /// A database is connected single-user (`-1`, because no server is
    /// running), so only one session at a time can use it.
    pub single_user: bool,
    /// abl.toml `[[procedures]]`: run before compile, run, test and REPL work.
    pub startup: Vec<(StartupMode, String)>,
}

impl Env {
    /// Resolves the version and install for a project (or the current folder
    /// when there is no abl.toml). Fails with a message saying what to fix.
    pub fn new(loaded: Option<&Loaded>, flag: Option<&str>, cwd: &Path) -> Result<Env, String> {
        let global = config::load_global()?;
        let (version, source) = config::resolve_version(flag, loaded.map(|l| &l.project), &global)
            .ok_or("no OpenEdge version set: set `oe` in abl.toml or run `abl oe use <version>`")?;
        let dlc = config::dlc_for(&global, &version)?.to_path_buf();
        let exe = config::progres(&dlc);
        if !exe.is_file() {
            return Err(format!(
                "{} not found; check the OE {version} install with `abl doctor`",
                exe.display()
            ));
        }
        let mut params = Vec::new();
        let mut single_user = false;
        let startup = loaded
            .map(|l| {
                l.project
                    .procedures
                    .iter()
                    .map(|p| (p.mode, p.path.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let (propath, root) = match loaded {
            Some(l) => {
                for (name, db) in &l.project.db {
                    match &db.connect {
                        Some(c) => params.extend(split_params(c)),
                        // A database abl manages: connected once it exists,
                        // through its server if one runs, else single-user.
                        None => {
                            let p = l.db_path(name, db);
                            if p.is_file() {
                                params.extend([
                                    "-db".to_string(),
                                    p.display().to_string(),
                                    "-ld".to_string(),
                                    name.clone(),
                                ]);
                                if !crate::db::in_use(&p) {
                                    params.push("-1".into());
                                    single_user = true;
                                }
                            }
                        }
                    }
                }
                // .NET: point the session at assemblies.xml when there is one.
                let asm = l.assemblies_dir();
                if asm.join("assemblies.xml").is_file() {
                    params.push("-assemblies".into());
                    params.push(asm.display().to_string());
                }
                if let Some(s) = &l.project.startup {
                    params.extend(split_params(s));
                }
                (l.propath(), l.root.clone())
            }
            None => (vec![cwd.to_path_buf()], cwd.to_path_buf()),
        };
        Ok(Env {
            version,
            source,
            dlc,
            propath,
            params,
            cwd: root,
            single_user,
            startup,
        })
    }

    pub fn propath_string(&self) -> String {
        let sep = if cfg!(windows) { ";" } else { ":" };
        self.propath
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(sep)
    }

    /// A `_progres -b -p <program>` command with this environment.
    fn command(&self, program: &Path, job: &Path, param: Option<&str>) -> Command {
        let mut c = Command::new(config::progres(&self.dlc));
        c.arg("-b").arg("-p").arg(program);
        if let Some(p) = param {
            c.arg("-param").arg(p);
        }
        c.args(&self.params)
            .current_dir(&self.cwd)
            .env("DLC", &self.dlc)
            .env("PROPATH", self.propath_string())
            .env("ABL_JOB", job);
        c
    }

    /// The name to give COMPILE for `file`: relative to the first PROPATH entry
    /// that holds it (so SAVE INTO keeps the folder structure), else absolute.
    pub fn compile_name(&self, file: &Path) -> String {
        for dir in &self.propath {
            if let Ok(rel) = file.strip_prefix(dir) {
                return rel.to_string_lossy().replace('\\', "/");
            }
        }
        file.display().to_string()
    }
}

/// Splits startup parameters on white space, keeping "quoted values" whole.
pub fn split_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// A temporary folder for one job's driver, job and result files.
struct JobDir(PathBuf);

impl JobDir {
    fn new() -> Result<JobDir, String> {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "abl-job-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(JobDir(dir))
    }

    fn write(&self, name: &str, text: &str) -> Result<PathBuf, String> {
        let p = self.0.join(name);
        fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))?;
        Ok(p)
    }

    /// Writes a driver. With startup procedures, the driver first runs
    /// `abl_startup.p`, which runs them (see [`startup_program`]).
    fn driver(&self, env: &Env, name: &str, text: &str) -> Result<PathBuf, String> {
        if env.startup.is_empty() {
            return self.write(name, text);
        }
        let err = self.0.join(STARTUP_ERROR);
        let start = self.write("abl_startup.p", &startup_program(&env.startup, &err))?;
        // After the marker line, so the driver keeps its kind.
        let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
        let call = format!("RUN VALUE({}).", abl_string(&start.display().to_string()));
        self.write(name, &format!("{first}\n{call}\n{rest}"))
    }

    /// The failure `abl_startup.p` wrote, if a startup procedure failed.
    fn startup_error(&self) -> Option<String> {
        let text = fs::read_to_string(self.0.join(STARTUP_ERROR)).ok()?;
        let text = text.trim_end();
        let (step, msg) = text.split_once('\t').unwrap_or((text, ""));
        Some(if msg.is_empty() {
            format!("startup procedure {step} failed")
        } else {
            format!("startup procedure {step} failed: {msg}")
        })
    }
}

const STARTUP_ERROR: &str = "startup-error.txt";

/// An ABL string literal. Paths use `/`, which OpenEdge takes everywhere
/// (a backslash is an escape character on Unix).
pub fn abl_string(s: &str) -> String {
    let s = s.replace('\\', "/").replace('~', "~~").replace('"', "~\"");
    format!("\"{s}\"")
}

/// The procedure that runs the startup procedures in order: RUN, RUN
/// PERSISTENT, or RUN PERSISTENT and SESSION:ADD-SUPER-PROCEDURE. When one
/// fails, it writes `<procedure> TAB <message>` to `err` and ends the session.
pub fn startup_program(procs: &[(StartupMode, String)], err: &Path) -> String {
    let mut body = String::new();
    for (mode, path) in procs {
        let p = abl_string(path);
        body.push_str(&format!("    cStep = {p}.\n"));
        match mode {
            StartupMode::Run => body.push_str(&format!("    RUN VALUE({p}).\n")),
            StartupMode::Persistent => {
                body.push_str(&format!("    RUN VALUE({p}) PERSISTENT SET hProc.\n"))
            }
            StartupMode::Super => {
                body.push_str(&format!("    RUN VALUE({p}) PERSISTENT SET hProc.\n"));
                body.push_str("    SESSION:ADD-SUPER-PROCEDURE(hProc).\n");
            }
        }
    }
    let err = abl_string(&err.display().to_string());
    let mark = driver::MARK_STARTUP;
    format!(
        r#"{mark}
DEFINE VARIABLE hProc AS HANDLE NO-UNDO.
DEFINE VARIABLE cStep AS CHARACTER NO-UNDO.
DEFINE VARIABLE cMsg AS CHARACTER NO-UNDO.
DEFINE VARIABLE lDone AS LOGICAL NO-UNDO.
DEFINE STREAM sErr.

DO ON ERROR UNDO, LEAVE ON STOP UNDO, LEAVE:
{body}    lDone = TRUE.
    CATCH e AS Progress.Lang.Error:
        cMsg = e:GetMessage(1).
    END CATCH.
END.
IF NOT lDone THEN DO:
    IF cMsg = "" THEN cMsg = "a STOP condition".
    OUTPUT STREAM sErr TO VALUE({err}).
    PUT STREAM sErr UNFORMATTED cStep "~t" REPLACE(cMsg, "~n", " ") SKIP.
    OUTPUT STREAM sErr CLOSE.
    QUIT.
END.
"#
    )
}

impl Drop for JobDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A message from the compiler.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileMessage {
    /// The name the unit was compiled as.
    pub unit: String,
    /// The file the message is about (the unit or one of its includes), as the
    /// compiler reports it; empty when unknown.
    pub file: String,
    /// 1-based; 0 when unknown.
    pub line: u32,
    pub column: u32,
    pub number: u32,
    pub error: bool,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct CompileOutcome {
    pub messages: Vec<CompileMessage>,
    /// Each unit and whether it compiled without errors.
    pub done: Vec<(String, bool)>,
}

/// Compiles `names` (as given by [`Env::compile_name`]) in one session. With
/// `save_into`, r-code is saved there; without, the files are only checked.
pub fn compile(
    env: &Env,
    names: &[String],
    save_into: Option<&Path>,
) -> Result<CompileOutcome, String> {
    let dir = JobDir::new()?;
    let program = dir.driver(env, "abl_compile.p", driver::COMPILE)?;
    let result = dir.0.join("result.txt");
    let mut job = format!("result\t{}\n", result.display());
    if let Some(out) = save_into {
        job.push_str(&format!("output\t{}\n", out.display()));
    }
    for n in names {
        job.push_str(&format!("file\t{n}\n"));
    }
    let job = dir.write("job.txt", &job)?;
    let out = env
        .command(&program, &job, None)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot start {}: {e}", config::progres(&env.dlc).display()))?;
    if let Some(e) = dir.startup_error() {
        return Err(e);
    }
    let Ok(text) = fs::read_to_string(&result) else {
        return Err(session_failed(&out));
    };
    let outcome = parse_compile_result(&text);
    if outcome.done.len() < names.len() {
        return Err(format!(
            "the OpenEdge session stopped after {} of {} files\n{}",
            outcome.done.len(),
            names.len(),
            session_failed(&out)
        ));
    }
    Ok(outcome)
}

fn session_failed(out: &std::process::Output) -> String {
    let mut msg = format!(
        "the OpenEdge session ended without a result ({})",
        out.status
    );
    for stream in [&out.stdout, &out.stderr] {
        let s = String::from_utf8_lossy(stream);
        if !s.trim().is_empty() {
            msg.push('\n');
            msg.push_str(s.trim_end());
        }
    }
    msg
}

pub fn parse_compile_result(text: &str) -> CompileOutcome {
    let mut o = CompileOutcome::default();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        let num = |i: usize| f.get(i).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        match f.first().copied() {
            Some("msg") if f.len() >= 8 => o.messages.push(CompileMessage {
                unit: f[1].to_string(),
                file: f[2].to_string(),
                line: num(3),
                column: num(4),
                number: num(5),
                error: f[6] == "error",
                // The text may itself have contained tabs before the driver replaced them.
                text: f[7..].join(" ").trim().to_string(),
            }),
            Some("done") if f.len() >= 3 => o.done.push((f[1].to_string(), f[2].trim() == "ok")),
            _ => {}
        }
    }
    o
}

/// Runs `target` (a procedure on the PROPATH, or a full path) with the user's
/// `-param`. Output goes straight to this process's stdout and stderr. Returns
/// the exit code: 0 ok, 1 the procedure raised an error, 2 a STOP condition.
pub fn run(env: &Env, target: &str, param: Option<&str>) -> Result<i32, String> {
    run_driver(env, "abl_run.p", driver::RUN, target, param, None)
}

/// Like [`run`], with the profiler writing its data to `out`.
pub fn profile(env: &Env, target: &str, param: Option<&str>, out: &Path) -> Result<i32, String> {
    run_driver(
        env,
        "abl_profile.p",
        driver::PROFILE,
        target,
        param,
        Some(out),
    )
}

fn run_driver(
    env: &Env,
    file: &str,
    text: &str,
    target: &str,
    param: Option<&str>,
    out: Option<&Path>,
) -> Result<i32, String> {
    let dir = JobDir::new()?;
    let program = dir.driver(env, file, text)?;
    let result = dir.0.join("result.txt");
    let mut job = format!("result\t{}\ntarget\t{target}\n", result.display());
    if let Some(o) = out {
        job.push_str(&format!("out\t{}\n", o.display()));
    }
    let job = dir.write("job.txt", &job)?;
    let status = env
        .command(&program, &job, param)
        .status()
        .map_err(|e| format!("cannot start {}: {e}", config::progres(&env.dlc).display()))?;
    if let Some(e) = dir.startup_error() {
        return Err(e);
    }
    match fs::read_to_string(&result) {
        Ok(text) => Ok(text
            .lines()
            .find_map(|l| l.strip_prefix("exit\t"))
            .and_then(|c| c.trim().parse().ok())
            .unwrap_or(1)),
        // No result: the procedure ended the session itself (QUIT), so it
        // never returned to the driver. Trust the process status.
        Err(_) => Ok(status.code().unwrap_or(1)),
    }
}

/// One test file for the test driver.
#[derive(Debug, Clone, Default)]
pub struct TestSuite {
    pub id: String,
    /// A class type name, or a procedure's PROPATH name.
    pub name: String,
    pub is_class: bool,
    pub before: Vec<String>,
    pub after: Vec<String>,
    pub setup: Vec<String>,
    pub teardown: Vec<String>,
    /// (case id, method, expected error type or "")
    pub tests: Vec<(String, String, String)>,
}

/// What the test driver wrote, and what the session printed meanwhile.
pub struct TestOutcome {
    pub results: String,
    pub output: String,
    pub status: Option<i32>,
}

/// Runs the tests of `suites` in one session (the test driver).
pub fn test(env: &Env, suites: &[TestSuite]) -> Result<TestOutcome, String> {
    let dir = JobDir::new()?;
    let program = dir.driver(env, "abl_test.p", driver::TEST)?;
    let result = dir.0.join("result.txt");
    let mut job = format!("result\t{}\n", result.display());
    for s in suites {
        job.push_str(&format!(
            "suite\t{}\t{}\t{}\n",
            s.id,
            if s.is_class { "cls" } else { "p" },
            s.name
        ));
        for (role, list) in [
            ("before", &s.before),
            ("after", &s.after),
            ("setup", &s.setup),
            ("teardown", &s.teardown),
        ] {
            for m in list {
                job.push_str(&format!("{role}\t{m}\n"));
            }
        }
        for (id, m, expected) in &s.tests {
            job.push_str(&format!("test\t{m}\t{id}\t{expected}\n"));
        }
    }
    let job = dir.write("job.txt", &job)?;
    let out = env
        .command(&program, &job, None)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot start {}: {e}", config::progres(&env.dlc).display()))?;
    if let Some(e) = dir.startup_error() {
        return Err(e);
    }
    let mut output = String::from_utf8_lossy(&out.stdout).to_string();
    output.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(TestOutcome {
        results: fs::read_to_string(&result).unwrap_or_default(),
        output,
        status: out.status.code(),
    })
}

/// What the REPL session said about one piece of code.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Ok,
    /// The code raised an error (or didn't compile): its messages.
    Error(Vec<String>),
    /// A STOP condition ended it.
    Stop,
}

/// One OpenEdge session that stays up and runs code sent to it (the REPL
/// driver). It connects back to a port abl listens on; its own output goes
/// to this process's stdout.
pub struct Repl {
    child: std::process::Child,
    stream: std::net::TcpStream,
    _dir: JobDir,
}

impl Repl {
    pub fn start(env: &Env) -> Result<Repl, String> {
        use std::net::TcpListener;
        use std::time::{Duration, Instant};
        let listener =
            TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot listen: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let dir = JobDir::new()?;
        let program = dir.driver(env, "abl_repl.p", driver::REPL)?;
        let job = dir.write(
            "job.txt",
            &format!(
                "port\t{port}\ndir\t{}\n",
                dir.0.display().to_string().replace('\\', "/")
            ),
        )?;
        let mut child = env
            .command(&program, &job, None)
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", config::progres(&env.dlc).display()))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(60);
        let stream = loop {
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Ok(Some(status)) = child.try_wait() {
                        if let Some(e) = dir.startup_error() {
                            return Err(e);
                        }
                        return Err(format!(
                            "the OpenEdge session ended before it connected ({status})"
                        ));
                    }
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        return Err("the OpenEdge session didn't connect within a minute".into());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(format!("accepting the session's connection: {e}")),
            }
        };
        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
        Ok(Repl {
            child,
            stream,
            _dir: dir,
        })
    }

    fn send(&mut self, text: &str) -> std::io::Result<()> {
        use std::io::Write;
        self.stream
            .write_all(format!("{:08}", text.len()).as_bytes())?;
        self.stream.write_all(text.as_bytes())?;
        self.stream.flush()
    }

    /// Runs `code` in the session and waits for it to finish.
    pub fn eval(&mut self, code: &str) -> Result<Reply, String> {
        use std::io::Read;
        let lost = |e: std::io::Error| format!("lost the OpenEdge session: {e}");
        self.send(code).map_err(lost)?;
        let mut head = [0u8; 8];
        self.stream.read_exact(&mut head).map_err(lost)?;
        let len: usize = std::str::from_utf8(&head)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or("the OpenEdge session sent a bad reply")?;
        let mut body = vec![0u8; len];
        self.stream.read_exact(&mut body).map_err(lost)?;
        let text = String::from_utf8_lossy(&body).to_string();
        let mut lines = text.lines();
        Ok(match lines.next().unwrap_or("") {
            "ok" => Reply::Ok,
            "stop" => Reply::Stop,
            _ => Reply::Error(lines.map(str::to_string).collect()),
        })
    }

    /// Ends the session.
    pub fn close(mut self) {
        let _ = self.send("");
        for _ in 0..100 {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

impl Drop for Repl {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// What a data driver (load-df, query, import) reported.
pub struct DataOutcome {
    /// Result lines, split on tabs.
    pub lines: Vec<Vec<String>>,
    /// The data file the driver wrote (query), if any.
    pub data: Option<String>,
}

impl DataOutcome {
    /// The `done` line's two counts, or the `error` lines as one message.
    pub fn done(&self) -> Result<(u64, u64), String> {
        let errors: Vec<String> = self
            .lines
            .iter()
            .filter(|l| l.first().map(String::as_str) == Some("error"))
            .map(|l| {
                l.get(2..)
                    .map(|t| t.join(" "))
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            })
            .collect();
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }
        let done = self
            .lines
            .iter()
            .find(|l| l.first().map(String::as_str) == Some("done"))
            .ok_or("the OpenEdge session reported nothing")?;
        let n = |i: usize| done.get(i).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        Ok((n(1), n(2)))
    }

    /// The first `error` line's number field (the row, for import).
    pub fn error_at(&self) -> Option<u64> {
        self.lines
            .iter()
            .find(|l| l.first().map(String::as_str) == Some("error"))
            .and_then(|l| l.get(1)?.trim().parse().ok())
    }
}

/// Runs one of the data drivers. `job` holds the driver's own job lines;
/// `data` is written to a data file named in the job (`data` line), and the
/// file the driver writes (`out` line) comes back in the outcome.
pub fn data_job(
    env: &Env,
    file: &str,
    driver: &str,
    job: &[(&str, String)],
    data: Option<&str>,
) -> Result<DataOutcome, String> {
    let dir = JobDir::new()?;
    let program = dir.write(file, driver)?;
    let result = dir.0.join("result.txt");
    let out = dir.0.join("out.txt");
    let mut text = format!("result\t{}\nout\t{}\n", result.display(), out.display());
    if let Some(d) = data {
        let p = dir.write("data.txt", d)?;
        text.push_str(&format!("data\t{}\n", p.display()));
    }
    for (k, v) in job {
        text.push_str(&format!("{k}\t{}\n", v.replace(['\t', '\n', '\r'], " ")));
    }
    let job = dir.write("job.txt", &text)?;
    let output = env
        .command(&program, &job, None)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot start {}: {e}", config::progres(&env.dlc).display()))?;
    let Ok(result) = fs::read_to_string(&result) else {
        return Err(session_failed(&output));
    };
    Ok(DataOutcome {
        lines: result
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.split('\t').map(str::to_string).collect())
            .collect(),
        data: fs::read_to_string(&out).ok(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_split_with_quotes() {
        assert_eq!(
            split_params(r#"-db sports -H localhost  -S 20000 -U "a b" -P """#),
            [
                "-db",
                "sports",
                "-H",
                "localhost",
                "-S",
                "20000",
                "-U",
                "a b",
                "-P",
                ""
            ]
        );
    }

    #[test]
    fn compile_result_lines() {
        let o = parse_compile_result(
            "msg\tsrc/a.p\tsrc/a.p\t3\t5\t247\terror\t** Unable to understand after -- \"x\". (247)\n\
             msg\tsrc/a.p\tinc/b.i\t1\t1\t0\twarning\tsomething\n\
             done\tsrc/a.p\terror\n\
             done\tsrc/b.p\tok\n",
        );
        assert_eq!(o.messages.len(), 2);
        assert_eq!(o.messages[0].line, 3);
        assert_eq!(o.messages[0].number, 247);
        assert!(o.messages[0].error);
        assert!(!o.messages[1].error);
        assert_eq!(
            o.done,
            [("src/a.p".into(), false), ("src/b.p".into(), true)]
        );
    }
}

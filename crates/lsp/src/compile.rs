//! Compile on save: when a compile unit is saved and the project has a usable
//! OpenEdge install, the real compiler checks it (without saving r-code) and its
//! messages are shown next to the parser's diagnostics.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use abl_core::analysis::Analysis;
use abl_core::workspace::{key, Workspace};
use abl_oe::config;
use abl_oe::session::{self, CompileMessage, Env};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Position, Range};

pub fn is_unit(p: &Path) -> bool {
    abl_core::workspace::is_compile_unit(p)
}

/// Compiler diagnostics per file. Each edit bumps the file's generation and
/// drops its results, so a compile that finishes after the user typed on is
/// thrown away instead of pointing at moved lines.
pub struct Compiled {
    pub enabled: AtomicBool,
    state: Mutex<HashMap<String, (u64, Vec<Diagnostic>)>>,
    last_error: Mutex<Option<String>>,
}

impl Default for Compiled {
    fn default() -> Self {
        Compiled {
            enabled: AtomicBool::new(true),
            state: Mutex::new(HashMap::new()),
            last_error: Mutex::new(None),
        }
    }
}

impl Compiled {
    pub fn changed(&self, path: &Path) {
        let mut s = self.state.lock().unwrap();
        let e = s.entry(key(path)).or_default();
        e.0 += 1;
        e.1.clear();
    }

    pub fn generation(&self, path: &Path) -> u64 {
        self.state
            .lock()
            .unwrap()
            .get(&key(path))
            .map_or(0, |e| e.0)
    }

    pub fn get(&self, path: &Path) -> Vec<Diagnostic> {
        self.state
            .lock()
            .unwrap()
            .get(&key(path))
            .map(|e| e.1.clone())
            .unwrap_or_default()
    }

    /// Stores results from a compile started at generation `gen`. Returns
    /// false (and stores nothing) if the file changed since.
    pub fn store(&self, path: &Path, gen: u64, diags: Vec<Diagnostic>) -> bool {
        let mut s = self.state.lock().unwrap();
        let e = s.entry(key(path)).or_default();
        if e.0 != gen {
            return false;
        }
        e.1 = diags;
        true
    }

    /// True the first time a given error is seen, so it is logged once.
    pub fn new_error(&self, e: &str) -> bool {
        let mut last = self.last_error.lock().unwrap();
        if last.as_deref() == Some(e) {
            return false;
        }
        *last = Some(e.to_string());
        true
    }

    pub fn clear_error(&self) {
        *self.last_error.lock().unwrap() = None;
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
}

/// Compiles `path` with the project's OpenEdge (check only). Blocking.
pub fn run(path: &Path) -> Result<(Env, Vec<CompileMessage>), String> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let loaded = config::load_project(dir)?;
    let env = Env::new(loaded.as_ref(), None, dir)?;
    let name = env.compile_name(path);
    let out = session::compile(&env, &[name], None)?;
    Ok((env, out.messages))
}

fn resolve(env: &Env, file: &str) -> Option<PathBuf> {
    let p = Path::new(file);
    if p.is_absolute() {
        return Some(p.to_path_buf());
    }
    env.propath.iter().map(|d| d.join(p)).find(|c| c.is_file())
}

/// The word at `pos`, or one character, so the squiggle is visible.
fn word_range(ana: &Analysis, pos: Position) -> Range {
    let start = ana.lines.offset(&ana.text, pos);
    let rest = &ana.text[start.min(ana.text.len())..];
    let mut len: usize = rest
        .char_indices()
        .take_while(|(_, c)| c.is_alphanumeric() || matches!(c, '-' | '_' | '&' | '{' | '}'))
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);
    if len == 0 {
        len = rest
            .chars()
            .next()
            .filter(|c| *c != '\n' && *c != '\r')
            .map_or(0, char::len_utf8);
    }
    ana.lines.range(&ana.text, start, start + len)
}

/// Turns compiler messages into diagnostics on `path`. Messages about an
/// include file are shown on the line that includes it.
pub fn diagnostics(
    ws: &Workspace,
    env: &Env,
    path: &Path,
    ana: &Analysis,
    messages: &[CompileMessage],
) -> Vec<Diagnostic> {
    let me = key(path);
    messages
        .iter()
        .map(|m| {
            let file = if m.file.is_empty() { &m.unit } else { &m.file };
            let target = resolve(env, file);
            let here = target.as_deref().map(key).is_none_or(|k| k == me);
            let (range, message) = if here {
                let pos = Position::new(m.line.saturating_sub(1), m.column.saturating_sub(1));
                (word_range(ana, pos), m.text.clone())
            } else {
                let tk = target.as_deref().map(key);
                let site = ana
                    .includes
                    .iter()
                    .find(|r| ws.resolve_file(&r.target, Some(path)).as_deref().map(key) == tk);
                let range = site.map_or_else(
                    || Range::new(Position::new(0, 0), Position::new(0, 0)),
                    |r| ana.lines.range(&ana.text, r.start, r.end),
                );
                (
                    range,
                    format!("{}:{}: {}", file.replace('\\', "/"), m.line, m.text),
                )
            };
            Diagnostic {
                range,
                severity: Some(if m.error {
                    DiagnosticSeverity::ERROR
                } else {
                    DiagnosticSeverity::WARNING
                }),
                code: (m.number > 0)
                    .then(|| NumberOrString::Number(i32::try_from(m.number).unwrap_or(0))),
                source: Some("abl compiler".into()),
                message,
                ..Default::default()
            }
        })
        .collect()
}

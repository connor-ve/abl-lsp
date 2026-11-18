//! Open documents, on-disk file cache, PROPATH resolution and the workspace symbol index.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use lsp_types::Range;
use walkdir::WalkDir;

use crate::analysis::{analyze, analyze_env, Analysis, SymKind};
use crate::lexer::{DefKind, Defines, IncludeCtx, PreEnv};

/// Map key for a path: case-insensitive on Windows.
pub fn key(p: &Path) -> String {
    let s = p.to_string_lossy().to_string();
    if cfg!(windows) {
        s.replace('/', "\\").to_lowercase()
    } else {
        s
    }
}

pub fn ext(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Extensions a project adds (abl.toml `[extensions]`): compiled, included.
static EXTRA_EXTENSIONS: std::sync::RwLock<(Vec<String>, Vec<String>)> =
    std::sync::RwLock::new((Vec::new(), Vec::new()));

/// Adds extensions (lower case, no dot) to those recognized as ABL source:
/// `main` are compiled like .p, `include` are included like .i.
pub fn add_extensions(main: &[String], include: &[String]) {
    if let Ok(mut e) = EXTRA_EXTENSIONS.write() {
        let (m, i) = &mut *e;
        for (list, new) in [(m, main), (i, include)] {
            for x in new {
                if !list.contains(x) {
                    list.push(x.clone());
                }
            }
        }
    }
}

fn extra(include: bool, e: &str) -> bool {
    EXTRA_EXTENSIONS
        .read()
        .is_ok_and(|x| if include { &x.1 } else { &x.0 }.iter().any(|v| v == e))
}

pub fn is_abl_file(p: &Path) -> bool {
    is_compile_unit(p) || is_include_file(p)
}

pub fn is_include_file(p: &Path) -> bool {
    let e = ext(p);
    e == "i" || extra(true, &e)
}

/// A file that is compiled on its own (not an include).
pub fn is_compile_unit(p: &Path) -> bool {
    let e = ext(p);
    matches!(e.as_str(), "p" | "w" | "cls" | "t") || (!is_include_file(p) && extra(false, &e))
}

/// Reads a source file. Falls back to Latin-1 for non-UTF-8 files, which is what
/// most ABL code bases use; that keeps one char per byte so columns line up.
pub fn read_text(p: &Path) -> Option<String> {
    let bytes = std::fs::read(p).ok()?;
    Some(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&b| b as char).collect(),
    })
}

#[derive(Clone, Debug)]
pub struct IndexSym {
    pub name: String,
    pub kind: SymKind,
    pub container: Option<String>,
    pub range: Range,
}

pub fn summarize(a: &Analysis) -> Vec<IndexSym> {
    use SymKind::*;
    a.symbols
        .iter()
        .filter(|s| {
            !s.proto
                && match s.kind {
                    Procedure | Function | Class | Interface | Enum | Method | Constructor
                    | Property | Event => true,
                    TempTable => s.scope == 0,
                    _ => false,
                }
        })
        .map(|s| IndexSym {
            name: s.name.clone(),
            kind: s.kind,
            container: s.parent.map(|p| a.symbols[p].name.clone()),
            range: a.lines.range(&a.text, s.sel_start, s.sel_end),
        })
        .collect()
}

fn skip_dir(e: &walkdir::DirEntry) -> bool {
    if e.depth() == 0 || !e.file_type().is_dir() {
        return false;
    }
    let n = e.file_name().to_string_lossy();
    (n.starts_with('.') && n.len() > 1) || n == "node_modules" || n == "target"
}

pub fn is_schema_file(p: &Path) -> bool {
    ext(p) == "df"
}

pub struct ScanResult {
    pub files: Vec<(PathBuf, Vec<IndexSym>)>,
    pub schema_files: Vec<PathBuf>,
}

/// Parses every ABL file under the roots and returns their index summaries,
/// plus any .df schema files found along the way.
pub fn scan(roots: &[PathBuf]) -> ScanResult {
    const MAX_FILES: usize = 50_000;
    let mut out = ScanResult {
        files: Vec::new(),
        schema_files: Vec::new(),
    };
    for root in roots {
        for entry in WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| !skip_dir(e))
            .filter_map(Result::ok)
        {
            if out.files.len() >= MAX_FILES {
                return out;
            }
            let p = entry.path();
            if !entry.file_type().is_file() {
                continue;
            }
            if is_schema_file(p) {
                out.schema_files.push(p.to_path_buf());
            } else if is_abl_file(p) {
                if let Some(text) = read_text(p) {
                    let a = analyze(text, is_include_file(p));
                    out.files.push((p.to_path_buf(), summarize(&a)));
                }
            }
        }
    }
    out
}

/// A loaded .df file: logical database name (the file stem) and its tables.
pub struct SchemaFile {
    pub path: PathBuf,
    pub db: String,
    pub ana: Arc<Analysis>,
}

fn load_schema(p: &Path) -> Option<SchemaFile> {
    let text = read_text(p)?;
    let db = p.file_stem()?.to_string_lossy().to_string();
    let ana = Arc::new(crate::schema::analyze_df(text, &db));
    Some(SchemaFile {
        path: p.to_path_buf(),
        db,
        ana,
    })
}

/// A cached analysis and the file modification time it was made from.
type Cached = (Option<SystemTime>, Arc<Analysis>);

pub struct Workspace {
    pub roots: Vec<PathBuf>,
    pub propath: Vec<PathBuf>,
    pub unresolved_include_diags: bool,
    pub lint: crate::lint::LintConfig,
    open: HashMap<String, Arc<Analysis>>,
    cache: Mutex<HashMap<String, Cached>>,
    /// Include files read with a particular include context (arguments and defined names).
    ctx_cache: Mutex<HashMap<(String, u64), Cached>>,
    /// Names from the `abl.preprocessor` setting, defined before every file.
    pub predefined: Defines,
    /// `abl.format.keywordCase`.
    pub keyword_case: crate::format::KeywordCase,
    index: HashMap<String, (PathBuf, Vec<IndexSym>)>,
    schema: Vec<SchemaFile>,
    /// .df files from the `abl.schema` setting; when empty, discovered files are used.
    schema_setting: Vec<PathBuf>,
    schema_discovered: Vec<PathBuf>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            roots: Vec::new(),
            propath: Vec::new(),
            unresolved_include_diags: true,
            lint: Default::default(),
            open: HashMap::new(),
            cache: Mutex::new(HashMap::new()),
            ctx_cache: Mutex::new(HashMap::new()),
            predefined: Defines::default(),
            keyword_case: Default::default(),
            index: HashMap::new(),
            schema: Vec::new(),
            schema_setting: Vec::new(),
            schema_discovered: Vec::new(),
        }
    }

    fn resolve_setting_path(&self, e: &str) -> PathBuf {
        let p = PathBuf::from(e.trim());
        match (self.roots.first(), p.is_absolute()) {
            (Some(r), false) => r.join(p),
            _ => p,
        }
    }

    pub fn set_schema_setting(&mut self, entries: Vec<String>) {
        self.schema_setting = entries
            .iter()
            .filter(|e| !e.trim().is_empty())
            .map(|e| self.resolve_setting_path(e))
            .collect();
        self.reload_schema();
    }

    pub fn set_discovered_schema(&mut self, files: Vec<PathBuf>) {
        self.schema_discovered = files;
        self.reload_schema();
    }

    fn reload_schema(&mut self) {
        let files = if self.schema_setting.is_empty() {
            &self.schema_discovered
        } else {
            &self.schema_setting
        };
        self.schema = files.iter().filter_map(|p| load_schema(p)).collect();
    }

    pub fn schema(&self) -> &[SchemaFile] {
        &self.schema
    }

    /// Is `name` the logical name of a loaded database (`sports2000` in `sports2000.customer`)?
    pub fn is_db(&self, name: &str) -> bool {
        self.schema.iter().any(|s| s.db.eq_ignore_ascii_case(name))
    }

    /// PROPATH entries; relative entries are resolved against the first workspace folder.
    pub fn set_propath(&mut self, entries: Vec<String>) {
        let propath: Vec<PathBuf> = entries
            .iter()
            .map(|e| e.trim())
            .filter(|e| !e.is_empty() && *e != ".")
            .map(|e| self.resolve_setting_path(e))
            .collect();
        if propath != self.propath {
            self.propath = propath;
            // Includes resolve differently now.
            self.forget_cached();
            self.reanalyze_open();
        }
    }

    /// `abl.preprocessor`: `{ "NAME": "value" }`.
    pub fn set_predefined(&mut self, v: &serde_json::Value) {
        let mut d = Defines::default();
        if let Some(map) = v.as_object() {
            for (k, val) in map {
                let text = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                d.set(k, DefKind::Global, text);
            }
        }
        self.predefined = d;
        self.forget_cached();
    }

    /// Analyses a file, reading its include files for preprocessor names.
    pub fn analyze_file(&self, path: &Path, text: String, ctx: Option<&IncludeCtx>) -> Analysis {
        let from = path.to_path_buf();
        let resolve = |t: &str| {
            self.resolve_file(t, Some(&from))
                .and_then(|p| read_text(&p))
        };
        let mut c = ctx.cloned().unwrap_or_default();
        if ctx.is_none() {
            c.defines = self.predefined.clone();
        }
        analyze_env(
            text,
            is_include_file(path),
            &PreEnv {
                ctx: c,
                resolve: Some(&resolve),
            },
        )
    }

    fn forget_cached(&mut self) {
        self.cache.lock().unwrap().clear();
        self.ctx_cache.lock().unwrap().clear();
    }

    /// Re-analyses the open documents (after an include file or a setting changed).
    pub fn reanalyze_open(&mut self) {
        let docs: Vec<(String, Arc<Analysis>)> = self
            .open
            .iter()
            .map(|(k, a)| (k.clone(), a.clone()))
            .collect();
        for (k, a) in docs {
            let Some(p) = self.index.get(&k).map(|(p, _)| p.clone()) else {
                continue;
            };
            let fresh = Arc::new(self.analyze_file(&p, a.text.clone(), None));
            self.open.insert(k, fresh);
        }
    }

    pub fn open_doc(&mut self, path: &Path, text: String) -> Arc<Analysis> {
        let a = Arc::new(self.analyze_file(path, text, None));
        let k = key(path);
        self.index
            .insert(k.clone(), (path.to_path_buf(), summarize(&a)));
        self.open.insert(k, a.clone());
        a
    }

    pub fn close_doc(&mut self, path: &Path) {
        let k = key(path);
        self.open.remove(&k);
        self.cache.lock().unwrap().remove(&k);
    }

    pub fn is_open(&self, path: &Path) -> bool {
        self.open.contains_key(&key(path))
    }

    /// Analysis of an open document, or of the file on disk (cached by mtime).
    pub fn get(&self, path: &Path) -> Option<Arc<Analysis>> {
        let k = key(path);
        if let Some(a) = self.open.get(&k) {
            return Some(a.clone());
        }
        if let Some(s) = self.schema.iter().find(|s| key(&s.path) == k) {
            return Some(s.ana.clone());
        }
        if is_schema_file(path) {
            return None;
        }
        let mtime = std::fs::metadata(path).ok()?.modified().ok();
        let cache = self.cache.lock().unwrap();
        if let Some((m, a)) = cache.get(&k) {
            if *m == mtime {
                return Some(a.clone());
            }
        }
        drop(cache);
        let text = read_text(path)?;
        let a = Arc::new(self.analyze_file(path, text, None));
        self.cache.lock().unwrap().insert(k, (mtime, a.clone()));
        Some(a)
    }

    /// An include file as seen from one include reference (its arguments and the
    /// names defined there).
    pub fn get_included(&self, path: &Path, ctx: &IncludeCtx) -> Option<Arc<Analysis>> {
        if self.open.contains_key(&key(path)) && ctx.positional.is_empty() && ctx.defines.is_empty()
        {
            return self.get(path);
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ctx.hash(&mut h);
        if let Some(open) = self.open.get(&key(path)) {
            open.text.hash(&mut h);
        }
        let ck = (key(path), h.finish());
        let mtime = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
        if let Some((m, a)) = self.ctx_cache.lock().unwrap().get(&ck) {
            if *m == mtime {
                return Some(a.clone());
            }
        }
        let text = match self.open.get(&key(path)) {
            Some(a) => a.text.clone(),
            None => read_text(path)?,
        };
        let a = Arc::new(self.analyze_file(path, text, Some(ctx)));
        let mut cache = self.ctx_cache.lock().unwrap();
        if cache.len() > 2000 {
            cache.clear();
        }
        cache.insert(ck, (mtime, a.clone()));
        Some(a)
    }

    pub fn file_changed(&mut self, path: &Path, deleted: bool) {
        let k = key(path);
        if is_schema_file(path) {
            self.schema_discovered.retain(|p| key(p) != k);
            if !deleted {
                self.schema_discovered.push(path.to_path_buf());
            }
            self.reload_schema();
            return;
        }
        self.cache.lock().unwrap().remove(&k);
        if is_include_file(path) {
            // Files that include it may define or expand differently now.
            self.forget_cached();
            self.reanalyze_open();
        }
        if deleted {
            self.index.remove(&k);
        } else if !self.open.contains_key(&k) && is_abl_file(path) {
            if let Some(a) = self.get(path) {
                self.index.insert(k, (path.to_path_buf(), summarize(&a)));
            }
        }
    }

    pub fn set_index(&mut self, entries: Vec<(PathBuf, Vec<IndexSym>)>) {
        for (p, syms) in entries {
            let k = key(&p);
            if !self.open.contains_key(&k) {
                self.index.insert(k, (p, syms));
            }
        }
    }

    pub fn index(&self) -> impl Iterator<Item = (&PathBuf, &Vec<IndexSym>)> {
        self.index.values().map(|(p, s)| (p, s))
    }

    fn search_dirs(&self, from: Option<&Path>) -> Vec<PathBuf> {
        let mut dirs = if self.propath.is_empty() {
            self.roots.clone()
        } else {
            self.propath.clone()
        };
        if let Some(dir) = from.and_then(Path::parent) {
            dirs.push(dir.to_path_buf());
        }
        dirs
    }

    /// Finds `rel` on the PROPATH (then next to `from`).
    pub fn resolve_file(&self, rel: &str, from: Option<&Path>) -> Option<PathBuf> {
        let rel = rel.trim().trim_matches(|c| c == '"' || c == '\'');
        if rel.is_empty() {
            return None;
        }
        let p = Path::new(rel);
        if p.is_absolute() {
            return p.is_file().then(|| p.to_path_buf());
        }
        self.search_dirs(from)
            .into_iter()
            .map(|d| d.join(rel))
            .find(|c| c.is_file())
    }

    /// RUN target: `x.r` and extensionless names also try `x.p` and `x.w`.
    pub fn resolve_run(&self, target: &str, from: Option<&Path>) -> Option<PathBuf> {
        if let Some(p) = self.resolve_file(target, from) {
            return Some(p);
        }
        let e = ext(Path::new(target));
        let stem = match e.as_str() {
            "r" => &target[..target.len() - 2],
            "" => target,
            _ => return None,
        };
        ["p", "w"]
            .iter()
            .find_map(|x| self.resolve_file(&format!("{stem}.{x}"), from))
    }

    /// Finds the .cls file for a class name, using USING statements for short names.
    pub fn resolve_class(&self, name: &str, usings: &[String]) -> Option<PathBuf> {
        let name = name.trim();
        if name.is_empty() || name.contains(['/', '\\', ':']) {
            return None;
        }
        let mut candidates = Vec::new();
        if name.contains('.') {
            candidates.push(name.replace('.', "/"));
        } else {
            for u in usings {
                if let Some(pkg) = u.strip_suffix(".*") {
                    candidates.push(format!("{}/{}", pkg.replace('.', "/"), name));
                } else if u
                    .rsplit('.')
                    .next()
                    .is_some_and(|last| last.eq_ignore_ascii_case(name))
                {
                    candidates.push(u.replace('.', "/"));
                }
            }
            candidates.push(name.to_string());
            // Progress.Lang.* is always in scope.
            candidates.push(format!("Progress/Lang/{name}"));
        }
        candidates
            .iter()
            .find_map(|c| self.resolve_file(&format!("{c}.cls"), None))
            .or_else(|| {
                candidates
                    .iter()
                    .find_map(|c| crate::builtins::class_file(c))
            })
    }

    /// Files included by `path`, transitively (breadth first, depth limited).
    pub fn includes_of(&self, path: &Path, ana: &Analysis) -> Vec<(PathBuf, Arc<Analysis>)> {
        if ana.includes.is_empty() {
            return Vec::new();
        }
        ana.include_cache
            .get_or_init(|| self.resolve_includes(path, ana))
            .clone()
    }

    fn resolve_includes(&self, path: &Path, ana: &Analysis) -> Vec<(PathBuf, Arc<Analysis>)> {
        let mut out = Vec::new();
        let mut seen: HashSet<(String, u64)> = HashSet::from([(key(path), 0)]);
        type Refs = Vec<(String, Option<Arc<IncludeCtx>>)>;
        let refs = |a: &Analysis| -> Refs {
            a.includes
                .iter()
                .map(|r| (r.target.clone(), r.ctx.clone()))
                .collect()
        };
        let mut queue: VecDeque<(PathBuf, Refs, usize)> = VecDeque::new();
        queue.push_back((path.to_path_buf(), refs(ana), 0));
        while let Some((from, targets, depth)) = queue.pop_front() {
            if depth >= 8 {
                continue;
            }
            for (t, ctx) in targets {
                let Some(p) = self.resolve_file(&t, Some(&from)) else {
                    continue;
                };
                // The same include with other arguments is another set of definitions.
                let ctx_hash = ctx.as_ref().map_or(0, |c| {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    (c.positional.as_slice(), c.defines.args()).hash(&mut h);
                    h.finish()
                });
                if !seen.insert((key(&p), ctx_hash)) {
                    continue;
                }
                let a = match &ctx {
                    Some(c) => self.get_included(&p, c),
                    None => self.get(&p),
                };
                if let Some(a) = a {
                    queue.push_back((p.clone(), refs(&a), depth + 1));
                    out.push((p, a));
                }
            }
        }
        out
    }

    /// Path relative to the first workspace folder that contains it, for display.
    pub fn display_path(&self, p: &Path) -> String {
        for r in &self.roots {
            if let Ok(rel) = p.strip_prefix(r) {
                return rel.to_string_lossy().replace('\\', "/");
            }
        }
        p.to_string_lossy().to_string()
    }
}

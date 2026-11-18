//! What the source commands (lint, fmt, deps) share: an analysis workspace set
//! up from abl.toml, the set of files to work on, and reading and writing files
//! in the encoding they came in.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use abl_core::analysis::Analysis;
use abl_core::lexer::IncludeCtx;
use abl_core::workspace::{self, is_abl_file, is_include_file, key, Workspace};

use crate::config::{self, Loaded};

pub struct Project {
    pub loaded: Option<Loaded>,
    pub ws: Workspace,
    /// Every ABL source file under the root (minus excluded folders), sorted.
    pub files: Vec<PathBuf>,
    /// For each include file (by [`key`]): the arguments and defined names of
    /// the first place in the project that includes it.
    include_ctx: HashMap<String, Arc<IncludeCtx>>,
}

impl Project {
    /// Loads abl.toml (if any) and indexes the project, the way the editor does.
    pub fn open(cwd: &Path) -> Result<Project, String> {
        let loaded = config::load_project(cwd)?;
        let root = loaded
            .as_ref()
            .map_or_else(|| cwd.to_path_buf(), |l| l.root.clone());
        let mut ws = Workspace::new();
        // Personal default from ~/.abl/config.toml; the project's settings win.
        if let Some(k) = config::load_global()
            .ok()
            .and_then(|g| g.format.keyword_case)
        {
            ws.keyword_case = abl_core::format::KeywordCase::parse(&k);
        }
        ws.roots.push(root.clone());
        let mut excluded: Vec<PathBuf> = Vec::new();
        if let Some(l) = &loaded {
            let p = &l.project;
            let (main, include) = p.extensions.normalized();
            workspace::add_extensions(&main, &include);
            let mut propath = p.propath.clone();
            if propath.is_empty() && !l.project.deps.is_empty() {
                propath.push(".".into());
            }
            propath.extend(l.package_propath());
            ws.set_propath(propath);
            let lint = to_json(&p.lint)?;
            let unknown = ws.lint.apply(&lint);
            if !unknown.is_empty() {
                return Err(format!(
                    "{}: unknown [lint] entries: {}",
                    l.file.display(),
                    unknown.join(", ")
                ));
            }
            if !p.preprocessor.is_empty() {
                ws.set_predefined(&to_json(&p.preprocessor)?);
            }
            if let Some(k) = &p.format.keyword_case {
                ws.keyword_case = abl_core::format::KeywordCase::parse(k);
            }
            let schemas: Vec<String> = p.db.values().filter_map(|d| d.schema.clone()).collect();
            if !schemas.is_empty() {
                ws.set_schema_setting(schemas);
            }
            excluded.extend(p.exclude.iter().map(|e| l.path(e)));
            // Installed packages are someone else's code: on the PROPATH, not linted.
            excluded.push(l.path(config::VENDOR_DIR));
            if let Some(o) = &p.output {
                excluded.push(l.path(o));
            }
        } else if let Some(s) = vscode_settings(&root) {
            // No abl.toml: use the editor's settings, so CLI and editor agree.
            let list = |v: &serde_json::Value| -> Vec<String> {
                match v {
                    serde_json::Value::String(s) => s.split([';', ',']).map(String::from).collect(),
                    serde_json::Value::Array(a) => a
                        .iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect(),
                    _ => Vec::new(),
                }
            };
            if let Some(p) = s.get("abl.propath") {
                ws.set_propath(list(p));
            }
            if let Some(l) = s.get("abl.lint") {
                ws.lint.apply(l);
            }
            if let Some(d) = s.get("abl.preprocessor") {
                ws.set_predefined(d);
            }
            if let Some(k) = s.get("abl.format.keywordCase").and_then(|k| k.as_str()) {
                ws.keyword_case = abl_core::format::KeywordCase::parse(k);
            }
            if let Some(sc) = s.get("abl.schema") {
                ws.set_schema_setting(list(sc));
            }
        }
        let scan = workspace::scan(std::slice::from_ref(&root));
        let is_excluded = |p: &Path| {
            let k = key(p);
            excluded.iter().any(|e| k.starts_with(&key(e)))
        };
        let mut files: Vec<PathBuf> = scan
            .files
            .iter()
            .map(|(p, _)| p.clone())
            .filter(|p| !is_excluded(p))
            .collect();
        files.sort();
        ws.set_index(scan.files);
        ws.set_discovered_schema(scan.schema_files);
        let include_ctx = include_contexts(&ws, &files);
        Ok(Project {
            loaded,
            ws,
            files,
            include_ctx,
        })
    }

    /// Analyses `text` as the contents of `path`. An include file is read the
    /// way it is used: with the arguments (`{1}`, `&key=...`) and names defined
    /// where the project first includes it. Read on its own, a parameterised
    /// include is usually not valid code.
    pub fn analyze(&self, path: &Path, text: String) -> Analysis {
        let ctx = self.include_ctx.get(&key(path)).map(Arc::as_ref);
        self.ws.analyze_file(path, text, ctx)
    }

    /// The files a command works on: those named (folders expanded), or every
    /// project file when none are named.
    pub fn select(&self, paths: &[PathBuf], cwd: &Path) -> Result<Vec<PathBuf>, String> {
        if paths.is_empty() {
            return Ok(self.files.clone());
        }
        let mut out = Vec::new();
        for p in paths {
            let abs = if p.is_absolute() {
                p.clone()
            } else {
                cwd.join(p)
            };
            if abs.is_dir() {
                let k = key(&abs);
                let inside: Vec<PathBuf> = self
                    .files
                    .iter()
                    .filter(|f| key(f).starts_with(&k))
                    .cloned()
                    .collect();
                if inside.is_empty() {
                    // A folder outside the project: walk it directly.
                    for e in walkdir::WalkDir::new(&abs)
                        .into_iter()
                        .filter_map(Result::ok)
                    {
                        if e.file_type().is_file() && is_abl_file(e.path()) {
                            out.push(e.into_path());
                        }
                    }
                } else {
                    out.extend(inside);
                }
            } else if abs.is_file() {
                out.push(abs);
            } else {
                return Err(format!("{}: no such file or folder", p.display()));
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// `p` relative to the current folder when it is inside it, for messages.
    pub fn show(&self, p: &Path, cwd: &Path) -> String {
        let rel = p.strip_prefix(cwd).unwrap_or(p);
        rel.display().to_string().replace('\\', "/")
    }
}

/// The first include reference (in file order) for every include in the project,
/// following includes of includes.
fn include_contexts(ws: &Workspace, files: &[PathBuf]) -> HashMap<String, Arc<IncludeCtx>> {
    let mut out: HashMap<String, Arc<IncludeCtx>> = HashMap::new();
    let mut record = |from: &Path, a: &Analysis| {
        for r in &a.includes {
            let (Some(ctx), Some(p)) = (&r.ctx, ws.resolve_file(&r.target, Some(from))) else {
                continue;
            };
            out.entry(key(&p)).or_insert_with(|| ctx.clone());
        }
    };
    for f in files.iter().filter(|f| !is_include_file(f)) {
        let Some(a) = ws.get(f) else { continue };
        record(f, &a);
        for (ip, ia) in ws.includes_of(f, &a) {
            record(&ip, &ia);
        }
    }
    out
}

/// `.vscode/settings.json` in `root`, if it exists and parses.
fn vscode_settings(root: &Path) -> Option<serde_json::Value> {
    let text = fs::read_to_string(root.join(".vscode").join("settings.json")).ok()?;
    serde_json::from_str(&strip_jsonc(&text)).ok()
}

/// Removes `//` and `/* */` comments and trailing commas, which VS Code allows
/// in settings files and JSON doesn't. Comments go first, so a comma followed
/// by a comment and then `}` is still seen as trailing.
fn strip_jsonc(text: &str) -> String {
    strip_trailing_commas(&strip_comments(text))
}

/// Calls `other` for every character outside a string; characters inside
/// strings (with their escapes) are copied as they are.
fn outside_strings(
    text: &str,
    mut other: impl FnMut(char, &mut std::iter::Peekable<std::str::Chars<'_>>, &mut String),
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_str = false,
                _ => {}
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else {
            other(c, &mut chars, &mut out);
        }
    }
    out
}

fn strip_trailing_commas(text: &str) -> String {
    outside_strings(text, |c, chars, out| {
        let next = chars.clone().find(|n| !n.is_whitespace());
        if c != ',' || !matches!(next, Some('}' | ']')) {
            out.push(c);
        }
    })
}

fn strip_comments(text: &str) -> String {
    outside_strings(text, |c, chars, out| match (c, chars.peek()) {
        ('/', Some('/')) => {
            for n in chars.by_ref() {
                if n == '\n' {
                    out.push('\n');
                    break;
                }
            }
        }
        ('/', Some('*')) => {
            chars.next();
            let mut prev = ' ';
            for n in chars.by_ref() {
                if prev == '*' && n == '/' {
                    break;
                }
                prev = n;
            }
        }
        _ => out.push(c),
    })
}

fn to_json(t: &toml::Table) -> Result<serde_json::Value, String> {
    serde_json::to_value(t).map_err(|e| e.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    /// Not valid UTF-8: read one char per byte, and written back the same way.
    Latin1,
}

pub struct Source {
    pub text: String,
    pub encoding: Encoding,
}

pub fn read(p: &Path) -> Result<Source, String> {
    let bytes = fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(match String::from_utf8(bytes) {
        Ok(text) => Source {
            text,
            encoding: Encoding::Utf8,
        },
        Err(e) => Source {
            text: e.into_bytes().iter().map(|&b| b as char).collect(),
            encoding: Encoding::Latin1,
        },
    })
}

/// Writes `text` back in `encoding`, so a Latin-1 file stays Latin-1.
pub fn write(p: &Path, text: &str, encoding: Encoding) -> Result<(), String> {
    let bytes = match encoding {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Latin1 => text
            .chars()
            .map(|c| u8::try_from(u32::from(c)))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|_| {
                format!(
                    "{}: not written; the change adds characters the file's encoding can't hold",
                    p.display()
                )
            })?,
    };
    fs::write(p, bytes).map_err(|e| format!("{}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_settings() {
        let text = r#"{
  // comment with "quotes"
  "abl.propath": ["src", "src/include"],  // or "src;src/include"
  /* block
     comment */
  "abl.note": "a // inside a string, and \" an escaped quote",
  "abl.lint": { "unused-variable": "off", },
  "abl.last": 1, // trailing comma, then a comment
}"#;
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(text)).unwrap();
        assert_eq!(v["abl.propath"][1], "src/include");
        assert_eq!(
            v["abl.note"],
            "a // inside a string, and \" an escaped quote"
        );
        assert_eq!(v["abl.lint"]["unused-variable"], "off");
    }

    #[test]
    fn latin1_round_trip() {
        let dir = std::env::temp_dir().join(format!("abl-src-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.p");
        fs::write(&f, b"MESSAGE \"caf\xe9\".\n").unwrap();
        let s = read(&f).unwrap();
        assert_eq!(s.encoding, Encoding::Latin1);
        assert!(s.text.contains("caf\u{e9}"));
        write(&f, &s.text.replace("MESSAGE", "  MESSAGE"), s.encoding).unwrap();
        assert_eq!(fs::read(&f).unwrap(), b"  MESSAGE \"caf\xe9\".\n");
        assert!(write(&f, "\u{2603}", Encoding::Latin1).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}

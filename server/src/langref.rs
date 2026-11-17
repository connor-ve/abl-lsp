//! Built-in functions, system handles and handle attributes/methods, read from
//! `builtin/abl-reference.txt` (format described at the top of that file).

use std::collections::HashMap;
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Function,
    Attribute,
    Method,
    /// A system handle (`SESSION`), documented by its kind.
    Handle,
}

#[derive(Debug)]
pub struct Param {
    /// `name AS TYPE` as written.
    pub label: String,
    pub optional: bool,
}

#[derive(Debug)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub params: Vec<Param>,
    /// Takes any number of further arguments (`...`).
    pub variadic: bool,
    /// Return or attribute type as written (`CHARACTER`, `HANDLE:BUFFER`, a class).
    pub returns: Option<String>,
    pub doc: String,
    /// Handle kind the member belongs to.
    pub owner: Option<String>,
}

impl Entry {
    pub fn has_parens(&self) -> bool {
        matches!(self.kind, EntryKind::Function | EntryKind::Method)
            && (!self.params.is_empty() || self.variadic || self.kind == EntryKind::Method)
    }

    /// Return type without the handle kind: `HANDLE:BUFFER` -> `HANDLE`.
    pub fn return_type(&self) -> Option<&str> {
        self.returns
            .as_deref()
            .map(|r| r.split(':').next().unwrap_or(r))
    }

    /// Handle kind of the result: `HANDLE:BUFFER` -> `BUFFER`.
    pub fn returns_handle(&self) -> Option<&str> {
        self.returns
            .as_deref()
            .and_then(|r| r.strip_prefix("HANDLE:"))
    }

    pub fn param_labels(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .params
            .iter()
            .map(|p| {
                if p.optional {
                    format!("[{}]", p.label)
                } else {
                    p.label.clone()
                }
            })
            .collect();
        if self.variadic {
            v.push("...".into());
        }
        v
    }

    pub fn signature(&self) -> String {
        let mut s = match &self.owner {
            Some(o) if self.kind != EntryKind::Handle => format!("{o}:{}", self.name),
            _ => self.name.clone(),
        };
        if self.has_parens() {
            s.push_str(&format!("({})", self.param_labels().join(", ")));
        }
        if let Some(r) = &self.returns {
            s.push_str(&format!(
                " AS {}",
                r.replace("HANDLE:", "HANDLE (") + if r.starts_with("HANDLE:") { ")" } else { "" }
            ));
        }
        s
    }

    pub fn markdown(&self) -> String {
        let what = match self.kind {
            EntryKind::Function => "built-in function".to_string(),
            EntryKind::Attribute => "attribute".to_string(),
            EntryKind::Method => "method".to_string(),
            EntryKind::Handle => "system handle".to_string(),
        };
        let mut md = format!("```abl\n{}\n```\n{what}", self.signature());
        if !self.doc.is_empty() {
            md.push_str("\n\n");
            md.push_str(&self.doc);
        }
        md
    }
}

pub struct HandleKind {
    pub members: Vec<Entry>,
}

pub struct Reference {
    functions: HashMap<String, Entry>,
    kinds: HashMap<String, HandleKind>,
    /// System handle name -> (kind, its doc entry).
    system: HashMap<String, (String, Entry)>,
}

static REF: LazyLock<Reference> =
    LazyLock::new(|| parse(include_str!("../builtin/abl-reference.txt")));

/// Splits `a, [b AS X], c` at top-level commas.
fn split_params(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(ch);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn parse_entry(line: &str, default_kind: EntryKind, owner: Option<&str>) -> Entry {
    let line = line.trim();
    let (head, params_txt, rest) = match line.find('(') {
        Some(p) => {
            let close = line.rfind(')').unwrap_or(line.len());
            (
                &line[..p],
                Some(&line[p + 1..close]),
                &line[(close + 1).min(line.len())..],
            )
        }
        None => match line.find(" AS ") {
            Some(p) => (&line[..p], None, &line[p..]),
            None => (line, None, ""),
        },
    };
    let returns = rest
        .trim()
        .strip_prefix("AS ")
        .map(|r| r.trim().to_string());
    let mut params = Vec::new();
    let mut variadic = false;
    for p in params_txt.map(split_params).unwrap_or_default() {
        if p == "..." {
            variadic = true;
            continue;
        }
        let optional = p.starts_with('[');
        let label = p.trim_matches(|c| c == '[' || c == ']').trim().to_string();
        params.push(Param { label, optional });
    }
    let kind = match default_kind {
        EntryKind::Function => EntryKind::Function,
        _ if params_txt.is_some() => EntryKind::Method,
        _ => EntryKind::Attribute,
    };
    Entry {
        name: head.trim().to_string(),
        kind,
        params,
        variadic,
        returns,
        doc: String::new(),
        owner: owner.map(str::to_string),
    }
}

fn parse(text: &str) -> Reference {
    let mut r = Reference {
        functions: HashMap::new(),
        kinds: HashMap::new(),
        system: HashMap::new(),
    };
    let mut kind: Option<String> = None;
    let mut last: Option<(Option<String>, String)> = None; // (kind, upper name) of the entry docs go to
    for raw in text.lines() {
        if raw.trim().is_empty() || raw.starts_with('#') {
            continue;
        }
        if raw.starts_with(char::is_whitespace) {
            let doc = raw.trim();
            let e = match &last {
                Some((None, n)) => r.functions.get_mut(n),
                Some((Some(k), n)) => r
                    .kinds
                    .get_mut(k)
                    .and_then(|h| h.members.iter_mut().rev().find(|m| &m.name == n)),
                None => None,
            };
            if let Some(e) = e {
                if !e.doc.is_empty() {
                    e.doc.push(' ');
                }
                e.doc.push_str(doc);
            }
            continue;
        }
        if let Some(sec) = raw.strip_prefix('@') {
            let mut words = sec.split_whitespace();
            match words.next() {
                Some("functions") => kind = None,
                Some("handle") => {
                    let name = words.next().unwrap_or_default().to_string();
                    for w in words {
                        if let Some(list) = w.strip_prefix("system=") {
                            for s in list.split(',') {
                                let doc = Entry {
                                    name: s.to_string(),
                                    kind: EntryKind::Handle,
                                    params: Vec::new(),
                                    variadic: false,
                                    returns: Some(format!("HANDLE:{name}")),
                                    doc: String::new(),
                                    owner: None,
                                };
                                r.system.insert(s.to_string(), (name.clone(), doc));
                            }
                        }
                    }
                    r.kinds.entry(name.clone()).or_insert_with(|| HandleKind {
                        members: Vec::new(),
                    });
                    kind = Some(name);
                }
                _ => {}
            }
            last = None;
            continue;
        }
        match &kind {
            None => {
                let e = parse_entry(raw, EntryKind::Function, None);
                let n = e.name.to_ascii_uppercase();
                last = Some((None, n.clone()));
                r.functions.insert(n, e);
            }
            Some(k) => {
                let e = parse_entry(raw, EntryKind::Attribute, Some(k));
                last = Some((Some(k.clone()), e.name.clone()));
                r.kinds.get_mut(k).unwrap().members.push(e);
            }
        }
    }
    // System handles describe themselves with their kind's purpose.
    for (_, (k, e)) in r.system.iter_mut() {
        e.doc = format!(
            "A `{k}` handle; type `{}:` for its attributes and methods.",
            e.name
        );
    }
    r
}

/// Built-in function by name or keyword abbreviation (`SUBSTR`).
pub fn function(name: &str) -> Option<&'static Entry> {
    let u = name.to_ascii_uppercase();
    REF.functions
        .get(&u)
        .or_else(|| crate::keywords::keyword(name).and_then(|k| REF.functions.get(k)))
}

pub fn functions() -> impl Iterator<Item = &'static Entry> {
    REF.functions.values()
}

/// Kind of a system handle (`SESSION` -> SESSION, `THIS-PROCEDURE` -> PROCEDURE).
pub fn system_handle(name: &str) -> Option<(&'static str, &'static Entry)> {
    REF.system
        .get(&name.to_ascii_uppercase())
        .map(|(k, e)| (k.as_str(), e))
}

pub fn system_handles() -> impl Iterator<Item = &'static Entry> {
    REF.system.values().map(|(_, e)| e)
}

pub fn kind(name: &str) -> Option<&'static HandleKind> {
    REF.kinds.get(&name.to_ascii_uppercase())
}

/// Kinds a plain HANDLE variable most often holds, when nothing tells which.
pub const COMMON_KINDS: &[&str] = &[
    "BUFFER",
    "QUERY",
    "TEMP-TABLE",
    "DATASET",
    "BUFFER-FIELD",
    "PROCEDURE",
    "WIDGET",
];

/// Member `name` of handle kind `kind` (`*` = any of [`COMMON_KINDS`]).
pub fn member(kind_name: &str, name: &str) -> Option<&'static Entry> {
    let find = |k: &str| {
        self::kind(k).and_then(|h| h.members.iter().find(|m| m.name.eq_ignore_ascii_case(name)))
    };
    if kind_name == "*" {
        COMMON_KINDS.iter().find_map(|k| find(k))
    } else {
        find(kind_name)
    }
}

/// Members of a handle kind (`*` = the union of [`COMMON_KINDS`], first one wins).
pub fn members(kind_name: &str) -> Vec<&'static Entry> {
    let kinds: Vec<&str> = if kind_name == "*" {
        COMMON_KINDS.to_vec()
    } else {
        vec![kind_name]
    };
    let mut seen = std::collections::HashSet::new();
    kinds
        .iter()
        .filter_map(|k| kind(k))
        .flat_map(|h| h.members.iter())
        .filter(|m| seen.insert(m.name.to_ascii_uppercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_parses() {
        let e = function("substr").unwrap();
        assert_eq!(e.name, "SUBSTRING");
        assert_eq!(e.params.iter().filter(|p| !p.optional).count(), 2);
        assert_eq!(e.return_type(), Some("CHARACTER"));
        assert!(e.doc.starts_with("Part of a string"), "{}", e.doc);
        assert!(function("SUBSTITUTE").unwrap().variadic);
        assert!(!function("TODAY").unwrap().has_parens());

        assert_eq!(system_handle("this-procedure").unwrap().0, "PROCEDURE");
        let bf = member("BUFFER", "buffer-field").unwrap();
        assert_eq!(bf.kind, EntryKind::Method);
        assert_eq!(bf.returns_handle(), Some("BUFFER-FIELD"));
        assert_eq!(
            member("SESSION", "DATE-FORMAT").unwrap().kind,
            EntryKind::Attribute
        );
        assert!(members("*").iter().any(|m| m.name == "QUERY-PREPARE"));
        assert!(
            bf.signature()
                .starts_with("BUFFER:BUFFER-FIELD(field-name-or-index) AS HANDLE (BUFFER-FIELD)"),
            "{}",
            bf.signature()
        );
    }
}

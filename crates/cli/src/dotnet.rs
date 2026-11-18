//! `abl dotnet`: the .NET side of a project. Lists the assemblies in
//! assemblies.xml next to the .NET namespaces the code uses, and adds or
//! removes assemblies.
//!
//! OpenEdge loads a few assemblies by itself (mscorlib, System,
//! System.Drawing, System.Windows.Forms and its own Progress ones); any other
//! assembly needs an entry in assemblies.xml, in the folder `-assemblies`
//! names (abl passes the folder from abl.toml's `assemblies`, default the
//! project root). Which namespace lives in which assembly can't be known
//! without the assemblies, so the match is by name: a namespace is covered
//! by an assembly whose name starts the same way.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use abl_core::analysis::Analysis;
use abl_core::lexer::TokKind;
use abl_oe::config::{self, Loaded};

use crate::source::{self, Project};

pub const FILE: &str = "assemblies.xml";

/// Namespaces in the assemblies OpenEdge loads without assemblies.xml.
const BUILT_IN: &[&str] = &[
    "System",
    "System.Collections",
    "System.ComponentModel",
    "System.Diagnostics",
    "System.Drawing",
    "System.Globalization",
    "System.IO",
    "System.Net",
    "System.Reflection",
    "System.Runtime",
    "System.Security",
    "System.Text",
    "System.Threading",
    "System.Timers",
    "System.Windows.Forms",
    "Microsoft.Win32",
    "Progress",
];

/// Assemblies that hold namespaces under a built-in one but ship separately.
const SEPARATE: &[&str] = &[
    "System.Net.Http",
    "System.Runtime.Serialization",
    "System.Security.Cryptography.Xml",
];

fn under(ns: &str, prefix: &str) -> bool {
    let (n, p) = (ns.to_ascii_lowercase(), prefix.to_ascii_lowercase());
    n == p || n.starts_with(&format!("{p}."))
}

pub fn built_in(ns: &str) -> bool {
    // System alone is built in, but not everything that starts with System.
    if SEPARATE.iter().any(|s| under(ns, s)) {
        return false;
    }
    BUILT_IN.iter().any(|b| {
        if *b == "System" {
            ns.eq_ignore_ascii_case("System")
        } else {
            under(ns, b)
        }
    })
}

/// An assembly's simple name: `Foo.Bar` of `Foo.Bar, Version=1.0, ...`.
pub fn simple_name(full: &str) -> &str {
    full.split(',').next().unwrap_or(full).trim()
}

/// `Infragistics4.Win.v18.1` -> `infragistics.win`: digits glued to a word and
/// version segments dropped, for matching against namespaces.
fn loose(name: &str) -> String {
    name.split('.')
        .filter(|seg| {
            let s = seg.to_ascii_lowercase();
            !(s.starts_with('v') && s[1..].chars().all(|c| c.is_ascii_digit())
                || s.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|seg| {
            seg.trim_end_matches(|c: char| c.is_ascii_digit())
                .to_ascii_lowercase()
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The assembly in `assemblies` that covers namespace `ns`, if any.
pub fn covering<'a>(ns: &str, assemblies: &'a [String]) -> Option<&'a str> {
    assemblies
        .iter()
        .map(String::as_str)
        .filter(|a| {
            let s = simple_name(a);
            under(ns, s) || under(&loose(ns), &loose(s)) || under(s, ns)
        })
        .max_by_key(|a| simple_name(a).len())
}

/// The `name` of every `<assembly>` element.
pub fn parse(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<assembly") {
        rest = &rest[i + "<assembly".len()..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        if let Some(n) = tag.find("name=") {
            let v = &tag[n + 5..];
            if let Some(q) = v.chars().next().filter(|c| *c == '"' || *c == '\'') {
                if let Some(close) = v[1..].find(q) {
                    out.push(unxml(&v[1..1 + close]));
                }
            }
        }
    }
    out
}

fn unxml(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The namespace of a .NET type or `USING` name: `System.IO.Path` -> `System.IO`.
fn namespace_of(name: &str) -> Option<String> {
    let name = name.trim_matches('"');
    let name = name.split('<').next().unwrap_or(name);
    if let Some(ns) = name.strip_suffix(".*") {
        return Some(ns.to_string());
    }
    name.rsplit_once('.').map(|(ns, _)| ns.to_string())
}

/// .NET namespaces a file uses: `USING ... FROM ASSEMBLY`, and `USING` or
/// qualified names under System. or Microsoft.
pub fn namespaces(ana: &Analysis) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let dotnet = |n: &str| {
        let n = n.trim_matches('"');
        under(n, "System") || under(n, "Microsoft")
    };
    for st in &ana.statements {
        let toks: Vec<usize> = ana.stmt_tokens(st).collect();
        if toks.len() >= 2 && ana.kw(toks[0]) == Some("USING") {
            // `a.b.*` may lex as several tokens: join them up to FROM.
            let name: String = toks[1..]
                .iter()
                .take_while(|&&t| !ana.tok_text(t).eq_ignore_ascii_case("FROM"))
                .map(|&t| ana.tok_text(t))
                .collect();
            let name = name.as_str();
            let from_assembly = toks.windows(2).any(|w| {
                ana.tok_text(w[0]).eq_ignore_ascii_case("FROM")
                    && ana.tok_text(w[1]).eq_ignore_ascii_case("ASSEMBLY")
            });
            if from_assembly || dotnet(name) {
                out.extend(namespace_of(name));
            }
        }
    }
    for (i, t) in ana.tokens.iter().enumerate() {
        if matches!(t.kind, TokKind::Word | TokKind::String) {
            let text = ana.tok_text(i);
            if text.contains('.') && dotnet(text) {
                out.extend(namespace_of(text));
            }
        }
    }
    out
}

pub struct Usage {
    pub file: Option<PathBuf>,
    pub assemblies: Vec<String>,
    /// Namespace -> files using it.
    pub used: BTreeMap<String, BTreeSet<PathBuf>>,
}

impl Usage {
    /// Namespaces that need an assembly the file doesn't list.
    pub fn missing(&self) -> Vec<&str> {
        self.used
            .keys()
            .map(String::as_str)
            .filter(|ns| !built_in(ns) && covering(ns, &self.assemblies).is_none())
            .collect()
    }
}

pub fn usage(project: &Project) -> Result<Usage, String> {
    let file = project
        .loaded
        .as_ref()
        .map(|l| l.assemblies_dir().join(FILE))
        .filter(|f| f.is_file());
    let assemblies = match &file {
        Some(f) => parse(&fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?),
        None => Vec::new(),
    };
    let mut used: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
    for f in &project.files {
        let Ok(src) = source::read(f) else { continue };
        let ana = project.analyze(f, src.text);
        for ns in namespaces(&ana) {
            used.entry(ns).or_default().insert(f.clone());
        }
    }
    Ok(Usage {
        file,
        assemblies,
        used,
    })
}

pub fn list(cwd: &Path) -> Result<u8, String> {
    let project = Project::open(cwd)?;
    let u = usage(&project)?;
    match &u.file {
        Some(f) => {
            println!(
                "{}: {} assembl{}",
                project.show(f, cwd),
                u.assemblies.len(),
                if u.assemblies.len() == 1 { "y" } else { "ies" }
            );
            for a in &u.assemblies {
                println!("  {a}");
            }
        }
        None => println!("No {FILE} (only the assemblies OpenEdge loads by itself)"),
    }
    if u.used.is_empty() {
        println!("\nThe code uses no .NET types.");
        return Ok(0);
    }
    println!("\n.NET namespaces in the code:");
    let width = u.used.keys().map(String::len).max().unwrap_or(0);
    for (ns, files) in &u.used {
        let state = if built_in(ns) {
            "built in".to_string()
        } else if let Some(a) = covering(ns, &u.assemblies) {
            format!("{FILE}: {}", simple_name(a))
        } else {
            format!("not in {FILE}: abl dotnet add \"<assembly name>\"")
        };
        let n = files.len();
        println!(
            "  {ns:<width$}  {n:>3} file{}  {state}",
            if n == 1 { " " } else { "s" }
        );
    }
    Ok(0)
}

fn project_file(cwd: &Path) -> Result<(Loaded, PathBuf), String> {
    let l = config::load_project(cwd)?.ok_or("no abl.toml here or above; run `abl init` first")?;
    let f = l.assemblies_dir().join(FILE);
    Ok((l, f))
}

pub fn add(cwd: &Path, name: &str) -> Result<u8, String> {
    let name = name.trim();
    let simple = simple_name(name);
    if simple.is_empty() || name.contains(['<', '>']) {
        return Err(format!("{name:?} isn't an assembly name"));
    }
    let (_, f) = project_file(cwd)?;
    let text = fs::read_to_string(&f).unwrap_or_default();
    if parse(&text)
        .iter()
        .any(|a| simple_name(a).eq_ignore_ascii_case(simple))
    {
        return Err(format!("{simple} is already in {}", f.display()));
    }
    let entry = format!("  <assembly name=\"{}\"/>\n", xml(name));
    let new = if text.trim().is_empty() {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<references xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\n{entry}</references>\n"
        )
    } else {
        let Some(at) = text.rfind("</references>") else {
            return Err(format!("{}: no </references> to add to", f.display()));
        };
        // Insert at the start of the closing tag's line.
        let line = text[..at].rfind('\n').map_or(0, |i| i + 1);
        let at = if text[line..at].trim().is_empty() {
            line
        } else {
            at
        };
        format!("{}{entry}{}", &text[..at], &text[at..])
    };
    if let Some(d) = f.parent() {
        fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    fs::write(&f, new).map_err(|e| format!("{}: {e}", f.display()))?;
    println!("Added {simple} to {}", f.display());
    if !name.contains("PublicKeyToken") {
        println!("(a name without Version, Culture and PublicKeyToken is looked for next to assemblies.xml, not in the GAC)");
    }
    Ok(0)
}

pub fn remove(cwd: &Path, name: &str) -> Result<u8, String> {
    let (_, f) = project_file(cwd)?;
    let text = fs::read_to_string(&f).map_err(|_| format!("no {}", f.display()))?;
    let want = simple_name(name);
    let mut removed = 0;
    let kept: Vec<&str> = text
        .split_inclusive('\n')
        .filter(|line| {
            let hit = line.contains("<assembly")
                && parse(line)
                    .iter()
                    .any(|a| simple_name(a).eq_ignore_ascii_case(want));
            if hit {
                removed += 1;
            }
            !hit
        })
        .collect();
    if removed == 0 {
        return Err(format!("no assembly {want} in {}", f.display()));
    }
    fs::write(&f, kept.concat()).map_err(|e| format!("{}: {e}", f.display()))?;
    println!("Removed {want} from {}", f.display());
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use abl_core::analysis::analyze;

    #[test]
    fn namespaces_and_coverage() {
        let a = analyze(
            "USING System.Text.StringBuilder FROM ASSEMBLY.\nUSING Newtonsoft.Json.* FROM ASSEMBLY.\n\
             USING app.orders.*.\nDEFINE VARIABLE x AS System.Xml.XmlDocument NO-UNDO.\n\
             DEFINE VARIABLE l AS \"System.Collections.Generic.List<CHARACTER>\" NO-UNDO.\n\
             MESSAGE System.Environment:MachineName.\n"
                .into(),
            false,
        );
        let ns: Vec<String> = namespaces(&a).into_iter().collect();
        assert_eq!(
            ns,
            [
                "Newtonsoft.Json",
                "System",
                "System.Collections.Generic",
                "System.Text",
                "System.Xml"
            ]
        );
        assert!(
            built_in("System.Collections.Generic")
                && built_in("System")
                && built_in("System.Windows.Forms")
        );
        assert!(
            !built_in("System.Xml") && !built_in("System.Net.Http") && !built_in("Newtonsoft.Json")
        );
        let asm = vec![
            "Infragistics4.Win.v18.1, Version=18.1.20181.100, Culture=neutral, PublicKeyToken=7dd5c3163f2cd0cb".to_string(),
            "Newtonsoft.Json".to_string(),
        ];
        assert_eq!(
            covering("Newtonsoft.Json.Linq", &asm),
            Some("Newtonsoft.Json")
        );
        assert!(covering("Infragistics.Win.UltraWinGrid", &asm).is_some());
        assert!(covering("System.Xml", &asm).is_none());
        assert_eq!(
            parse("<references>\n  <assembly name=\"A, Version=1\"/>\n  <assembly name='B &amp; C'/>\n</references>"),
            ["A, Version=1", "B & C"]
        );
    }
}

//! `abl repl`: type ABL, see what it does, in one OpenEdge session that stays
//! up. Each entry runs as its own small procedure, so to keep variables
//! between entries, `DEFINE VARIABLE` and `DEFINE TEMP-TABLE` are rewritten to
//! `DEFINE NEW GLOBAL SHARED ...` and repeated at the top of later entries.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use abl_core::analysis::analyze;
use abl_oe::config;
use abl_oe::session::{Env, Repl, Reply};

const HELP: &str = "\
Type ABL statements and end them with a period. A block (DO:, FOR EACH ...:)
runs once its END is typed. Variables and temp-tables you define are kept.

  = expr     show a value: = TODAY + 1
  :defs      the definitions kept so far
  :reset     a fresh session, forgetting all definitions
  :help      this text
  :quit      leave (also Ctrl+Z / Ctrl+D)
";

/// A kept definition: lower-case name and its rewritten statement.
pub type Def = (String, String);

/// Is `code` a complete entry, or does it need more lines?
pub fn complete(code: &str) -> bool {
    let a = analyze(code.to_string(), false);
    let open = a
        .diags
        .iter()
        .any(|d| d.message.contains("no matching END") || d.message.starts_with("Unterminated"));
    !open && code.trim_end().ends_with('.')
}

/// Rewrites top-level `DEFINE VARIABLE` / `DEFINE TEMP-TABLE` statements to
/// global shared ones. Returns the code and the definitions it makes.
pub fn rewrite(code: &str) -> (String, Vec<Def>) {
    let a = analyze(code.to_string(), false);
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut defs = Vec::new();
    for st in a.statements.iter().filter(|s| s.depth == 0) {
        let toks: Vec<usize> = a.stmt_tokens(st).collect();
        if toks.len() < 3 || a.kw(toks[0]) != Some("DEFINE") {
            continue;
        }
        let kind = match a.kw(toks[1]) {
            Some(k @ ("VARIABLE" | "TEMP-TABLE")) => k,
            _ => continue, // already NEW/SHARED, PRIVATE, or something else
        };
        let name = a.tok_text(toks[2]).to_ascii_lowercase();
        let (s, e) = (a.tokens[toks[0]].start, a.tokens[toks[1]].end);
        let replacement = format!("DEFINE NEW GLOBAL SHARED {kind}");
        let stmt_end = st
            .term
            .map_or(a.tokens[*toks.last().unwrap()].end, |t| a.tokens[t].end);
        let full = format!("{replacement}{}", &code[e..stmt_end]);
        defs.push((name, full));
        edits.push((s, e, replacement));
    }
    let mut out = code.to_string();
    for (s, e, r) in edits.into_iter().rev() {
        out.replace_range(s..e, &r);
    }
    (out, defs)
}

/// The code to send: kept definitions the entry doesn't redefine, then the entry.
pub fn program(kept: &[Def], entry: &str) -> (String, Vec<Def>) {
    let (code, new) = rewrite(entry);
    let mut text = String::new();
    for (name, d) in kept {
        if !new.iter().any(|(n, _)| n == name) {
            text.push_str(d);
            text.push('\n');
        }
    }
    text.push_str(&code);
    text.push('\n');
    (text, new)
}

fn keep(kept: &mut Vec<Def>, new: Vec<Def>) {
    for d in new {
        kept.retain(|(n, _)| *n != d.0);
        kept.push(d);
    }
}

pub fn run(cwd: &Path, flag: Option<&str>) -> Result<u8, String> {
    let loaded = config::load_project(cwd)?;
    let env = Env::new(loaded.as_ref(), flag, cwd)?;
    let mut session = Repl::start(&env)?;
    let interactive = std::io::stdin().is_terminal();
    if interactive {
        println!(
            "ABL on OE {} (`:help` for help, `:quit` to leave)",
            env.version
        );
    }
    let mut kept: Vec<Def> = Vec::new();
    let mut buf = String::new();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        if interactive {
            print!("{}", if buf.is_empty() { "abl> " } else { "...> " });
            let _ = std::io::stdout().flush();
        }
        let Some(Ok(line)) = lines.next() else { break };
        if buf.is_empty() {
            match line.trim() {
                "" => continue,
                ":quit" | ":q" | ":exit" => break,
                ":help" | ":h" | "?" => {
                    print!("{HELP}");
                    continue;
                }
                ":defs" => {
                    if kept.is_empty() {
                        println!("(no definitions)");
                    }
                    for (_, d) in &kept {
                        println!("{d}");
                    }
                    continue;
                }
                ":reset" => {
                    session.close();
                    session = Repl::start(&env)?;
                    kept.clear();
                    println!("(fresh session)");
                    continue;
                }
                l if l.starts_with(':') => {
                    println!("unknown command {l}; :help lists them");
                    continue;
                }
                _ => {}
            }
        }
        buf.push_str(&line);
        buf.push('\n');
        let entry = match buf.trim().strip_prefix('=') {
            Some(expr) => format!("MESSAGE {}.", expr.trim().trim_end_matches('.')),
            None if complete(&buf) => buf.trim().to_string(),
            None => continue,
        };
        buf.clear();
        let (code, new) = program(&kept, &entry);
        match session.eval(&code)? {
            Reply::Ok => keep(&mut kept, new),
            Reply::Error(msgs) => {
                for m in msgs.iter().filter(|m| !m.trim().is_empty()) {
                    println!("{m}");
                }
            }
            Reply::Stop => println!("(stopped)"),
        }
    }
    if !buf.trim().is_empty() {
        println!("(not run: the last entry was never finished)");
    }
    session.close();
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_complete() {
        assert!(complete("MESSAGE 1."));
        assert!(!complete("MESSAGE 1"));
        assert!(!complete("DO i = 1 TO 3:\n  MESSAGE i."));
        assert!(complete("DO i = 1 TO 3:\n  MESSAGE i.\nEND."));
        assert!(!complete("MESSAGE \"open."));
    }

    #[test]
    fn definitions_become_global() {
        let (code, defs) =
            rewrite("DEFINE VARIABLE i AS INTEGER NO-UNDO.\ni = 5.\nDEF VAR c AS CHAR.");
        assert_eq!(
            code,
            "DEFINE NEW GLOBAL SHARED VARIABLE i AS INTEGER NO-UNDO.\ni = 5.\nDEFINE NEW GLOBAL SHARED VARIABLE c AS CHAR."
        );
        assert_eq!(
            defs[0],
            (
                "i".into(),
                "DEFINE NEW GLOBAL SHARED VARIABLE i AS INTEGER NO-UNDO.".into()
            )
        );
        // Shared already, or inside a block: left alone.
        let (code, defs) = rewrite("DEFINE SHARED VARIABLE x AS INTEGER.");
        assert!(defs.is_empty());
        assert_eq!(code, "DEFINE SHARED VARIABLE x AS INTEGER.");

        let mut kept = Vec::new();
        keep(&mut kept, defs_of("DEFINE VARIABLE i AS INTEGER NO-UNDO."));
        let (text, new) = program(&kept, "DEFINE VARIABLE i AS DECIMAL NO-UNDO.\nMESSAGE i.");
        assert!(
            !text.contains("AS INTEGER"),
            "a redefinition replaces the old one: {text}"
        );
        keep(&mut kept, new);
        assert_eq!(kept.len(), 1);
        let (text, _) = program(&kept, "MESSAGE i.");
        assert!(
            text.starts_with("DEFINE NEW GLOBAL SHARED VARIABLE i AS DECIMAL NO-UNDO.\nMESSAGE i.")
        );
    }

    fn defs_of(code: &str) -> Vec<Def> {
        rewrite(code).1
    }
}

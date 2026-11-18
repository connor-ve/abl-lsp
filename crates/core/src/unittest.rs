//! Finds unit tests in a file, ABLUnit style: `@Test.` in front of a method
//! of a class, or of an internal procedure of a `.p`.
//!
//! The other annotations it knows: `@Ignore.` skips a test, `@Before.` and
//! `@After.` run once around all the tests of the file, `@Setup.` and
//! `@TearDown.` run around each test. `@Test (expected="Some.Error").` passes
//! only when the test raises that error (or a subclass of it).

use crate::analysis::{Analysis, SymKind};
use crate::lexer::TokKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestKind {
    /// Methods of a class, run on one instance.
    Class,
    /// Internal procedures, run in the procedure run persistent.
    Procedure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestCase {
    pub name: String,
    /// Byte offset of the routine, for line numbers.
    pub start: usize,
    pub expected: Option<String>,
    pub ignored: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestFile {
    pub kind: TestKind,
    pub before: Vec<String>,
    pub after: Vec<String>,
    pub setup: Vec<String>,
    pub teardown: Vec<String>,
    pub tests: Vec<TestCase>,
}

/// One annotation: its name (lower case) and its `expected` attribute.
fn annotation(ana: &Analysis, toks: &[usize]) -> Option<(String, Option<String>)> {
    let first = *toks.first()?;
    if ana.tok_text(first) != "@" || toks.len() < 2 || ana.tokens[toks[1]].kind != TokKind::Word {
        return None;
    }
    let name = ana.tok_text(toks[1]).to_ascii_lowercase();
    let mut expected = None;
    for w in toks.windows(3) {
        if ana.tok_text(w[0]).eq_ignore_ascii_case("expected")
            && ana.tok_text(w[1]) == "="
            && ana.tokens[w[2]].kind == TokKind::String
        {
            // "Some.Error" or "Some.Error":U: the text between the quotes.
            let s = ana.tok_text(w[2]);
            let q = s.chars().next().unwrap_or('"');
            let end = s.rfind(q).filter(|&e| e > 0).unwrap_or(s.len());
            expected = Some(s[q.len_utf8().min(end)..end].to_string());
        }
    }
    Some((name, expected))
}

/// The tests in a file, or None when it has none.
pub fn find(ana: &Analysis) -> Option<TestFile> {
    let kind = if ana.class_name.is_some() {
        TestKind::Class
    } else {
        TestKind::Procedure
    };
    let routine = match kind {
        TestKind::Class => SymKind::Method,
        TestKind::Procedure => SymKind::Procedure,
    };
    let mut file = TestFile {
        kind,
        before: Vec::new(),
        after: Vec::new(),
        setup: Vec::new(),
        teardown: Vec::new(),
        tests: Vec::new(),
    };
    let mut pending: Vec<(String, Option<String>)> = Vec::new();
    for st in &ana.statements {
        let toks: Vec<usize> = ana.stmt_tokens(st).collect();
        if let Some(a) = annotation(ana, &toks) {
            pending.push(a);
            continue;
        }
        if pending.is_empty() {
            continue;
        }
        let (s, e) = (
            ana.tokens[st.first].start,
            st.term
                .map_or(ana.tokens[st.last].end, |t| ana.tokens[t].end),
        );
        let sym = ana
            .symbols
            .iter()
            .find(|x| x.kind == routine && !x.proto && x.sel_start >= s && x.sel_end <= e);
        if let Some(sym) = sym {
            let has = |n: &str| pending.iter().any(|(a, _)| a == n);
            let name = sym.name.clone();
            if has("test") {
                file.tests.push(TestCase {
                    name: name.clone(),
                    start: s,
                    expected: pending
                        .iter()
                        .find_map(|(a, x)| (a == "test").then(|| x.clone()).flatten()),
                    ignored: has("ignore"),
                });
            }
            for (list, n) in [
                (&mut file.before, "before"),
                (&mut file.after, "after"),
                (&mut file.setup, "setup"),
                (&mut file.teardown, "teardown"),
            ] {
                if has(n) {
                    list.push(name.clone());
                }
            }
        }
        pending.clear();
    }
    (!file.tests.is_empty()).then_some(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::analyze;

    #[test]
    fn finds_class_tests() {
        let src = "USING OpenEdge.Core.Assert.\n\
            CLASS test.CalcTest:\n\
            @Before.\nMETHOD PUBLIC VOID Init():\nEND METHOD.\n\
            @Setup.\nMETHOD PUBLIC VOID Each():\nEND METHOD.\n\
            @Test.\nMETHOD PUBLIC VOID Adds():\n  Assert:Equals(2, 1 + 1).\nEND METHOD.\n\
            @Test (expected=\"Progress.Lang.AppError\").\nMETHOD PUBLIC VOID Throws():\nEND METHOD.\n\
            @Test.\n@Ignore.\nMETHOD PUBLIC VOID Later():\nEND METHOD.\n\
            METHOD PRIVATE VOID Helper():\nEND METHOD.\n\
            END CLASS.\n";
        let a = analyze(src.to_string(), false);
        assert!(a.diags.is_empty(), "{:?}", a.diags);
        let f = find(&a).unwrap();
        assert_eq!(f.kind, TestKind::Class);
        assert_eq!(f.before, ["Init"]);
        assert_eq!(f.setup, ["Each"]);
        let names: Vec<&str> = f.tests.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Adds", "Throws", "Later"]);
        assert_eq!(
            f.tests[1].expected.as_deref(),
            Some("Progress.Lang.AppError")
        );
        assert!(f.tests[2].ignored && !f.tests[0].ignored);
    }

    #[test]
    fn finds_procedure_tests() {
        let src = "@Test.\nPROCEDURE testOne:\nEND PROCEDURE.\n\
            @TearDown.\nPROCEDURE cleanUp:\nEND PROCEDURE.\n\
            PROCEDURE notATest:\nEND PROCEDURE.\n";
        let a = analyze(src.to_string(), false);
        let f = find(&a).unwrap();
        assert_eq!(f.kind, TestKind::Procedure);
        assert_eq!(f.tests.len(), 1);
        assert_eq!(f.teardown, ["cleanUp"]);
        assert!(find(&analyze("MESSAGE 1.".into(), false)).is_none());
    }
}

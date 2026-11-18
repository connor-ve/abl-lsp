//! The driver programs run inside a real OpenEdge session, which the tests
//! can't start. The next best check: abl's own parser and lint rules find
//! nothing wrong with them.

use std::path::Path;
use std::sync::Arc;

use abl_core::features;
use abl_core::workspace::Workspace;
use abl_oe::driver;

fn findings(name: &str, text: &str) -> Vec<String> {
    let ws = Workspace::new();
    let path = Path::new(name);
    let ana = Arc::new(ws.analyze_file(path, text.to_string(), None));
    features::diagnostics(&ws, path, &ana)
        .into_iter()
        .map(|d| format!("{}:{}: {}", name, d.range.start.line + 1, d.message))
        .collect()
}

#[test]
fn drivers_are_clean_abl() {
    for (name, text) in [
        ("abl_compile.p", driver::COMPILE),
        ("abl_run.p", driver::RUN),
        ("abl_load_df.p", driver::LOAD_DF),
        ("abl_query.p", driver::QUERY),
        ("abl_import.p", driver::IMPORT),
        ("abl_profile.p", driver::PROFILE),
        ("abl_repl.p", driver::REPL),
        ("abl_test.p", driver::TEST),
    ] {
        let f = findings(name, text);
        assert!(f.is_empty(), "{}", f.join("\n"));
    }
}

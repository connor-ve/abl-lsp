//! Stubs for OpenEdge's built-in classes (Progress.Lang.*, Progress.Json.ObjectModel.*).
//!
//! They are ordinary ABL class files with signatures only, written once to a cache
//! folder so class resolution, hover and go-to-definition treat them like your own
//! classes. Lint skips them. Edit or add stubs in `server/builtin/` (regenerate the
//! list below when adding a file).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const FILES: &[(&str, &str)] = &[
    (
        "Progress/Json/ObjectModel/JsonArray.cls",
        include_str!("../builtin/Progress/Json/ObjectModel/JsonArray.cls"),
    ),
    (
        "Progress/Json/ObjectModel/JsonConstruct.cls",
        include_str!("../builtin/Progress/Json/ObjectModel/JsonConstruct.cls"),
    ),
    (
        "Progress/Json/ObjectModel/JsonDataType.cls",
        include_str!("../builtin/Progress/Json/ObjectModel/JsonDataType.cls"),
    ),
    (
        "Progress/Json/ObjectModel/JsonObject.cls",
        include_str!("../builtin/Progress/Json/ObjectModel/JsonObject.cls"),
    ),
    (
        "Progress/Json/ObjectModel/ObjectModelParser.cls",
        include_str!("../builtin/Progress/Json/ObjectModel/ObjectModelParser.cls"),
    ),
    (
        "Progress/Lang/AppError.cls",
        include_str!("../builtin/Progress/Lang/AppError.cls"),
    ),
    (
        "Progress/Lang/Class.cls",
        include_str!("../builtin/Progress/Lang/Class.cls"),
    ),
    (
        "Progress/Lang/Error.cls",
        include_str!("../builtin/Progress/Lang/Error.cls"),
    ),
    (
        "Progress/Lang/Object.cls",
        include_str!("../builtin/Progress/Lang/Object.cls"),
    ),
    (
        "Progress/Lang/ProError.cls",
        include_str!("../builtin/Progress/Lang/ProError.cls"),
    ),
    (
        "Progress/Lang/Stop.cls",
        include_str!("../builtin/Progress/Lang/Stop.cls"),
    ),
    (
        "Progress/Lang/StopError.cls",
        include_str!("../builtin/Progress/Lang/StopError.cls"),
    ),
    (
        "Progress/Lang/SysError.cls",
        include_str!("../builtin/Progress/Lang/SysError.cls"),
    ),
];

/// The folder holding the stubs, written on first use.
pub fn dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let root =
            std::env::temp_dir().join(format!("abl-lsp-builtin-{}", env!("CARGO_PKG_VERSION")));
        for (rel, text) in FILES {
            let p = root.join(rel);
            if std::fs::read_to_string(&p).ok().as_deref() != Some(*text) {
                std::fs::create_dir_all(p.parent()?).ok()?;
                std::fs::write(&p, text).ok()?;
            }
        }
        Some(root)
    })
    .as_deref()
}

pub fn is_builtin(p: &Path) -> bool {
    use crate::workspace::key;
    dir().is_some_and(|d| key(p).starts_with(&key(d)))
}

/// Stub file for a class path such as `Progress/Lang/AppError`.
pub fn class_file(rel: &str) -> Option<PathBuf> {
    let p = dir()?.join(format!("{rel}.cls"));
    p.is_file().then_some(p)
}

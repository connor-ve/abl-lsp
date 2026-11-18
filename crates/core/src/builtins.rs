//! Stubs for OpenEdge's built-in classes (Progress.Lang.*, Progress.Json.ObjectModel.*)
//! and common .NET types (System.*), for code that uses the .NET bridge.
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
    (
        "System/Collections/Generic/Dictionary.cls",
        include_str!("../builtin/System/Collections/Generic/Dictionary.cls"),
    ),
    (
        "System/Collections/Generic/List.cls",
        include_str!("../builtin/System/Collections/Generic/List.cls"),
    ),
    (
        "System/Convert.cls",
        include_str!("../builtin/System/Convert.cls"),
    ),
    (
        "System/DateTime.cls",
        include_str!("../builtin/System/DateTime.cls"),
    ),
    (
        "System/Environment.cls",
        include_str!("../builtin/System/Environment.cls"),
    ),
    (
        "System/EventArgs.cls",
        include_str!("../builtin/System/EventArgs.cls"),
    ),
    (
        "System/Exception.cls",
        include_str!("../builtin/System/Exception.cls"),
    ),
    (
        "System/Guid.cls",
        include_str!("../builtin/System/Guid.cls"),
    ),
    (
        "System/IO/Directory.cls",
        include_str!("../builtin/System/IO/Directory.cls"),
    ),
    (
        "System/IO/File.cls",
        include_str!("../builtin/System/IO/File.cls"),
    ),
    (
        "System/IO/Path.cls",
        include_str!("../builtin/System/IO/Path.cls"),
    ),
    (
        "System/Math.cls",
        include_str!("../builtin/System/Math.cls"),
    ),
    (
        "System/Object.cls",
        include_str!("../builtin/System/Object.cls"),
    ),
    (
        "System/String.cls",
        include_str!("../builtin/System/String.cls"),
    ),
    (
        "System/Text/StringBuilder.cls",
        include_str!("../builtin/System/Text/StringBuilder.cls"),
    ),
    (
        "System/TimeSpan.cls",
        include_str!("../builtin/System/TimeSpan.cls"),
    ),
    (
        "System/Windows/Forms/Application.cls",
        include_str!("../builtin/System/Windows/Forms/Application.cls"),
    ),
    (
        "System/Windows/Forms/Button.cls",
        include_str!("../builtin/System/Windows/Forms/Button.cls"),
    ),
    (
        "System/Windows/Forms/Control.cls",
        include_str!("../builtin/System/Windows/Forms/Control.cls"),
    ),
    (
        "System/Windows/Forms/Form.cls",
        include_str!("../builtin/System/Windows/Forms/Form.cls"),
    ),
    (
        "System/Windows/Forms/MessageBox.cls",
        include_str!("../builtin/System/Windows/Forms/MessageBox.cls"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{analyze, Severity};

    #[test]
    fn stubs_parse_cleanly() {
        for (rel, text) in FILES {
            let a = analyze(text.to_string(), false);
            let errors: Vec<&str> = a
                .diags
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .map(|d| d.message.as_str())
                .collect();
            assert!(errors.is_empty(), "{rel}: {errors:?}");
            let name = rel.trim_end_matches(".cls").replace('/', ".");
            assert_eq!(a.class_name.as_deref(), Some(name.as_str()), "{rel}");
        }
    }
}

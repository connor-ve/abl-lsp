//! ABL keyword table with abbreviation support.
//!
//! Entries are written as `FULL|REST`, where the part before `|` is the shortest
//! abbreviation the compiler accepts (`DEF|INE` accepts DEF, DEFI, DEFIN, DEFINE).
//! Entries without `|` must be written in full. Add keywords here as needed.

use std::collections::HashMap;
use std::sync::LazyLock;

const KEYWORDS: &[&str] = &[
    "ABS|OLUTE",
    "ABSTRACT",
    "ACCUM|ULATE",
    "ADD",
    "ALERT-BOX",
    "ALL",
    "AMBIG|UOUS",
    "AND",
    "ANY",
    "ANYWHERE",
    "APPEND",
    "APPLY",
    "AS",
    "ASC",
    "ASC|ENDING",
    "ASSIGN",
    "AT",
    "AVAIL|ABLE",
    "BEFORE-TABLE",
    "BEGINS",
    "BELL",
    "BLOCK-LEVEL",
    "BREAK",
    "BROWSE",
    "BUFFER",
    "BUFFER-COMPARE",
    "BUFFER-COPY",
    "BUTTON",
    "BUTTONS",
    "BY",
    "BY-REFERENCE",
    "BY-VALUE",
    "CAN-DO",
    "CAN-FIND",
    "CAPS",
    "CASE",
    "CAST",
    "CATCH",
    "CENTERED",
    "CHR",
    "CLASS",
    "CLEAR",
    "CLOSE",
    "COL|UMN",
    "COLUMN-LABEL",
    "COMPILE",
    "CONNECT",
    "CONNECTED",
    "CONSTRUCTOR",
    "CONTAINS",
    "COPY-LOB",
    "COUNT-OF",
    "CREATE",
    "CURRENT",
    "CURRENT-CHANGED",
    "DATA-SOURCE",
    "DATASET",
    "DATASET-HANDLE",
    "DEFAULT",
    "DEF|INE",
    "DEFINED",
    "DELETE",
    "DELIMITER",
    "DESC|ENDING",
    "DESTRUCTOR",
    "DISCONNECT",
    "DISP|LAY",
    "DO",
    "DOWN",
    "DYNAMIC-FUNCTION",
    "DYNAMIC-NEW",
    "EACH",
    "EDITING",
    "ELSE",
    "EMPTY",
    "END",
    "ENDKEY",
    "ENTRY",
    "ENUM",
    "EQ",
    "ERROR",
    "ERROR-STATUS",
    "ETIME",
    "EVENT",
    "EXCEPT",
    "EXCLUSIVE|-LOCK",
    "EXPORT",
    "EXTENT",
    "EXTERNAL",
    "FALSE",
    "FETCH",
    "FIELD",
    "FIELDS",
    "FILL",
    "FINAL",
    "FINALLY",
    "FIND",
    "FIRST",
    "FIRST-OF",
    "FLAGS",
    "FOR",
    "FORM",
    "FORMAT",
    "FORWARD",
    "FRAME",
    "FROM",
    "FUNCTION",
    "GE",
    "GET",
    "GET-CLASS",
    "GLOBAL",
    "GT",
    "HIDE",
    "IF",
    "IMAGE",
    "IMPLEMENTS",
    "IMPORT",
    "IN",
    "INDEX",
    "INFO|RMATION",
    "INHERITS",
    "INIT|IAL",
    "INPUT",
    "INPUT-OUTPUT",
    "INTERFACE",
    "INTO",
    "IS",
    "LABEL",
    "LAST",
    "LAST-OF",
    "LC",
    "LE",
    "LEAVE",
    "LENGTH",
    "LIKE",
    "LOCKED",
    "LOOKUP",
    "LT",
    "MATCHES",
    "MAX|IMUM",
    "MENU",
    "MESSAGE",
    "METHOD",
    "MIN|IMUM",
    "MODULO",
    "MTIME",
    "NE",
    "NEW",
    "NEXT",
    "NO",
    "NO-ERROR",
    "NO-LABELS",
    "NO-LOCK",
    "NO-UNDO",
    "NO-WAIT",
    "NON-SERIALIZABLE",
    "NOT",
    "NOW",
    "NUM-ENTRIES",
    "OF",
    "ON",
    "OPEN",
    "OR",
    "OTHERWISE",
    "OUTPUT",
    "OVERRIDE",
    "PACKAGE-PRIVATE",
    "PACKAGE-PROTECTED",
    "PARAM|ETER",
    "PERSIST|ENT",
    "PRESELECT",
    "PRIMARY",
    "PRIVATE",
    "PROCE|DURE",
    "PROGRAM-NAME",
    "PROPATH",
    "PROPERTY",
    "PROTECTED",
    "PUBLIC",
    "PUT",
    "QUERY",
    "QUESTION",
    "QUIT",
    "QUOTER",
    "R-INDEX",
    "RECTANGLE",
    "RELEASE",
    "REPEAT",
    "REPLACE",
    "RETURN",
    "RETURN-VALUE",
    "RETURNS",
    "ROUND",
    "ROUTINE-LEVEL",
    "ROW",
    "RUN",
    "SAVE",
    "SCREEN-VALUE",
    "SEARCH",
    "SELF",
    "SERIALIZABLE",
    "SESSION",
    "SET",
    "SHARE|-LOCK",
    "SHARED",
    "SIDE-LABELS",
    "SIGNATURE",
    "SIZE",
    "SKIP",
    "SOURCE-PROCEDURE",
    "STATIC",
    "STOP",
    "STREAM",
    "STRING",
    "SUB-MENU",
    "SUBSTITUTE",
    "SUBSTR|ING",
    "SUPER",
    "TABLE",
    "TABLE-HANDLE",
    "TARGET-PROCEDURE",
    "TEMP-TABLE",
    "THEN",
    "THIS-OBJECT",
    "THIS-PROCEDURE",
    "THROW",
    "TIME",
    "TITLE",
    "TO",
    "TODAY",
    "TRANS|ACTION",
    "TRIGGER",
    "TRIGGERS",
    "TRIM",
    "TRUE",
    "TRUNC|ATE",
    "TYPE-OF",
    "UNDO",
    "UNFORMATTED",
    "UNIQUE",
    "UPDATE",
    "USE-INDEX",
    "USE-WIDGET-POOL",
    "USING",
    "VALID-HANDLE",
    "VALID-OBJECT",
    "VALIDATE",
    "VALUE",
    "VAR|IABLE",
    "VIEW",
    "VIEW-AS",
    "WAIT-FOR",
    "WARNING",
    "WHEN",
    "WHERE",
    "WHILE",
    "WINDOW",
    "WITH",
    "WORK-TAB|LE",
    "WORKFILE",
    "YES",
    "YES-NO",
    "YES-NO-CANCEL",
];

/// Built-in data types (also keywords).
const TYPES: &[&str] = &[
    "BLOB",
    "CHAR|ACTER",
    "CLOB",
    "COM-HANDLE",
    "DATE",
    "DATETIME",
    "DATETIME-TZ",
    "DEC|IMAL",
    "HANDLE",
    "INT64",
    "INT|EGER",
    "LOG|ICAL",
    "LONGCHAR",
    "MEMPTR",
    "RAW",
    "RECID",
    "ROWID",
    "VOID",
    "WIDGET-H|ANDLE",
];

struct Table {
    exact: HashMap<String, &'static str>,
    abbrev: HashMap<String, &'static str>,
    all: Vec<&'static str>,
}

fn build(lists: &[&[&'static str]]) -> Table {
    let mut t = Table {
        exact: HashMap::new(),
        abbrev: HashMap::new(),
        all: Vec::new(),
    };
    for list in lists {
        for entry in *list {
            let (full, min) = match entry.find('|') {
                Some(p) => (entry.replace('|', ""), p),
                None => (entry.to_string(), entry.len()),
            };
            let full: &'static str = Box::leak(full.into_boxed_str());
            if t.exact.insert(full.to_string(), full).is_none() {
                t.all.push(full);
            }
            for len in min..full.len() {
                t.abbrev.entry(full[..len].to_string()).or_insert(full);
            }
        }
    }
    t.all.sort_unstable();
    t
}

static KW: LazyLock<Table> = LazyLock::new(|| build(&[KEYWORDS, TYPES]));
static TY: LazyLock<Table> = LazyLock::new(|| build(&[TYPES]));

fn lookup(t: &Table, word: &str) -> Option<&'static str> {
    if word.len() > 32 {
        return None;
    }
    let u = word.to_ascii_uppercase();
    t.exact.get(&u).or_else(|| t.abbrev.get(&u)).copied()
}

/// Full keyword for `word` (exact or a valid abbreviation), if it is one.
pub fn keyword(word: &str) -> Option<&'static str> {
    lookup(&KW, word)
}

/// Full built-in type name for `word` (`CHAR` -> `CHARACTER`).
pub fn type_name(word: &str) -> Option<&'static str> {
    lookup(&TY, word)
}

pub fn all_keywords() -> &'static [&'static str] {
    &KW.all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbreviations() {
        assert_eq!(keyword("def"), Some("DEFINE"));
        assert_eq!(keyword("DEFINE"), Some("DEFINE"));
        assert_eq!(keyword("de"), None);
        assert_eq!(keyword("var"), Some("VARIABLE"));
        assert_eq!(keyword("asc"), Some("ASC"));
        assert_eq!(keyword("asce"), Some("ASCENDING"));
        assert_eq!(type_name("char"), Some("CHARACTER"));
        assert_eq!(type_name("int"), Some("INTEGER"));
        assert_eq!(type_name("int64"), Some("INT64"));
        assert_eq!(type_name("OrderService"), None);
    }
}

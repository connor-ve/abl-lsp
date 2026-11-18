//! A structured model of `.df` schema dumps, a diff between two of them, and a
//! writer for the delta `.df` that turns one into the other.
//!
//! `schema.rs` turns a .df into symbols for the editor; this module keeps every
//! attribute line as written so a delta repeats the original definitions exactly.

use std::collections::BTreeSet;
use std::fmt::Write as _;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attr {
    /// Upper-cased first word: `FORMAT`, `LABEL`, `MANDATORY`.
    pub key: String,
    /// The rest of the line(s), as written (may be empty for flags).
    pub value: String,
}

impl Attr {
    /// What identifies this attribute among its siblings. Most keys appear once;
    /// triggers repeat with a different event (`TABLE-TRIGGER "CREATE" ...`).
    fn ident(&self) -> String {
        if self.key.ends_with("-TRIGGER") {
            let event = self.value.split_whitespace().next().unwrap_or("");
            format!("{} {}", self.key, event.to_ascii_uppercase())
        } else {
            self.key.clone()
        }
    }

    fn line(&self) -> String {
        if self.value.is_empty() {
            format!("  {}", self.key)
        } else {
            format!("  {} {}", self.key, self.value)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Field {
    pub name: String,
    pub data_type: String,
    pub attrs: Vec<Attr>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexField {
    pub name: String,
    /// The rest of the INDEX-FIELD line: `ASCENDING`, `DESCENDING ABBREVIATED`.
    pub options: String,
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    pub name: String,
    pub attrs: Vec<Attr>,
    pub fields: Vec<IndexField>,
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    pub name: String,
    pub attrs: Vec<Attr>,
    pub fields: Vec<Field>,
    pub indexes: Vec<Index>,
}

#[derive(Clone, Debug, Default)]
pub struct Sequence {
    pub name: String,
    pub attrs: Vec<Attr>,
}

#[derive(Clone, Debug, Default)]
pub struct Schema {
    pub tables: Vec<Table>,
    pub sequences: Vec<Sequence>,
    /// Problems met while reading, with 1-based line numbers.
    pub warnings: Vec<String>,
}

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

impl Schema {
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| eq(&t.name, name))
    }
}

impl Table {
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| eq(&f.name, name))
    }
    pub fn index(&self, name: &str) -> Option<&Index> {
        self.indexes.iter().find(|i| eq(&i.name, name))
    }
    pub fn primary(&self) -> Option<&Index> {
        self.indexes.iter().find(|i| i.has("PRIMARY"))
    }
}

impl Index {
    pub fn has(&self, flag: &str) -> bool {
        self.attrs.iter().any(|a| a.key == flag)
    }
}

/// Quoted strings at the start of `s`, in order, with `""` unescaped.
fn quoted_all(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut cur = String::new();
        loop {
            match chars.next() {
                None => return out,
                Some('"') if chars.peek() == Some(&'"') => {
                    chars.next();
                    cur.push('"');
                }
                Some('"') => break,
                Some(ch) => cur.push(ch),
            }
        }
        out.push(cur);
    }
    out
}

/// True when `s` has an unclosed double quote (a value continues on the next line).
fn open_quote(s: &str) -> bool {
    s.matches('"').count() % 2 == 1
}

fn q(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

enum Cur {
    None,
    Table(usize),
    Field(usize, usize),
    Index(usize, usize),
    Sequence(usize),
}

/// Reads a full schema dump. Delta statements (UPDATE, DROP, RENAME) are noted
/// in `warnings` and skipped.
pub fn parse(text: &str) -> Schema {
    let mut s = Schema::default();
    let mut cur = Cur::None;
    let mut lines = text.lines().enumerate().peekable();
    while let Some((n, raw)) = lines.next() {
        let line = raw.trim_end();
        if line == "." {
            break; // end of definitions; the PSC trailer follows
        }
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            let mut t = line.trim().to_string();
            while open_quote(&t) {
                let Some((_, next)) = lines.next() else { break };
                t.push('\n');
                t.push_str(next);
            }
            let (key, value) = match t.split_once(char::is_whitespace) {
                Some((k, v)) => (k.to_ascii_uppercase(), v.trim().to_string()),
                None => (t.to_ascii_uppercase(), String::new()),
            };
            let attr = Attr { key, value };
            match cur {
                Cur::Table(t) => s.tables[t].attrs.push(attr),
                Cur::Field(t, f) => s.tables[t].fields[f].attrs.push(attr),
                Cur::Index(t, i) if attr.key == "INDEX-FIELD" => {
                    let name = quoted_all(&attr.value)
                        .into_iter()
                        .next()
                        .unwrap_or_default();
                    let options = attr
                        .value
                        .rsplit_once('"')
                        .map_or("", |(_, rest)| rest)
                        .trim()
                        .to_string();
                    s.tables[t].indexes[i]
                        .fields
                        .push(IndexField { name, options });
                }
                Cur::Index(t, i) => s.tables[t].indexes[i].attrs.push(attr),
                Cur::Sequence(q) => s.sequences[q].attrs.push(attr),
                Cur::None => {}
            }
            continue;
        }
        let words: Vec<String> = line
            .split_whitespace()
            .take(2)
            .map(str::to_ascii_uppercase)
            .collect();
        let names = quoted_all(line);
        let after_as = || {
            let upper = line.to_ascii_uppercase();
            upper
                .rfind(" AS ")
                .map(|i| line[i + 4..].trim().to_ascii_lowercase())
                .unwrap_or_default()
        };
        cur = match (
            words.first().map(String::as_str),
            words.get(1).map(String::as_str),
        ) {
            (Some("ADD"), Some("TABLE")) if !names.is_empty() => {
                s.tables.push(Table {
                    name: names[0].clone(),
                    ..Default::default()
                });
                Cur::Table(s.tables.len() - 1)
            }
            (Some("ADD"), Some("FIELD")) if names.len() >= 2 => {
                match s.tables.iter().position(|t| eq(&t.name, &names[1])) {
                    Some(t) => {
                        s.tables[t].fields.push(Field {
                            name: names[0].clone(),
                            data_type: after_as(),
                            attrs: Vec::new(),
                        });
                        Cur::Field(t, s.tables[t].fields.len() - 1)
                    }
                    None => {
                        s.warnings.push(format!(
                            "line {}: field of unknown table {}",
                            n + 1,
                            names[1]
                        ));
                        Cur::None
                    }
                }
            }
            (Some("ADD"), Some("INDEX")) if names.len() >= 2 => {
                match s.tables.iter().position(|t| eq(&t.name, &names[1])) {
                    Some(t) => {
                        s.tables[t].indexes.push(Index {
                            name: names[0].clone(),
                            ..Default::default()
                        });
                        Cur::Index(t, s.tables[t].indexes.len() - 1)
                    }
                    None => {
                        s.warnings.push(format!(
                            "line {}: index of unknown table {}",
                            n + 1,
                            names[1]
                        ));
                        Cur::None
                    }
                }
            }
            (Some("ADD"), Some("SEQUENCE")) if !names.is_empty() => {
                s.sequences.push(Sequence {
                    name: names[0].clone(),
                    attrs: Vec::new(),
                });
                Cur::Sequence(s.sequences.len() - 1)
            }
            (Some(w @ ("UPDATE" | "DROP" | "RENAME")), _) => {
                s.warnings.push(format!(
                    "line {}: {w} statement skipped (this looks like a delta .df, not a full dump)",
                    n + 1
                ));
                Cur::None
            }
            _ => {
                s.warnings
                    .push(format!("line {}: not understood: {}", n + 1, line.trim()));
                Cur::None
            }
        };
    }
    s
}

// ---- Diff ----

/// Attributes that describe physical layout, not the definition. Changes to
/// them are not schema changes a delta can make.
const PHYSICAL: &[&str] = &["POSITION", "AREA"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttrChange {
    Added(Attr),
    Removed(Attr),
    Changed { from: Attr, to: Attr },
}

fn attr_changes(old: &[Attr], new: &[Attr], skip: &[&str]) -> Vec<AttrChange> {
    let keep = |a: &&Attr| !skip.contains(&a.key.as_str());
    let mut out = Vec::new();
    for n in new.iter().filter(keep) {
        match old.iter().filter(keep).find(|o| o.ident() == n.ident()) {
            None => out.push(AttrChange::Added(n.clone())),
            Some(o) if o.value != n.value => out.push(AttrChange::Changed {
                from: o.clone(),
                to: n.clone(),
            }),
            _ => {}
        }
    }
    for o in old.iter().filter(keep) {
        if !new.iter().any(|n| n.ident() == o.ident()) {
            out.push(AttrChange::Removed(o.clone()));
        }
    }
    out
}

#[derive(Clone, Debug)]
pub enum FieldChange {
    Added(Field),
    Removed(Field),
    /// Same data type and extent: an UPDATE FIELD.
    Updated {
        name: String,
        changes: Vec<AttrChange>,
    },
    /// Type or extent changed: the field must be dropped and added again (its data is lost).
    Recreated {
        old: Field,
        new: Field,
    },
}

#[derive(Clone, Debug)]
pub enum IndexChange {
    Added(Index),
    Removed(Index),
    /// Fields or flags changed: dropped and added again.
    Recreated {
        old: Index,
        new: Index,
    },
}

#[derive(Clone, Debug)]
pub struct TableDiff {
    pub name: String,
    pub attrs: Vec<AttrChange>,
    pub fields: Vec<FieldChange>,
    pub indexes: Vec<IndexChange>,
    /// The new primary index, when it moved to another existing index.
    pub new_primary: Option<String>,
    pub area_changed: Option<(String, String)>,
}

impl TableDiff {
    fn is_empty(&self) -> bool {
        self.attrs.is_empty()
            && self.fields.is_empty()
            && self.indexes.is_empty()
            && self.new_primary.is_none()
            && self.area_changed.is_none()
    }
}

#[derive(Clone, Debug)]
pub enum SequenceChange {
    Added(Sequence),
    Removed(Sequence),
    Updated {
        name: String,
        changes: Vec<AttrChange>,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Diff {
    pub added_tables: Vec<Table>,
    pub removed_tables: Vec<Table>,
    pub changed_tables: Vec<TableDiff>,
    pub sequences: Vec<SequenceChange>,
    /// Things a reader should know: data loss, likely renames, steps the delta can't do.
    pub warnings: Vec<String>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.added_tables.is_empty()
            && self.removed_tables.is_empty()
            && self.changed_tables.is_empty()
            && self.sequences.is_empty()
    }
}

fn attr<'a>(attrs: &'a [Attr], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| a.key == key)
        .map(|a| a.value.as_str())
}

fn index_shape(i: &Index) -> (Vec<(String, String)>, BTreeSet<String>) {
    let fields = i
        .fields
        .iter()
        .map(|f| (f.name.to_ascii_lowercase(), f.options.to_ascii_uppercase()))
        .collect();
    let flags = i
        .attrs
        .iter()
        .filter(|a| !PHYSICAL.contains(&a.key.as_str()) && a.key != "PRIMARY")
        .map(|a| format!("{} {}", a.key, a.value))
        .collect();
    (fields, flags)
}

pub fn diff(old: &Schema, new: &Schema) -> Diff {
    let mut d = Diff::default();
    for t in &new.tables {
        match old.table(&t.name) {
            None => d.added_tables.push(t.clone()),
            Some(o) => {
                let td = diff_table(o, t, &mut d.warnings);
                if !td.is_empty() {
                    d.changed_tables.push(td);
                }
            }
        }
    }
    for t in &old.tables {
        if new.table(&t.name).is_none() {
            d.warnings.push(format!(
                "table {} is dropped: its data is deleted when the delta is loaded",
                t.name
            ));
            d.removed_tables.push(t.clone());
        }
    }
    for s in &new.sequences {
        match old.sequences.iter().find(|o| eq(&o.name, &s.name)) {
            None => d.sequences.push(SequenceChange::Added(s.clone())),
            Some(o) => {
                let changes = attr_changes(&o.attrs, &s.attrs, &[]);
                if !changes.is_empty() {
                    d.sequences.push(SequenceChange::Updated {
                        name: s.name.clone(),
                        changes,
                    });
                }
            }
        }
    }
    for s in &old.sequences {
        if !new.sequences.iter().any(|n| eq(&n.name, &s.name)) {
            d.sequences.push(SequenceChange::Removed(s.clone()));
        }
    }
    d
}

fn diff_table(o: &Table, n: &Table, warnings: &mut Vec<String>) -> TableDiff {
    let mut td = TableDiff {
        name: n.name.clone(),
        attrs: attr_changes(&o.attrs, &n.attrs, PHYSICAL),
        fields: Vec::new(),
        indexes: Vec::new(),
        new_primary: None,
        area_changed: None,
    };
    if let (Some(a), Some(b)) = (attr(&o.attrs, "AREA"), attr(&n.attrs, "AREA")) {
        if a != b {
            warnings.push(format!(
                "table {}: AREA {a} -> {b} is not in the delta; move a table between areas with `proutil tablemove`",
                n.name
            ));
            td.area_changed = Some((a.to_string(), b.to_string()));
        }
    }
    for f in &n.fields {
        match o.field(&f.name) {
            None => td.fields.push(FieldChange::Added(f.clone())),
            Some(of) => {
                let extent = |x: &Field| attr(&x.attrs, "EXTENT").unwrap_or("0").to_string();
                if !eq(&of.data_type, &f.data_type) || extent(of) != extent(f) {
                    warnings.push(format!(
                        "field {}.{}: {} -> {} needs a drop and re-add; its data is lost",
                        n.name,
                        f.name,
                        describe_type(of),
                        describe_type(f)
                    ));
                    td.fields.push(FieldChange::Recreated {
                        old: of.clone(),
                        new: f.clone(),
                    });
                } else {
                    let changes = attr_changes(&of.attrs, &f.attrs, PHYSICAL);
                    if !changes.is_empty() {
                        td.fields.push(FieldChange::Updated {
                            name: f.name.clone(),
                            changes,
                        });
                    }
                }
            }
        }
    }
    for f in &o.fields {
        if n.field(&f.name).is_none() {
            td.fields.push(FieldChange::Removed(f.clone()));
        }
    }
    // A dropped and an added field of the same type in one table is often a rename.
    let added: Vec<&Field> = td
        .fields
        .iter()
        .filter_map(|c| match c {
            FieldChange::Added(f) => Some(f),
            _ => None,
        })
        .collect();
    for c in &td.fields {
        if let FieldChange::Removed(r) = c {
            if let Some(a) = added.iter().find(|a| eq(&a.data_type, &r.data_type)) {
                warnings.push(format!(
                    "field {}.{} is dropped and {} added: if this is a rename, use RENAME FIELD \"{}\" OF \"{}\" TO \"{}\" to keep the data",
                    n.name, r.name, a.name, r.name, n.name, a.name
                ));
            }
        }
    }
    for i in &n.indexes {
        match o.index(&i.name) {
            None => td.indexes.push(IndexChange::Added(i.clone())),
            Some(oi) if index_shape(oi) != index_shape(i) => {
                td.indexes.push(IndexChange::Recreated {
                    old: oi.clone(),
                    new: i.clone(),
                });
            }
            _ => {}
        }
    }
    for i in &o.indexes {
        if n.index(&i.name).is_none() {
            td.indexes.push(IndexChange::Removed(i.clone()));
        }
    }
    let (op, np) = (o.primary(), n.primary());
    if let (Some(op), Some(np)) = (op, np) {
        let recreated = |name: &str| {
            td.indexes.iter().any(|c| match c {
                IndexChange::Recreated { new, .. } | IndexChange::Added(new) => eq(&new.name, name),
                IndexChange::Removed(_) => false,
            })
        };
        if !eq(&op.name, &np.name) && !recreated(&np.name) {
            td.new_primary = Some(np.name.clone());
        }
        let old_primary_dropped = td.indexes.iter().any(|c| match c {
            IndexChange::Removed(i) | IndexChange::Recreated { old: i, .. } => {
                eq(&i.name, &op.name)
            }
            IndexChange::Added(_) => false,
        });
        if old_primary_dropped && eq(&op.name, &np.name) {
            warnings.push(format!(
                "table {}: primary index {} changes; Progress won't drop the primary index, so make another index primary before loading, or recreate it by hand",
                n.name, op.name
            ));
        }
    }
    td
}

fn describe_type(f: &Field) -> String {
    match attr(&f.attrs, "EXTENT") {
        Some(e) if e != "0" => format!("{} EXTENT {e}", f.data_type),
        _ => f.data_type.clone(),
    }
}

// ---- Delta writer ----

fn add_table(out: &mut String, t: &Table) {
    let _ = writeln!(out, "ADD TABLE {}", q(&t.name));
    for a in &t.attrs {
        let _ = writeln!(out, "{}", a.line());
    }
    out.push('\n');
    for f in &t.fields {
        add_field(out, &t.name, f);
    }
    for i in &t.indexes {
        add_index(out, &t.name, i);
    }
}

fn add_field(out: &mut String, table: &str, f: &Field) {
    let _ = writeln!(
        out,
        "ADD FIELD {} OF {} AS {}",
        q(&f.name),
        q(table),
        f.data_type
    );
    for a in &f.attrs {
        let _ = writeln!(out, "{}", a.line());
    }
    out.push('\n');
}

fn add_index(out: &mut String, table: &str, i: &Index) {
    let _ = writeln!(out, "ADD INDEX {} ON {}", q(&i.name), q(table));
    for a in &i.attrs {
        let _ = writeln!(out, "{}", a.line());
    }
    for f in &i.fields {
        let opts = if f.options.is_empty() {
            String::new()
        } else {
            format!(" {}", f.options)
        };
        let _ = writeln!(out, "  INDEX-FIELD {}{opts}", q(&f.name));
    }
    out.push('\n');
}

/// Writes `UPDATE ...` lines for changed attributes. Removed attributes with a
/// value are set to `?` (unknown); removed flags can't be expressed and are
/// returned as notes.
fn update_attrs(out: &mut String, header: &str, changes: &[AttrChange]) -> Vec<String> {
    let mut notes = Vec::new();
    let mut lines = Vec::new();
    for c in changes {
        match c {
            AttrChange::Added(a) | AttrChange::Changed { to: a, .. } => lines.push(a.line()),
            AttrChange::Removed(a) if !a.value.is_empty() => lines.push(format!("  {} ?", a.key)),
            AttrChange::Removed(a) => notes.push(a.key.clone()),
        }
    }
    if !lines.is_empty() {
        let _ = writeln!(out, "{header}");
        for l in lines {
            let _ = writeln!(out, "{l}");
        }
        out.push('\n');
    }
    notes
}

/// The delta `.df` that turns `old` into `new`, plus notes about steps it
/// could not express. Ends with the `.` terminator and no PSC trailer.
pub fn delta(d: &Diff) -> (String, Vec<String>) {
    let mut out = String::new();
    let mut notes = Vec::new();
    for t in &d.changed_tables {
        // Drop indexes first (a field in an index can't be dropped), then fields;
        // then add fields before the indexes that use them.
        for c in &t.indexes {
            if let IndexChange::Removed(i) | IndexChange::Recreated { old: i, .. } = c {
                let _ = writeln!(out, "DROP INDEX {} ON {}\n", q(&i.name), q(&t.name));
            }
        }
        for c in &t.fields {
            if let FieldChange::Removed(f) | FieldChange::Recreated { old: f, .. } = c {
                let _ = writeln!(out, "DROP FIELD {} OF {}\n", q(&f.name), q(&t.name));
            }
        }
        for flag in update_attrs(&mut out, &format!("UPDATE TABLE {}", q(&t.name)), &t.attrs) {
            notes.push(format!("table {}: remove {flag} by hand", t.name));
        }
        for c in &t.fields {
            match c {
                FieldChange::Added(f) | FieldChange::Recreated { new: f, .. } => {
                    add_field(&mut out, &t.name, f)
                }
                FieldChange::Updated { name, changes } => {
                    let header = format!("UPDATE FIELD {} OF {}", q(name), q(&t.name));
                    for flag in update_attrs(&mut out, &header, changes) {
                        notes.push(format!("field {}.{name}: remove {flag} by hand", t.name));
                    }
                }
                FieldChange::Removed(_) => {}
            }
        }
        for c in &t.indexes {
            if let IndexChange::Added(i) | IndexChange::Recreated { new: i, .. } = c {
                add_index(&mut out, &t.name, i);
            }
        }
        if let Some(p) = &t.new_primary {
            let _ = writeln!(out, "UPDATE PRIMARY INDEX {} ON {}\n", q(p), q(&t.name));
        }
    }
    for t in &d.added_tables {
        add_table(&mut out, t);
    }
    for c in &d.sequences {
        match c {
            SequenceChange::Added(s) => {
                let _ = writeln!(out, "ADD SEQUENCE {}", q(&s.name));
                for a in &s.attrs {
                    let _ = writeln!(out, "{}", a.line());
                }
                out.push('\n');
            }
            SequenceChange::Updated { name, changes } => {
                for flag in update_attrs(&mut out, &format!("UPDATE SEQUENCE {}", q(name)), changes)
                {
                    notes.push(format!("sequence {name}: remove {flag} by hand"));
                }
            }
            SequenceChange::Removed(s) => {
                let _ = writeln!(out, "DROP SEQUENCE {}\n", q(&s.name));
            }
        }
    }
    for t in &d.removed_tables {
        let _ = writeln!(out, "DROP TABLE {}\n", q(&t.name));
    }
    out.push_str(".\n");
    (out, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = r#"ADD TABLE "Customer"
  AREA "Data"
  LABEL "Customer"
  DUMP-NAME "customer"

ADD FIELD "CustNum" OF "Customer" AS integer
  FORMAT ">>>>9"
  INITIAL "0"
  POSITION 2
  ORDER 10

ADD FIELD "Name" OF "Customer" AS character
  FORMAT "x(30)"
  LABEL "Name"
  POSITION 3
  ORDER 20

ADD FIELD "Fax" OF "Customer" AS character
  FORMAT "x(20)"
  POSITION 4
  ORDER 30

ADD FIELD "Balance" OF "Customer" AS integer
  FORMAT "->>>,>>9"
  POSITION 5
  ORDER 40

ADD INDEX "CustNum" ON "Customer"
  AREA "Index"
  UNIQUE
  PRIMARY
  INDEX-FIELD "CustNum" ASCENDING

ADD INDEX "Name" ON "Customer"
  AREA "Index"
  INDEX-FIELD "Name" ASCENDING

ADD TABLE "OldStuff"
  AREA "Data"

ADD FIELD "x" OF "OldStuff" AS character
  FORMAT "x(8)"

ADD SEQUENCE "NextCust"
  INITIAL 1
  INCREMENT 1

.
PSC
cpstream=UTF-8
.
0000001234
"#;

    const NEW: &str = r#"ADD TABLE "Customer"
  AREA "Data"
  LABEL "Customers"
  DESCRIPTION "All customers, with ""quotes""
over two lines"
  DUMP-NAME "customer"

ADD FIELD "CustNum" OF "Customer" AS integer
  FORMAT ">>>>9"
  INITIAL "0"
  POSITION 2
  ORDER 10

ADD FIELD "Name" OF "Customer" AS character
  FORMAT "x(40)"
  POSITION 3
  ORDER 20

ADD FIELD "Email" OF "Customer" AS character
  FORMAT "x(60)"
  POSITION 6
  ORDER 30

ADD FIELD "Balance" OF "Customer" AS decimal
  FORMAT "->>>,>>9.99"
  DECIMALS 2
  POSITION 5
  ORDER 40

ADD INDEX "CustNum" ON "Customer"
  AREA "Index"
  UNIQUE
  PRIMARY
  INDEX-FIELD "CustNum" ASCENDING

ADD INDEX "Name" ON "Customer"
  AREA "Index"
  INDEX-FIELD "Name" ASCENDING
  INDEX-FIELD "Email" ASCENDING

ADD TABLE "Invoice"
  AREA "Data"

ADD FIELD "InvNum" OF "Invoice" AS integer
  FORMAT ">>>>>9"

ADD INDEX "InvNum" ON "Invoice"
  UNIQUE
  PRIMARY
  INDEX-FIELD "InvNum" ASCENDING

ADD SEQUENCE "NextCust"
  INITIAL 1
  INCREMENT 5

ADD SEQUENCE "NextInv"
  INITIAL 1
  INCREMENT 1

.
"#;

    #[test]
    fn parses_a_dump() {
        let s = parse(NEW);
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        assert_eq!(s.tables.len(), 2);
        let c = s.table("customer").unwrap();
        assert_eq!(c.fields.len(), 4);
        assert_eq!(c.field("balance").unwrap().data_type, "decimal");
        let desc = attr(&c.attrs, "DESCRIPTION").unwrap();
        assert!(desc.contains("\nover two lines"), "{desc}");
        let name_idx = c.index("Name").unwrap();
        assert_eq!(name_idx.fields.len(), 2);
        assert_eq!(name_idx.fields[1].options, "ASCENDING");
        assert_eq!(c.primary().unwrap().name, "CustNum");
        assert_eq!(s.sequences.len(), 2);
    }

    #[test]
    fn delta_statements_are_reported() {
        let s = parse("UPDATE FIELD \"x\" OF \"t\"\n  FORMAT \"x(9)\"\n.\n");
        assert!(s.warnings[0].contains("delta"), "{:?}", s.warnings);
    }

    #[test]
    fn diff_and_delta() {
        let d = diff(&parse(OLD), &parse(NEW));
        assert_eq!(d.added_tables.len(), 1);
        assert_eq!(d.removed_tables.len(), 1);
        assert_eq!(d.changed_tables.len(), 1);
        let t = &d.changed_tables[0];
        // LABEL changed and DESCRIPTION added; AREA/POSITION are not definition changes.
        assert_eq!(t.attrs.len(), 2, "{:?}", t.attrs);
        let kinds: Vec<String> = t
            .fields
            .iter()
            .map(|c| match c {
                FieldChange::Added(f) => format!("+{}", f.name),
                FieldChange::Removed(f) => format!("-{}", f.name),
                FieldChange::Updated { name, .. } => format!("~{name}"),
                FieldChange::Recreated { new, .. } => format!("!{}", new.name),
            })
            .collect();
        assert_eq!(kinds, ["~Name", "+Email", "!Balance", "-Fax"]);
        assert!(matches!(
            t.indexes.as_slice(),
            [IndexChange::Recreated { .. }]
        ));
        assert!(d
            .warnings
            .iter()
            .any(|w| w.contains("Balance") && w.contains("data is lost")));
        assert!(
            d.warnings
                .iter()
                .any(|w| w.contains("RENAME FIELD \"Fax\"")),
            "{:?}",
            d.warnings
        );
        assert!(d.warnings.iter().any(|w| w.contains("OldStuff")));

        let (text, notes) = delta(&d);
        // Name lost its LABEL: set to unknown.
        assert!(
            text.contains("UPDATE FIELD \"Name\" OF \"Customer\"\n  FORMAT \"x(40)\"\n  LABEL ?\n"),
            "{text}"
        );
        let pos = |s: &str| {
            text.find(s)
                .unwrap_or_else(|| panic!("missing {s} in\n{text}"))
        };
        assert!(pos("DROP INDEX \"Name\"") < pos("DROP FIELD \"Fax\""));
        assert!(
            pos("DROP FIELD \"Balance\"") < pos("ADD FIELD \"Balance\" OF \"Customer\" AS decimal")
        );
        assert!(pos("ADD FIELD \"Email\"") < pos("ADD INDEX \"Name\" ON \"Customer\""));
        assert!(pos("ADD TABLE \"Invoice\"") < pos("DROP TABLE \"OldStuff\""));
        assert!(text.contains("UPDATE SEQUENCE \"NextCust\"\n  INCREMENT 5\n"));
        assert!(text.contains("ADD SEQUENCE \"NextInv\""));
        assert!(text.contains("DESCRIPTION \"All customers, with \"\"quotes\"\"\nover two lines\""));
        assert!(text.ends_with("\n.\n"));
        assert!(notes.is_empty(), "{notes:?}");

        // Loading the delta's ADD statements back gives the same definitions.
        let again = parse(&text);
        let inv = again.table("Invoice").unwrap();
        assert_eq!(inv.fields.len(), 1);
        assert_eq!(inv.primary().unwrap().name, "InvNum");

        assert!(diff(&parse(NEW), &parse(NEW)).is_empty());
    }

    #[test]
    fn primary_index_moves() {
        let old = "ADD TABLE \"t\"\n\nADD FIELD \"a\" OF \"t\" AS integer\n\nADD INDEX \"a\" ON \"t\"\n  PRIMARY\n  INDEX-FIELD \"a\" ASCENDING\n\nADD INDEX \"b\" ON \"t\"\n  INDEX-FIELD \"a\" DESCENDING\n\n.\n";
        let new = old
            .replace(
                "  PRIMARY\n  INDEX-FIELD \"a\" ASCENDING",
                "  INDEX-FIELD \"a\" ASCENDING",
            )
            .replace(
                "ADD INDEX \"b\" ON \"t\"\n",
                "ADD INDEX \"b\" ON \"t\"\n  PRIMARY\n",
            );
        let d = diff(&parse(old), &parse(&new));
        let t = &d.changed_tables[0];
        assert!(t.indexes.is_empty(), "{:?}", t.indexes);
        assert_eq!(t.new_primary.as_deref(), Some("b"));
        assert!(delta(&d).0.contains("UPDATE PRIMARY INDEX \"b\" ON \"t\""));
    }
}

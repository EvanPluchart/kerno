//! Schema compare (K30; DBeaver parity NAV-57, CM-21, CM-59, TOOL-22): the
//! structure of a *source* schema (the model) against a *target* schema
//! (what the script changes), on two sessions of the same engine family,
//! and the script that brings the target to the source.
//!
//! Tables are compared through their [`TableStructure`] (columns, keys,
//! check and unique constraints, indexes): the source's structure becomes a
//! [`TableDraft`] tied to the target's items by name, and
//! [`table_draft::changes`] says what differs, which the target's dialect
//! writes as it does for the structure editor. Views, materialized views,
//! functions, procedures, sequences, types and triggers are compared by
//! the DDL the driver prints, whitespace aside. Rows are never read.
//!
//! The diff is pure; nothing here runs the script ([`script`] returns
//! text that the user reads and runs in a query tab). DBeaver Community's
//! simple compare (`org.jkiss.dbeaver.tools.compare.simple`) was read for
//! the flow; no logic is ported (D-103).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::config::Engine;
use crate::dialect::Dialect;
use crate::driver::Session;
use crate::erd;
use crate::error::{Error, Result, describe};
use crate::schema::{
    ConstraintInfo, ConstraintKind, IndexInfo, ObjectRef, RelationKind, RoutineKind, TableRef,
    TableStructure,
};
use crate::table_draft::{self, ForeignKeyDraft, TableChange, TableDraft, present};

/// Engines whose schemas compare with each other: the types of one map to
/// the other's one to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Postgres,
    /// MySQL and MariaDB.
    MySql,
}

impl Family {
    /// The family of `engine`; `None` for an engine the compare does not
    /// handle yet.
    pub fn of(engine: Engine) -> Option<Family> {
        if engine == Engine::Postgres {
            Some(Family::Postgres)
        } else if matches!(engine, Engine::MySql | Engine::MariaDb) {
            Some(Family::MySql)
        } else {
            None
        }
    }

    /// Whether two names of this kind of thing name the same object, as
    /// the engine compares identifiers. MySQL and MariaDB compare column,
    /// index, constraint and routine names without case on every platform;
    /// table, view and trigger names are compared as written (as on a Linux
    /// server, `lower_case_table_names = 0`).
    fn same(family: Option<Family>, folded: bool, a: &str, b: &str) -> bool {
        match family {
            Some(Family::MySql) if folded => a.to_lowercase() == b.to_lowercase(),
            _ => a == b,
        }
    }
}

/// Why `source` cannot be compared with `target`, in plain words; `None`
/// when it can.
pub fn refusal(source: Engine, target: Engine) -> Option<String> {
    match (Family::of(source), Family::of(target)) {
        (Some(a), Some(b)) if a == b => None,
        (Some(_), Some(_)) => Some(format!(
            "Comparing a {} schema with a {} one is not supported yet: the types do not map \
             one to one.",
            source.label(),
            target.label()
        )),
        _ => Some(format!(
            "Comparing {} schemas is not supported yet.",
            if Family::of(source).is_none() {
                source.label()
            } else {
                target.label()
            }
        )),
    }
}

/// What kind of object an [`Item`] is, in the order the view groups them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Table,
    View,
    MaterializedView,
    Function,
    Procedure,
    Sequence,
    Type,
    Trigger,
}

impl Kind {
    pub const ALL: [Kind; 8] = [
        Kind::Table,
        Kind::View,
        Kind::MaterializedView,
        Kind::Function,
        Kind::Procedure,
        Kind::Sequence,
        Kind::Type,
        Kind::Trigger,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Table => "table",
            Kind::View => "view",
            Kind::MaterializedView => "materialized view",
            Kind::Function => "function",
            Kind::Procedure => "procedure",
            Kind::Sequence => "sequence",
            Kind::Type => "type",
            Kind::Trigger => "trigger",
        }
    }

    pub fn plural(self) -> &'static str {
        match self {
            Kind::Table => "Tables",
            Kind::View => "Views",
            Kind::MaterializedView => "Materialized views",
            Kind::Function => "Functions",
            Kind::Procedure => "Procedures",
            Kind::Sequence => "Sequences",
            Kind::Type => "Types",
            Kind::Trigger => "Triggers",
        }
    }

    fn of(object: &ObjectRef) -> Option<Kind> {
        Some(match object {
            ObjectRef::Relation(_, RelationKind::View) => Kind::View,
            ObjectRef::Relation(_, RelationKind::MaterializedView) => Kind::MaterializedView,
            ObjectRef::Relation(..) => Kind::Table,
            ObjectRef::Routine {
                kind: RoutineKind::Procedure,
                ..
            } => Kind::Procedure,
            ObjectRef::Routine { .. } => Kind::Function,
            ObjectRef::Sequence(_) => Kind::Sequence,
            ObjectRef::Type(_) => Kind::Type,
            ObjectRef::Trigger { .. } => Kind::Trigger,
            ObjectRef::Event(_)
            | ObjectRef::Index { .. }
            | ObjectRef::Schema(_)
            | ObjectRef::Database(_) => return None,
        })
    }
}

/// How an object is compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Definition {
    /// A table, by its structure.
    Table(TableStructure),
    /// Anything else, by the DDL the driver prints.
    Ddl(String),
}

/// One object of a side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub object: ObjectRef,
    pub definition: Definition,
}

/// What was read of one schema (a database on engines without schemas).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Side {
    pub schema: String,
    pub objects: Vec<Object>,
}

/// How far a side's load is: objects read and to read.
pub type Progress = erd::Progress;

/// Reads what the compare needs of `schema`: the list of its objects, the
/// structure of each table and the DDL of everything else, eight at a time
/// (the diagram's loop, [`erd::buffered`]), telling `progress` after each.
/// Partitions and foreign tables are left out, and so are sequences owned
/// by a column (they come with their column), aggregates and window
/// functions, and what the driver cannot print. Stops with
/// [`Error::Cancelled`] once `cancel` is set.
pub async fn load(
    session: Arc<dyn Session>,
    schema: &str,
    progress: impl Fn(Progress) + Send,
    cancel: &AtomicBool,
) -> Result<Side> {
    let dialect = session.dialect();
    if dialect.has_schemas() {
        // The server then prints this schema's own names unqualified, as it
        // does on the other side; the query tab enters its schema this way.
        let sql = format!("SET search_path TO {}", dialect.quote_ident(schema));
        crate::exec::collect(session.clone(), sql).await?;
    }
    let listed = session.objects(schema).await?;
    let at = |name: &str| TableRef::new(schema, name);
    let mut tables = Vec::new();
    let mut others = Vec::new();
    for relation in listed.relations {
        let table = at(&relation.name);
        match relation.kind {
            _ if relation.is_partition => {}
            RelationKind::Table | RelationKind::PartitionedTable => {
                tables.push((table, relation.kind))
            }
            RelationKind::View | RelationKind::MaterializedView => {
                others.push(ObjectRef::Relation(table, relation.kind))
            }
            RelationKind::ForeignTable => {}
        }
    }
    others.extend(
        listed
            .routines
            .into_iter()
            .filter(|routine| {
                matches!(routine.kind, RoutineKind::Function | RoutineKind::Procedure)
            })
            .map(|routine| ObjectRef::Routine {
                schema: schema.to_string(),
                name: routine.name,
                kind: routine.kind,
                identity_arguments: routine.identity_arguments,
            }),
    );
    others.extend(
        listed
            .sequences
            .into_iter()
            .filter(|sequence| sequence.owned_by.is_none())
            .map(|sequence| ObjectRef::Sequence(at(&sequence.name))),
    );
    others.extend(
        listed
            .types
            .into_iter()
            .map(|ty| ObjectRef::Type(at(&ty.name))),
    );
    others.extend(
        listed
            .triggers
            .into_iter()
            .map(|trigger| ObjectRef::Trigger {
                table: at(&trigger.table),
                name: trigger.name,
            }),
    );

    let total = tables.len() + others.len();
    let mut done = 0;
    progress(Progress { done, total });
    let mut objects = Vec::new();
    let structure = |(table, kind): (TableRef, RelationKind)| {
        let session = session.clone();
        async move {
            let read = session.table_structure(&table).await;
            (ObjectRef::Relation(table, kind), read)
        }
    };
    erd::buffered(tables, structure, cancel, |(object, read)| {
        objects.push(Object {
            object,
            definition: Definition::Table(read?),
        });
        done += 1;
        progress(Progress { done, total });
        Ok(())
    })
    .await?;
    let ddl = |object: ObjectRef| {
        let session = session.clone();
        async move {
            let read = session.ddl(&object).await;
            (object, read)
        }
    };
    erd::buffered(others, ddl, cancel, |(object, read)| {
        match read {
            Ok(ddl) => objects.push(Object {
                object,
                definition: Definition::Ddl(ddl),
            }),
            Err(Error::Unsupported(_)) => {}
            Err(error) => return Err(error),
        }
        done += 1;
        progress(Progress { done, total });
        Ok(())
    })
    .await?;
    Ok(Side {
        schema: schema.to_string(),
        objects,
    })
}

/// Where an object stands between the two sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    OnlyInSource,
    OnlyInTarget,
    Different,
    Same,
}

impl State {
    pub const ALL: [State; 4] = [
        State::OnlyInSource,
        State::OnlyInTarget,
        State::Different,
        State::Same,
    ];

    /// A mark that does not rely on colour, and the words for it.
    pub fn mark(self) -> (&'static str, &'static str) {
        match self {
            State::OnlyInSource => ("+", "only in source"),
            State::OnlyInTarget => ("−", "only in target"),
            State::Different => ("≠", "different"),
            State::Same => ("=", "same"),
        }
    }
}

/// When a line of the script runs: the order that works (types before the
/// tables using them, new tables before the keys that point at them, code
/// after the tables, drops last).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    CreateTypes,
    DropConstraints,
    CreateTables,
    AlterTables,
    AddConstraints,
    AddForeignKeys,
    Routines,
    Views,
    Triggers,
    DropTriggers,
    DropViews,
    DropRoutines,
    DropKeys,
    DropTables,
    DropTypes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LineKind {
    /// A statement.
    Run,
    /// A statement that drops what only the target has: written as a
    /// comment unless drops are asked for.
    Drop,
    /// A comment: what is not written, and why.
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    phase: Phase,
    kind: LineKind,
    text: String,
}

/// One object of the comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub kind: Kind,
    /// The name shown: `name`, `name(arguments)` for a routine, `name on
    /// table` for a trigger.
    pub name: String,
    /// The object on each side, when it is there.
    pub source: Option<ObjectRef>,
    pub target: Option<ObjectRef>,
    pub state: State,
    /// What differs, in plain words, each as the change the script makes
    /// in the target.
    pub differences: Vec<String>,
    /// Each side's definition, as shown side by side (tables as the
    /// dialect writes them from their structure).
    pub source_ddl: String,
    pub target_ddl: String,
    lines: Vec<Line>,
}

impl Item {
    /// Whether the script holds a statement for this object that drops
    /// what only the target has.
    pub fn drops(&self) -> bool {
        self.lines.iter().any(|line| line.kind == LineKind::Drop)
    }
}

/// `text` with the schema qualifier `from.` (quoted or not) of
/// `from`'s own objects replaced by `to`'s: the source's DDL and
/// definitions name the source schema where the target's name the target.
// ponytail: a textual rewrite, so a schema name followed by a dot inside a
// string literal is rewritten too; parse the SQL if that ever matters.
pub fn retarget(text: &str, from: &str, to: &str, dialect: &dyn Dialect) -> String {
    if from == to || from.is_empty() {
        return text.to_string();
    }
    let quoted_to = format!("{}.", dialect.quote_ident(to));
    let text = text.replace(&format!("{}.", dialect.quote_ident(from)), &quoted_to);
    // A plain name is written plain, so that it reads as the other side does.
    let plain = !to.starts_with(|c: char| c.is_ascii_digit())
        && to
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    let bare_to = if plain { format!("{to}.") } else { quoted_to };
    let pattern = format!("{from}.");
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$' || c == '.';
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(at) = rest.find(&pattern) {
        out.push_str(&rest[..at]);
        let joined = rest[..at].chars().next_back().is_some_and(word);
        out.push_str(if joined { &pattern } else { &bare_to });
        rest = &rest[at + pattern.len()..];
    }
    out.push_str(rest);
    out
}

/// `text` with every run of whitespace one space: what is compared.
fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `sql` ending with one semicolon (a `DELIMITER` block ends itself).
fn terminated(sql: &str) -> String {
    let sql = sql.trim_end();
    if sql.ends_with(';') || sql.ends_with("DELIMITER ;") {
        sql.to_string()
    } else {
        format!("{sql};")
    }
}

/// `text` as SQL comment lines: the one writer of the script's `--`
/// lines. Every line break ends a comment for some reader (`\r\n`, `\n`, a
/// lone `\r`, and NEL, LINE and PARAGRAPH SEPARATOR to be safe), so the text
/// is split on each and every piece commented: a name or a server text
/// holding a line break never ends its comment and leaves SQL to run.
pub fn commented(text: &str) -> String {
    text.replace("\r\n", "\n")
        .split(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}'])
        .map(|line| format!("-- {line}").trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The source side's structures and DDL written for the target schema.
fn retargeted(side: &Side, to: &str, dialect: &dyn Dialect) -> Side {
    let from = side.schema.as_str();
    let text = |text: &str| retarget(text, from, to, dialect);
    let table = |table: &TableRef| {
        if table.schema == from {
            TableRef::new(to, table.name.clone())
        } else {
            table.clone()
        }
    };
    let object = |object: &ObjectRef| match object {
        ObjectRef::Relation(t, kind) => ObjectRef::Relation(table(t), *kind),
        ObjectRef::Routine {
            schema,
            name,
            kind,
            identity_arguments,
        } => ObjectRef::Routine {
            schema: if schema == from {
                to.into()
            } else {
                schema.clone()
            },
            name: name.clone(),
            kind: *kind,
            identity_arguments: text(identity_arguments),
        },
        ObjectRef::Sequence(t) => ObjectRef::Sequence(table(t)),
        ObjectRef::Type(t) => ObjectRef::Type(table(t)),
        ObjectRef::Trigger { table: t, name } => ObjectRef::Trigger {
            table: table(t),
            name: name.clone(),
        },
        other => other.clone(),
    };
    let structure = |s: &TableStructure| {
        let mut s = s.clone();
        for column in &mut s.columns {
            column.data_type = text(&column.data_type);
            column.default = column.default.as_deref().map(text);
        }
        for constraint in &mut s.constraints {
            constraint.definition = text(&constraint.definition);
        }
        for index in &mut s.indexes {
            index.definition = index.definition.as_deref().map(text);
            index.predicate = index.predicate.as_deref().map(text);
        }
        for key in &mut s.foreign_keys {
            key.table = table(&key.table);
            key.referenced_table = table(&key.referenced_table);
        }
        s.referenced_by.clear();
        s.triggers.clear();
        s
    };
    Side {
        schema: to.to_string(),
        objects: side
            .objects
            .iter()
            .map(|o| Object {
                object: object(&o.object),
                definition: match &o.definition {
                    Definition::Table(s) => Definition::Table(structure(s)),
                    Definition::Ddl(ddl) => Definition::Ddl(text(ddl)),
                },
            })
            .collect(),
    }
}

/// The name an [`Item`] shows.
fn shown_name(object: &ObjectRef) -> String {
    match object {
        ObjectRef::Routine {
            name,
            identity_arguments,
            ..
        } if !identity_arguments.is_empty() => format!("{name}({identity_arguments})"),
        ObjectRef::Trigger { table, name } => format!("{name} on {}", table.name),
        other => other.name().to_string(),
    }
}

/// Whether two objects of the same kind are the same object.
fn same_object(family: Option<Family>, a: &ObjectRef, b: &ObjectRef) -> bool {
    let same = |folded, x: &str, y: &str| Family::same(family, folded, x, y);
    match (a, b) {
        (
            ObjectRef::Routine {
                name: n1,
                identity_arguments: a1,
                ..
            },
            ObjectRef::Routine {
                name: n2,
                identity_arguments: a2,
                ..
            },
        ) => same(true, n1, n2) && normalized(a1) == normalized(a2),
        (
            ObjectRef::Trigger {
                table: t1,
                name: n1,
            },
            ObjectRef::Trigger {
                table: t2,
                name: n2,
            },
        ) => same(false, &t1.name, &t2.name) && same(false, n1, n2),
        _ => same(false, a.name(), b.name()),
    }
}

/// The check, unique and exclusion constraints a table's draft does not
/// hold: all but the primary key and (MySQL) the unique keys, which are
/// indexes there.
fn own_constraints(structure: &TableStructure) -> Vec<&ConstraintInfo> {
    structure
        .constraints
        .iter()
        .filter(|constraint| constraint.kind != ConstraintKind::PrimaryKey)
        .filter(|constraint| {
            !structure
                .indexes
                .iter()
                .any(|index| index.name == constraint.name && !index.backs_constraint)
        })
        .collect()
}

/// The indexes a table's draft cannot describe (partial, expression, other
/// methods), compared and written by their definition.
fn other_indexes(structure: &TableStructure) -> Vec<&IndexInfo> {
    structure
        .indexes
        .iter()
        .filter(|index| {
            !index.primary
                && !index.backs_constraint
                && !table_draft::is_editable(index, structure)
                && !structure
                    .foreign_keys
                    .iter()
                    .any(|key| key.name == index.name)
        })
        .collect()
}

/// What an index is, for comparing: its definition, else its parts.
fn index_shape(index: &IndexInfo) -> String {
    match &index.definition {
        Some(definition) => normalized(definition.trim_end_matches(';')),
        None => format!(
            "{} {} {:?} {:?}",
            index.unique, index.method, index.columns, index.predicate
        ),
    }
}

/// A table as the dialect writes it from its structure, for the side by
/// side view: the same writer on both sides, so only what differs differs.
fn table_ddl(dialect: &dyn Dialect, table: &TableRef, structure: &TableStructure) -> String {
    let mut statements = dialect
        .create_table_sql(&TableDraft::from_structure(table, structure))
        .unwrap_or_else(|error| vec![commented(&describe(&error).to_string())]);
    for constraint in own_constraints(structure) {
        statements.push(dialect.add_constraint_sql(
            table,
            &constraint.name,
            &constraint.definition,
        ));
    }
    statements.extend(
        other_indexes(structure)
            .into_iter()
            .filter_map(|index| index.definition.clone()),
    );
    statements
        .iter()
        .map(|statement| terminated(statement))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The reason an error gives, for a comment of the script.
fn reason(error: &Error) -> String {
    match error {
        Error::Unsupported(text) => text.clone(),
        other => describe(other).to_string(),
    }
}

/// `changes` written by the dialect, as it merges them; when it refuses
/// the whole, each on its own, with the refused ones and why.
fn alter(
    dialect: &dyn Dialect,
    table: &TableRef,
    changes: &[TableChange],
) -> (Vec<String>, Vec<(TableChange, String)>) {
    if changes.is_empty() {
        return (Vec::new(), Vec::new());
    }
    if let Ok(statements) = dialect.alter_table_sql(table, changes) {
        return (statements, Vec::new());
    }
    let mut refused = Vec::new();
    let mut accepted = Vec::new();
    for change in changes {
        match dialect.alter_table_sql(table, std::slice::from_ref(change)) {
            Ok(_) => accepted.push(change.clone()),
            Err(error) => refused.push((change.clone(), reason(&error))),
        }
    }
    let statements = dialect
        .alter_table_sql(table, &accepted)
        .unwrap_or_default();
    (statements, refused)
}

/// `name` in backticks, for the plain-words differences.
fn q(name: &str) -> String {
    format!("`{name}`")
}

fn shown(text: &Option<String>) -> String {
    match present(text) {
        Some(text) => text.to_string(),
        None => "none".into(),
    }
}

/// What a column change does, in plain words.
fn column_differences(change: &TableChange) -> Vec<String> {
    let TableChange::AlterColumn { name, from, to } = change else {
        return Vec::new();
    };
    let was = table_draft::ColumnDraft::from(from);
    let column = format!("column {}", q(name));
    let mut out = Vec::new();
    if !was
        .data_type
        .trim()
        .eq_ignore_ascii_case(to.data_type.trim())
    {
        out.push(format!("{column}: {} → {}", was.data_type, to.data_type));
    }
    if was.nullable != to.nullable {
        let words = |nullable| if nullable { "NULL allowed" } else { "NOT NULL" };
        out.push(format!(
            "{column}: {} → {}",
            words(was.nullable),
            words(to.nullable)
        ));
    }
    if present(&was.default) != present(&to.default) {
        out.push(format!(
            "{column}: default {} → {}",
            shown(&was.default),
            shown(&to.default)
        ));
    }
    if was.auto_increment != to.auto_increment {
        let words = |on: bool| if on { "on" } else { "off" };
        out.push(format!(
            "{column}: auto-increment {} → {}",
            words(was.auto_increment.is_on()),
            words(to.auto_increment.is_on())
        ));
    }
    if present(&was.on_update) != present(&to.on_update) {
        out.push(format!(
            "{column}: on update {} → {}",
            shown(&was.on_update),
            shown(&to.on_update)
        ));
    }
    if was.invisible != to.invisible {
        out.push(format!(
            "{column}: {}",
            if to.invisible {
                "becomes invisible"
            } else {
                "becomes visible"
            }
        ));
    }
    if present(&was.comment) != present(&to.comment) {
        out.push(format!(
            "{column}: comment “{}” → “{}”",
            shown(&was.comment),
            shown(&to.comment)
        ));
    }
    out
}

/// The differences and lines of a table both sides have; `table` is the
/// target's.
fn table_diff(
    family: Option<Family>,
    dialect: &dyn Dialect,
    table: &TableRef,
    source: &TableStructure,
    target: &TableStructure,
    differences: &mut Vec<String>,
    lines: &mut Vec<Line>,
) {
    let same = |a: &str, b: &str| Family::same(family, true, a, b);
    // The source's draft, each item tied to the target's of the same name
    // and written with the target's spelling of it.
    let mut draft = TableDraft::from_structure(table, source);
    let existing = TableDraft::from_structure(table, target);
    let column_name = |name: &String| {
        target
            .columns
            .iter()
            .find(|column| same(name, &column.name))
            .map_or_else(|| name.clone(), |column| column.name.clone())
    };
    let names = |names: &mut Vec<String>| {
        for name in names.iter_mut() {
            *name = column_name(name);
        }
    };
    for column in &mut draft.columns {
        column.original = target
            .columns
            .iter()
            .find(|c| same(&column.name, &c.name))
            .map(|c| c.name.clone());
        if let Some(name) = &column.original {
            column.name = name.clone();
        }
    }
    names(&mut draft.primary_key);
    for index in &mut draft.indexes {
        names(&mut index.columns);
        index.original = existing
            .indexes
            .iter()
            .find(|e| same(&index.name, &e.name))
            .map(|e| e.name.clone());
        if let Some(name) = &index.original {
            index.name = name.clone();
        }
    }
    for key in &mut draft.foreign_keys {
        names(&mut key.columns);
        if &key.referenced_table == table {
            names(&mut key.referenced_columns);
        }
        key.original = existing
            .foreign_keys
            .iter()
            .find(|e| same(&key.name, &e.name))
            .map(|e| e.name.clone());
        if let Some(name) = &key.original {
            key.name = name.clone();
        }
    }

    let changes = table_draft::changes(table, target, &draft);
    let (mut keys, mut dropped, mut rest) = (Vec::new(), Vec::new(), Vec::new());
    for change in changes {
        match change {
            TableChange::AddForeignKey(_) => keys.push(change),
            TableChange::DropColumn(_) => dropped.push(change),
            _ => rest.push(change),
        }
    }
    let (statements, refused) = alter(dialect, table, &rest);
    // A move the engine cannot make is no difference: its columns have
    // the order they were added in, which cannot change (PostgreSQL).
    let unmovable = |change: &TableChange| {
        matches!(change, TableChange::MoveColumn { .. })
            && refused.iter().any(|(refused, _)| refused == change)
    };
    let added_columns: Vec<String> = rest
        .iter()
        .filter_map(|change| match change {
            TableChange::AddColumn(column) => Some(column.name.clone()),
            _ => None,
        })
        .collect();
    let dropped_index = |name: &str| {
        rest.iter()
            .any(|c| matches!(c, TableChange::DropIndex(n) if n == name))
    };
    let dropped_key = |name: &str| {
        rest.iter()
            .any(|c| matches!(c, TableChange::DropForeignKey(n) if n == name))
    };
    let added_key = |name: &str| {
        keys.iter()
            .any(|c| matches!(c, TableChange::AddForeignKey(k) if k.name == name))
    };
    let added_index = |name: &str| {
        rest.iter()
            .any(|c| matches!(c, TableChange::AddIndex(i) if i.name == name))
    };
    let primary_key = |columns: &[String]| format!("({})", columns.join(", "));
    for change in rest.iter().chain(&dropped).chain(&keys) {
        if unmovable(change) {
            continue;
        }
        match change {
            TableChange::DropColumn(name) => {
                differences.push(format!("column {} is only in the target", q(name)))
            }
            TableChange::AddColumn(column) => differences.push(format!(
                "column {} is missing in the target",
                q(&column.name)
            )),
            TableChange::AlterColumn { .. } => differences.extend(column_differences(change)),
            TableChange::MoveColumn { name, after, .. } => {
                if !added_columns.contains(name) {
                    differences.push(match after {
                        Some(after) => format!("column {} moves after {}", q(name), q(after)),
                        None => format!("column {} moves first", q(name)),
                    });
                }
            }
            TableChange::DropPrimaryKey(_) => {
                if draft.primary_key.is_empty() {
                    differences.push("the primary key is only in the target".into());
                } else {
                    differences.push(format!(
                        "primary key: {} → {}",
                        primary_key(&existing.primary_key),
                        primary_key(&draft.primary_key)
                    ));
                }
            }
            TableChange::AddPrimaryKey(columns) => {
                if existing.primary_key.is_empty() {
                    differences.push(format!(
                        "primary key {} is missing in the target",
                        primary_key(columns)
                    ));
                }
            }
            TableChange::DropIndex(name) if added_index(name) => {
                differences.push(format!("index {} differs", q(name)))
            }
            TableChange::DropIndex(name) => {
                differences.push(format!("index {} is only in the target", q(name)))
            }
            TableChange::AddIndex(index) if !dropped_index(&index.name) => {
                differences.push(format!("index {} is missing in the target", q(&index.name)))
            }
            TableChange::DropForeignKey(name) if added_key(name) => {
                differences.push(format!("foreign key {} differs", q(name)))
            }
            TableChange::DropForeignKey(name) => {
                differences.push(format!("foreign key {} is only in the target", q(name)))
            }
            TableChange::AddForeignKey(key) if !dropped_key(&key.name) => differences.push(
                format!("foreign key {} is missing in the target", q(&key.name)),
            ),
            TableChange::SetTableComment(comment) => differences.push(format!(
                "table comment: “{}” → “{}”",
                shown(&target.comment),
                shown(comment)
            )),
            TableChange::AddIndex(_)
            | TableChange::AddForeignKey(_)
            | TableChange::RenameTable { .. }
            | TableChange::RenameColumn { .. } => {}
        }
    }

    let line = |phase, kind, text: String| Line { phase, kind, text };
    lines.extend(
        statements
            .into_iter()
            .map(|sql| line(Phase::AlterTables, LineKind::Run, sql)),
    );
    for (change, why) in refused.iter().filter(|(change, _)| !unmovable(change)) {
        let what = match change {
            TableChange::AlterColumn { name, .. }
            | TableChange::MoveColumn { name, .. }
            | TableChange::RenameColumn { to: name, .. } => format!("column {name}"),
            TableChange::AddColumn(column) => format!("column {}", column.name),
            _ => "this change".into(),
        };
        lines.push(line(
            Phase::AlterTables,
            LineKind::Note,
            format!("Not written ({what}): {why}"),
        ));
    }
    for change in &dropped {
        let (statements, refused) = alter(dialect, table, std::slice::from_ref(change));
        lines.extend(
            statements
                .into_iter()
                .map(|sql| line(Phase::AlterTables, LineKind::Drop, sql)),
        );
        lines.extend(refused.into_iter().map(|(_, why)| {
            line(
                Phase::AlterTables,
                LineKind::Note,
                format!("Not written: {why}"),
            )
        }));
    }
    let (statements, refused) = alter(dialect, table, &keys);
    lines.extend(
        statements
            .into_iter()
            .map(|sql| line(Phase::AddForeignKeys, LineKind::Run, sql)),
    );
    lines.extend(refused.into_iter().map(|(_, why)| {
        line(
            Phase::AddForeignKeys,
            LineKind::Note,
            format!("Not written: {why}"),
        )
    }));

    // Constraints and indexes the draft does not hold.
    let (theirs, ours) = (own_constraints(target), own_constraints(source));
    let constraint_words = |c: &ConstraintInfo| match c.kind {
        ConstraintKind::Check => "check",
        ConstraintKind::Unique => "unique constraint",
        ConstraintKind::Exclusion => "exclusion constraint",
        ConstraintKind::PrimaryKey => "primary key",
    };
    for constraint in &ours {
        let add = dialect.add_constraint_sql(table, &constraint.name, &constraint.definition);
        match theirs.iter().find(|t| same(&t.name, &constraint.name)) {
            Some(t) if normalized(&t.definition) == normalized(&constraint.definition) => {}
            Some(t) => {
                differences.push(format!(
                    "{} {}: {} → {}",
                    constraint_words(constraint),
                    q(&t.name),
                    t.definition,
                    constraint.definition
                ));
                let drop = dialect.drop_constraint_sql(table, &t.name);
                lines.push(line(Phase::DropConstraints, LineKind::Run, drop));
                lines.push(line(Phase::AddConstraints, LineKind::Run, add));
            }
            None => {
                differences.push(format!(
                    "{} {} is missing in the target",
                    constraint_words(constraint),
                    q(&constraint.name)
                ));
                lines.push(line(Phase::AddConstraints, LineKind::Run, add));
            }
        }
    }
    for t in theirs
        .iter()
        .filter(|t| !ours.iter().any(|c| same(&c.name, &t.name)))
    {
        differences.push(format!(
            "{} {} is only in the target",
            constraint_words(t),
            q(&t.name)
        ));
        let drop = dialect.drop_constraint_sql(table, &t.name);
        lines.push(line(Phase::DropConstraints, LineKind::Run, drop));
    }
    let (theirs, ours) = (other_indexes(target), other_indexes(source));
    let drop_index = |name: &str| {
        dialect.drop_sql(&ObjectRef::Index {
            table: table.clone(),
            name: name.to_string(),
        })
    };
    let create_index = |index: &IndexInfo| match &index.definition {
        Some(definition) => line(Phase::AddConstraints, LineKind::Run, definition.clone()),
        None => line(
            Phase::AddConstraints,
            LineKind::Note,
            format!(
                "Not written (index {}): the server did not print its definition.",
                index.name
            ),
        ),
    };
    for index in &ours {
        match theirs.iter().find(|t| same(&t.name, &index.name)) {
            Some(t) if index_shape(t) == index_shape(index) => {}
            found => {
                if let Some(t) = found {
                    differences.push(format!("index {} differs", q(&t.name)));
                    match drop_index(&t.name) {
                        Ok(sql) => lines.push(line(Phase::DropConstraints, LineKind::Run, sql)),
                        Err(error) => lines.push(line(
                            Phase::DropConstraints,
                            LineKind::Note,
                            format!("Not written: {}", reason(&error)),
                        )),
                    }
                } else {
                    differences.push(format!("index {} is missing in the target", q(&index.name)));
                }
                lines.push(create_index(index));
            }
        }
    }
    for t in theirs
        .iter()
        .filter(|t| !ours.iter().any(|i| same(&i.name, &t.name)))
    {
        differences.push(format!("index {} is only in the target", q(&t.name)));
        if let Ok(sql) = drop_index(&t.name) {
            lines.push(line(Phase::DropConstraints, LineKind::Run, sql));
        }
    }
}

/// The phase that creates or replaces an object of `kind`, and the one
/// that drops it.
fn phases(kind: Kind) -> (Phase, Phase) {
    match kind {
        Kind::Table => (Phase::CreateTables, Phase::DropTables),
        Kind::View | Kind::MaterializedView => (Phase::Views, Phase::DropViews),
        Kind::Function | Kind::Procedure => (Phase::Routines, Phase::DropRoutines),
        Kind::Sequence | Kind::Type => (Phase::CreateTypes, Phase::DropTypes),
        Kind::Trigger => (Phase::Triggers, Phase::DropTriggers),
    }
}

/// Compares `source` with `target` and writes, per object, the lines that
/// bring the target to the source in the target's dialect. Objects of the
/// same kind and name are the same object (names compared as the engine
/// compares them); a renamed object is one only in the source and one
/// only in the target. Items come grouped by [`Kind`], then by name.
pub fn compare(
    source: &Side,
    target: &Side,
    source_dialect: &dyn Dialect,
    target_dialect: &dyn Dialect,
) -> Vec<Item> {
    let family = Family::of(target_dialect.engine());
    let model = retargeted(source, &target.schema, source_dialect);
    let ddl_of = |dialect: &dyn Dialect, object: &Object| match (&object.object, &object.definition)
    {
        (ObjectRef::Relation(table, _), Definition::Table(structure)) => {
            table_ddl(dialect, table, structure)
        }
        (_, Definition::Table(_)) => String::new(),
        (_, Definition::Ddl(ddl)) => ddl.clone(),
    };
    // Target tables only the target has, for their drop order.
    let only_in_target: Vec<&TableRef> = target
        .objects
        .iter()
        .filter_map(|object| match &object.object {
            ObjectRef::Relation(table, _)
                if Kind::of(&object.object) == Some(Kind::Table)
                    && !model
                        .objects
                        .iter()
                        .any(|m| same_object(family, &m.object, &object.object)) =>
            {
                Some(table)
            }
            _ => None,
        })
        .collect();

    let mut items = Vec::new();
    for (shaped, original) in model.objects.iter().zip(&source.objects) {
        let Some(kind) = Kind::of(&shaped.object) else {
            continue;
        };
        let theirs = target.objects.iter().find(|t| {
            Kind::of(&t.object) == Some(kind) && same_object(family, &t.object, &shaped.object)
        });
        let mut differences = Vec::new();
        let mut lines = Vec::new();
        let (create, _) = phases(kind);
        let line = |phase, kind, text: String| Line { phase, kind, text };
        match (theirs, &shaped.definition) {
            (None, Definition::Table(structure)) => {
                let ObjectRef::Relation(table, _) = &shaped.object else {
                    continue;
                };
                differences.push("the table is missing in the target".into());
                let mut draft = TableDraft::from_structure(table, structure);
                let keys: Vec<ForeignKeyDraft> = std::mem::take(&mut draft.foreign_keys);
                match target_dialect.create_table_sql(&draft) {
                    Ok(statements) => lines.extend(
                        statements
                            .into_iter()
                            .map(|sql| line(Phase::CreateTables, LineKind::Run, sql)),
                    ),
                    Err(error) => lines.push(line(
                        Phase::CreateTables,
                        LineKind::Note,
                        format!("Not written: {}", reason(&error)),
                    )),
                }
                for constraint in own_constraints(structure) {
                    lines.push(line(
                        Phase::AddConstraints,
                        LineKind::Run,
                        target_dialect.add_constraint_sql(
                            table,
                            &constraint.name,
                            &constraint.definition,
                        ),
                    ));
                }
                for index in other_indexes(structure) {
                    if let Some(definition) = &index.definition {
                        lines.push(line(
                            Phase::AddConstraints,
                            LineKind::Run,
                            definition.clone(),
                        ));
                    }
                }
                let keys: Vec<TableChange> =
                    keys.into_iter().map(TableChange::AddForeignKey).collect();
                let (statements, refused) = alter(target_dialect, table, &keys);
                lines.extend(
                    statements
                        .into_iter()
                        .map(|sql| line(Phase::AddForeignKeys, LineKind::Run, sql)),
                );
                lines.extend(refused.into_iter().map(|(_, why)| {
                    line(
                        Phase::AddForeignKeys,
                        LineKind::Note,
                        format!("Not written: {why}"),
                    )
                }));
            }
            (None, Definition::Ddl(ddl)) => {
                differences.push(format!("the {} is missing in the target", kind.label()));
                lines.push(line(create, LineKind::Run, ddl.clone()));
            }
            (Some(theirs), Definition::Table(structure)) => {
                if let (ObjectRef::Relation(table, _), Definition::Table(existing)) =
                    (&theirs.object, &theirs.definition)
                {
                    table_diff(
                        family,
                        target_dialect,
                        table,
                        structure,
                        existing,
                        &mut differences,
                        &mut lines,
                    );
                }
            }
            (Some(theirs), Definition::Ddl(ddl)) => {
                let existing = match &theirs.definition {
                    Definition::Ddl(existing) => existing.as_str(),
                    Definition::Table(_) => "",
                };
                if normalized(existing) != normalized(ddl) {
                    differences.push("the definitions differ".into());
                    let replaced = match kind {
                        Kind::Trigger => target_dialect
                            .drop_sql(&theirs.object)
                            .map(|drop| format!("{};\n\n{}", drop, ddl.trim())),
                        Kind::Sequence | Kind::Type => Err(Error::Unsupported(format!(
                            "Kerno does not change a {} in place. Change it by hand; the \
                             source's definition follows.\n{}",
                            kind.label(),
                            ddl.trim()
                        ))),
                        _ => target_dialect.replace_source_sql(&theirs.object, ddl),
                    };
                    match replaced {
                        Ok(sql) => lines.push(line(create, LineKind::Run, sql)),
                        Err(error) => lines.push(line(
                            create,
                            LineKind::Note,
                            format!(
                                "Not written ({} {}): {}",
                                kind.label(),
                                shown_name(&theirs.object),
                                reason(&error)
                            ),
                        )),
                    }
                }
            }
        }
        let state = match (theirs, differences.is_empty()) {
            (None, _) => State::OnlyInSource,
            (Some(_), true) => State::Same,
            (Some(_), false) => State::Different,
        };
        items.push(Item {
            kind,
            name: shown_name(&original.object),
            source: Some(original.object.clone()),
            target: theirs.map(|t| t.object.clone()),
            state,
            differences,
            source_ddl: ddl_of(source_dialect, original),
            target_ddl: theirs.map_or_else(String::new, |t| ddl_of(target_dialect, t)),
            lines,
        });
    }
    for theirs in &target.objects {
        let Some(kind) = Kind::of(&theirs.object) else {
            continue;
        };
        if model.objects.iter().any(|m| {
            Kind::of(&m.object) == Some(kind) && same_object(family, &m.object, &theirs.object)
        }) {
            continue;
        }
        let (_, drop) = phases(kind);
        let mut lines = Vec::new();
        if let (ObjectRef::Relation(table, _), Definition::Table(structure)) =
            (&theirs.object, &theirs.definition)
        {
            // Its keys to other tables dropped with it go first, so the
            // tables can go in any order.
            let keys: Vec<TableChange> = structure
                .foreign_keys
                .iter()
                .filter(|key| {
                    &key.referenced_table != table
                        && only_in_target.contains(&&key.referenced_table)
                })
                .map(|key| TableChange::DropForeignKey(key.name.clone()))
                .collect();
            let (statements, _) = alter(target_dialect, table, &keys);
            lines.extend(statements.into_iter().map(|text| Line {
                phase: Phase::DropKeys,
                kind: LineKind::Drop,
                text,
            }));
        }
        lines.push(match target_dialect.drop_sql(&theirs.object) {
            Ok(text) => Line {
                phase: drop,
                kind: LineKind::Drop,
                text,
            },
            Err(error) => Line {
                phase: drop,
                kind: LineKind::Note,
                text: format!("Not written: {}", reason(&error)),
            },
        });
        items.push(Item {
            kind,
            name: shown_name(&theirs.object),
            source: None,
            target: Some(theirs.object.clone()),
            state: State::OnlyInTarget,
            differences: vec![format!("the {} is only in the target", kind.label())],
            source_ddl: String::new(),
            target_ddl: ddl_of(target_dialect, theirs),
            lines,
        });
    }
    items.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
    items
}

/// The script of the items `included` says yes to, in the order that
/// works; drops of what only the target has are statements when `drops`,
/// comments otherwise. Each block says which object it is for.
pub fn script(items: &[Item], included: impl Fn(usize) -> bool, drops: bool) -> String {
    let mut lines: Vec<(Phase, usize, &Line)> = items
        .iter()
        .enumerate()
        .filter(|(index, _)| included(*index))
        .flat_map(|(index, item)| item.lines.iter().map(move |line| (line.phase, index, line)))
        .collect();
    lines.sort_by_key(|(phase, index, _)| (*phase, *index));
    if lines.is_empty() {
        return commented("Nothing to change: the objects picked are the same on both sides.")
            + "\n";
    }
    let mut out = String::new();
    if !drops && lines.iter().any(|(_, _, line)| line.kind == LineKind::Drop) {
        out.push_str(&commented(
            "What only the target has is not dropped: its DROP statements are comments.\n\
             Tick “Also drop what only the target has” to write them.",
        ));
        out.push_str("\n\n");
    }
    let mut block = None;
    for (phase, index, line) in lines {
        if block != Some((phase, index)) {
            block = Some((phase, index));
            let item = &items[index];
            out.push_str(&commented(&format!("{} {}", item.kind.label(), item.name)));
            out.push('\n');
        }
        let text = match line.kind {
            LineKind::Run => terminated(&line.text),
            LineKind::Drop if drops => terminated(&line.text),
            LineKind::Drop => commented(&terminated(&line.text)),
            LineKind::Note => commented(&line.text),
        };
        out.push_str(&text);
        out.push_str("\n\n");
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    //! K30: the diff and the script of the schema compare (NAV-57, CM-21,
    //! CM-59, TOOL-22).

    use super::*;
    use crate::dialect::default_alter_table_sql;
    use crate::result::TypeKind;
    use crate::schema::{ColumnInfo, ForeignKeyInfo};
    use crate::sql::LexRules;

    /// PostgreSQL-like: columns cannot move; a view or routine is replaced
    /// by its DDL.
    struct Pg;
    /// MySQL-like: columns move (`MODIFY … AFTER`), names fold.
    struct My;

    macro_rules! dialect {
        ($name:ident, $engine:expr, $quote:literal, $schemas:literal) => {
            impl Dialect for $name {
                fn engine(&self) -> Engine {
                    $engine
                }
                fn has_schemas(&self) -> bool {
                    $schemas
                }
                fn lex_rules(&self) -> LexRules {
                    LexRules::POSTGRES
                }
                fn quote_ident(&self, ident: &str) -> String {
                    format!("{}{ident}{}", $quote, $quote)
                }
                fn quote_string(&self, text: &str) -> String {
                    format!("'{text}'")
                }
                fn quote_bytes(&self, _: &[u8]) -> String {
                    String::new()
                }
                fn explain(&self, sql: &str, _: bool) -> String {
                    sql.into()
                }
                fn type_for(&self, _: TypeKind) -> &'static str {
                    "text"
                }
                fn estimated_rows_sql(&self, _: &TableRef) -> String {
                    String::new()
                }
                fn replace_source_sql(&self, object: &ObjectRef, ddl: &str) -> Result<String> {
                    match object {
                        ObjectRef::Relation(..) | ObjectRef::Routine { .. } => Ok(ddl.into()),
                        _ => Err(Error::Unsupported("editing this source".into())),
                    }
                }
                fn alter_table_sql(
                    &self,
                    table: &TableRef,
                    changes: &[TableChange],
                ) -> Result<Vec<String>> {
                    let mut out = Vec::new();
                    for change in changes {
                        match change {
                            TableChange::MoveColumn { name, after, .. } if !$schemas => {
                                out.push(format!(
                                    "ALTER TABLE {} MODIFY {name} {}",
                                    self.table_name(table),
                                    after
                                        .as_ref()
                                        .map_or("FIRST".into(), |a| format!("AFTER {a}"))
                                ))
                            }
                            _ => out.extend(default_alter_table_sql(
                                self,
                                table,
                                std::slice::from_ref(change),
                            )?),
                        }
                    }
                    Ok(out)
                }
            }
        };
    }
    dialect!(Pg, Engine::Postgres, "\"", true);
    dialect!(My, Engine::MySql, "`", false);

    fn column(name: &str, data_type: &str, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            kind: TypeKind::Text,
            nullable,
            default: None,
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: None,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    fn id() -> ColumnInfo {
        ColumnInfo {
            primary_key_position: Some(1),
            ..column("id", "integer", false)
        }
    }

    fn structure(columns: Vec<ColumnInfo>) -> TableStructure {
        TableStructure {
            columns,
            ..TableStructure::default()
        }
    }

    fn index(name: &str, columns: &[&str]) -> IndexInfo {
        IndexInfo {
            name: name.into(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            unique: false,
            primary: false,
            backs_constraint: false,
            method: "btree".into(),
            predicate: None,
            definition: None,
        }
    }

    fn key(schema: &str, table: &str, name: &str, column: &str, to: &str) -> ForeignKeyInfo {
        ForeignKeyInfo {
            name: name.into(),
            table: TableRef::new(schema, table),
            columns: vec![column.into()],
            referenced_table: TableRef::new(schema, to),
            referenced_columns: vec!["id".into()],
            on_update: "NO ACTION".into(),
            on_delete: "NO ACTION".into(),
        }
    }

    fn table(schema: &str, name: &str, structure: TableStructure) -> Object {
        Object {
            object: ObjectRef::Relation(TableRef::new(schema, name), RelationKind::Table),
            definition: Definition::Table(structure),
        }
    }

    fn ddl(object: ObjectRef, ddl: &str) -> Object {
        Object {
            object,
            definition: Definition::Ddl(ddl.into()),
        }
    }

    fn view(schema: &str, name: &str, text: &str) -> Object {
        ddl(
            ObjectRef::Relation(TableRef::new(schema, name), RelationKind::View),
            text,
        )
    }

    fn side(schema: &str, objects: Vec<Object>) -> Side {
        Side {
            schema: schema.into(),
            objects,
        }
    }

    fn item<'a>(items: &'a [Item], name: &str) -> &'a Item {
        items
            .iter()
            .find(|item| item.name == name)
            .unwrap_or_else(|| panic!("no item {name} in {items:#?}"))
    }

    fn all(items: &[Item], drops: bool) -> String {
        script(items, |_| true, drops)
    }

    /// The position of `needle` in `text`, which must hold it.
    fn at(text: &str, needle: &str) -> usize {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} is not in\n{text}"))
    }

    #[test]
    fn only_an_engine_of_the_same_family_is_compared() {
        assert_eq!(refusal(Engine::Postgres, Engine::Postgres), None);
        assert_eq!(refusal(Engine::MySql, Engine::MariaDb), None);
        assert_eq!(refusal(Engine::MariaDb, Engine::MySql), None);
        assert_eq!(
            refusal(Engine::Postgres, Engine::MySql).as_deref(),
            Some(
                "Comparing a PostgreSQL schema with a MySQL one is not supported yet: the types \
                 do not map one to one."
            )
        );
        assert!(refusal(Engine::MariaDb, Engine::Postgres).is_some());
    }

    #[test]
    fn every_table_difference_is_said_in_plain_words() {
        let mut price = column("price", "numeric(10,2)", true);
        let source = side(
            "a",
            vec![table(
                "a",
                "orders",
                TableStructure {
                    columns: vec![
                        id(),
                        ColumnInfo {
                            default: Some("0".into()),
                            comment: Some("in euros".into()),
                            ..column("price", "numeric(12,2)", false)
                        },
                        column("note", "text", true),
                    ],
                    indexes: vec![index("orders_price_idx", &["price"])],
                    foreign_keys: vec![key("a", "orders", "orders_customer_fk", "id", "customers")],
                    comment: Some("Orders".into()),
                    ..TableStructure::default()
                },
            )],
        );
        price.comment = None;
        let target = side(
            "b",
            vec![table(
                "b",
                "orders",
                TableStructure {
                    columns: vec![id(), price, column("legacy", "text", true)],
                    indexes: vec![index("orders_legacy_idx", &["legacy"])],
                    ..TableStructure::default()
                },
            )],
        );
        let items = compare(&source, &target, &Pg, &Pg);
        let orders = item(&items, "orders");
        assert_eq!(orders.state, State::Different);
        assert_eq!(
            orders.differences,
            [
                "index `orders_legacy_idx` is only in the target",
                "column `note` is missing in the target",
                "column `price`: numeric(10,2) → numeric(12,2)",
                "column `price`: NULL allowed → NOT NULL",
                "column `price`: default none → 0",
                "column `price`: comment “none” → “in euros”",
                "index `orders_price_idx` is missing in the target",
                "table comment: “none” → “Orders”",
                "column `legacy` is only in the target",
                "foreign key `orders_customer_fk` is missing in the target",
            ]
        );
        let script = all(&items, false);
        // The key points at the target schema, not the source's.
        assert!(
            script.contains("REFERENCES \"b\".\"customers\""),
            "{script}"
        );
        assert!(!script.contains("\"a\"."), "{script}");
        assert!(
            script.contains(
                "ALTER TABLE \"b\".\"orders\" ALTER COLUMN \"price\" TYPE numeric(12,2);"
            )
        );
    }

    #[test]
    fn identical_schemas_give_no_difference_and_an_empty_script() {
        let structure = TableStructure {
            columns: vec![id(), column("name", "text", true)],
            indexes: vec![index("t_name_idx", &["name"])],
            ..TableStructure::default()
        };
        let source = side(
            "a",
            vec![
                table("a", "t", structure.clone()),
                view(
                    "a",
                    "v",
                    "CREATE VIEW \"a\".\"v\" AS\n  SELECT name FROM a.t;",
                ),
            ],
        );
        let target = side(
            "b",
            vec![
                table("b", "t", structure),
                view(
                    "b",
                    "v",
                    "CREATE VIEW \"b\".\"v\" AS SELECT name   FROM b.t;",
                ),
            ],
        );
        let items = compare(&source, &target, &Pg, &Pg);
        assert!(
            items.iter().all(|item| item.state == State::Same),
            "{items:#?}"
        );
        assert_eq!(
            all(&items, true),
            "-- Nothing to change: the objects picked are the same on both sides.\n"
        );
    }

    #[test]
    fn the_script_runs_in_an_order_that_works() {
        let source = side(
            "a",
            vec![
                ddl(
                    ObjectRef::Type(TableRef::new("a", "mood")),
                    "CREATE TYPE \"a\".\"mood\" AS ENUM ('sad');",
                ),
                table(
                    "a",
                    "lines",
                    TableStructure {
                        foreign_keys: vec![key(
                            "a",
                            "lines",
                            "lines_order_fk",
                            "order_id",
                            "orders",
                        )],
                        ..structure(vec![id(), column("order_id", "integer", false)])
                    },
                ),
                table("a", "orders", structure(vec![id()])),
                view("a", "totals", "CREATE VIEW \"a\".\"totals\" AS SELECT 1;"),
                ddl(
                    ObjectRef::Routine {
                        schema: "a".into(),
                        name: "f".into(),
                        kind: RoutineKind::Function,
                        identity_arguments: String::new(),
                    },
                    "CREATE FUNCTION a.f() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql;",
                ),
                ddl(
                    ObjectRef::Trigger {
                        table: TableRef::new("a", "orders"),
                        name: "t".into(),
                    },
                    "CREATE TRIGGER t BEFORE INSERT ON a.orders EXECUTE FUNCTION a.f();",
                ),
            ],
        );
        let target = side(
            "b",
            vec![
                table(
                    "b",
                    "old_children",
                    TableStructure {
                        foreign_keys: vec![key("b", "old_children", "old_fk", "parent", "old")],
                        ..structure(vec![id(), column("parent", "integer", true)])
                    },
                ),
                table("b", "old", structure(vec![id()])),
            ],
        );
        let items = compare(&source, &target, &Pg, &Pg);
        assert_eq!(
            items
                .iter()
                .map(|item| (item.name.as_str(), item.state))
                .collect::<Vec<_>>(),
            [
                ("lines", State::OnlyInSource),
                ("old", State::OnlyInTarget),
                ("old_children", State::OnlyInTarget),
                ("orders", State::OnlyInSource),
                ("totals", State::OnlyInSource),
                ("f", State::OnlyInSource),
                ("mood", State::OnlyInSource),
                ("t on orders", State::OnlyInSource),
            ]
        );
        let script = all(&items, true);
        let order = [
            "CREATE TYPE \"b\".\"mood\"",
            "CREATE TABLE \"b\".\"lines\"",
            "CREATE TABLE \"b\".\"orders\"",
            "ADD CONSTRAINT \"lines_order_fk\" FOREIGN KEY",
            "CREATE FUNCTION b.f()",
            "CREATE VIEW \"b\".\"totals\"",
            "CREATE TRIGGER t BEFORE INSERT ON b.orders EXECUTE FUNCTION b.f()",
            "ALTER TABLE \"b\".\"old_children\" DROP CONSTRAINT \"old_fk\"",
            "DROP TABLE \"b\".\"old\";",
        ];
        for pair in order.windows(2) {
            assert!(
                at(&script, pair[0]) < at(&script, pair[1]),
                "{pair:?} in\n{script}"
            );
        }
        // A new table's keys are not written inline: the table they point
        // at may come after it.
        let create = &script[at(&script, "CREATE TABLE \"b\".\"lines\"")..];
        assert!(!create[..create.find(';').unwrap()].contains("FOREIGN KEY"));
        assert!(script.contains("-- table lines\n"), "{script}");
    }

    #[test]
    fn drops_are_comments_until_asked_for() {
        let source = side("a", vec![table("a", "t", structure(vec![id()]))]);
        let target = side(
            "b",
            vec![
                table("b", "t", structure(vec![id(), column("old", "text", true)])),
                table("b", "gone", structure(vec![id()])),
                view("b", "gone_view", "CREATE VIEW b.gone_view AS SELECT 1;"),
            ],
        );
        let items = compare(&source, &target, &Pg, &Pg);
        assert!(item(&items, "gone").drops() && item(&items, "t").drops());
        let kept = all(&items, false);
        for line in [
            "-- ALTER TABLE \"b\".\"t\" DROP COLUMN \"old\";",
            "-- DROP TABLE \"b\".\"gone\";",
            "-- DROP VIEW \"b\".\"gone_view\";",
            "-- Tick “Also drop what only the target has” to write them.",
        ] {
            assert!(kept.contains(line), "{line} in\n{kept}");
        }
        let dropped = all(&items, true);
        for line in [
            "\nALTER TABLE \"b\".\"t\" DROP COLUMN \"old\";",
            "\nDROP TABLE \"b\".\"gone\";",
            "\nDROP VIEW \"b\".\"gone_view\";",
        ] {
            assert!(dropped.contains(line), "{line} in\n{dropped}");
        }
        assert!(!dropped.contains("Tick"), "{dropped}");
        // An object left out of the script is not in it.
        let some = script(&items, |index| items[index].name != "gone", true);
        assert!(!some.contains("DROP TABLE \"b\".\"gone\""), "{some}");
    }

    #[test]
    fn names_compare_as_the_engine_compares_them() {
        // PostgreSQL: case matters everywhere.
        let source = side("a", vec![table("a", "Orders", structure(vec![id()]))]);
        let target = side("b", vec![table("b", "orders", structure(vec![id()]))]);
        let items = compare(&source, &target, &Pg, &Pg);
        assert_eq!(item(&items, "Orders").state, State::OnlyInSource);
        assert_eq!(item(&items, "orders").state, State::OnlyInTarget);
        // MySQL and MariaDB: columns, indexes and routines fold; tables
        // compare as written.
        let routine = |schema: &str, name: &str| {
            ddl(
                ObjectRef::Routine {
                    schema: schema.into(),
                    name: name.into(),
                    kind: RoutineKind::Procedure,
                    identity_arguments: String::new(),
                },
                "CREATE PROCEDURE p() SELECT 1",
            )
        };
        let source = side(
            "a",
            vec![
                table(
                    "a",
                    "t",
                    TableStructure {
                        indexes: vec![index("By_Price", &["Price"])],
                        ..structure(vec![id(), column("Price", "int", true)])
                    },
                ),
                table("a", "T2", structure(vec![id()])),
                routine("a", "Refresh"),
            ],
        );
        let target = side(
            "b",
            vec![
                table(
                    "b",
                    "t",
                    TableStructure {
                        indexes: vec![index("by_price", &["price"])],
                        ..structure(vec![id(), column("price", "int", true)])
                    },
                ),
                table("b", "t2", structure(vec![id()])),
                routine("b", "refresh"),
            ],
        );
        let items = compare(&source, &target, &My, &My);
        assert_eq!(item(&items, "t").state, State::Same, "{items:#?}");
        assert_eq!(item(&items, "Refresh").state, State::Same);
        assert_eq!(item(&items, "T2").state, State::OnlyInSource);
        assert_eq!(item(&items, "t2").state, State::OnlyInTarget);
    }

    #[test]
    fn a_column_moves_only_where_the_engine_can_move_it() {
        let source = side(
            "a",
            vec![table(
                "a",
                "t",
                structure(vec![
                    id(),
                    column("added", "int", true),
                    column("b", "int", true),
                    column("c", "int", true),
                ]),
            )],
        );
        let target = side(
            "b",
            vec![table(
                "b",
                "t",
                structure(vec![
                    id(),
                    column("c", "int", true),
                    column("b", "int", true),
                ]),
            )],
        );
        // PostgreSQL cannot: the order is no difference, the column added
        // goes last, and nothing says it was not moved.
        let items = compare(&source, &target, &Pg, &Pg);
        assert_eq!(
            item(&items, "t").differences,
            ["column `added` is missing in the target"]
        );
        let script = all(&items, false);
        assert!(!script.contains("Not written"), "{script}");
        // MySQL can.
        let items = compare(&source, &target, &My, &My);
        assert_eq!(
            item(&items, "t").differences,
            [
                "column `added` is missing in the target",
                "column `b` moves after `added`",
                "column `c` moves after `b`",
            ]
        );
        assert!(
            all(&items, false).contains("ALTER TABLE `b`.`t` MODIFY b AFTER added;"),
            "{}",
            all(&items, false)
        );
    }

    #[test]
    fn what_cannot_change_in_place_is_a_comment_that_says_why() {
        let mood = |schema: &str, labels: &str| {
            ddl(
                ObjectRef::Type(TableRef::new(schema, "mood")),
                &format!("CREATE TYPE {schema}.mood AS ENUM ({labels});"),
            )
        };
        let source = side("a", vec![mood("a", "'sad', 'happy'")]);
        let target = side("b", vec![mood("b", "'sad'")]);
        let items = compare(&source, &target, &Pg, &Pg);
        assert_eq!(item(&items, "mood").state, State::Different);
        let script = all(&items, true);
        assert!(
            script.contains(
                "-- Not written (type mood): Kerno does not change a type in place. Change it \
                 by hand; the source's definition follows.\n-- CREATE TYPE b.mood AS ENUM \
                 ('sad', 'happy');"
            ),
            "{script}"
        );
        assert!(
            !script.lines().any(|line| line.starts_with("CREATE")),
            "{script}"
        );
    }

    #[test]
    fn views_and_routines_compare_by_their_ddl_and_are_replaced() {
        let source = side(
            "a",
            vec![view(
                "a",
                "v",
                "CREATE OR REPLACE VIEW \"a\".\"v\" AS SELECT 2 FROM a.t;",
            )],
        );
        let target = side(
            "b",
            vec![view(
                "b",
                "v",
                "CREATE OR REPLACE VIEW \"b\".\"v\" AS SELECT 1 FROM b.t;",
            )],
        );
        let items = compare(&source, &target, &Pg, &Pg);
        let v = item(&items, "v");
        assert_eq!(v.state, State::Different);
        assert_eq!(v.differences, ["the definitions differ"]);
        // Each side's own text is shown; the script's is the target's.
        assert!(v.source_ddl.contains("\"a\".\"v\""));
        assert!(v.target_ddl.contains("SELECT 1"));
        assert!(
            all(&items, false).contains("CREATE OR REPLACE VIEW \"b\".\"v\" AS SELECT 2 FROM b.t;"),
            "{}",
            all(&items, false)
        );
    }

    #[test]
    fn checks_unique_constraints_and_other_indexes() {
        let constrained = |check: &str| TableStructure {
            constraints: vec![
                ConstraintInfo {
                    name: "t_price_check".into(),
                    kind: ConstraintKind::Check,
                    columns: vec!["price".into()],
                    definition: check.into(),
                },
                ConstraintInfo {
                    name: "t_code_key".into(),
                    kind: ConstraintKind::Unique,
                    columns: vec!["code".into()],
                    definition: "UNIQUE (code)".into(),
                },
            ],
            indexes: vec![IndexInfo {
                backs_constraint: true,
                unique: true,
                ..index("t_code_key", &["code"])
            }],
            ..structure(vec![
                id(),
                column("price", "int", true),
                column("code", "text", true),
                column("tags", "text[]", true),
            ])
        };
        let mut source = constrained("CHECK (price >= 0)");
        source.indexes.push(IndexInfo {
            method: "gin".into(),
            definition: Some("CREATE INDEX t_tags_idx ON a.t USING gin (tags)".into()),
            ..index("t_tags_idx", &["tags"])
        });
        let mut target = constrained("CHECK (price > 0)");
        target
            .constraints
            .retain(|c| c.kind == ConstraintKind::Check);
        target.indexes.clear();
        let items = compare(
            &side("a", vec![table("a", "t", source)]),
            &side("b", vec![table("b", "t", target)]),
            &Pg,
            &Pg,
        );
        assert_eq!(
            item(&items, "t").differences,
            [
                "check `t_price_check`: CHECK (price > 0) → CHECK (price >= 0)",
                "unique constraint `t_code_key` is missing in the target",
                "index `t_tags_idx` is missing in the target",
            ]
        );
        let script = all(&items, false);
        let order = [
            "ALTER TABLE \"b\".\"t\" DROP CONSTRAINT \"t_price_check\";",
            "ALTER TABLE \"b\".\"t\" ADD CONSTRAINT \"t_price_check\" CHECK (price >= 0);",
            "ALTER TABLE \"b\".\"t\" ADD CONSTRAINT \"t_code_key\" UNIQUE (code);",
            "CREATE INDEX t_tags_idx ON b.t USING gin (tags);",
        ];
        for pair in order.windows(2) {
            assert!(
                at(&script, pair[0]) < at(&script, pair[1]),
                "{pair:?} in\n{script}"
            );
        }
    }

    /// A name or a server text with a line break never ends its comment:
    /// every line the compare writes itself is a comment or empty.
    #[test]
    fn a_line_break_in_a_name_never_leaves_sql_out_of_a_comment() {
        for brk in ["\n", "\r", "\r\n", "\u{2028}", "\u{85}"] {
            let evil = format!("x{brk}DROP TABLE kerno_test_victim;");
            let mood = |schema: &str, labels: &str| {
                ddl(
                    ObjectRef::Type(TableRef::new(schema, &evil)),
                    &format!("CREATE TYPE t AS ENUM ({labels})"),
                )
            };
            let source = side("a", vec![mood("a", "'sad', 'happy'")]);
            let target = side(
                "b",
                vec![
                    mood("b", "'sad'"),
                    view("b", &evil, "CREATE VIEW v AS SELECT 1"),
                ],
            );
            let items = compare(&source, &target, &Pg, &Pg);
            for drops in [false, true] {
                let script = all(&items, drops);
                // The view's DROP is the dialect's, its name quoted over two
                // lines; every other line is the compare's own.
                let stray: Vec<&str> = script
                    .split(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}'])
                    .filter(|line| !line.is_empty() && !line.starts_with("--"))
                    .filter(|line| !(drops && line.starts_with("DROP VIEW \"b\".\"x")))
                    .filter(|line| !(drops && *line == "DROP TABLE kerno_test_victim;\";"))
                    .collect();
                assert!(stray.is_empty(), "{brk:?} {drops}: {stray:?} in\n{script}");
            }
            assert_eq!(commented(&format!("a{brk}b")), "-- a\n-- b");
        }
    }

    #[test]
    fn the_schema_qualifier_is_rewritten_and_nothing_else() {
        let d = &Pg;
        assert_eq!(
            retarget("SELECT a.x, \"a\".t, xa.y, b.a.z FROM a.t", "a", "b", d),
            "SELECT b.x, \"b\".t, xa.y, b.a.z FROM b.t"
        );
        // A name that needs quotes is quoted where it was bare.
        assert_eq!(retarget("a.t", "a", "New", d), "\"New\".t");
        assert_eq!(retarget("a.t", "a", "a", d), "a.t");
    }
}

//! What introspection returns: the objects of a database, described the
//! same way for every engine.
//!
//! "Schema" means a PostgreSQL schema or a MySQL database: the namespace
//! that directly contains tables.

use crate::result::TypeKind;

/// A database on the server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DatabaseInfo {
    /// Database name.
    pub name: String,
    /// Owner, when the engine has one.
    pub owner: Option<String>,
    /// Character set / encoding.
    pub encoding: Option<String>,
    /// Default collation.
    pub collation: Option<String>,
    /// Size on disk, when the user may see it.
    pub size_bytes: Option<u64>,
    /// Built-in database (`template0`, `mysql`, `information_schema`, ...).
    pub is_system: bool,
    /// Comment.
    pub comment: Option<String>,
}

/// A schema of the connected database (PostgreSQL only).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SchemaInfo {
    /// Schema name.
    pub name: String,
    /// Owner.
    pub owner: Option<String>,
    /// Built-in schema (`pg_catalog`, `information_schema`, ...).
    pub is_system: bool,
    /// Comment.
    pub comment: Option<String>,
}

/// A table-like object, addressed by schema and name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableRef {
    /// PostgreSQL schema or MySQL database.
    pub schema: String,
    /// Object name.
    pub name: String,
}

impl TableRef {
    /// A reference to `schema.name`.
    pub fn new(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
        }
    }
}

/// The flavour of a table-like object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationKind {
    /// Ordinary table.
    Table,
    /// Partitioned table (the parent).
    PartitionedTable,
    /// Foreign table.
    ForeignTable,
    /// View.
    View,
    /// Materialized view.
    MaterializedView,
}

impl RelationKind {
    /// Whether rows can be written through the object.
    pub fn is_table(self) -> bool {
        matches!(
            self,
            Self::Table | Self::PartitionedTable | Self::ForeignTable
        )
    }
}

/// A table, view or materialized view, as listed in the navigator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationInfo {
    /// Name.
    pub name: String,
    /// Flavour.
    pub kind: RelationKind,
    /// Planner's row estimate; `None` when the table was never analysed.
    pub row_estimate: Option<u64>,
    /// Total size on disk including indexes.
    pub size_bytes: Option<u64>,
    /// Comment.
    pub comment: Option<String>,
    /// Storage engine (MySQL).
    pub engine: Option<String>,
    /// Partition of another table: hidden from the top level of the tree.
    pub is_partition: bool,
}

/// What a routine is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoutineKind {
    /// Returns a value.
    Function,
    /// Called with `CALL`.
    Procedure,
    /// Aggregate function.
    Aggregate,
    /// Window function.
    Window,
}

/// A function or procedure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineInfo {
    /// Name.
    pub name: String,
    /// Kind.
    pub kind: RoutineKind,
    /// Argument list as the engine prints it, without parentheses.
    pub arguments: String,
    /// Return type, for functions.
    pub returns: Option<String>,
    /// Implementation language.
    pub language: Option<String>,
    /// What tells overloads apart when asking for the source: the argument
    /// type list on PostgreSQL, empty on MySQL (no overloading).
    pub identity_arguments: String,
    /// Comment.
    pub comment: Option<String>,
}

/// A sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceInfo {
    /// Name.
    pub name: String,
    /// Value type.
    pub data_type: Option<String>,
    /// Start, increment, bounds and last value as text (they may exceed i64
    /// on some engines and are only displayed).
    pub start: Option<String>,
    /// Increment.
    pub increment: Option<String>,
    /// Minimum.
    pub min: Option<String>,
    /// Maximum.
    pub max: Option<String>,
    /// Last value handed out, when readable.
    pub last_value: Option<String>,
    /// Whether the sequence wraps around.
    pub cycles: bool,
    /// Table column that owns the sequence (`table.column`).
    pub owned_by: Option<String>,
}

/// The shape of a user-defined type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeShape {
    /// Enumeration, with its labels in order.
    Enum(Vec<String>),
    /// Composite type, with `(attribute, type)` pairs.
    Composite(Vec<(String, String)>),
    /// Domain over a base type, with its check constraints.
    Domain {
        /// Underlying type.
        base: String,
        /// `CHECK` definitions.
        checks: Vec<String>,
    },
    /// Range type over a subtype.
    Range(String),
    /// Anything else.
    Other,
}

/// A user-defined type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeInfo {
    /// Name.
    pub name: String,
    /// Shape.
    pub shape: TypeShape,
    /// Comment.
    pub comment: Option<String>,
}

/// A trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerInfo {
    /// Name.
    pub name: String,
    /// Table the trigger is on.
    pub table: String,
    /// `BEFORE`, `AFTER` or `INSTEAD OF`.
    pub timing: String,
    /// Events, for example `INSERT OR UPDATE`.
    pub events: String,
    /// `ROW` or `STATEMENT`.
    pub level: String,
    /// Whether the trigger fires.
    pub enabled: bool,
    /// Function called (PostgreSQL) or the statement body (MySQL).
    pub action: String,
}

/// A scheduled event (MySQL, MariaDB).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventInfo {
    /// Name.
    pub name: String,
    /// `ENABLED`, `DISABLED`, ...
    pub status: String,
    /// Schedule in words (`EVERY 1 DAY`, `AT 2030-01-01 03:00:00`).
    pub schedule: String,
    /// Comment.
    pub comment: Option<String>,
}

/// An extension (PostgreSQL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionInfo {
    /// Name.
    pub name: String,
    /// Installed version, `None` when only available.
    pub installed_version: Option<String>,
    /// Version `CREATE EXTENSION` would install.
    pub default_version: Option<String>,
    /// Schema holding the extension's objects, when installed.
    pub schema: Option<String>,
    /// Description.
    pub comment: Option<String>,
}

/// Everything a schema contains, for the navigator.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SchemaObjects {
    /// Tables, views and materialized views.
    pub relations: Vec<RelationInfo>,
    /// Functions and procedures.
    pub routines: Vec<RoutineInfo>,
    /// Sequences.
    pub sequences: Vec<SequenceInfo>,
    /// User-defined types.
    pub types: Vec<TypeInfo>,
    /// Triggers of the schema's tables.
    pub triggers: Vec<TriggerInfo>,
    /// Scheduled events.
    pub events: Vec<EventInfo>,
}

/// How a column gets its value when none is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnGeneration {
    /// `GENERATED ALWAYS AS IDENTITY`.
    IdentityAlways,
    /// `GENERATED BY DEFAULT AS IDENTITY`.
    IdentityByDefault,
    /// `AUTO_INCREMENT`, or a PostgreSQL `serial` default.
    AutoIncrement,
    /// Computed and stored from this expression.
    Stored(String),
    /// Computed on read from this expression.
    Virtual(String),
}

/// A column of a table or view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    /// Name.
    pub name: String,
    /// Full type as the engine prints it (`numeric(10,2)`, `varchar(190)`,
    /// `bigint unsigned`).
    pub data_type: String,
    /// Family of the type.
    pub kind: TypeKind,
    /// Whether NULL is allowed.
    pub nullable: bool,
    /// Default expression, as SQL.
    pub default: Option<String>,
    /// Identity / auto-increment / generated column.
    pub generation: Option<ColumnGeneration>,
    /// Position in the primary key (1-based), when part of it.
    pub primary_key_position: Option<u32>,
    /// Comment.
    pub comment: Option<String>,
    /// Collation, when not the default.
    pub collation: Option<String>,
    /// Labels, for enum (and MySQL set) columns.
    pub enum_values: Vec<String>,
}

/// An index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    /// Name.
    pub name: String,
    /// Indexed columns or expressions, in order.
    pub columns: Vec<String>,
    /// Unique index.
    pub unique: bool,
    /// Backs the primary key.
    pub primary: bool,
    /// Access method (`btree`, `gin`, `FULLTEXT`, ...).
    pub method: String,
    /// Partial-index predicate.
    pub predicate: Option<String>,
    /// Full `CREATE INDEX` statement, when the engine can print it.
    pub definition: Option<String>,
}

/// The kind of a table constraint other than a foreign key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstraintKind {
    /// Primary key.
    PrimaryKey,
    /// Unique constraint.
    Unique,
    /// Check constraint.
    Check,
    /// Exclusion constraint (PostgreSQL).
    Exclusion,
}

/// A primary key, unique, check or exclusion constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintInfo {
    /// Name.
    pub name: String,
    /// Kind.
    pub kind: ConstraintKind,
    /// Constrained columns, in order (empty for expression checks).
    pub columns: Vec<String>,
    /// Definition as SQL (`CHECK ((price >= 0))`, `UNIQUE (email)`).
    pub definition: String,
}

/// A foreign key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKeyInfo {
    /// Constraint name.
    pub name: String,
    /// Table holding the referencing columns.
    pub table: TableRef,
    /// Referencing columns, in order.
    pub columns: Vec<String>,
    /// Referenced table.
    pub referenced_table: TableRef,
    /// Referenced columns, matching `columns` by position.
    pub referenced_columns: Vec<String>,
    /// `ON UPDATE` action (`NO ACTION`, `CASCADE`, ...).
    pub on_update: String,
    /// `ON DELETE` action.
    pub on_delete: String,
}

/// The full structure of one table or view.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableStructure {
    /// Columns, in table order.
    pub columns: Vec<ColumnInfo>,
    /// Indexes.
    pub indexes: Vec<IndexInfo>,
    /// Primary key, unique, check and exclusion constraints.
    pub constraints: Vec<ConstraintInfo>,
    /// Foreign keys from this table to others.
    pub foreign_keys: Vec<ForeignKeyInfo>,
    /// Foreign keys from other tables to this one.
    pub referenced_by: Vec<ForeignKeyInfo>,
    /// Triggers on the table.
    pub triggers: Vec<TriggerInfo>,
    /// Table comment.
    pub comment: Option<String>,
}

impl TableStructure {
    /// The columns that identify a row for editing: the primary key, or
    /// failing that the first unique index whose columns are all NOT NULL.
    /// `None` means rows cannot be addressed and the table is read-only in
    /// the data view.
    pub fn row_identity(&self) -> Option<Vec<String>> {
        let mut primary: Vec<(u32, &str)> = self
            .columns
            .iter()
            .filter_map(|c| c.primary_key_position.map(|p| (p, c.name.as_str())))
            .collect();
        if !primary.is_empty() {
            primary.sort_unstable();
            return Some(
                primary
                    .into_iter()
                    .map(|(_, name)| name.to_string())
                    .collect(),
            );
        }
        let not_null = |name: &String| self.columns.iter().any(|c| &c.name == name && !c.nullable);
        self.indexes
            .iter()
            .filter(|index| index.unique && index.predicate.is_none() && !index.columns.is_empty())
            .find(|index| index.columns.iter().all(not_null))
            .map(|index| index.columns.clone())
    }
}

/// A database object the navigator can act on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ObjectRef {
    /// Table, view or materialized view.
    Relation(TableRef, RelationKind),
    /// Function or procedure; `identity_arguments` picks the overload.
    Routine {
        /// Schema.
        schema: String,
        /// Name.
        name: String,
        /// Kind.
        kind: RoutineKind,
        /// Argument types that identify the overload (PostgreSQL).
        identity_arguments: String,
    },
    /// Sequence.
    Sequence(TableRef),
    /// User-defined type.
    Type(TableRef),
    /// Trigger on a table.
    Trigger {
        /// Table the trigger is on.
        table: TableRef,
        /// Trigger name.
        name: String,
    },
    /// Scheduled event.
    Event(TableRef),
    /// Index on a table.
    Index {
        /// Table the index is on.
        table: TableRef,
        /// Index name.
        name: String,
    },
    /// Schema.
    Schema(String),
    /// Database.
    Database(String),
}

/// Version and identity of the connected server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// Full version string as the server reports it.
    pub version: String,
    /// `(major, minor, patch)`.
    pub version_number: (u32, u32, u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, nullable: bool, pk: Option<u32>) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "int".into(),
            kind: TypeKind::Integer,
            nullable,
            default: None,
            generation: None,
            primary_key_position: pk,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    fn unique(name: &str, columns: &[&str], predicate: Option<&str>) -> IndexInfo {
        IndexInfo {
            name: name.into(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            unique: true,
            primary: false,
            method: "btree".into(),
            predicate: predicate.map(Into::into),
            definition: None,
        }
    }

    #[test]
    fn row_identity_prefers_the_primary_key_in_key_order() {
        let structure = TableStructure {
            columns: vec![
                column("line_no", false, Some(2)),
                column("order_id", false, Some(1)),
                column("email", false, None),
            ],
            indexes: vec![unique("u", &["email"], None)],
            ..TableStructure::default()
        };
        assert_eq!(
            structure.row_identity(),
            Some(vec!["order_id".to_string(), "line_no".to_string()])
        );
    }

    #[test]
    fn row_identity_falls_back_to_a_not_null_unique_index() {
        let structure = TableStructure {
            columns: vec![
                column("a", true, None),
                column("b", false, None),
                column("c", false, None),
            ],
            indexes: vec![
                // Nullable column: two rows may both hold NULL, so not an identity.
                unique("u_nullable", &["a"], None),
                // Partial index: does not cover every row.
                unique("u_partial", &["b"], Some("c > 0")),
                unique("u_ok", &["b", "c"], None),
            ],
            ..TableStructure::default()
        };
        assert_eq!(
            structure.row_identity(),
            Some(vec!["b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn no_key_means_no_row_identity() {
        let structure = TableStructure {
            columns: vec![column("a", true, None)],
            indexes: vec![unique("u_nullable", &["a"], None)],
            ..TableStructure::default()
        };
        assert_eq!(structure.row_identity(), None);
    }
}

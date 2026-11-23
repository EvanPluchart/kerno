//! What a right-click asks the assistant (K53, D-122): about a table, some of its columns, a
//! result on screen or a selection of its cells. Each [`Ask`] gives the first message of a
//! conversation, the tables put first (and in full) in the context, and the values that go only
//! when the user ticks them. No GPUI: the panel shows the message's subject as a chip and the
//! whole message in "What is sent…"; nothing is sent before the user sends it.
//!
//! What goes is what J17 and D-119 allow: names, types, keys, a statement the user ran, a row
//! count. A cell's value goes only in [`Values`], and only when its tick is on.

use crate::schema::{RelationKind, TableRef, TableStructure};
use crate::sql::Catalog;

use super::{Action, Context, MAX_CELL, MAX_NAMES, MAX_QUERY, MAX_TABLES, Table};

/// Rows "Generate test data…" asks for, unless the user says otherwise, and at most.
pub const TEST_ROWS: usize = 10;
pub const MAX_TEST_ROWS: usize = 100;
/// Rows of a result sent when its tick is on, and cells of a selection.
pub const MAX_RESULT_ROWS: usize = 20;
pub const MAX_SELECTED_CELLS: usize = 500;
/// Columns of a result or a selection whose values may go; beyond, left out.
pub const MAX_VALUE_COLUMNS: usize = 50;
/// Columns named in a message about columns or a result; beyond, their count.
const MAX_NAMED_COLUMNS: usize = 100;

/// The number of rows the field of "Generate test data…" holds, within `1..=`[`MAX_TEST_ROWS`];
/// anything that is not a number is [`TEST_ROWS`].
pub fn test_rows(text: &str) -> usize {
    match text.trim().parse::<usize>() {
        Ok(rows) => rows.clamp(1, MAX_TEST_ROWS),
        Err(_) => TEST_ROWS,
    }
}

/// The table or view a right-click is on, as its structure describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub table: TableRef,
    pub view: bool,
    /// Its columns' names and types: what describes it while the tab's catalog is not read yet.
    pub columns: Vec<(String, String)>,
    /// The tables its foreign keys point to, each once, itself left out.
    pub references: Vec<TableRef>,
    /// The tables whose foreign keys point to it, each once, itself left out.
    pub referenced_by: Vec<TableRef>,
}

impl Subject {
    /// `table` as `structure` describes it: names, types and the tables its foreign keys link it
    /// to; nothing else of the structure.
    pub fn of(table: TableRef, kind: RelationKind, structure: &TableStructure) -> Self {
        let others = |tables: &mut dyn Iterator<Item = &TableRef>| {
            let mut found: Vec<TableRef> = Vec::new();
            for other in tables {
                if *other != table && !found.contains(other) {
                    found.push(other.clone());
                }
            }
            found
        };
        Self {
            view: matches!(kind, RelationKind::View | RelationKind::MaterializedView),
            columns: (structure.columns.iter())
                .map(|column| (column.name.clone(), column.data_type.clone()))
                .collect(),
            references: others(&mut structure.foreign_keys.iter().map(|k| &k.referenced_table)),
            referenced_by: others(&mut structure.referenced_by.iter().map(|k| &k.table)),
            table,
        }
    }

    /// `the table shop.orders`, `the view shop.totals`.
    fn named(&self) -> String {
        let kind = if self.view { "view" } else { "table" };
        format!("the {kind} {}", qualified(&self.table))
    }
}

/// A result on screen: a query's, or the rows of a table's data tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// The statement that produced it.
    pub statement: String,
    /// The result's columns: name and type as the server gave them.
    pub columns: Vec<(String, String)>,
    /// Rows fetched, and whether the server has more.
    pub rows: usize,
    pub more: bool,
    /// The table the rows are of, when they are one table's (a data tab).
    pub table: Option<TableRef>,
}

/// Cell values the user may send with the message by ticking them, never otherwise: the first
/// rows of a result, or the cells of a selection; long values cut, as sample rows are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Values {
    pub columns: Vec<String>,
    /// `None` for NULL.
    pub rows: Vec<Vec<Option<String>>>,
    /// Cells there were (the selection's, or the fetched rows'), of which these are the first.
    pub of: usize,
}

impl Values {
    /// Cells that go when ticked.
    pub fn cells(&self) -> usize {
        self.rows.len() * self.columns.len()
    }

    /// The tick's words, which say how many values go when it is ticked.
    pub fn label(&self, selection: bool) -> String {
        let cells = self.cells();
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        match selection {
            true if cells == self.of => {
                format!("Include the selected values ({})", plural(cells, "cell"))
            }
            true => format!("Include the selected values ({cells} of {} cells)", self.of),
            false => format!(
                "Include its first {} ({})",
                plural(self.rows.len(), "row"),
                plural(cells, "cell")
            ),
        }
    }

    /// The values as the message holds them: a header, then one line per row.
    fn text(&self) -> String {
        let mut text = self.columns.join(" | ");
        for row in &self.rows {
            let cells: Vec<String> = (row.iter())
                .map(|cell| match cell {
                    None => "NULL".to_string(),
                    Some(value) if value.chars().count() > MAX_CELL => {
                        format!("{}…", value.chars().take(MAX_CELL).collect::<String>())
                    }
                    Some(value) => value.clone(),
                })
                .collect();
            text.push_str(&format!("\n{}", cells.join(" | ")));
        }
        text
    }
}

/// What a right-click asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// "Ask about this table…": the user's question about it.
    Table(Subject),
    /// "Explain this table".
    ExplainTable(Subject),
    /// "Write a query on this table…": as "AI: Write SQL…", the table named.
    WriteQuery(Subject),
    /// "Generate test data…": `INSERT` statements for `rows` rows.
    TestData { subject: Subject, rows: usize },
    /// "Explain these columns": of a table, or of a result.
    Columns {
        table: Option<TableRef>,
        /// The result's statement, for columns of a query's result.
        statement: Option<String>,
        columns: Vec<(String, String)>,
    },
    /// "Explain this result": the statement, its columns, its row count; its first rows only
    /// when ticked.
    Result { shown: Shown, values: Values },
    /// "Ask about the selection…": the selected cells' columns and count; their values only when
    /// ticked.
    Selection { shown: Shown, values: Values },
}

impl Ask {
    /// The chip of the message being written: what it is about.
    pub fn chip(&self) -> String {
        match self {
            Self::Table(subject) => format!("About {}", subject.named()),
            Self::ExplainTable(subject) => format!("Explain {}", subject.named()),
            Self::WriteQuery(subject) => format!("Write a query on {}", subject.named()),
            Self::TestData { subject, .. } => format!("Test data for {}", subject.named()),
            Self::Columns { table, columns, .. } => {
                let names: Vec<&str> = columns.iter().map(|(name, _)| name.as_str()).collect();
                match table {
                    Some(table) => format!(
                        "Explain the columns {} of {}",
                        names.join(", "),
                        qualified(table)
                    ),
                    None => format!("Explain the columns {}", names.join(", ")),
                }
            }
            Self::Result { shown, .. } => {
                format!("Explain the result of: {}", first_line(&shown.statement))
            }
            Self::Selection { shown, values } => format!(
                "About {} selected cells of: {}",
                values.of,
                first_line(&shown.statement)
            ),
        }
    }

    /// The message needs the user's words before it can go.
    pub fn needs_words(&self) -> bool {
        matches!(
            self,
            Self::Table(_) | Self::WriteQuery(_) | Self::Selection { .. }
        )
    }

    /// The values a tick may add, if any.
    pub fn values(&self) -> Option<&Values> {
        match self {
            Self::Result { values, .. } | Self::Selection { values, .. } => {
                (values.cells() > 0).then_some(values)
            }
            _ => None,
        }
    }

    /// The tables put first in the context, in full: the subject, then the tables it is linked
    /// to where the question is about them.
    pub fn tables(&self) -> Vec<TableRef> {
        let mut tables = match self {
            Self::Table(subject) | Self::ExplainTable(subject) => {
                let mut tables = vec![subject.table.clone()];
                tables.extend(subject.references.iter().cloned());
                tables.extend(subject.referenced_by.iter().cloned());
                tables
            }
            Self::WriteQuery(subject) | Self::TestData { subject, .. } => {
                let mut tables = vec![subject.table.clone()];
                tables.extend(subject.references.iter().cloned());
                tables
            }
            Self::Columns { table, .. } => table.iter().cloned().collect(),
            Self::Result { shown, .. } | Self::Selection { shown, .. } => {
                shown.table.iter().cloned().collect()
            }
        };
        let mut seen = Vec::new();
        tables.retain(|table| {
            let new = !seen.contains(table);
            seen.push(table.clone());
            new
        });
        tables
    }

    fn subject(&self) -> Option<&Subject> {
        match self {
            Self::Table(subject)
            | Self::ExplainTable(subject)
            | Self::WriteQuery(subject)
            | Self::TestData { subject, .. } => Some(subject),
            _ => None,
        }
    }

    /// The message as sent: the instruction and what it is about, then `typed` (the user's
    /// words, which may be empty), then the values when `values` is on. `None` while the user's
    /// words are needed and missing.
    pub fn message(&self, typed: &str, engine: &str, values: bool) -> Option<String> {
        let typed = typed.trim();
        if typed.is_empty() && self.needs_words() {
            return None;
        }
        let engine = if engine.is_empty() { "SQL" } else { engine };
        let mut message = match self {
            Self::Table(subject) => format!(
                "My question is about {}, described first above.",
                subject.named()
            ),
            Self::ExplainTable(subject) => format!(
                "Explain what {} holds and how it relates to the other tables, in plain words: \
                 what its columns are for, where their names and types say it, its keys, and \
                 what its foreign keys link it to.\n{}\n{}",
                subject.named(),
                listed(
                    "It references",
                    &subject.references,
                    "It references no other table."
                ),
                listed(
                    "Tables that reference it",
                    &subject.referenced_by,
                    "No table references it."
                ),
            ),
            // The words of "AI: Write SQL…", with the table named first.
            Self::WriteQuery(subject) => {
                return Some(
                    Action::WriteSql {
                        sentence: format!("On {}: {typed}", subject.named()),
                    }
                    .message(engine),
                );
            }
            Self::TestData { subject, rows } => format!(
                "Write {engine} INSERT statements that add {rows} rows of realistic test data to \
                 {}, in one ```sql block, then at most two sentences. Respect each column's type \
                 and NOT NULL, the primary key and the unique indexes, and the foreign keys. {}",
                subject.named(),
                listed(
                    "Its rows depend on rows of these tables, through its foreign keys: use key \
                     values that exist there, or insert rows into them first, in the same block",
                    &subject.references,
                    "It has no foreign key.",
                ),
            ),
            Self::Columns {
                table,
                statement,
                columns,
            } => {
                let of = match (table, statement) {
                    (Some(table), _) => format!("of the table {}", qualified(table)),
                    (None, Some(statement)) => {
                        format!("of the result of this statement:\n{}", fenced(statement))
                    }
                    (None, None) => String::new(),
                };
                format!(
                    "Explain these columns {of}: what each one likely holds and how it is used, \
                     from its name and type.\n{}",
                    column_lines(columns)
                )
            }
            Self::Result { shown, values: _ } => format!(
                "Explain this result: what the statement does and what each column of its \
                 result holds.\n\nStatement:\n{}\n\nColumns (name type):\n{}\n\nRows: {}",
                fenced(&shown.statement),
                column_lines(&shown.columns),
                row_count(shown)
            ),
            Self::Selection { shown, values: _ } => format!(
                "My question is about cells I selected in the result of this statement:\n{}\n\n\
                 Columns of the result (name type):\n{}\n\nRows: {}",
                fenced(&shown.statement),
                column_lines(&shown.columns),
                row_count(shown)
            ),
        };
        if let Some(sent) = self.values() {
            message.push_str("\n\n");
            match (self, values) {
                (Self::Result { .. }, true) => message.push_str(&format!(
                    "Its first {} rows, which I chose to send:\n{}",
                    sent.rows.len(),
                    sent.text()
                )),
                (_, true) => message.push_str(&format!(
                    "The selected values, which I chose to send ({} of {} cells):\n{}",
                    sent.cells(),
                    sent.of,
                    sent.text()
                )),
                (Self::Result { .. }, false) => message.push_str("No value of its rows is sent."),
                (_, false) => message.push_str(&format!(
                    "I selected {} cells; their values are not sent.",
                    sent.of
                )),
            }
        }
        if !typed.is_empty() {
            message.push_str(&format!("\n\n{typed}"));
        }
        Some(message)
    }

    /// Puts the tables of [`Ask::tables`] first in `context`, described with their columns: from
    /// the catalog, else (for the subject, while the catalog is not read yet) from the subject's
    /// own columns; a table known to neither keeps its place. The structures are read for them
    /// afterwards, as for the others ([`Context::detail_tables`]).
    pub fn put_first(&self, context: &mut Context, catalog: &Catalog) {
        let mut first = Vec::new();
        for table in self.tables() {
            let at = (context.tables.iter())
                .position(|t| t.schema == table.schema && t.name == table.name);
            let had = at.map(|at| context.tables.remove(at));
            let fallback = self
                .subject()
                .filter(|subject| subject.table == table && !subject.columns.is_empty())
                .map(|subject| Table {
                    schema: table.schema.clone(),
                    name: table.name.clone(),
                    view: subject.view,
                    columns: subject.columns.clone(),
                    foreign_keys: Vec::new(),
                    detail: None,
                });
            let described = described(catalog, &table)
                .or(had.clone().filter(|had| !had.columns.is_empty()))
                .or(fallback)
                .or(had);
            first.extend(described);
        }
        first.append(&mut context.tables);
        first.truncate(MAX_TABLES + MAX_NAMES);
        context.tables = first;
    }
}

/// `table` described with its columns and foreign keys, as the catalog has it.
fn described(catalog: &Catalog, table: &TableRef) -> Option<Table> {
    let schema = catalog.schemas.iter().find(|s| s.name == table.schema)?;
    let relation = schema.relations.iter().find(|r| r.name == table.name)?;
    (!relation.columns.is_empty()).then(|| Table {
        schema: schema.name.clone(),
        name: relation.name.clone(),
        view: matches!(
            relation.kind,
            RelationKind::View | RelationKind::MaterializedView
        ),
        columns: (relation.columns.iter())
            .map(|column| (column.name.clone(), column.data_type.clone()))
            .collect(),
        foreign_keys: (relation.foreign_keys.iter())
            .map(|key| {
                format!(
                    "{} -> {}.{}({})",
                    key.columns.join(", "),
                    key.referenced_schema,
                    key.referenced_table,
                    key.referenced_columns.join(", ")
                )
            })
            .collect(),
        detail: None,
    })
}

fn qualified(table: &TableRef) -> String {
    format!("{}.{}", table.schema, table.name)
}

/// `lead: a, b.`, or `none` when there is no table.
fn listed(lead: &str, tables: &[TableRef], none: &str) -> String {
    if tables.is_empty() {
        return none.to_string();
    }
    let names: Vec<String> = tables.iter().map(qualified).collect();
    format!("{lead}: {}.", names.join(", "))
}

/// One `name type` line per column, the first [`MAX_NAMED_COLUMNS`].
fn column_lines(columns: &[(String, String)]) -> String {
    let mut lines: Vec<String> = (columns.iter())
        .take(MAX_NAMED_COLUMNS)
        .map(|(name, data_type)| format!("- {name} {data_type}"))
        .collect();
    if columns.len() > MAX_NAMED_COLUMNS {
        lines.push(format!(
            "- … {} more columns",
            columns.len() - MAX_NAMED_COLUMNS
        ));
    }
    lines.join("\n")
}

fn row_count(shown: &Shown) -> String {
    let rows = shown.rows;
    let plural = if rows == 1 { "" } else { "s" };
    match shown.more {
        true => format!("{rows} row{plural} fetched, more on the server."),
        false => format!("{rows} row{plural}."),
    }
}

/// `sql` in a ```sql block, cut at [`MAX_QUERY`] characters (said in it).
fn fenced(sql: &str) -> String {
    let sql = sql.trim();
    let shown = if sql.chars().count() > MAX_QUERY {
        let cut: String = sql.chars().take(MAX_QUERY).collect();
        format!("{cut}\n-- (cut here: the statement is longer)")
    } else {
        sql.to_string()
    };
    format!("```sql\n{shown}\n```")
}

/// A statement's first line, short, for a chip.
fn first_line(sql: &str) -> String {
    let line = sql.trim().lines().next().unwrap_or_default();
    if line.chars().count() > 60 {
        format!("{}…", line.chars().take(60).collect::<String>())
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::build_chat;
    use crate::schema::{ColumnInfo, ForeignKeyInfo};
    use crate::sql::{CatalogColumn, CatalogForeignKey, CatalogRelation, CatalogSchema};

    fn relation(name: &str, columns: &[(&str, &str)]) -> CatalogRelation {
        CatalogRelation {
            name: name.into(),
            kind: RelationKind::Table,
            columns: (columns.iter())
                .map(|(name, data_type)| CatalogColumn {
                    name: (*name).into(),
                    data_type: (*data_type).into(),
                })
                .collect(),
            foreign_keys: Vec::new(),
        }
    }

    /// The fixture's shape, with decoy tables named by the words of the messages ("data",
    /// "test", "rows", "table", "columns", "tables", "keys"): a word of an instruction picks
    /// tables too, so the subject must come first whatever the words say.
    fn catalog() -> Catalog {
        let mut orders = relation(
            "orders",
            &[
                ("id", "bigint"),
                ("customer_id", "bigint"),
                ("status", "text"),
            ],
        );
        orders.foreign_keys.push(CatalogForeignKey {
            columns: vec!["customer_id".into()],
            referenced_schema: "shop".into(),
            referenced_table: "customers".into(),
            referenced_columns: vec!["id".into()],
        });
        let decoys = ["data", "test", "rows", "table", "columns", "keys", "values"]
            .map(|name| relation(name, &[("x", "integer")]));
        let mut public = vec![relation("all_types", &[("c_int", "integer")])];
        public.extend(decoys);
        Catalog {
            schemas: vec![
                CatalogSchema {
                    name: "public".into(),
                    relations: public,
                    routines: Vec::new(),
                },
                CatalogSchema {
                    name: "shop".into(),
                    relations: vec![
                        relation("customers", &[("id", "bigint"), ("email", "text")]),
                        orders,
                        relation("order_items", &[("order_id", "bigint")]),
                    ],
                    routines: Vec::new(),
                },
            ],
            search_path: vec!["public".into()],
        }
    }

    fn column(name: &str, data_type: &str, nullable: bool, pk: Option<u32>) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            kind: crate::TypeKind::Text,
            nullable,
            default: Some("'secret default'".into()),
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: pk,
            comment: Some("a comment that is never sent".into()),
            collation: None,
            enum_values: Vec::new(),
        }
    }

    fn key(table: &str, column: &str, to: &str) -> ForeignKeyInfo {
        ForeignKeyInfo {
            name: format!("{table}_{column}_fkey"),
            table: TableRef::new("shop", table),
            columns: vec![column.into()],
            referenced_table: TableRef::new("shop", to),
            referenced_columns: vec!["id".into()],
            on_update: "NO ACTION".into(),
            on_delete: "CASCADE".into(),
        }
    }

    fn orders() -> Subject {
        let structure = TableStructure {
            columns: vec![
                column("id", "bigint", false, Some(1)),
                column("customer_id", "bigint", false, None),
                column("status", "text", true, None),
            ],
            foreign_keys: vec![key("orders", "customer_id", "customers")],
            referenced_by: vec![
                key("order_items", "order_id", "orders"),
                key("order_items", "order_id", "orders"),
            ],
            comment: Some("never sent".into()),
            ..TableStructure::default()
        };
        Subject::of(
            TableRef::new("shop", "orders"),
            RelationKind::Table,
            &structure,
        )
    }

    /// The context of `ask` as the panel builds it: the tables its whole message names, then
    /// the ask's tables put first.
    fn context_of(ask: &Ask, typed: &str) -> Context {
        let message = ask.message(typed, "PostgreSQL", false).unwrap_or_default();
        let mut context =
            Context::from_catalog(&catalog(), "PostgreSQL", None, None, None, &message);
        ask.put_first(&mut context, &catalog());
        context
    }

    fn names(context: &Context) -> Vec<String> {
        (context.tables.iter())
            .map(|t| format!("{}.{}", t.schema, t.name))
            .collect()
    }

    #[test]
    fn a_subject_is_named_with_its_links_and_nothing_else_of_its_structure() {
        let subject = orders();
        assert_eq!(subject.references, [TableRef::new("shop", "customers")]);
        assert_eq!(
            subject.referenced_by,
            [TableRef::new("shop", "order_items")]
        );
        let message = Ask::ExplainTable(subject)
            .message("", "PostgreSQL", false)
            .unwrap();
        assert!(
            message.starts_with("Explain what the table shop.orders holds"),
            "{message}"
        );
        assert!(
            message.contains("It references: shop.customers."),
            "{message}"
        );
        assert!(
            message.contains("Tables that reference it: shop.order_items."),
            "{message}"
        );
        for never in ["secret default", "comment", "never sent", "CASCADE"] {
            assert!(!message.contains(never), "{never}: {message}");
        }
    }

    /// "Ask about this table…" and "Explain this table": the subject first and described in
    /// full, then the tables it links to, before any table a word of the message names.
    #[test]
    fn the_subject_comes_first_and_in_full_whatever_the_words_name() {
        let ask = Ask::ExplainTable(orders());
        let context = context_of(&ask, "");
        let listed = names(&context);
        assert_eq!(
            listed[..3],
            ["shop.orders", "shop.customers", "shop.order_items"],
            "{listed:?}"
        );
        assert!(listed.len() > 3, "the words still name others: {listed:?}");
        assert_eq!(
            context.detail_tables()[..3],
            [
                TableRef::new("shop", "orders"),
                TableRef::new("shop", "customers"),
                TableRef::new("shop", "order_items")
            ]
        );
        assert_eq!(context.tables[0].columns.len(), 3);
        assert_eq!(
            context.tables[0].foreign_keys,
            ["customer_id -> shop.customers(id)"]
        );
        // The question asked about it names a decoy first: the subject stays first.
        let ask = Ask::Table(orders());
        assert!(ask.message("", "PostgreSQL", false).is_none());
        let context = context_of(&ask, "which rows of data have no status");
        assert_eq!(names(&context)[0], "shop.orders");
        let message = ask
            .message("which rows of data have no status", "PostgreSQL", false)
            .unwrap();
        assert_eq!(
            message,
            "My question is about the table shop.orders, described first above.\n\n\
             which rows of data have no status"
        );
    }

    /// The tab's catalog not read yet: the subject is described by its own columns, so its
    /// structure is read and it goes first all the same.
    #[test]
    fn without_a_catalog_the_subject_is_described_by_its_structure() {
        let ask = Ask::TestData {
            subject: orders(),
            rows: 10,
        };
        let mut context = Context {
            engine: "PostgreSQL".into(),
            ..Context::default()
        };
        ask.put_first(&mut context, &Catalog::default());
        assert_eq!(names(&context), ["shop.orders"]);
        assert_eq!(context.detail_tables(), [TableRef::new("shop", "orders")]);
        assert_eq!(
            context.tables[0].columns,
            [
                ("id".to_string(), "bigint".to_string()),
                ("customer_id".into(), "bigint".into()),
                ("status".into(), "text".into())
            ]
        );
    }

    #[test]
    fn generate_test_data_names_the_tables_it_depends_on_and_caps_its_rows() {
        let ask = Ask::TestData {
            subject: orders(),
            rows: test_rows("25"),
        };
        let message = ask.message("", "MySQL", false).unwrap();
        assert!(
            message.starts_with(
                "Write MySQL INSERT statements that add 25 rows of realistic test data to the \
                 table shop.orders, in one ```sql block"
            ),
            "{message}"
        );
        assert!(
            message
                .contains("NOT NULL, the primary key and the unique indexes, and the foreign keys"),
            "{message}"
        );
        assert!(
            message.contains(
                "through its foreign keys: use key values that exist there, or \
                              insert rows into them first, in the same block: shop.customers."
            ),
            "{message}"
        );
        // Its context: the table and the tables it depends on, first.
        assert_eq!(
            names(&context_of(&ask, ""))[..2],
            ["shop.orders", "shop.customers"]
        );
        assert_eq!(test_rows(" 7 "), 7);
        assert_eq!(test_rows("100000"), MAX_TEST_ROWS);
        assert_eq!(test_rows("0"), 1);
        assert_eq!(test_rows("ten"), TEST_ROWS);
        assert_eq!(test_rows(""), TEST_ROWS);
        let alone = Subject {
            references: Vec::new(),
            ..orders()
        };
        let message = Ask::TestData {
            subject: alone,
            rows: 3,
        }
        .message("with French names", "", false)
        .unwrap();
        assert!(message.contains("It has no foreign key."), "{message}");
        assert!(message.ends_with("\n\nwith French names"), "{message}");
    }

    #[test]
    fn write_a_query_is_write_sql_with_the_table_named() {
        let ask = Ask::WriteQuery(orders());
        assert!(ask.message("  ", "PostgreSQL", false).is_none());
        assert_eq!(
            ask.message("the last ten", "PostgreSQL", false).unwrap(),
            Action::WriteSql {
                sentence: "On the table shop.orders: the last ten".into()
            }
            .message("PostgreSQL")
        );
    }

    #[test]
    fn explain_these_columns_names_the_columns_picked_in_their_table() {
        let ask = Ask::Columns {
            table: Some(TableRef::new("shop", "orders")),
            statement: None,
            columns: vec![
                ("status".into(), "text".into()),
                ("customer_id".into(), "bigint".into()),
            ],
        };
        let message = ask.message("", "PostgreSQL", false).unwrap();
        assert_eq!(
            message,
            "Explain these columns of the table shop.orders: what each one likely holds and how \
             it is used, from its name and type.\n- status text\n- customer_id bigint"
        );
        assert_eq!(names(&context_of(&ask, ""))[0], "shop.orders");
        assert_eq!(
            ask.chip(),
            "Explain the columns status, customer_id of shop.orders"
        );
        // Of a query's result: its statement says where they come from.
        let of_result = Ask::Columns {
            table: None,
            statement: Some("select status from shop.orders".into()),
            columns: vec![("status".into(), "text".into())],
        };
        let message = of_result.message("", "PostgreSQL", false).unwrap();
        assert!(
            message.contains(
                "of the result of this statement:\n```sql\nselect status from shop.orders\n```"
            ),
            "{message}"
        );
    }

    fn shown() -> Shown {
        Shown {
            statement: "select id, email from shop.customers".into(),
            columns: vec![
                ("id".into(), "int8".into()),
                ("email".into(), "email".into()),
            ],
            rows: 120,
            more: true,
            table: None,
        }
    }

    fn values() -> Values {
        Values {
            columns: vec!["id".into(), "email".into()],
            rows: vec![
                vec![Some("1".into()), Some("ada@example.com".into())],
                vec![Some("2".into()), None],
            ],
            of: 240,
        }
    }

    /// "Explain this result": the statement, the column names and types, the row count, and no
    /// value of a row unless the tick is on; then exactly the values counted.
    #[test]
    fn a_result_goes_without_its_values_until_ticked() {
        let ask = Ask::Result {
            shown: shown(),
            values: values(),
        };
        let message = ask.message("", "PostgreSQL", false).unwrap();
        assert_eq!(
            message,
            "Explain this result: what the statement does and what each column of its result \
             holds.\n\nStatement:\n```sql\nselect id, email from shop.customers\n```\n\n\
             Columns (name type):\n- id int8\n- email email\n\nRows: 120 rows fetched, more on \
             the server.\n\nNo value of its rows is sent."
        );
        assert!(!message.contains("example.com"));
        let ticked = ask.message("", "PostgreSQL", true).unwrap();
        assert!(
            ticked.ends_with(
                "Its first 2 rows, which I chose to send:\nid | email\n1 | ada@example.com\n2 | NULL"
            ),
            "{ticked}"
        );
        assert_eq!(values().label(false), "Include its first 2 rows (4 cells)");
        // The whole request: nothing of a row in the context either.
        let context = context_of(&ask, "");
        let body =
            String::from_utf8(crate::ai::body(&build_chat(&context, &[], &message, "m"))).unwrap();
        assert!(!body.contains("example.com"), "{body}");
    }

    #[test]
    fn a_selection_says_how_many_cells_and_sends_them_only_when_ticked() {
        let ask = Ask::Selection {
            shown: shown(),
            values: values(),
        };
        assert!(
            ask.message("", "PostgreSQL", true).is_none(),
            "needs a question"
        );
        let message = ask
            .message("why is one empty", "PostgreSQL", false)
            .unwrap();
        assert!(
            message.contains("I selected 240 cells; their values are not sent."),
            "{message}"
        );
        assert!(!message.contains("example.com"), "{message}");
        assert!(message.ends_with("\n\nwhy is one empty"), "{message}");
        let ticked = ask.message("why is one empty", "PostgreSQL", true).unwrap();
        assert!(
            ticked.contains(
                "The selected values, which I chose to send (4 of 240 cells):\nid | email\n1 | \
                 ada@example.com\n2 | NULL"
            ),
            "{ticked}"
        );
        assert_eq!(
            values().label(true),
            "Include the selected values (4 of 240 cells)"
        );
        let all = Values { of: 4, ..values() };
        assert_eq!(all.label(true), "Include the selected values (4 cells)");
        assert_eq!(
            ask.chip(),
            "About 240 selected cells of: select id, email from shop.customers"
        );
        // A long value is cut as sample rows are.
        let long = Values {
            columns: vec!["note".into()],
            rows: vec![vec![Some("y".repeat(MAX_CELL + 50))]],
            of: 1,
        };
        assert_eq!(long.text().chars().count(), "note\n".len() + MAX_CELL + 1);
    }
}

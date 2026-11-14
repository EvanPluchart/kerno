//! What can be done to each kind of node: the content of its context menu.

use kerno_core::Dialect;
use kerno_core::maintenance::Maintenance;
use kerno_core::schema::{ObjectRef, RelationKind, RoutineKind, TableRef};

use super::model::{Group, Node, NodeKind, filter_name};
use crate::settings::TableDoubleClick;

/// A statement written for a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    Select,
    Insert,
    Update,
    Delete,
}

impl Template {
    pub fn label(self) -> &'static str {
        match self {
            Template::Select => "Generate SELECT",
            Template::Insert => "Generate INSERT",
            Template::Update => "Generate UPDATE",
            Template::Delete => "Generate DELETE",
        }
    }
}

/// One entry of a node's menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    OpenData,
    OpenStructure,
    NewQueryHere,
    Generate(Template),
    ShowDdl,
    CopyName,
    CopyQualifiedName,
    SetDefault,
    Rename,
    Truncate,
    Drop,
    CreateTable,
    CreateView,
    CreateSchema,
    CreateDatabase,
    EditSource,
    InstallExtension,
    CreateEnum,
    AddEnumValue,
    RenameEnumValue,
    Export,
    ImportCsv,
    /// Writes a backup of a database with the engine's dump tool.
    Backup,
    /// Restores a backup file into a database with the engine's client tool.
    Restore,
    Refresh,
    /// PostgreSQL `DROP … CASCADE`: also drops what depends on the object.
    DropCascade,
    CreateFunction,
    CreateProcedure,
    CreateSequence,
    CreateTrigger,
    CreateEvent,
    AddColumn,
    RenameColumn,
    DropColumn,
    /// Selects the table at the other end of a foreign key in the tree.
    RevealTable,
    /// The engine's maintenance tools (Tools ▸, see [`tools`]): never in
    /// the lists of [`actions`], which know no dialect; the menu adds it.
    Tools,
    /// A `SELECT` of the table run in a new query tab (DBeaver's "Read data
    /// in SQL console", `core.sql.editor.forSelection`).
    ReadInConsole,
    /// The call of a routine with a parameter per argument, in a new query
    /// tab (DBeaver's Generate SQL ▸ CALL).
    GenerateCall,
    /// That call, run: Kerno asks for the parameters (DBeaver's "Execute
    /// stored procedure").
    ExecuteRoutine,
    /// The table's form with the cursor on this column (DBeaver's Edit on
    /// a column).
    EditColumn,
    /// The structure tab of a table or view on its Properties sub-tab:
    /// where DBeaver's object editor ("Edit", F4) opens.
    Properties,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::OpenData => "Open data",
            Action::OpenStructure => "Open structure",
            Action::NewQueryHere => "New query here",
            Action::Generate(template) => template.label(),
            Action::ShowDdl => "Show DDL",
            Action::CopyName => "Copy name",
            Action::CopyQualifiedName => "Copy qualified name",
            Action::SetDefault => "Set as default for this tab",
            Action::Rename => "Rename…",
            Action::Truncate => "Truncate…",
            Action::Drop => "Drop…",
            Action::CreateTable => "Create table…",
            Action::CreateView => "Create view…",
            Action::CreateSchema => "Create schema…",
            Action::CreateDatabase => "Create database…",
            Action::EditSource => "Edit source",
            Action::InstallExtension => "Install…",
            Action::CreateEnum => "Create enum…",
            Action::AddEnumValue => "Add value…",
            Action::RenameEnumValue => "Rename value…",
            Action::Export => "Export…",
            Action::ImportCsv => "Import CSV…",
            Action::Backup => "Back up…",
            Action::Restore => "Restore…",
            Action::Refresh => "Refresh",
            Action::DropCascade => "Drop with cascade…",
            Action::CreateFunction => "Create function…",
            Action::CreateProcedure => "Create procedure…",
            Action::CreateSequence => "Create sequence…",
            Action::CreateTrigger => "Create trigger…",
            Action::CreateEvent => "Create event…",
            Action::AddColumn => "Add column…",
            Action::RenameColumn => "Rename column…",
            Action::DropColumn => "Drop column…",
            Action::RevealTable => "Show table in tree",
            Action::Tools => "Tools",
            Action::ReadInConsole => "Read data in SQL console",
            Action::GenerateCall => "Generate CALL",
            Action::ExecuteRoutine => "Execute…",
            Action::EditColumn => "Edit column…",
            Action::Properties => "Properties",
        }
    }

    /// Whether the action changes the database, which a read-only
    /// connection does not allow. Editing a source opens a tab whose only
    /// purpose is to apply it.
    pub fn writes(self) -> bool {
        matches!(
            self,
            Action::Rename
                | Action::Truncate
                | Action::Drop
                | Action::CreateTable
                | Action::CreateSchema
                | Action::CreateDatabase
                | Action::EditSource
                | Action::InstallExtension
                | Action::CreateEnum
                | Action::AddEnumValue
                | Action::RenameEnumValue
                | Action::ImportCsv
                | Action::Restore
                | Action::DropCascade
                | Action::RenameColumn
                | Action::DropColumn
                | Action::Tools
        )
    }
}

/// The menu of a node, in groups (a separator goes between two groups).
/// `has_schemas` tells PostgreSQL-like engines from MySQL-like ones.
pub fn actions(node: &Node, has_schemas: bool) -> Vec<Vec<Action>> {
    use Action::*;
    let all_templates = [
        Template::Select,
        Template::Insert,
        Template::Update,
        Template::Delete,
    ]
    .map(Generate);
    let groups: Vec<Vec<Action>> = match &node.kind {
        NodeKind::Object(ObjectRef::Database(_)) if has_schemas => {
            vec![
                vec![NewQueryHere, CreateSchema, CreateDatabase],
                vec![Properties, CopyName, CopyQualifiedName],
                vec![Drop],
            ]
        }
        // A MySQL database is where its tables are, like a schema.
        NodeKind::Object(ObjectRef::Database(_)) => vec![
            vec![
                NewQueryHere,
                SetDefault,
                CreateTable,
                CreateView,
                CreateDatabase,
            ],
            vec![ImportCsv],
            vec![Properties, ShowDdl, CopyName, CopyQualifiedName],
            vec![Drop],
        ],
        NodeKind::Object(ObjectRef::Schema(_)) => vec![
            vec![
                NewQueryHere,
                SetDefault,
                CreateTable,
                CreateView,
                CreateSchema,
            ],
            vec![ImportCsv],
            vec![Properties, ShowDdl, CopyName, CopyQualifiedName],
            vec![Rename, Drop],
        ],
        NodeKind::Object(ObjectRef::Relation(_, kind)) if kind.is_table() => vec![
            vec![
                OpenData,
                OpenStructure,
                Properties,
                NewQueryHere,
                ReadInConsole,
            ],
            // Backup and Truncate are under Tools ▸ ([`tool_entries`]).
            vec![Export, ImportCsv],
            all_templates.to_vec(),
            vec![AddColumn],
            vec![ShowDdl, CopyName, CopyQualifiedName],
            vec![Rename, Drop],
        ],
        NodeKind::Object(ObjectRef::Relation(
            _,
            RelationKind::View | RelationKind::MaterializedView,
        )) => vec![
            vec![
                OpenData,
                OpenStructure,
                Properties,
                NewQueryHere,
                ReadInConsole,
                Generate(Template::Select),
            ],
            vec![Export],
            vec![ShowDdl, EditSource, CopyName, CopyQualifiedName],
            vec![Rename, Drop],
        ],
        NodeKind::Object(ObjectRef::Routine {
            kind: RoutineKind::Function | RoutineKind::Procedure,
            ..
        }) => vec![
            vec![NewQueryHere, GenerateCall, ExecuteRoutine],
            vec![ShowDdl, EditSource, CopyName, CopyQualifiedName],
            vec![Rename, Drop],
        ],
        NodeKind::Object(ObjectRef::Type(_)) if node.is_enum() => vec![
            vec![ShowDdl, CopyName, CopyQualifiedName],
            vec![AddEnumValue, Rename, Drop],
        ],
        NodeKind::Object(_) => vec![
            vec![ShowDdl, CopyName, CopyQualifiedName],
            vec![Rename, Drop],
        ],
        NodeKind::Group(Group::Tables) => vec![vec![CreateTable, ImportCsv]],
        NodeKind::Group(Group::Views) => vec![vec![CreateView]],
        // The folder exists on PostgreSQL only.
        NodeKind::Group(Group::Types) => vec![vec![CreateEnum]],
        NodeKind::Group(Group::Functions) => vec![vec![CreateFunction]],
        NodeKind::Group(Group::Procedures) => vec![vec![CreateProcedure]],
        // MySQL's one folder of routines.
        NodeKind::Group(Group::Routines) => vec![vec![CreateFunction, CreateProcedure]],
        NodeKind::Group(Group::Sequences) => vec![vec![CreateSequence]],
        NodeKind::Group(Group::Triggers) => vec![vec![CreateTrigger]],
        NodeKind::Group(Group::Events) => vec![vec![CreateEvent]],
        NodeKind::Group(Group::Columns) => vec![vec![AddColumn]],
        NodeKind::Group(_) => Vec::new(),
        NodeKind::EnumValue { .. } => vec![vec![CopyName], vec![AddEnumValue, RenameEnumValue]],
        NodeKind::Extension { installed, .. } => {
            vec![
                vec![CopyName],
                vec![if *installed { Drop } else { InstallExtension }],
            ]
        }
        // F4 opens the table's form on the column; "Open structure" shows
        // the table the column is in.
        NodeKind::Column { .. } => vec![
            vec![EditColumn, OpenStructure],
            vec![CopyName],
            vec![AddColumn, RenameColumn, DropColumn],
        ],
        NodeKind::Constraint { .. } => vec![vec![CopyName]],
        // The structure of the table at the other end.
        NodeKind::ForeignKey { .. } => vec![vec![OpenStructure, RevealTable], vec![CopyName]],
    };
    // PostgreSQL drops what depends on an object with CASCADE (DBeaver's
    // cascade option of its delete confirmation); a database has none.
    let cascade = has_schemas && !matches!(node.kind, NodeKind::Object(ObjectRef::Database(_)));
    groups
        .into_iter()
        .map(|group| {
            group
                .into_iter()
                .flat_map(|action| match action {
                    Drop if cascade => vec![Drop, DropCascade],
                    action => vec![action],
                })
                .collect()
        })
        .chain([vec![Refresh]])
        .collect()
}

/// An entry of a node's Filter ▸ menu (DBeaver's `navigator.filter`: hide
/// or show only the selected object, configure, toggle, clear the filter of
/// its kind). Like Tools ▸, the menu adds it: never in [`actions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterAction {
    Hide,
    ShowOnly,
    Configure,
    Toggle,
    Clear,
}

impl FilterAction {
    pub fn label(self) -> &'static str {
        match self {
            FilterAction::Hide => "Hide this object",
            FilterAction::ShowOnly => "Show only this object",
            FilterAction::Configure => "Configure…",
            FilterAction::Toggle => "Turn on or off",
            FilterAction::Clear => "Clear",
        }
    }
}

/// The Filter ▸ entries of a node: an object filters its own kind, a
/// folder the kind it holds; folders under a table hold columns and keys
/// too. Enum values are never filtered.
pub fn filter_actions(node: &Node) -> Vec<FilterAction> {
    use FilterAction::*;
    match node.kind {
        NodeKind::Group(_) => vec![Configure, Toggle, Clear],
        _ if filter_name(node).is_some() => vec![Hide, ShowOnly, Configure, Toggle, Clear],
        _ => Vec::new(),
    }
}

/// The maintenance tools of a node (DBeaver's Tools ▸), at their default
/// options: what the dialect writes for its object. A trigger is enabled
/// when disabled (shown muted) and disabled when enabled.
pub fn tools(node: &Node, dialect: &dyn Dialect) -> Vec<Maintenance> {
    let NodeKind::Object(object) = &node.kind else {
        return Vec::new();
    };
    let mut tools = dialect.maintenance_tools(object);
    if matches!(object, ObjectRef::Trigger { .. }) {
        let other = if node.muted {
            Maintenance::DisableTrigger
        } else {
            Maintenance::EnableTrigger
        };
        tools.retain(|tool| *tool != other);
    }
    tools
}

/// An entry of Tools ▸, after DBeaver's tool groups (`toolGroup`s of
/// `ext.postgresql.ui/plugin.xml` and `ext.mysql.ui/plugin.xml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolEntry {
    /// The backup wizard (`pg_dump`, `mysqldump`).
    Backup,
    /// `pg_dumpall`: the whole server.
    GlobalBackup,
    Restore,
    /// A SQL file run by `psql` / `mysql`.
    ExecuteScript,
    /// A maintenance tool at its default options.
    Maintain(Maintenance),
    /// PostgreSQL's foreign servers, user mappings and imported schemas.
    ForeignDataWrappers,
    /// The administration tab.
    Dashboard,
    /// The administration tab on its sessions.
    SessionManager,
    /// The administration tab on its locks.
    LockManager,
    /// A tool's wizard whose aim is a saved task.
    NewTask,
    /// The Tasks view.
    Tasks,
    /// Copies tables with their data to another connection (J13).
    CopyTables,
}

impl ToolEntry {
    pub fn label(&self) -> String {
        match self {
            Self::Backup => "Back up…".into(),
            Self::GlobalBackup => "Global backup…".into(),
            Self::Restore => "Restore…".into(),
            Self::ExecuteScript => "Execute script…".into(),
            Self::Maintain(Maintenance::RefreshMaterializedView(_)) => {
                "Refresh materialized view…".into()
            }
            Self::Maintain(tool) => format!("{}…", crate::maintenance_dialog::label(tool)),
            Self::ForeignDataWrappers => "Foreign data wrappers…".into(),
            Self::Dashboard => "Open dashboard".into(),
            Self::SessionManager => "Session manager".into(),
            Self::LockManager => "Lock manager".into(),
            Self::NewTask => "Create new task…".into(),
            Self::Tasks => "Tasks".into(),
            Self::CopyTables => "Copy tables to another connection…".into(),
        }
    }

    /// Whether it changes the database, which a read-only connection does
    /// not allow (as before, every maintenance tool counts).
    pub fn writes(&self) -> bool {
        matches!(
            self,
            Self::Restore | Self::ExecuteScript | Self::Maintain(_)
        )
    }
}

/// The Tools ▸ menu of `node` (of a connection when `None`) in groups,
/// with only the entries that can apply to its kind (an entry that never
/// could is left out, not greyed: its menu stays short), as DBeaver lists
/// its tools per engine: PostgreSQL — Backup, Global backup,
/// Restore, Execute script | Analyze, Vacuum, Truncate, Refresh
/// materialized view, Enable / Disable trigger | Foreign data wrappers |
/// dashboard, sessions, locks | tasks; MySQL and MariaDB — Backup, Restore,
/// Execute script | Analyze, Check, Optimize, Repair, Truncate | dashboard,
/// sessions, locks | tasks. `maintenance` is what the dialect offers for the
/// node ([`tools`]). Only databases, schemas, tables, views and triggers
/// have the menu; a connection has no maintenance group (its tools act on
/// objects of the tree). The reasons are left to [`tools_now`]: what the
/// connection's state forbids.
pub fn tool_entries(
    node: Option<&Node>,
    has_schemas: bool,
    maintenance: &[Maintenance],
) -> ToolGroups {
    use ToolEntry::*;
    let object = match node.map(|node| &node.kind) {
        None => None,
        Some(NodeKind::Object(
            object @ (ObjectRef::Database(_)
            | ObjectRef::Schema(_)
            | ObjectRef::Relation(..)
            | ObjectRef::Trigger { .. }),
        )) => Some(object),
        Some(_) => return Vec::new(),
    };
    let whole = matches!(
        object,
        None | Some(ObjectRef::Database(_) | ObjectRef::Schema(_))
    );
    let backup = match object {
        Some(ObjectRef::Relation(_, kind)) => kind.is_table(),
        Some(ObjectRef::Trigger { .. }) => false,
        _ => true,
    };
    let client_tools = [
        (Backup, backup),
        (GlobalBackup, has_schemas),
        (Restore, whole),
        (ExecuteScript, whole),
        // It reads here and writes on the other connection: on an object
        // of the tree only, never greyed by this one being read-only.
        (CopyTables, backup && object.is_some()),
    ];
    let mut groups = vec![
        client_tools
            .into_iter()
            .filter(|(_, applies)| *applies)
            .map(|(entry, _)| (entry, None))
            .collect(),
    ];
    if object.is_some() {
        // The tools the dialect offers for the node, in DBeaver's order.
        let order: &[Maintenance] = if has_schemas {
            &[
                Maintenance::Analyze,
                Maintenance::Vacuum(Default::default()),
                Maintenance::Truncate(Default::default()),
                Maintenance::RefreshMaterializedView(Default::default()),
                Maintenance::EnableTrigger,
                Maintenance::DisableTrigger,
            ]
        } else {
            &[
                Maintenance::Analyze,
                Maintenance::Check(None),
                Maintenance::Optimize,
                Maintenance::Repair(Default::default()),
                Maintenance::Truncate(Default::default()),
            ]
        };
        groups.push(
            order
                .iter()
                .filter_map(|wanted| {
                    maintenance.iter().find(|tool| {
                        std::mem::discriminant(*tool) == std::mem::discriminant(wanted)
                    })
                })
                .map(|tool| (Maintain(tool.clone()), None))
                .collect(),
        );
    }
    if has_schemas {
        groups.push(vec![(ForeignDataWrappers, None)]);
    }
    groups.push(vec![
        (Dashboard, None),
        (SessionManager, None),
        (LockManager, None),
    ]);
    groups.push(vec![(NewTask, None), (Tasks, None)]);
    groups.retain(|group| !group.is_empty());
    groups
}

/// Tools ▸ in groups, each entry with why it cannot be used, if it cannot.
pub type ToolGroups = Vec<Vec<(ToolEntry, Option<&'static str>)>>;

/// `groups` with why each entry cannot be used now, besides the kind of
/// node: a closed connection opens only the Tasks view, a read-only one
/// refuses what writes.
pub fn tools_now(groups: ToolGroups, connected: bool, read_only: bool) -> ToolGroups {
    groups
        .into_iter()
        .map(|group| {
            group
                .into_iter()
                .map(|(entry, reason)| {
                    let now = if !connected && entry != ToolEntry::Tasks {
                        Some("not connected")
                    } else {
                        (entry.writes() && read_only).then_some("read-only connection")
                    };
                    (entry, reason.or(now))
                })
                .collect()
        })
        .collect()
}

/// Whether two Tools ▸ entries are the same tool, options aside.
pub fn same_tool(a: &ToolEntry, b: &ToolEntry) -> bool {
    match (a, b) {
        (ToolEntry::Maintain(a), ToolEntry::Maintain(b)) => {
            std::mem::discriminant(a) == std::mem::discriminant(b)
        }
        _ => a == b,
    }
}

/// What a double click or Enter does to a node: an entry of its menu, or
/// `None` to open or close it in the tree. Tables and views follow the
/// user's setting; routines open their source; other objects their DDL.
pub fn default_action(node: &Node, has_schemas: bool, table: TableDoubleClick) -> Option<Action> {
    match &node.kind {
        NodeKind::Object(ObjectRef::Relation(..)) => match table {
            TableDoubleClick::Data => Some(Action::OpenData),
            TableDoubleClick::Structure => Some(Action::OpenStructure),
            TableDoubleClick::Expand => None,
        },
        NodeKind::Object(ObjectRef::Routine {
            kind: RoutineKind::Function | RoutineKind::Procedure,
            ..
        }) => Some(Action::EditSource),
        NodeKind::Object(ObjectRef::Schema(_) | ObjectRef::Database(_)) | NodeKind::Group(_) => {
            None
        }
        NodeKind::ForeignKey { .. } => Some(Action::OpenStructure),
        _ => actions(node, has_schemas)
            .concat()
            .contains(&Action::ShowDdl)
            .then_some(Action::ShowDdl),
    }
}

/// What F4 does to a node (DBeaver's "Edit"): the table's form on a
/// column, the properties of a table or view, the source of a routine, the
/// DDL of other objects; `None` when the node has none of these.
pub fn edit_action(node: &Node, has_schemas: bool) -> Option<Action> {
    let available = actions(node, has_schemas).concat();
    [
        Action::EditColumn,
        Action::Properties,
        Action::OpenStructure,
        Action::EditSource,
        Action::ShowDdl,
    ]
    .into_iter()
    .find(|action| available.contains(action))
}

/// The statement a "Create …" entry opens in a query
/// tab, to complete and run, with its tab title. The new object is named
/// `new_…` in `schema`; a trigger is on `table`, or on a `table_name` to
/// replace when it is created from a schema's folder. MySQL bodies sit
/// between `DELIMITER //` lines so that `BEGIN … END` runs as one statement.
/// `None` when the action writes no template.
pub fn create_template(
    action: Action,
    dialect: &dyn Dialect,
    schema: &str,
    table: Option<&TableRef>,
) -> Option<(String, String)> {
    let name = |object: &str| dialect.table_name(&TableRef::new(schema, object));
    let on = table.map_or_else(|| name("table_name"), |table| dialect.table_name(table));
    let mysql = !dialect.has_schemas();
    let block = |head: String, body: &str| {
        format!("DELIMITER //\n{head}\nBEGIN\n    {body}\nEND//\nDELIMITER ;\n")
    };
    let sql = match action {
        Action::CreateFunction if mysql => block(
            format!(
                "CREATE FUNCTION {}(value INT)\nRETURNS INT\nDETERMINISTIC",
                name("new_function")
            ),
            "RETURN value;",
        ),
        Action::CreateFunction => format!(
            "CREATE FUNCTION {}(value integer)\nRETURNS integer\nLANGUAGE plpgsql\nAS $$\nBEGIN\n    RETURN value;\nEND;\n$$;\n",
            name("new_function")
        ),
        Action::CreateProcedure if mysql => block(
            format!("CREATE PROCEDURE {}()", name("new_procedure")),
            "-- statements",
        ),
        Action::CreateProcedure => format!(
            "CREATE PROCEDURE {}()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n    NULL;\nEND;\n$$;\n",
            name("new_procedure")
        ),
        Action::CreateSequence => format!(
            "CREATE SEQUENCE {}\n    START WITH 1\n    INCREMENT BY 1;\n",
            name("new_sequence")
        ),
        Action::CreateTrigger if mysql => block(
            format!(
                "CREATE TRIGGER {}\nBEFORE INSERT ON {on}\nFOR EACH ROW",
                name("new_trigger")
            ),
            "-- SET NEW.column = value;",
        ),
        // A PostgreSQL trigger calls a function that returns `trigger`.
        Action::CreateTrigger => {
            let function = name("new_trigger_function");
            format!(
                "CREATE FUNCTION {function}()\nRETURNS trigger\nLANGUAGE plpgsql\nAS $$\nBEGIN\n    RETURN NEW;\nEND;\n$$;\n\n\
                 CREATE TRIGGER {}\nBEFORE INSERT OR UPDATE ON {on}\nFOR EACH ROW\nEXECUTE FUNCTION {function}();\n",
                dialect.quote_ident("new_trigger")
            )
        }
        Action::CreateEvent => block(
            format!(
                "CREATE EVENT {}\nON SCHEDULE EVERY 1 DAY\nDO",
                name("new_event")
            ),
            "-- statements",
        ),
        _ => return None,
    };
    let title = action
        .label()
        .trim_end_matches('…')
        .replace("Create", "New");
    Some((title, sql))
}

/// The name of an object with its schema (on MySQL, its database), each
/// part quoted only when it has to be: `shop.orders`, `"Shop"."order items"`.
/// Triggers and indexes take the schema of their table.
pub fn qualified_name(object: &ObjectRef, dialect: &dyn Dialect) -> String {
    let parts: [&str; 2] = match object {
        ObjectRef::Relation(table, _)
        | ObjectRef::Sequence(table)
        | ObjectRef::Type(table)
        | ObjectRef::Event(table) => [&table.schema, &table.name],
        ObjectRef::Routine { schema, name, .. } => [schema, name],
        ObjectRef::Trigger { table, name } | ObjectRef::Index { table, name } => {
            [&table.schema, name]
        }
        ObjectRef::Schema(name) | ObjectRef::Database(name) => {
            return quote_if_needed(name, dialect);
        }
    };
    parts.map(|part| quote_if_needed(part, dialect)).join(".")
}

fn quote_if_needed(name: &str, dialect: &dyn Dialect) -> String {
    // PostgreSQL folds unquoted names to lower case; MySQL keeps them.
    let mysql = !dialect.has_schemas();
    let start = |c: char| {
        c == '_'
            || if mysql {
                c.is_ascii_alphabetic()
            } else {
                c.is_ascii_lowercase()
            }
    };
    let mut chars = name.chars();
    let plain = chars.next().is_some_and(start)
        && chars.all(|c| start(c) || c.is_ascii_digit() || c == '$');
    // ponytail: no list of reserved words (core's `needs_quotes` is
    // private), so a table named `order` is copied unquoted; use the core's
    // check once it is public.
    if plain {
        name.to_string()
    } else {
        dialect.quote_ident(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigator::model::Children;

    fn node(kind: NodeKind) -> Node {
        Node {
            id: "n".into(),
            kind,
            label: "n".into(),
            hint: None,
            muted: false,
            database: "kerno".into(),
            schema: Some("shop".into()),
            children: Children::None,
            expanded: false,
        }
    }

    fn flat(kind: NodeKind, has_schemas: bool) -> Vec<Action> {
        actions(&node(kind), has_schemas).concat()
    }

    #[test]
    fn each_kind_of_node_gets_the_actions_that_make_sense_for_it() {
        use Action::*;
        let orders = TableRef::new("shop", "orders");
        let table = flat(
            NodeKind::Object(ObjectRef::Relation(orders.clone(), RelationKind::Table)),
            true,
        );
        for action in [
            OpenData,
            OpenStructure,
            Generate(Template::Select),
            Generate(Template::Delete),
            ShowDdl,
            CopyName,
            Rename,
            Drop,
        ] {
            assert!(table.contains(&action), "{action:?}");
        }
        // Backup, Restore and Truncate moved under Tools ▸.
        assert!(!table.contains(&Truncate) && !table.contains(&Backup));
        // A view is read, not written or emptied.
        let view = flat(
            NodeKind::Object(ObjectRef::Relation(orders.clone(), RelationKind::View)),
            true,
        );
        assert!(view.contains(&Generate(Template::Select)) && view.contains(&Drop));
        assert!(view.contains(&OpenData) && view.contains(&OpenStructure));
        assert!(!view.contains(&Truncate) && !view.contains(&Generate(Template::Insert)));
        // G7: tables and views are read in a console; a table takes a new
        // column in its form (CM-73); routines are called.
        assert!(table.contains(&ReadInConsole) && view.contains(&ReadInConsole));
        assert!(table.contains(&AddColumn) && !view.contains(&AddColumn));
        for kind in [RoutineKind::Function, RoutineKind::Procedure] {
            let routine = flat(
                NodeKind::Object(ObjectRef::Routine {
                    schema: "shop".into(),
                    name: "f".into(),
                    kind,
                    identity_arguments: String::new(),
                }),
                true,
            );
            assert!(
                routine.contains(&GenerateCall) && routine.contains(&ExecuteRoutine),
                "{kind:?}"
            );
        }

        let schema = flat(NodeKind::Object(ObjectRef::Schema("shop".into())), true);
        assert!(
            schema.contains(&CreateTable)
                && schema.contains(&CreateSchema)
                && schema.contains(&NewQueryHere)
        );
        // PostgreSQL databases hold schemas; MySQL databases hold tables.
        let pg_database = flat(NodeKind::Object(ObjectRef::Database("kerno".into())), true);
        assert!(pg_database.contains(&CreateSchema) && !pg_database.contains(&CreateTable));
        let mysql_database = flat(NodeKind::Object(ObjectRef::Database("kerno".into())), false);
        assert!(mysql_database.contains(&CreateTable) && !mysql_database.contains(&CreateSchema));

        assert_eq!(
            flat(NodeKind::Group(Group::Tables), true),
            [CreateTable, ImportCsv, Refresh]
        );
        // Rows go out of tables and views, and come into tables.
        assert!(table.contains(&Export) && table.contains(&ImportCsv));
        assert!(view.contains(&Export) && !view.contains(&ImportCsv));
        assert!(schema.contains(&ImportCsv) && mysql_database.contains(&ImportCsv));
        assert!(!pg_database.contains(&ImportCsv));
        // The client tools are under Tools ▸ (see `tool_entries`).
        for menu in [&pg_database, &mysql_database, &schema] {
            assert!(!menu.contains(&Backup) && !menu.contains(&Restore));
        }
        assert_eq!(
            flat(
                NodeKind::Column {
                    table: orders.clone(),
                    name: "id".into()
                },
                true
            ),
            [
                EditColumn,
                OpenStructure,
                CopyName,
                AddColumn,
                RenameColumn,
                DropColumn,
                Refresh
            ]
        );
        assert_eq!(
            flat(NodeKind::Group(Group::Columns), false),
            [AddColumn, Refresh]
        );
        // Constraints are copied; a key leads to the table at its other end.
        assert_eq!(
            flat(
                NodeKind::Constraint {
                    table: orders.clone(),
                    name: "orders_pkey".into()
                },
                true
            ),
            [CopyName, Refresh]
        );
        assert_eq!(
            flat(
                NodeKind::ForeignKey {
                    name: "orders_customer_id_fkey".into(),
                    other: TableRef::new("shop", "customers")
                },
                false
            ),
            [OpenStructure, RevealTable, CopyName, Refresh]
        );
        // A trigger under a table is a trigger.
        let trigger = flat(
            NodeKind::Object(ObjectRef::Trigger {
                table: orders.clone(),
                name: "orders_log".into(),
            }),
            false,
        );
        assert_eq!(
            trigger,
            [ShowDdl, CopyName, CopyQualifiedName, Rename, Drop, Refresh]
        );

        // Each folder of objects creates its kind (DBeaver's Create ▸).
        let folder = |group, has_schemas| flat(NodeKind::Group(group), has_schemas);
        assert_eq!(folder(Group::Functions, true), [CreateFunction, Refresh]);
        assert_eq!(folder(Group::Procedures, true), [CreateProcedure, Refresh]);
        assert_eq!(
            folder(Group::Routines, false),
            [CreateFunction, CreateProcedure, Refresh]
        );
        assert_eq!(folder(Group::Sequences, true), [CreateSequence, Refresh]);
        assert_eq!(folder(Group::Triggers, true), [CreateTrigger, Refresh]);
        assert_eq!(folder(Group::Events, false), [CreateEvent, Refresh]);
        assert_eq!(folder(Group::Indexes, true), [Refresh]);

        // PostgreSQL drops with cascade, right after Drop; MySQL has no
        // cascade, and a database none either.
        let at = table.iter().position(|action| *action == Drop).unwrap();
        assert_eq!(table[at + 1], DropCascade);
        let mysql_table = flat(
            NodeKind::Object(ObjectRef::Relation(orders, RelationKind::Table)),
            false,
        );
        assert!(mysql_table.contains(&Drop) && !mysql_table.contains(&DropCascade));
        assert!(pg_database.contains(&Drop) && !pg_database.contains(&DropCascade));
        // Every menu ends with Refresh, in a group of its own.
        let extension = |installed| NodeKind::Extension {
            name: "pg_trgm".into(),
            installed,
        };
        assert_eq!(
            actions(&node(extension(true)), true).last(),
            Some(&vec![Refresh])
        );
        // An extension is installed or dropped, as it is.
        assert_eq!(
            flat(extension(true), true),
            [CopyName, Drop, DropCascade, Refresh]
        );
        assert_eq!(
            flat(extension(false), true),
            [CopyName, InstallExtension, Refresh]
        );

        // A query tab can be opened on a database, a schema, a table, a
        // view or a routine; a schema (or MySQL database) can become the
        // default of the active tab.
        let function = NodeKind::Object(ObjectRef::Routine {
            schema: "shop".into(),
            name: "f".into(),
            kind: RoutineKind::Function,
            identity_arguments: String::new(),
        });
        for menu in [&pg_database, &mysql_database, &schema, &table, &view] {
            assert!(menu.contains(&NewQueryHere), "{menu:?}");
        }
        assert!(flat(function.clone(), true).contains(&NewQueryHere));
        assert!(schema.contains(&SetDefault) && mysql_database.contains(&SetDefault));
        assert!(!pg_database.contains(&SetDefault) && !table.contains(&SetDefault));
        // Every object can be copied with its schema, right after its name.
        for menu in [&pg_database, &schema, &table, &view, &flat(function, true)] {
            let at = menu.iter().position(|action| *action == CopyName).unwrap();
            assert_eq!(menu[at + 1], CopyQualifiedName, "{menu:?}");
        }

        // Views and routines have a source to edit; tables do not.
        assert!(view.contains(&EditSource) && !table.contains(&EditSource));
        let routine = |kind| {
            flat(
                NodeKind::Object(ObjectRef::Routine {
                    schema: "shop".into(),
                    name: "f".into(),
                    kind,
                    identity_arguments: String::new(),
                }),
                true,
            )
        };
        assert!(routine(RoutineKind::Function).contains(&EditSource));
        assert!(routine(RoutineKind::Procedure).contains(&EditSource));
        assert!(!routine(RoutineKind::Aggregate).contains(&EditSource));

        // Enums: created in the Types folder, values added and renamed.
        assert_eq!(
            flat(NodeKind::Group(Group::Types), true),
            [CreateEnum, Refresh]
        );
        let ty = NodeKind::Object(ObjectRef::Type(TableRef::new("shop", "mood")));
        let mut mood = node(ty.clone());
        mood.children = Children::Loaded(Vec::new());
        assert!(actions(&mood, true).concat().contains(&AddEnumValue));
        // Other types have no values.
        assert!(!flat(ty, true).contains(&AddEnumValue));
        assert_eq!(
            flat(
                NodeKind::EnumValue {
                    ty: TableRef::new("shop", "mood"),
                    value: "sad".into()
                },
                true
            ),
            [CopyName, AddEnumValue, RenameEnumValue, Refresh]
        );
    }

    #[test]
    fn a_double_click_opens_what_the_node_is_for() {
        use Action::*;
        let orders = TableRef::new("shop", "orders");
        let open = |kind: NodeKind, table| default_action(&node(kind), true, table);
        for kind in [
            RelationKind::Table,
            RelationKind::View,
            RelationKind::MaterializedView,
        ] {
            let relation = NodeKind::Object(ObjectRef::Relation(orders.clone(), kind));
            assert_eq!(
                open(relation.clone(), TableDoubleClick::Data),
                Some(OpenData)
            );
            assert_eq!(
                open(relation.clone(), TableDoubleClick::Structure),
                Some(OpenStructure)
            );
            assert_eq!(open(relation, TableDoubleClick::Expand), None);
        }
        let routine = |kind| {
            NodeKind::Object(ObjectRef::Routine {
                schema: "shop".into(),
                name: "f".into(),
                kind,
                identity_arguments: String::new(),
            })
        };
        let data = TableDoubleClick::Data;
        assert_eq!(open(routine(RoutineKind::Function), data), Some(EditSource));
        assert_eq!(
            open(routine(RoutineKind::Procedure), data),
            Some(EditSource)
        );
        assert_eq!(open(routine(RoutineKind::Aggregate), data), Some(ShowDdl));
        assert_eq!(
            open(NodeKind::Object(ObjectRef::Sequence(orders.clone())), data),
            Some(ShowDdl)
        );
        // Containers open and close.
        assert_eq!(
            open(NodeKind::Object(ObjectRef::Schema("shop".into())), data),
            None
        );
        assert_eq!(
            open(NodeKind::Object(ObjectRef::Database("kerno".into())), data),
            None
        );
        assert_eq!(open(NodeKind::Group(Group::Tables), data), None);
        // Nothing to show for a column or a constraint.
        let column = NodeKind::Column {
            table: orders.clone(),
            name: "id".into(),
        };
        assert_eq!(open(column, data), None);
        let constraint = NodeKind::Constraint {
            table: orders,
            name: "orders_pkey".into(),
        };
        assert_eq!(open(constraint, data), None);
        // A foreign key opens the structure of the table at its other end.
        let key = NodeKind::ForeignKey {
            name: "orders_customer_id_fkey".into(),
            other: TableRef::new("shop", "customers"),
        };
        assert_eq!(open(key, data), Some(OpenStructure));
    }

    #[test]
    fn f4_opens_the_editor_of_what_the_node_is() {
        use Action::*;
        let orders = TableRef::new("shop", "orders");
        let edit = |kind: NodeKind, has_schemas| edit_action(&node(kind), has_schemas);
        for kind in [
            RelationKind::Table,
            RelationKind::View,
            RelationKind::MaterializedView,
        ] {
            // DBeaver's object editor opens on its properties (G17).
            let relation = NodeKind::Object(ObjectRef::Relation(orders.clone(), kind));
            assert_eq!(edit(relation, true), Some(Properties));
        }
        let routine = |kind| {
            NodeKind::Object(ObjectRef::Routine {
                schema: "shop".into(),
                name: "f".into(),
                kind,
                identity_arguments: String::new(),
            })
        };
        assert_eq!(
            edit(routine(RoutineKind::Procedure), true),
            Some(EditSource)
        );
        assert_eq!(edit(routine(RoutineKind::Aggregate), true), Some(ShowDdl));
        // A database and a schema show their properties (CM-33).
        let schema = NodeKind::Object(ObjectRef::Schema("shop".into()));
        assert_eq!(edit(schema, true), Some(Properties));
        let database = NodeKind::Object(ObjectRef::Database("kerno".into()));
        assert_eq!(edit(database.clone(), false), Some(Properties));
        // A column is edited in the form of its table (CM-72).
        let column = NodeKind::Column {
            table: orders,
            name: "id".into(),
        };
        assert_eq!(edit(column, true), Some(EditColumn));
        assert_eq!(edit(database, true), Some(Properties));
        // Nothing to edit on a folder.
        assert_eq!(edit(NodeKind::Group(Group::Tables), true), None);
    }

    #[test]
    fn a_qualified_name_is_quoted_only_where_it_has_to_be() {
        let pg = kerno_postgres::PgDialect;
        let mysql = kerno_mysql::MysqlDialect::new(kerno_core::Engine::MySql);
        let table = |schema: &str, name: &str| {
            ObjectRef::Relation(TableRef::new(schema, name), RelationKind::Table)
        };
        assert_eq!(qualified_name(&table("shop", "orders"), &pg), "shop.orders");
        assert_eq!(
            qualified_name(&table("Shop", "order items"), &pg),
            r#""Shop"."order items""#
        );
        assert_eq!(
            qualified_name(&table("shop", "2fast"), &pg),
            r#"shop."2fast""#
        );
        // MySQL keeps the case of unquoted names, and quotes with backticks.
        assert_eq!(
            qualified_name(&table("Shop", "Orders"), &mysql),
            "Shop.Orders"
        );
        assert_eq!(
            qualified_name(&table("shop", "order items"), &mysql),
            "shop.`order items`"
        );
        let routine = ObjectRef::Routine {
            schema: "shop".into(),
            name: "total$1".into(),
            kind: RoutineKind::Function,
            identity_arguments: "integer".into(),
        };
        assert_eq!(qualified_name(&routine, &pg), "shop.total$1");
        let index = ObjectRef::Index {
            table: TableRef::new("shop", "orders"),
            name: "orders_pkey".into(),
        };
        assert_eq!(qualified_name(&index, &pg), "shop.orders_pkey");
        assert_eq!(
            qualified_name(&ObjectRef::Schema("Shop".into()), &pg),
            r#""Shop""#
        );
        assert_eq!(
            qualified_name(&ObjectRef::Database("kerno".into()), &mysql),
            "kerno"
        );
    }

    #[test]
    fn only_statements_that_change_the_database_count_as_writes() {
        use Action::*;
        for action in [
            Rename,
            Truncate,
            Drop,
            CreateTable,
            CreateSchema,
            CreateDatabase,
            EditSource,
            InstallExtension,
            CreateEnum,
            AddEnumValue,
            RenameEnumValue,
            ImportCsv,
            Restore,
            DropCascade,
            RenameColumn,
            DropColumn,
            Tools,
        ] {
            assert!(action.writes(), "{action:?}");
        }
        // Templates and DDL open a tab; nothing runs.
        for action in [
            OpenData,
            OpenStructure,
            NewQueryHere,
            Generate(Template::Delete),
            ShowDdl,
            CopyName,
            CopyQualifiedName,
            SetDefault,
            CreateView,
            Export,
            Backup,
            Refresh,
            CreateFunction,
            CreateProcedure,
            CreateSequence,
            CreateTrigger,
            CreateEvent,
            AddColumn,
            RevealTable,
            ReadInConsole,
            GenerateCall,
            ExecuteRoutine,
            EditColumn,
        ] {
            assert!(!action.writes(), "{action:?}");
        }
    }

    #[test]
    fn the_tools_menu_lists_what_the_engine_offers_for_each_object() {
        use Maintenance::*;
        use kerno_core::maintenance::{
            RefreshOptions, RepairOptions, TruncateOptions, VacuumOptions,
        };
        let pg = kerno_postgres::PgDialect;
        let mysql = kerno_mysql::MysqlDialect::new(kerno_core::Engine::MySql);
        let orders = TableRef::new("shop", "orders");
        let relation = |kind| NodeKind::Object(ObjectRef::Relation(orders.clone(), kind));
        let of = |kind: NodeKind, dialect: &dyn Dialect| tools(&node(kind), dialect);
        let (vacuum, truncate) = (
            Vacuum(VacuumOptions::default()),
            Truncate(TruncateOptions::default()),
        );
        for kind in [RelationKind::Table, RelationKind::PartitionedTable] {
            assert_eq!(
                of(relation(kind), &pg),
                [vacuum.clone(), Analyze, truncate.clone()]
            );
            assert_eq!(
                of(relation(kind), &mysql),
                [
                    Analyze,
                    truncate.clone(),
                    Check(None),
                    Optimize,
                    Repair(RepairOptions::default())
                ]
            );
        }
        assert_eq!(
            of(relation(RelationKind::ForeignTable), &pg),
            [Analyze, truncate.clone()]
        );
        assert_eq!(
            of(relation(RelationKind::MaterializedView), &pg),
            [
                vacuum.clone(),
                Analyze,
                RefreshMaterializedView(RefreshOptions::default())
            ]
        );
        assert!(of(relation(RelationKind::View), &pg).is_empty());
        assert!(of(relation(RelationKind::View), &mysql).is_empty());
        // A trigger offers the one switch that changes it.
        let trigger = NodeKind::Object(ObjectRef::Trigger {
            table: orders.clone(),
            name: "orders_log".into(),
        });
        assert_eq!(of(trigger.clone(), &pg), [DisableTrigger]);
        let mut disabled = node(trigger.clone());
        disabled.muted = true;
        assert_eq!(tools(&disabled, &pg), [EnableTrigger]);
        assert!(of(trigger, &mysql).is_empty());
        // A PostgreSQL database is vacuumed and analysed; MySQL's is not.
        let database = NodeKind::Object(ObjectRef::Database("kerno".into()));
        assert_eq!(of(database.clone(), &pg), [vacuum, Analyze]);
        assert!(of(database, &mysql).is_empty());
        for other in [
            NodeKind::Object(ObjectRef::Schema("shop".into())),
            NodeKind::Group(Group::Tables),
            NodeKind::Column {
                table: orders,
                name: "id".into(),
            },
        ] {
            assert!(of(other, &pg).is_empty());
        }
    }

    /// Tools ▸ as the menu shows it: groups apart, each disabled entry
    /// with its reason.
    fn tool_labels(node: Option<&Node>, dialect: &dyn Dialect) -> Vec<String> {
        let offered = node.map(|node| tools(node, dialect)).unwrap_or_default();
        tool_entries(node, dialect.has_schemas(), &offered)
            .into_iter()
            .map(|group| {
                group
                    .into_iter()
                    .map(|(entry, reason)| match reason {
                        Some(reason) => format!("{} ({reason})", entry.label()),
                        None => entry.label(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .collect()
    }

    #[test]
    fn the_tools_submenu_follows_the_engine_and_the_kind_of_node() {
        let pg = kerno_postgres::PgDialect;
        let mysql = kerno_mysql::MysqlDialect::new(kerno_core::Engine::MySql);
        let orders = TableRef::new("shop", "orders");
        let relation = |kind| node(NodeKind::Object(ObjectRef::Relation(orders.clone(), kind)));
        let database = node(NodeKind::Object(ObjectRef::Database("kerno".into())));
        let schema = node(NodeKind::Object(ObjectRef::Schema("shop".into())));
        let tail = [
            "Foreign data wrappers…",
            "Open dashboard, Session manager, Lock manager",
            "Create new task…, Tasks",
        ];
        let with_tail = |head: &[&str]| {
            head.iter()
                .chain(tail.iter())
                .map(|group| group.to_string())
                .collect::<Vec<_>>()
        };
        // PostgreSQL: a database, a schema, a table, a materialized view,
        // a view, a trigger. Only what can apply to the kind is listed (no
        // entry greyed "on a trigger" on a table).
        assert_eq!(
            tool_labels(Some(&database), &pg),
            with_tail(&[
                "Back up…, Global backup…, Restore…, Execute script…, Copy tables to another connection…",
                "Analyze…, Vacuum…",
            ])
        );
        assert_eq!(
            tool_labels(Some(&schema), &pg),
            with_tail(&[
                "Back up…, Global backup…, Restore…, Execute script…, Copy tables to another connection…"
            ])
        );
        assert_eq!(
            tool_labels(Some(&relation(RelationKind::Table)), &pg),
            with_tail(&[
                "Back up…, Global backup…, Copy tables to another connection…",
                "Analyze…, Vacuum…, Truncate…"
            ])
        );
        assert_eq!(
            tool_labels(Some(&relation(RelationKind::MaterializedView)), &pg),
            with_tail(&[
                "Global backup…",
                "Analyze…, Vacuum…, Refresh materialized view…"
            ])
        );
        assert_eq!(
            tool_labels(Some(&relation(RelationKind::View)), &pg),
            with_tail(&["Global backup…"])
        );
        let mut trigger = node(NodeKind::Object(ObjectRef::Trigger {
            table: orders.clone(),
            name: "orders_log".into(),
        }));
        assert_eq!(
            tool_labels(Some(&trigger), &pg),
            with_tail(&["Global backup…", "Disable trigger…"])
        );
        trigger.muted = true;
        assert_eq!(tool_labels(Some(&trigger), &pg)[1], "Enable trigger…");
        // No reason is left for the kind: what is listed applies.
        assert!(
            tool_labels(Some(&relation(RelationKind::Table)), &pg)
                .concat()
                .find('(')
                .is_none()
        );
        // A connection: the client tools and the rest, no maintenance.
        assert_eq!(
            tool_labels(None, &pg),
            with_tail(&["Back up…, Global backup…, Restore…, Execute script…"])
        );

        // MySQL and MariaDB: no global backup, no foreign data wrappers.
        let mysql_tail = &tail[1..];
        assert_eq!(
            tool_labels(Some(&database), &mysql),
            ["Back up…, Restore…, Execute script…, Copy tables to another connection…"]
                .iter()
                .chain(mysql_tail)
                .map(|group| group.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            tool_labels(Some(&relation(RelationKind::Table)), &mysql)[..2],
            [
                "Back up…, Copy tables to another connection…",
                "Analyze…, Check…, Optimize…, Repair…, Truncate…"
            ]
        );
        assert_eq!(
            tool_labels(None, &mysql),
            ["Back up…, Restore…, Execute script…"]
                .iter()
                .chain(mysql_tail)
                .map(|group| group.to_string())
                .collect::<Vec<_>>()
        );
        // Other nodes have no Tools ▸.
        for other in [
            NodeKind::Group(Group::Tables),
            NodeKind::Column {
                table: orders.clone(),
                name: "id".into(),
            },
            NodeKind::Object(ObjectRef::Sequence(orders)),
        ] {
            assert!(tool_labels(Some(&node(other)), &pg).is_empty());
        }
        // What writes is refused on a read-only connection; the rest is not.
        for (entry, writes) in [
            (ToolEntry::Backup, false),
            (ToolEntry::GlobalBackup, false),
            (ToolEntry::Restore, true),
            (ToolEntry::ExecuteScript, true),
            (ToolEntry::Maintain(Maintenance::Analyze), true),
            (ToolEntry::ForeignDataWrappers, false),
            (ToolEntry::Dashboard, false),
            (ToolEntry::LockManager, false),
            (ToolEntry::NewTask, false),
            (ToolEntry::Tasks, false),
            (ToolEntry::CopyTables, false),
        ] {
            assert_eq!(entry.writes(), writes, "{entry:?}");
        }
    }

    #[test]
    fn create_entries_write_a_template_per_engine() {
        use Action::*;
        let pg = kerno_postgres::PgDialect;
        let mysql = kerno_mysql::MysqlDialect::new(kerno_core::Engine::MySql);
        let orders = TableRef::new("shop", "orders");
        let template = |action, dialect: &dyn Dialect, table| {
            create_template(action, dialect, "shop", table).expect("a template")
        };

        let (title, sql) = template(CreateFunction, &pg, None);
        assert_eq!(title, "New function");
        assert!(
            sql.starts_with("CREATE FUNCTION \"shop\".\"new_function\"(value integer)")
                && sql.contains("LANGUAGE plpgsql")
                && sql.ends_with("$$;\n"),
            "{sql}"
        );
        assert!(
            template(CreateProcedure, &pg, None)
                .1
                .contains("CREATE PROCEDURE")
        );
        assert_eq!(
            template(CreateSequence, &mysql, None).1,
            "CREATE SEQUENCE `shop`.`new_sequence`\n    START WITH 1\n    INCREMENT BY 1;\n"
        );
        // A trigger comes with its function on PostgreSQL, and is on the
        // table of its folder, or on a name to replace.
        let (_, sql) = template(CreateTrigger, &pg, Some(&orders));
        assert!(
            sql.contains("RETURNS trigger")
                && sql.contains("BEFORE INSERT OR UPDATE ON \"shop\".\"orders\"")
                && sql.contains("EXECUTE FUNCTION \"shop\".\"new_trigger_function\"();"),
            "{sql}"
        );
        assert!(
            template(CreateTrigger, &pg, None)
                .1
                .contains("ON \"shop\".\"table_name\"")
        );

        // MySQL bodies are kept whole by DELIMITER lines.
        let (title, sql) = template(CreateProcedure, &mysql, None);
        assert_eq!(title, "New procedure");
        assert_eq!(
            sql,
            "DELIMITER //\nCREATE PROCEDURE `shop`.`new_procedure`()\nBEGIN\n    -- statements\nEND//\nDELIMITER ;\n"
        );
        let statements = kerno_core::sql::split_statements(&sql, mysql.lex_rules());
        assert_eq!(statements.len(), 1, "{statements:?}");
        assert!(
            template(CreateFunction, &mysql, None)
                .1
                .contains("DETERMINISTIC")
        );
        assert!(
            template(CreateTrigger, &mysql, Some(&orders))
                .1
                .contains("BEFORE INSERT ON `shop`.`orders`")
        );
        assert!(
            template(CreateEvent, &mysql, None)
                .1
                .contains("ON SCHEDULE EVERY 1 DAY")
        );

        // A new column opens the table's form instead.
        assert_eq!(create_template(AddColumn, &pg, "shop", Some(&orders)), None);
        assert_eq!(create_template(Drop, &pg, "shop", None), None);
    }
}

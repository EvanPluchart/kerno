//! What can be done to each kind of node: the content of its context menu.

use kerno_core::Dialect;
use kerno_core::schema::{ObjectRef, RelationKind, RoutineKind};

use super::model::{Group, Node, NodeKind};
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
    Refresh,
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
            Action::Refresh => "Refresh",
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
                vec![CopyName, CopyQualifiedName],
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
            vec![ShowDdl, CopyName, CopyQualifiedName],
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
            vec![ShowDdl, CopyName, CopyQualifiedName],
            vec![Rename, Drop],
        ],
        NodeKind::Object(ObjectRef::Relation(_, kind)) if kind.is_table() => vec![
            vec![OpenData, OpenStructure, NewQueryHere],
            vec![Export, ImportCsv],
            all_templates.to_vec(),
            vec![ShowDdl, CopyName, CopyQualifiedName],
            vec![Rename, Truncate, Drop],
        ],
        NodeKind::Object(ObjectRef::Relation(
            _,
            RelationKind::View | RelationKind::MaterializedView,
        )) => vec![
            vec![
                OpenData,
                OpenStructure,
                NewQueryHere,
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
            vec![NewQueryHere],
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
        NodeKind::Group(_) => Vec::new(),
        NodeKind::EnumValue { .. } => vec![vec![CopyName], vec![AddEnumValue, RenameEnumValue]],
        NodeKind::Extension { installed, .. } => {
            vec![
                vec![CopyName],
                vec![if *installed { Drop } else { InstallExtension }],
            ]
        }
        NodeKind::Column { .. } => vec![vec![CopyName]],
    };
    groups.into_iter().chain([vec![Refresh]]).collect()
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
        _ => actions(node, has_schemas)
            .concat()
            .contains(&Action::ShowDdl)
            .then_some(Action::ShowDdl),
    }
}

/// What F4 does to a node (DBeaver's "Edit"): the structure of a table or
/// view, the source of a routine, the DDL of other objects; `None` when the
/// node has none of these.
pub fn edit_action(node: &Node, has_schemas: bool) -> Option<Action> {
    let available = actions(node, has_schemas).concat();
    [Action::OpenStructure, Action::EditSource, Action::ShowDdl]
        .into_iter()
        .find(|action| available.contains(action))
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
    use kerno_core::schema::TableRef;

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
            Truncate,
            Drop,
        ] {
            assert!(table.contains(&action), "{action:?}");
        }
        // A view is read, not written or emptied.
        let view = flat(
            NodeKind::Object(ObjectRef::Relation(orders.clone(), RelationKind::View)),
            true,
        );
        assert!(view.contains(&Generate(Template::Select)) && view.contains(&Drop));
        assert!(view.contains(&OpenData) && view.contains(&OpenStructure));
        assert!(!view.contains(&Truncate) && !view.contains(&Generate(Template::Insert)));

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
        assert_eq!(flat(NodeKind::Group(Group::Triggers), true), [Refresh]);
        assert_eq!(
            flat(
                NodeKind::Column {
                    table: orders,
                    name: "id".into()
                },
                true
            ),
            [CopyName, Refresh]
        );
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
        assert_eq!(flat(extension(true), true), [CopyName, Drop, Refresh]);
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
        // Nothing to show for a column.
        let column = NodeKind::Column {
            table: orders,
            name: "id".into(),
        };
        assert_eq!(open(column, data), None);
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
            let relation = NodeKind::Object(ObjectRef::Relation(orders.clone(), kind));
            assert_eq!(edit(relation, true), Some(OpenStructure));
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
        let schema = NodeKind::Object(ObjectRef::Schema("shop".into()));
        assert_eq!(edit(schema, true), Some(ShowDdl));
        let database = NodeKind::Object(ObjectRef::Database("kerno".into()));
        assert_eq!(edit(database.clone(), false), Some(ShowDdl));
        // Nothing to edit on a PostgreSQL database, a folder or a column.
        assert_eq!(edit(database, true), None);
        assert_eq!(edit(NodeKind::Group(Group::Tables), true), None);
        let column = NodeKind::Column {
            table: orders,
            name: "id".into(),
        };
        assert_eq!(edit(column, true), None);
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
            Refresh,
        ] {
            assert!(!action.writes(), "{action:?}");
        }
    }
}

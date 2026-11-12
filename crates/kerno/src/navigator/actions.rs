//! What can be done to each kind of node: the content of its context menu.

use kerno_core::schema::{ObjectRef, RelationKind, RoutineKind};

use super::model::{Group, Node, NodeKind};

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
                vec![CreateSchema, CreateDatabase],
                vec![CopyName],
                vec![Drop],
            ]
        }
        // A MySQL database is where its tables are, like a schema.
        NodeKind::Object(ObjectRef::Database(_)) => vec![
            vec![NewQueryHere, CreateTable, CreateView, CreateDatabase],
            vec![ShowDdl, CopyName],
            vec![Drop],
        ],
        NodeKind::Object(ObjectRef::Schema(_)) => vec![
            vec![NewQueryHere, CreateTable, CreateView, CreateSchema],
            vec![ShowDdl, CopyName],
            vec![Rename, Drop],
        ],
        NodeKind::Object(ObjectRef::Relation(_, kind)) if kind.is_table() => vec![
            vec![OpenData, OpenStructure],
            all_templates.to_vec(),
            vec![ShowDdl, CopyName],
            vec![Rename, Truncate, Drop],
        ],
        NodeKind::Object(ObjectRef::Relation(
            _,
            RelationKind::View | RelationKind::MaterializedView,
        )) => vec![
            vec![OpenData, OpenStructure, Generate(Template::Select)],
            vec![ShowDdl, EditSource, CopyName],
            vec![Rename, Drop],
        ],
        NodeKind::Object(ObjectRef::Routine {
            kind: RoutineKind::Function | RoutineKind::Procedure,
            ..
        }) => vec![vec![ShowDdl, EditSource, CopyName], vec![Rename, Drop]],
        NodeKind::Object(ObjectRef::Type(_)) if node.is_enum() => {
            vec![vec![ShowDdl, CopyName], vec![AddEnumValue, Rename, Drop]]
        }
        NodeKind::Object(_) => vec![vec![ShowDdl, CopyName], vec![Rename, Drop]],
        NodeKind::Group(Group::Tables) => vec![vec![CreateTable]],
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
            [CreateTable, Refresh]
        );
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
            CreateView,
            Refresh,
        ] {
            assert!(!action.writes(), "{action:?}");
        }
    }
}

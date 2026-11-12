//! What can be done to each kind of node: the content of its context menu.

use kerno_core::schema::{ObjectRef, RelationKind};

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
    Refresh,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
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
            Action::Refresh => "Refresh",
        }
    }

    /// Whether the action changes the database, which a read-only
    /// connection does not allow.
    pub fn writes(self) -> bool {
        matches!(
            self,
            Action::Rename
                | Action::Truncate
                | Action::Drop
                | Action::CreateSchema
                | Action::CreateDatabase
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
            all_templates.to_vec(),
            vec![ShowDdl, CopyName],
            vec![Rename, Truncate, Drop],
        ],
        NodeKind::Object(ObjectRef::Relation(
            _,
            RelationKind::View | RelationKind::MaterializedView,
        )) => vec![
            vec![Generate(Template::Select)],
            vec![ShowDdl, CopyName],
            vec![Rename, Drop],
        ],
        NodeKind::Object(_) => vec![vec![ShowDdl, CopyName], vec![Rename, Drop]],
        NodeKind::Group(Group::Tables) => vec![vec![CreateTable]],
        NodeKind::Group(Group::Views) => vec![vec![CreateView]],
        NodeKind::Group(_) => Vec::new(),
        NodeKind::Column { .. } | NodeKind::Extension(_) => vec![vec![CopyName]],
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
        assert_eq!(
            actions(&node(NodeKind::Extension("pg_trgm".into())), true).last(),
            Some(&vec![Refresh])
        );
    }

    #[test]
    fn only_statements_that_change_the_database_count_as_writes() {
        use Action::*;
        for action in [Rename, Truncate, Drop, CreateSchema, CreateDatabase] {
            assert!(action.writes(), "{action:?}");
        }
        // Templates and DDL open a tab; nothing runs.
        for action in [
            NewQueryHere,
            Generate(Template::Delete),
            ShowDdl,
            CopyName,
            CreateTable,
            CreateView,
            Refresh,
        ] {
            assert!(!action.writes(), "{action:?}");
        }
    }
}

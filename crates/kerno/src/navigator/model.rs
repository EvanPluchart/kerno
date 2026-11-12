//! The schema tree as plain data: what is loaded, what is open and what the
//! filter lets through. Nothing here touches the UI or a server; the view
//! asks for [`Load`]s, runs them and gives the answers back.

use std::collections::HashMap;

use kerno_core::schema::{
    DatabaseInfo, ExtensionInfo, ObjectRef, RelationKind, RoutineKind, SchemaInfo, SchemaObjects,
    TableRef, TableStructure, TypeShape,
};

/// Separates the segments of a node id. Not a character names contain.
const SEP: char = '\u{1f}';

/// A folder of objects of one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    Tables,
    Views,
    MaterializedViews,
    Functions,
    Procedures,
    Routines,
    Sequences,
    Types,
    Triggers,
    Events,
    Extensions,
    Columns,
    Indexes,
}

impl Group {
    pub fn label(self) -> &'static str {
        match self {
            Group::Tables => "Tables",
            Group::Views => "Views",
            Group::MaterializedViews => "Materialized views",
            Group::Functions => "Functions",
            Group::Procedures => "Procedures",
            Group::Routines => "Routines",
            Group::Sequences => "Sequences",
            Group::Types => "Types",
            Group::Triggers => "Triggers",
            Group::Events => "Events",
            Group::Extensions => "Extensions",
            Group::Columns => "Columns",
            Group::Indexes => "Indexes",
        }
    }
}

/// What a node stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// A database, schema, table, view, routine, sequence, type, trigger,
    /// event or index.
    Object(ObjectRef),
    /// A folder of objects.
    Group(Group),
    /// A column of a table or view.
    Column { table: TableRef, name: String },
    /// A PostgreSQL extension, installed in the database or available.
    Extension { name: String, installed: bool },
    /// A value of a PostgreSQL enum type.
    EnumValue { ty: TableRef, value: String },
}

/// The children of a node.
#[derive(Debug, Clone, PartialEq)]
pub enum Children {
    /// The node never has children.
    None,
    /// Not asked for yet.
    Unloaded,
    /// Asked for; the answer is on its way.
    Loading,
    /// What the server answered.
    Loaded(Vec<Node>),
    /// Why the server did not answer.
    Failed(String),
}

/// One node of the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Unique and stable across reloads: the path from the root.
    pub id: String,
    pub kind: NodeKind,
    pub label: String,
    /// Shown dimmed after the label: row estimate and size, a column's
    /// type, a count.
    pub hint: Option<String>,
    /// System objects and the like are drawn dimmed.
    pub muted: bool,
    /// The database whose session answers for this node.
    pub database: String,
    /// The schema (or MySQL database) this node is in, when it is in one.
    pub schema: Option<String>,
    pub children: Children,
    pub expanded: bool,
}

impl Node {
    /// Whether the node is a PostgreSQL enum type: the only type whose
    /// children (its values) are known, even when it has none.
    pub fn is_enum(&self) -> bool {
        matches!(self.kind, NodeKind::Object(ObjectRef::Type(_)))
            && matches!(self.children, Children::Loaded(_))
    }
}

/// Something to fetch from the server for the node `id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Load {
    pub id: String,
    pub database: String,
    pub what: LoadKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadKind {
    Schemas,
    Objects { schema: String },
    Structure { table: TableRef },
    Extensions,
}

/// The server's answer to a [`Load`].
#[derive(Debug)]
pub enum Loaded {
    Schemas(Vec<SchemaInfo>),
    Objects(Box<SchemaObjects>),
    Structure(Box<TableStructure>),
    Extensions(Vec<ExtensionInfo>),
}

/// A row of the tree as the view shows it, the filter applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub expanded: bool,
    pub children: Vec<Item>,
}

/// The tree of one connection.
#[derive(Debug, Default)]
pub struct Tree {
    /// Whether databases hold schemas (PostgreSQL) or tables (MySQL).
    has_schemas: bool,
    roots: Vec<Node>,
    filter: String,
    /// The folders that were open under a node being refreshed, to open
    /// again when its new children arrive.
    reopen: HashMap<String, Vec<String>>,
}

impl Tree {
    pub fn new(has_schemas: bool) -> Self {
        Self {
            has_schemas,
            ..Self::default()
        }
    }

    /// Replaces the tree with the server's databases, user databases first.
    /// `current` (the database of the connection) is opened; the returned
    /// load fetches what is in it.
    pub fn set_databases(
        &mut self,
        databases: Vec<DatabaseInfo>,
        current: Option<&str>,
    ) -> Option<Load> {
        let (system, user): (Vec<_>, Vec<_>) = databases
            .into_iter()
            .partition(|database| database.is_system);
        self.roots = user
            .into_iter()
            .chain(system)
            .map(|database| Node {
                id: format!("db:{}", database.name),
                kind: NodeKind::Object(ObjectRef::Database(database.name.clone())),
                hint: database.size_bytes.map(compact_bytes),
                muted: database.is_system,
                schema: (!self.has_schemas).then(|| database.name.clone()),
                database: database.name.clone(),
                label: database.name,
                children: Children::Unloaded,
                expanded: false,
            })
            .collect();
        let current = self
            .roots
            .iter()
            .find(|node| Some(node.label.as_str()) == current)?;
        let id = current.id.clone();
        self.expand(&id)
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        find(&self.roots, id)
    }

    fn node_mut(&mut self, id: &str) -> Option<&mut Node> {
        find_mut(&mut self.roots, id)
    }

    /// Opens a node. Returns what to fetch when its children are not known.
    pub fn expand(&mut self, id: &str) -> Option<Load> {
        let has_schemas = self.has_schemas;
        let node = self.node_mut(id)?;
        node.expanded = true;
        if !matches!(node.children, Children::Unloaded | Children::Failed(_)) {
            return None;
        }
        let load = load_for(node, has_schemas)?;
        node.children = Children::Loading;
        Some(load)
    }

    pub fn collapse(&mut self, id: &str) {
        if let Some(node) = self.node_mut(id) {
            node.expanded = false;
        }
    }

    /// Fetches the children of a node again; what is open below it closes.
    pub fn refresh(&mut self, id: &str) -> Option<Load> {
        let has_schemas = self.has_schemas;
        let node = self.node_mut(id)?;
        if matches!(node.children, Children::Loading) {
            return None;
        }
        let load = load_for(node, has_schemas)?;
        let mut open = Vec::new();
        open_below(node, &mut open);
        node.children = Children::Loading;
        node.expanded = true;
        self.reopen.insert(id.to_string(), open);
        Some(load)
    }

    /// The node whose children a refresh of `id` fetches: `id` itself when
    /// it loads its own children, otherwise the nearest ancestor that does
    /// (a table inside the "Tables" folder is refreshed with its schema).
    pub fn refresh_target(&self, id: &str) -> Option<String> {
        let mut id = id.to_string();
        loop {
            let node = self.node(&id)?;
            if load_for(node, self.has_schemas).is_some() {
                return Some(id);
            }
            id.truncate(id.rfind(SEP)?);
        }
    }

    /// Stores the answer to a load.
    pub fn finish(&mut self, id: &str, outcome: Result<Loaded, String>) {
        let has_schemas = self.has_schemas;
        let Some(node) = self.node_mut(id) else {
            return;
        };
        node.children = match outcome {
            Err(message) => Children::Failed(message),
            Ok(Loaded::Schemas(schemas)) => Children::Loaded(schema_nodes(node, schemas)),
            Ok(Loaded::Objects(objects)) => {
                Children::Loaded(object_groups(node, *objects, has_schemas))
            }
            Ok(Loaded::Structure(structure)) => Children::Loaded(structure_nodes(node, *structure)),
            Ok(Loaded::Extensions(extensions)) => {
                Children::Loaded(extension_nodes(node, extensions))
            }
        };
        // Folders that were open before a refresh are open after it. A
        // table that was open closes: its columns would have to be fetched.
        for open in self.reopen.remove(id).unwrap_or_default() {
            if let Some(node) = self.node_mut(&open)
                && matches!(node.children, Children::Loaded(_))
            {
                node.expanded = true;
            }
        }
    }

    /// Shows only what matches `filter` (and what leads to it).
    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.trim().to_lowercase();
    }

    /// The rows to show.
    pub fn items(&self) -> Vec<Item> {
        self.roots
            .iter()
            .filter_map(|node| item(node, &self.filter))
            .collect()
    }
}

/// The ids of the open nodes below `node`.
fn open_below(node: &Node, open: &mut Vec<String>) {
    if let Children::Loaded(children) = &node.children {
        for child in children {
            if child.expanded {
                open.push(child.id.clone());
            }
            open_below(child, open);
        }
    }
}

fn find<'a>(nodes: &'a [Node], id: &str) -> Option<&'a Node> {
    let node = nodes.iter().find(|node| leads_to(&node.id, id))?;
    if node.id == id {
        return Some(node);
    }
    match &node.children {
        Children::Loaded(children) => find(children, id),
        _ => None,
    }
}

fn find_mut<'a>(nodes: &'a mut [Node], id: &str) -> Option<&'a mut Node> {
    let node = nodes.iter_mut().find(|node| leads_to(&node.id, id))?;
    if node.id == id {
        return Some(node);
    }
    match &mut node.children {
        Children::Loaded(children) => find_mut(children, id),
        _ => None,
    }
}

/// Whether `target` is `id` or below it.
fn leads_to(id: &str, target: &str) -> bool {
    target
        .strip_prefix(id)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(SEP))
}

fn load_for(node: &Node, has_schemas: bool) -> Option<Load> {
    let what = match &node.kind {
        NodeKind::Object(ObjectRef::Database(_)) if has_schemas => LoadKind::Schemas,
        NodeKind::Object(ObjectRef::Database(name)) => LoadKind::Objects {
            schema: name.clone(),
        },
        NodeKind::Object(ObjectRef::Schema(name)) => LoadKind::Objects {
            schema: name.clone(),
        },
        NodeKind::Object(ObjectRef::Relation(table, _)) => LoadKind::Structure {
            table: table.clone(),
        },
        NodeKind::Group(Group::Extensions) => LoadKind::Extensions,
        _ => return None,
    };
    Some(Load {
        id: node.id.clone(),
        database: node.database.clone(),
        what,
    })
}

/// A child of `parent` with nothing below it yet.
fn child(parent: &Node, segment: &str, kind: NodeKind, label: impl Into<String>) -> Node {
    Node {
        id: format!("{}{SEP}{segment}", parent.id),
        kind,
        label: label.into(),
        hint: None,
        muted: false,
        database: parent.database.clone(),
        schema: parent.schema.clone(),
        children: Children::None,
        expanded: false,
    }
}

fn schema_nodes(database: &Node, schemas: Vec<SchemaInfo>) -> Vec<Node> {
    let (system, user): (Vec<_>, Vec<_>) = schemas.into_iter().partition(|schema| schema.is_system);
    let mut nodes: Vec<Node> = user
        .into_iter()
        .chain(system)
        .map(|schema| Node {
            muted: schema.is_system,
            schema: Some(schema.name.clone()),
            children: Children::Unloaded,
            ..child(
                database,
                &format!("schema:{}", schema.name),
                NodeKind::Object(ObjectRef::Schema(schema.name.clone())),
                schema.name,
            )
        })
        .collect();
    nodes.push(Node {
        children: Children::Unloaded,
        ..child(
            database,
            "extensions",
            NodeKind::Group(Group::Extensions),
            Group::Extensions.label(),
        )
    });
    nodes
}

fn extension_nodes(group: &Node, extensions: Vec<ExtensionInfo>) -> Vec<Node> {
    // Installed ones first; the rest can be installed.
    let (installed, available): (Vec<_>, Vec<_>) = extensions
        .into_iter()
        .partition(|extension| extension.installed_version.is_some());
    installed
        .into_iter()
        .chain(available)
        .map(|extension| Node {
            hint: Some(match &extension.installed_version {
                Some(version) => version.clone(),
                None => "not installed".into(),
            }),
            muted: extension.installed_version.is_none(),
            ..child(
                group,
                &format!("ext:{}", extension.name),
                NodeKind::Extension {
                    name: extension.name.clone(),
                    installed: extension.installed_version.is_some(),
                },
                extension.name,
            )
        })
        .collect()
}

/// A folder holding `children`, with their number as its hint.
fn group(parent: &Node, kind: Group, build: impl FnOnce(&Node) -> Vec<Node>) -> Node {
    let mut node = child(
        parent,
        &kind.label().to_lowercase().replace(' ', "-"),
        NodeKind::Group(kind),
        kind.label(),
    );
    let children = build(&node);
    node.hint = Some(children.len().to_string());
    // An empty folder is there to say so, and for its "Create…" entry.
    node.muted = children.is_empty();
    node.children = if children.is_empty() {
        Children::None
    } else {
        Children::Loaded(children)
    };
    node
}

fn object_groups(parent: &Node, objects: SchemaObjects, has_schemas: bool) -> Vec<Node> {
    let schema = parent.schema.clone().unwrap_or_default();
    let SchemaObjects {
        relations,
        routines,
        sequences,
        types,
        triggers,
        events,
    } = objects;

    let relations_of = |kinds: &'static [RelationKind], group_kind: Group| {
        let relations = &relations;
        let schema = schema.clone();
        group(parent, group_kind, move |folder| {
            relations
                .iter()
                // A partition is reached through the table it belongs to.
                .filter(|relation| kinds.contains(&relation.kind) && !relation.is_partition)
                .map(|relation| {
                    let table = TableRef::new(schema.clone(), relation.name.clone());
                    let hint = match (relation.row_estimate, relation.size_bytes) {
                        (Some(rows), Some(bytes)) => Some(format!(
                            "{} · {}",
                            compact_count(rows),
                            compact_bytes(bytes)
                        )),
                        (Some(rows), None) => Some(compact_count(rows)),
                        (None, Some(bytes)) => Some(compact_bytes(bytes)),
                        (None, None) => None,
                    };
                    Node {
                        hint,
                        children: Children::Unloaded,
                        ..child(
                            folder,
                            &format!("rel:{}", relation.name),
                            NodeKind::Object(ObjectRef::Relation(table, relation.kind)),
                            relation.name.clone(),
                        )
                    }
                })
                .collect()
        })
    };
    let routines_of = |kinds: &'static [RoutineKind], group_kind: Group| {
        let routines = &routines;
        let schema = schema.clone();
        group(parent, group_kind, move |folder| {
            routines
                .iter()
                .filter(|routine| kinds.contains(&routine.kind))
                .map(|routine| Node {
                    hint: routine.returns.clone(),
                    ..child(
                        folder,
                        // Overloads share a name: the argument types tell them apart.
                        &format!("routine:{}({})", routine.name, routine.identity_arguments),
                        NodeKind::Object(ObjectRef::Routine {
                            schema: schema.clone(),
                            name: routine.name.clone(),
                            kind: routine.kind,
                            identity_arguments: routine.identity_arguments.clone(),
                        }),
                        format!("{}({})", routine.name, routine.arguments),
                    )
                })
                .collect()
        })
    };
    let sequences_group = group(parent, Group::Sequences, |folder| {
        sequences
            .iter()
            .map(|sequence| Node {
                hint: sequence.last_value.clone(),
                ..child(
                    folder,
                    &format!("seq:{}", sequence.name),
                    NodeKind::Object(ObjectRef::Sequence(TableRef::new(
                        schema.clone(),
                        sequence.name.clone(),
                    ))),
                    sequence.name.clone(),
                )
            })
            .collect()
    });
    let triggers_group = group(parent, Group::Triggers, |folder| {
        triggers
            .iter()
            .map(|trigger| Node {
                hint: Some(format!("on {}", trigger.table)),
                muted: !trigger.enabled,
                ..child(
                    folder,
                    &format!("trigger:{}.{}", trigger.table, trigger.name),
                    NodeKind::Object(ObjectRef::Trigger {
                        table: TableRef::new(schema.clone(), trigger.table.clone()),
                        name: trigger.name.clone(),
                    }),
                    trigger.name.clone(),
                )
            })
            .collect()
    });

    const TABLES: &[RelationKind] = &[
        RelationKind::Table,
        RelationKind::PartitionedTable,
        RelationKind::ForeignTable,
    ];
    let mut groups = vec![
        relations_of(TABLES, Group::Tables),
        relations_of(&[RelationKind::View], Group::Views),
    ];
    if has_schemas {
        groups.push(relations_of(
            &[RelationKind::MaterializedView],
            Group::MaterializedViews,
        ));
        groups.push(routines_of(
            &[
                RoutineKind::Function,
                RoutineKind::Aggregate,
                RoutineKind::Window,
            ],
            Group::Functions,
        ));
        groups.push(routines_of(&[RoutineKind::Procedure], Group::Procedures));
        groups.push(sequences_group);
        groups.push(group(parent, Group::Types, |folder| {
            types
                .iter()
                .map(|kind| {
                    let ty = TableRef::new(schema.clone(), kind.name.clone());
                    let mut node = Node {
                        hint: Some(
                            match &kind.shape {
                                TypeShape::Enum(_) => "enum",
                                TypeShape::Composite(_) => "composite",
                                TypeShape::Domain { .. } => "domain",
                                TypeShape::Range(_) => "range",
                                TypeShape::Other => "type",
                            }
                            .into(),
                        ),
                        ..child(
                            folder,
                            &format!("type:{}", kind.name),
                            NodeKind::Object(ObjectRef::Type(ty.clone())),
                            kind.name.clone(),
                        )
                    };
                    // An enum lists its values, in order.
                    if let TypeShape::Enum(values) = &kind.shape {
                        let values = values
                            .iter()
                            .map(|value| {
                                child(
                                    &node,
                                    &format!("value:{value}"),
                                    NodeKind::EnumValue {
                                        ty: ty.clone(),
                                        value: value.clone(),
                                    },
                                    value.clone(),
                                )
                            })
                            .collect();
                        node.children = Children::Loaded(values);
                    }
                    node
                })
                .collect()
        }));
        groups.push(triggers_group);
    } else {
        groups.push(routines_of(
            &[
                RoutineKind::Function,
                RoutineKind::Procedure,
                RoutineKind::Aggregate,
                RoutineKind::Window,
            ],
            Group::Routines,
        ));
        groups.push(triggers_group);
        groups.push(group(parent, Group::Events, |folder| {
            events
                .iter()
                .map(|event| Node {
                    hint: Some(event.status.to_lowercase()),
                    ..child(
                        folder,
                        &format!("event:{}", event.name),
                        NodeKind::Object(ObjectRef::Event(TableRef::new(
                            schema.clone(),
                            event.name.clone(),
                        ))),
                        event.name.clone(),
                    )
                })
                .collect()
        }));
        // MariaDB has sequences; MySQL does not, and gets no empty folder.
        if !sequences.is_empty() {
            groups.push(sequences_group);
        }
    }
    groups
}

fn structure_nodes(relation: &Node, structure: TableStructure) -> Vec<Node> {
    let NodeKind::Object(ObjectRef::Relation(table, _)) = &relation.kind else {
        return Vec::new();
    };
    let columns = group(relation, Group::Columns, |folder| {
        structure
            .columns
            .iter()
            .map(|column| {
                let mut hint = column.data_type.clone();
                if column.primary_key_position.is_some() {
                    hint.push_str(" · key");
                } else if !column.nullable {
                    hint.push_str(" · not null");
                }
                Node {
                    hint: Some(hint),
                    ..child(
                        folder,
                        &format!("col:{}", column.name),
                        NodeKind::Column {
                            table: table.clone(),
                            name: column.name.clone(),
                        },
                        column.name.clone(),
                    )
                }
            })
            .collect()
    });
    let indexes = group(relation, Group::Indexes, |folder| {
        structure
            .indexes
            .iter()
            .map(|index| {
                let mut hint = index.columns.join(", ");
                if index.primary {
                    hint.push_str(" · primary");
                } else if index.unique {
                    hint.push_str(" · unique");
                }
                Node {
                    hint: Some(hint),
                    ..child(
                        folder,
                        &format!("index:{}", index.name),
                        NodeKind::Object(ObjectRef::Index {
                            table: table.clone(),
                            name: index.name.clone(),
                        }),
                        index.name.clone(),
                    )
                }
            })
            .collect()
    });
    vec![columns, indexes]
}

/// A line of text under `parent`.
fn note(parent: &Node, text: &str) -> Item {
    Item {
        id: format!("{}{SEP}note", parent.id),
        label: text.into(),
        expanded: false,
        children: Vec::new(),
    }
}

/// Whether `id` is a note line rather than a node.
pub fn is_note(id: &str) -> bool {
    id.rsplit(SEP).next() == Some("note")
}

/// The id of the node (or, for a note, of the node) just above `id`.
pub fn parent_of(id: &str) -> Option<&str> {
    id.rfind(SEP).map(|at| &id[..at])
}

fn matches(node: &Node, filter: &str) -> bool {
    !matches!(node.kind, NodeKind::Group(_)) && node.label.to_lowercase().contains(filter)
}

fn item(node: &Node, filter: &str) -> Option<Item> {
    // A node that matches keeps everything below it.
    let below = if filter.is_empty() || matches(node, filter) {
        ""
    } else {
        filter
    };
    let children = match &node.children {
        Children::None => Vec::new(),
        Children::Unloaded | Children::Loading => vec![note(node, "Loading…")],
        Children::Failed(message) => vec![note(node, message)],
        Children::Loaded(children) if children.is_empty() => vec![note(node, "Nothing here")],
        Children::Loaded(children) => children
            .iter()
            .filter_map(|child| item(child, below))
            .collect(),
    };
    let found_below = !below.is_empty() && children.iter().any(|child| !is_note(&child.id));
    if !below.is_empty() && !found_below {
        return None;
    }
    Some(Item {
        id: node.id.clone(),
        label: node.label.clone(),
        // What the filter found is shown without having to open its way.
        expanded: node.expanded || found_below,
        children,
    })
}

/// `1234567` as `1.2M`: a row estimate needs no more precision.
pub fn compact_count(count: u64) -> String {
    // The bounds are where rounding reaches the next unit.
    match count {
        0..=999 => count.to_string(),
        1_000..=999_499 => scaled(count, 1_000, "k"),
        999_500..=999_499_999 => scaled(count, 1_000_000, "M"),
        _ => scaled(count, 1_000_000_000, "B"),
    }
}

/// A size in binary units, as database tools report sizes.
pub fn compact_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    match bytes {
        0..KIB => format!("{bytes} B"),
        b if b < KIB * KIB => scaled(b, KIB, " kB"),
        b if b < KIB * KIB * KIB => scaled(b, KIB * KIB, " MB"),
        b => scaled(b, KIB * KIB * KIB, " GB"),
    }
}

/// One decimal below ten units, none above: `1.2M`, `12M`.
fn scaled(value: u64, unit: u64, suffix: &str) -> String {
    let tenths = (value * 10 + unit / 2) / unit;
    if tenths < 100 {
        format!("{}.{}{suffix}", tenths / 10, tenths % 10)
    } else {
        format!("{}{suffix}", (value + unit / 2) / unit)
    }
}

#[cfg(test)]
mod tests {
    use kerno_core::TypeKind;
    use kerno_core::schema::{ColumnInfo, IndexInfo, RelationInfo, RoutineInfo, TypeInfo};

    use super::*;

    fn database(name: &str, is_system: bool) -> DatabaseInfo {
        DatabaseInfo {
            name: name.into(),
            is_system,
            size_bytes: Some(8 * 1024 * 1024),
            ..DatabaseInfo::default()
        }
    }

    fn schema(name: &str, is_system: bool) -> SchemaInfo {
        SchemaInfo {
            name: name.into(),
            is_system,
            ..SchemaInfo::default()
        }
    }

    fn relation(name: &str, kind: RelationKind) -> RelationInfo {
        RelationInfo {
            name: name.into(),
            kind,
            row_estimate: Some(1_000_000),
            size_bytes: Some(190 * 1024 * 1024),
            comment: None,
            engine: None,
            is_partition: false,
        }
    }

    fn routine(name: &str, kind: RoutineKind, arguments: &str) -> RoutineInfo {
        RoutineInfo {
            name: name.into(),
            kind,
            arguments: arguments.into(),
            returns: Some("integer".into()),
            language: None,
            identity_arguments: arguments.into(),
            comment: None,
        }
    }

    fn column(name: &str, key: bool, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "integer".into(),
            kind: TypeKind::Integer,
            nullable,
            default: None,
            generation: None,
            on_update: None,
            primary_key_position: key.then_some(1),
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    fn objects() -> SchemaObjects {
        SchemaObjects {
            relations: vec![
                relation("orders", RelationKind::Table),
                relation("order_items", RelationKind::Table),
                relation("customers", RelationKind::Table),
                relation("recent_orders", RelationKind::View),
                relation("sales_by_day", RelationKind::MaterializedView),
            ],
            routines: vec![
                routine("total", RoutineKind::Function, "integer"),
                routine("total", RoutineKind::Function, "integer, integer"),
                routine("archive", RoutineKind::Procedure, ""),
            ],
            types: vec![
                TypeInfo {
                    name: "mood".into(),
                    shape: TypeShape::Enum(vec!["sad".into(), "ok".into()]),
                    comment: None,
                },
                TypeInfo {
                    name: "nothing_yet".into(),
                    shape: TypeShape::Enum(Vec::new()),
                    comment: None,
                },
                TypeInfo {
                    name: "address".into(),
                    shape: TypeShape::Composite(Vec::new()),
                    comment: None,
                },
            ],
            ..SchemaObjects::default()
        }
    }

    /// A PostgreSQL tree with `shop` loaded, as it is after connecting.
    fn loaded() -> (Tree, String) {
        let mut tree = Tree::new(true);
        let load = tree
            .set_databases(
                vec![database("template1", true), database("kerno", false)],
                Some("kerno"),
            )
            .expect("the current database is opened");
        assert_eq!(
            load,
            Load {
                id: "db:kerno".into(),
                database: "kerno".into(),
                what: LoadKind::Schemas
            }
        );
        tree.finish(
            "db:kerno",
            Ok(Loaded::Schemas(vec![
                schema("pg_catalog", true),
                schema("shop", false),
            ])),
        );
        let shop = format!("db:kerno{SEP}schema:shop");
        let load = tree.expand(&shop).expect("a schema loads its objects");
        assert_eq!(
            load.what,
            LoadKind::Objects {
                schema: "shop".into()
            }
        );
        tree.finish(&shop, Ok(Loaded::Objects(Box::new(objects()))));
        (tree, shop)
    }

    /// Labels of the rows, indented by depth, folders that are open marked.
    fn outline(items: &[Item]) -> Vec<String> {
        fn walk(items: &[Item], depth: usize, out: &mut Vec<String>) {
            for item in items {
                let mark = if item.children.is_empty() {
                    ""
                } else if item.expanded {
                    "v "
                } else {
                    "> "
                };
                out.push(format!("{}{mark}{}", "  ".repeat(depth), item.label));
                if item.expanded {
                    walk(&item.children, depth + 1, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(items, 0, &mut out);
        out
    }

    #[test]
    fn databases_then_schemas_then_folders_of_objects() {
        let (tree, shop) = loaded();
        assert_eq!(
            outline(&tree.items()),
            [
                "v kerno",
                "  v shop",
                "    > Tables",
                "    > Views",
                "    > Materialized views",
                "    > Functions",
                "    > Procedures",
                "    Sequences",
                "    > Types",
                "    Triggers",
                // System objects come last.
                "  > pg_catalog",
                "  > Extensions",
                "> template1",
            ]
        );
        let tables = tree.node(&format!("{shop}{SEP}tables")).unwrap();
        assert_eq!(tables.hint.as_deref(), Some("3"));
        let orders = tree
            .node(&format!("{shop}{SEP}tables{SEP}rel:orders"))
            .unwrap();
        assert_eq!(orders.hint.as_deref(), Some("1.0M · 190 MB"));
        assert_eq!(
            (orders.database.as_str(), orders.schema.as_deref()),
            ("kerno", Some("shop"))
        );
        // Overloads are two nodes.
        let Children::Loaded(functions) = &tree
            .node(&format!("{shop}{SEP}functions"))
            .unwrap()
            .children
        else {
            panic!("functions are loaded with the schema");
        };
        assert_eq!(
            functions
                .iter()
                .map(|node| node.label.as_str())
                .collect::<Vec<_>>(),
            ["total(integer)", "total(integer, integer)"]
        );
        assert_ne!(functions[0].id, functions[1].id);
        // An enum lists its values; it is an enum even without any.
        let types = format!("{shop}{SEP}types");
        let ty = |name: &str| tree.node(&format!("{types}{SEP}type:{name}")).unwrap();
        let Children::Loaded(values) = &ty("mood").children else {
            panic!("an enum's values come with it");
        };
        assert_eq!(
            values.iter().map(|node| &node.kind).collect::<Vec<_>>(),
            [
                &NodeKind::EnumValue {
                    ty: TableRef::new("shop", "mood"),
                    value: "sad".into()
                },
                &NodeKind::EnumValue {
                    ty: TableRef::new("shop", "mood"),
                    value: "ok".into()
                },
            ]
        );
        assert!(ty("mood").is_enum() && ty("nothing_yet").is_enum());
        assert!(!ty("address").is_enum());
        assert!(!values[0].is_enum());
    }

    #[test]
    fn mysql_databases_hold_the_objects_directly() {
        let mut tree = Tree::new(false);
        let load = tree
            .set_databases(vec![database("kerno", false)], Some("kerno"))
            .unwrap();
        assert_eq!(
            load.what,
            LoadKind::Objects {
                schema: "kerno".into()
            }
        );
        tree.finish("db:kerno", Ok(Loaded::Objects(Box::new(objects()))));
        assert_eq!(
            outline(&tree.items()),
            [
                "v kerno",
                "  > Tables",
                "  > Views",
                "  > Routines",
                "  Triggers",
                "  Events"
            ]
        );
        let routines = tree.node(&format!("db:kerno{SEP}routines")).unwrap();
        assert_eq!(routines.hint.as_deref(), Some("3"));
    }

    #[test]
    fn a_table_loads_its_columns_and_indexes_once() {
        let (mut tree, shop) = loaded();
        let orders = format!("{shop}{SEP}tables{SEP}rel:orders");
        tree.expand(&format!("{shop}{SEP}tables"));
        let load = tree.expand(&orders).expect("a table loads its structure");
        assert_eq!(
            load.what,
            LoadKind::Structure {
                table: TableRef::new("shop", "orders")
            }
        );
        // Until the answer arrives the table can be opened and says so.
        assert!(outline(&tree.items()).contains(&"        Loading…".to_string()));
        assert_eq!(tree.expand(&orders), None, "one load at a time");

        tree.finish(
            &orders,
            Ok(Loaded::Structure(Box::new(TableStructure {
                columns: vec![
                    column("id", true, false),
                    column("customer_id", false, false),
                    column("note", false, true),
                ],
                indexes: vec![IndexInfo {
                    name: "orders_pkey".into(),
                    columns: vec!["id".into()],
                    unique: true,
                    primary: true,
                    backs_constraint: true,
                    method: "btree".into(),
                    predicate: None,
                    definition: None,
                }],
                ..TableStructure::default()
            }))),
        );
        tree.expand(&format!("{orders}{SEP}columns"));
        let hints: Vec<_> = ["col:id", "col:customer_id", "col:note"]
            .iter()
            .map(|column| {
                tree.node(&format!("{orders}{SEP}columns{SEP}{column}"))
                    .unwrap()
                    .hint
                    .clone()
                    .unwrap()
            })
            .collect();
        assert_eq!(hints, ["integer · key", "integer · not null", "integer"]);
        let index = tree
            .node(&format!("{orders}{SEP}indexes{SEP}index:orders_pkey"))
            .unwrap();
        assert_eq!(index.hint.as_deref(), Some("id · primary"));

        // Closing and opening again asks the server nothing.
        tree.collapse(&orders);
        assert_eq!(tree.expand(&orders), None);
    }

    #[test]
    fn a_failed_load_says_why_and_is_tried_again() {
        let (mut tree, shop) = loaded();
        let orders = format!("{shop}{SEP}tables{SEP}rel:orders");
        tree.expand(&format!("{shop}{SEP}tables"));
        tree.expand(&orders).unwrap();
        tree.finish(&orders, Err("permission denied for table orders".into()));
        assert!(
            outline(&tree.items())
                .contains(&"        permission denied for table orders".to_string())
        );
        tree.collapse(&orders);
        assert!(tree.expand(&orders).is_some(), "opening it again retries");
    }

    #[test]
    fn refresh_reloads_the_nearest_node_that_owns_its_children() {
        let (mut tree, shop) = loaded();
        let orders = format!("{shop}{SEP}tables{SEP}rel:orders");
        // The "Tables" folder came with the schema: refreshing it reloads the schema.
        assert_eq!(
            tree.refresh_target(&format!("{shop}{SEP}tables"))
                .as_deref(),
            Some(shop.as_str())
        );
        // A table owns its columns.
        assert_eq!(
            tree.refresh_target(&orders).as_deref(),
            Some(orders.as_str())
        );
        // Folders that are open stay open across a refresh.
        let tables = format!("{shop}{SEP}tables");
        tree.expand(&tables);
        let load = tree.refresh(&shop).unwrap();
        assert_eq!(
            load.what,
            LoadKind::Objects {
                schema: "shop".into()
            }
        );
        assert_eq!(tree.refresh(&shop), None, "already loading");
        tree.finish(&shop, Ok(Loaded::Objects(Box::new(objects()))));
        assert!(tree.node(&tables).unwrap().expanded);
        assert!(!tree.node(&format!("{shop}{SEP}views")).unwrap().expanded);

        tree.refresh(&shop).unwrap();
        tree.finish(&shop, Ok(Loaded::Objects(Box::default())));
        assert!(tree.node(&shop).unwrap().expanded);
        assert_eq!(
            tree.node(&format!("{shop}{SEP}tables"))
                .unwrap()
                .hint
                .as_deref(),
            Some("0")
        );
    }

    #[test]
    fn the_filter_keeps_what_matches_and_the_way_to_it() {
        let (mut tree, _) = loaded();
        tree.set_filter("  ORDER ");
        assert_eq!(
            outline(&tree.items()),
            [
                "v kerno",
                "  v shop",
                "    v Tables",
                "      > orders",
                "      > order_items",
                "    v Views",
                "      > recent_orders",
            ]
        );
        // A folder's own name is not a match; a schema's is, and keeps all it holds.
        tree.set_filter("tables");
        assert_eq!(tree.items(), Vec::new());
        tree.set_filter("shop");
        assert_eq!(
            outline(&tree.items())[..3],
            ["v kerno", "  v shop", "    > Tables"]
        );
        tree.set_filter("");
        assert_eq!(outline(&tree.items())[0], "v kerno");
    }

    #[test]
    fn names_that_start_alike_are_different_nodes() {
        let mut tree = Tree::new(true);
        tree.set_databases(
            vec![database("app", false), database("app_test", false)],
            None,
        );
        assert_eq!(tree.node("db:app_test").unwrap().label, "app_test");
        assert_eq!(tree.node("db:app").unwrap().label, "app");
        assert_eq!(tree.node("db:ap"), None);
        // Nothing was current, so nothing is open or loading.
        assert_eq!(outline(&tree.items()), ["> app", "> app_test"]);
    }

    #[test]
    fn notes_are_told_apart_from_nodes() {
        let (tree, shop) = loaded();
        let items = tree.items();
        let template = items.iter().find(|item| item.label == "template1").unwrap();
        let note = &template.children[0];
        assert!(is_note(&note.id) && !is_note(&shop));
        assert_eq!(parent_of(&note.id), Some("db:template1"));
    }

    #[test]
    fn counts_and_sizes_are_compact() {
        let counts = [
            (0, "0"),
            (999, "999"),
            (1_000, "1.0k"),
            (12_345, "12k"),
            (999_499, "999k"),
            (999_999, "1.0M"),
            (1_000_000, "1.0M"),
            (2_500_000_000, "2.5B"),
        ];
        for (count, text) in counts {
            assert_eq!(compact_count(count), text);
        }
        let sizes = [
            (0, "0 B"),
            (1023, "1023 B"),
            (8192, "8.0 kB"),
            (190 * 1024 * 1024, "190 MB"),
            (3 * 1024 * 1024 * 1024 / 2, "1.5 GB"),
        ];
        for (bytes, text) in sizes {
            assert_eq!(compact_bytes(bytes), text);
        }
    }
}

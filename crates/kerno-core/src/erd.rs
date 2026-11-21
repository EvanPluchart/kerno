//! Entity-relationship diagrams (CLAUDE.md §4 J18; DBeaver #36058,
//! Beekeeper #492, #1160): the tables of a schema, or a table and its
//! neighbours, with their columns and keys, read from
//! [`Session::table_structure`]; a layered layout that never overlaps two
//! tables; orthogonal edges between the rows of the key columns; SVG and
//! Mermaid exports. Read-only: nothing here writes to the database.
//!
//! The geometry is computed here once, from an estimate of the text widths,
//! so the app's canvas, the SVG and the PNG drawn from it show the same
//! boxes with the same (truncated) text.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::StreamExt as _;

use crate::driver::Session;
use crate::error::{Error, Result};
use crate::schema::{TableRef, TableStructure};

/// What a diagram shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every table of a schema (a database on MySQL).
    Schema(String),
    /// A table, the tables it references and the tables that reference it.
    Around(TableRef),
}

impl Scope {
    /// The schema the diagram is drawn for: its tables are named without it.
    pub fn schema(&self) -> &str {
        match self {
            Scope::Schema(schema) => schema,
            Scope::Around(table) => &table.schema,
        }
    }
}

/// A column as the diagram shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeColumn {
    pub name: String,
    pub data_type: String,
    pub primary_key: bool,
    /// Part of a foreign key of its table.
    pub foreign_key: bool,
    pub nullable: bool,
}

/// A table of the diagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub table: TableRef,
    pub columns: Vec<NodeColumn>,
}

/// A foreign key: from the referencing table's columns to the referenced
/// table's. One edge per key, whatever its number of columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub name: String,
    /// The referencing table (index in [`Diagram::nodes`]).
    pub from: usize,
    pub from_columns: Vec<String>,
    /// The referenced table.
    pub to: usize,
    pub to_columns: Vec<String>,
    /// A referencing column is nullable: a row may reference nothing.
    pub optional: bool,
}

/// Tables and the foreign keys between them, sorted by table: the same
/// catalog always gives the same diagram.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diagram {
    /// The schema whose tables are named without it.
    pub schema: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Diagram {
    /// The diagram of these tables; a foreign key to a table that is not
    /// among them is left out.
    pub fn new(schema: impl Into<String>, mut tables: Vec<(TableRef, TableStructure)>) -> Self {
        tables.sort_by(|a, b| a.0.cmp(&b.0));
        tables.dedup_by(|a, b| a.0 == b.0);
        let nodes: Vec<Node> = tables
            .iter()
            .map(|(table, structure)| {
                let in_key: BTreeSet<&str> = structure
                    .foreign_keys
                    .iter()
                    .flat_map(|key| key.columns.iter().map(String::as_str))
                    .collect();
                Node {
                    table: table.clone(),
                    columns: structure
                        .columns
                        .iter()
                        .map(|column| NodeColumn {
                            name: column.name.clone(),
                            data_type: column.data_type.clone(),
                            primary_key: column.primary_key_position.is_some(),
                            foreign_key: in_key.contains(column.name.as_str()),
                            nullable: column.nullable,
                        })
                        .collect(),
                }
            })
            .collect();
        let index = |table: &TableRef| nodes.binary_search_by(|node| node.table.cmp(table)).ok();
        let mut edges = Vec::new();
        for (from, (_, structure)) in tables.iter().enumerate() {
            let mut keys: Vec<_> = structure.foreign_keys.iter().collect();
            keys.sort_by(|a, b| a.name.cmp(&b.name));
            for key in keys {
                let Some(to) = index(&key.referenced_table) else {
                    continue;
                };
                let optional = key.columns.iter().any(|name| {
                    nodes[from]
                        .columns
                        .iter()
                        .any(|column| &column.name == name && column.nullable)
                });
                edges.push(Edge {
                    name: key.name.clone(),
                    from,
                    from_columns: key.columns.clone(),
                    to,
                    to_columns: key.referenced_columns.clone(),
                    optional,
                });
            }
        }
        Self {
            schema: schema.into(),
            nodes,
            edges,
        }
    }

    /// What the header of a node says: the table's name, with its schema
    /// when it is not the diagram's.
    pub fn title(&self, node: usize) -> String {
        let table = &self.nodes[node].table;
        let name = if table.schema == self.schema {
            table.name.clone()
        } else {
            format!("{}.{}", table.schema, table.name)
        };
        shorten(&name, HEADER_CHARS)
    }
}

/// The tables a table is linked to by its foreign keys, both ways, itself
/// left out: the neighbours of [`Scope::Around`].
pub fn neighbours(table: &TableRef, structure: &TableStructure) -> Vec<TableRef> {
    let linked: BTreeSet<TableRef> = structure
        .foreign_keys
        .iter()
        .map(|key| key.referenced_table.clone())
        .chain(structure.referenced_by.iter().map(|key| key.table.clone()))
        .filter(|other| other != table)
        .collect();
    linked.into_iter().collect()
}

/// How far a load is: tables read and to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

/// A diagram read from the server, with the tables that could not be read
/// (dropped meanwhile, no privilege) and why.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub diagram: Diagram,
    pub skipped: Vec<(TableRef, String)>,
}

/// Tables whose structure is read at once.
const CONCURRENT: usize = 8;

/// Runs `read` on each of `items`, [`CONCURRENT`] at a time, and hands the
/// results to `each` in the items' order. Stops with [`Error::Cancelled`]
/// once `cancel` is set, or at the first error `each` returns. Shared by the
/// diagram and the schema compare ([`crate::compare::load`]).
pub(crate) async fn buffered<T, R, F>(
    items: Vec<T>,
    read: impl FnMut(T) -> F,
    cancel: &AtomicBool,
    mut each: impl FnMut(R) -> Result<()>,
) -> Result<()>
where
    F: std::future::Future<Output = R>,
{
    let mut reading = futures_util::stream::iter(items.into_iter().map(read)).buffered(CONCURRENT);
    while let Some(result) = reading.next().await {
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        each(result)?;
    }
    Ok(())
}

/// Reads the diagram of `scope`: the list of tables, then their structures,
/// [`CONCURRENT`] at a time, telling `progress` after each. Stops with
/// [`Error::Cancelled`] once `cancel` is set. A table that cannot be read
/// is skipped and listed, unless none can.
pub async fn load(
    session: Arc<dyn Session>,
    scope: &Scope,
    progress: impl Fn(Progress) + Send,
    cancel: &AtomicBool,
) -> Result<Loaded> {
    let mut read = Vec::new();
    let tables = match scope {
        Scope::Schema(schema) => {
            let objects = session.objects(schema).await?;
            objects
                .relations
                .into_iter()
                .filter(|relation| relation.kind.is_table() && !relation.is_partition)
                .map(|relation| TableRef::new(schema.clone(), relation.name))
                .collect::<Vec<_>>()
        }
        Scope::Around(table) => {
            progress(Progress { done: 0, total: 1 });
            let structure = session.table_structure(table).await?;
            let others = neighbours(table, &structure);
            read.push((table.clone(), structure));
            others
        }
    };
    let total = read.len() + tables.len();
    progress(Progress {
        done: read.len(),
        total,
    });
    let mut skipped = Vec::new();
    let mut first_error = None;
    let reading = |table: TableRef| {
        let session = session.clone();
        async move {
            let structure = session.table_structure(&table).await;
            (table, structure)
        }
    };
    buffered(tables, reading, cancel, |(table, structure)| {
        match structure {
            Ok(structure) => read.push((table, structure)),
            Err(error) => {
                skipped.push((table, error.to_string()));
                first_error.get_or_insert(error);
            }
        }
        progress(Progress {
            done: read.len() + skipped.len(),
            total,
        });
        Ok(())
    })
    .await?;
    if read.is_empty()
        && let Some(error) = first_error
    {
        return Err(error);
    }
    Ok(Loaded {
        diagram: Diagram::new(scope.schema(), read),
        skipped,
    })
}

// Geometry --------------------------------------------------------------------

/// Height of a table's header.
pub const HEADER_HEIGHT: f32 = 30.;
/// Height of a column's row.
pub const ROW_HEIGHT: f32 = 22.;
/// Space under the last row.
const BOTTOM: f32 = 6.;
/// Left and right padding inside a table.
pub const PADDING: f32 = 10.;
/// Width of the key marks (`PK`, `FK`) before a column's name.
pub const MARK_WIDTH: f32 = 34.;
/// Space between a column's name and its type.
const NAME_GAP: f32 = 16.;
/// Estimated advance of a character of the header (13 px semibold), a
/// column's name (12 px) and its type (11 px monospace): generous, so text
/// fits.
const HEADER_ADVANCE: f32 = 7.8;
const NAME_ADVANCE: f32 = 7.;
const TYPE_ADVANCE: f32 = 6.7;
/// Longer text is cut with an ellipsis.
const HEADER_CHARS: usize = 36;
const NAME_CHARS: usize = 28;
const TYPE_CHARS: usize = 20;
const MIN_WIDTH: f32 = 160.;
/// Space between two columns of tables, where the edges run.
const LAYER_GAP: f32 = 96.;
/// Space between two tables of a column.
const STACK_GAP: f32 = 36.;
/// Tables per column of the layout; a longer layer wraps.
const WRAP: usize = 10;
/// How far an edge leaves a table before it turns.
const STUB: f32 = 18.;

/// `text` cut to `limit` characters, the last one an ellipsis.
fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut short: String = text.chars().take(limit - 1).collect();
    short.push('…');
    short
}

/// A column's row as drawn: key mark, name and type, cut to fit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `PK`, `FK`, `PK FK` or empty: keys are named, not only coloured.
    pub mark: &'static str,
    pub name: String,
    pub data_type: String,
}

/// The rows of a node, as drawn.
pub fn rows(node: &Node) -> Vec<Row> {
    node.columns
        .iter()
        .map(|column| Row {
            mark: match (column.primary_key, column.foreign_key) {
                (true, true) => "PK FK",
                (true, false) => "PK",
                (false, true) => "FK",
                (false, false) => "",
            },
            name: shorten(&column.name, NAME_CHARS),
            data_type: shorten(&column.data_type, TYPE_CHARS),
        })
        .collect()
}

/// A rectangle in diagram units (pixels at 100 %).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.right() && y >= self.y && y <= self.bottom()
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    /// The smallest rectangle around both.
    pub fn union(&self, other: &Rect) -> Rect {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Rect {
            x,
            y,
            w: self.right().max(other.right()) - x,
            h: self.bottom().max(other.bottom()) - y,
        }
    }
}

/// The box of each node, in [`Diagram::nodes`] order.
pub type Layout = Vec<Rect>;

/// Width and height of a node's box.
pub fn size(diagram: &Diagram, node: usize) -> (f32, f32) {
    let header = diagram.title(node).chars().count() as f32 * HEADER_ADVANCE;
    let widest = rows(&diagram.nodes[node])
        .iter()
        .map(|row| {
            MARK_WIDTH
                + row.name.chars().count() as f32 * NAME_ADVANCE
                + NAME_GAP
                + row.data_type.chars().count() as f32 * TYPE_ADVANCE
        })
        .fold(header, f32::max);
    let rows = diagram.nodes[node].columns.len() as f32;
    (
        (widest + 2. * PADDING).max(MIN_WIDTH).ceil(),
        HEADER_HEIGHT + rows * ROW_HEIGHT + BOTTOM,
    )
}

/// Places the tables: those linked by foreign keys in columns ranked by
/// key depth (a referenced table left of the tables that reference it),
/// ordered inside a column by the barycentre of their neighbours; tables
/// without a link in a grid under them. Deterministic, and no two boxes
/// overlap.
pub fn layout(diagram: &Diagram) -> Layout {
    let count = diagram.nodes.len();
    let sizes: Vec<(f32, f32)> = (0..count).map(|node| size(diagram, node)).collect();
    let mut parents = vec![Vec::new(); count];
    let mut linked = vec![Vec::new(); count];
    for edge in diagram.edges.iter().filter(|edge| edge.from != edge.to) {
        parents[edge.from].push(edge.to);
        linked[edge.from].push(edge.to);
        linked[edge.to].push(edge.from);
    }

    // Rank: 0 for a table that references none; else one more than the
    // tables it references. A cycle is cut where the walk meets it again.
    let mut rank: Vec<Option<usize>> = vec![None; count];
    let mut on_path = vec![false; count];
    for node in 0..count {
        rank_of(node, &parents, &mut rank, &mut on_path);
    }
    let layers_count = (0..count)
        .filter(|node| !linked[*node].is_empty())
        .filter_map(|node| rank[node])
        .max()
        .map_or(0, |top| top + 1);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); layers_count];
    for node in (0..count).filter(|node| !linked[*node].is_empty()) {
        layers[rank[node].unwrap_or(0)].push(node);
    }

    // Order inside a layer by the mean position of the neighbours in the
    // layer before it, then in the layer after it, a few times over.
    let mut position = vec![0usize; count];
    let place = |layers: &[Vec<usize>], position: &mut [usize]| {
        for layer in layers {
            for (index, node) in layer.iter().enumerate() {
                position[*node] = index;
            }
        }
    };
    place(&layers, &mut position);
    for _ in 0..4 {
        for (current, reference) in (1..layers_count).map(|layer| (layer, layer - 1)).chain(
            (0..layers_count.saturating_sub(1))
                .rev()
                .map(|layer| (layer, layer + 1)),
        ) {
            let others: BTreeSet<usize> = layers[reference].iter().copied().collect();
            let key = |node: usize| {
                let near: Vec<f32> = linked[node]
                    .iter()
                    .filter(|other| others.contains(other))
                    .map(|other| position[*other] as f32)
                    .collect();
                if near.is_empty() {
                    position[node] as f32
                } else {
                    near.iter().sum::<f32>() / near.len() as f32
                }
            };
            let mut keyed: Vec<(f32, usize)> = layers[current]
                .iter()
                .map(|node| (key(*node), *node))
                .collect();
            // Stable: equal keys keep their order.
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
            layers[current] = keyed.into_iter().map(|(_, node)| node).collect();
            for (index, node) in layers[current].iter().enumerate() {
                position[*node] = index;
            }
        }
    }

    // Columns: each layer, wrapped every WRAP tables, side by side; each
    // column centred on the tallest.
    let columns: Vec<&[usize]> = layers.iter().flat_map(|layer| layer.chunks(WRAP)).collect();
    let height = |column: &[usize]| {
        column.iter().map(|node| sizes[*node].1).sum::<f32>()
            + column.len().saturating_sub(1) as f32 * STACK_GAP
    };
    let tallest = columns
        .iter()
        .map(|column| height(column))
        .fold(0., f32::max);
    let mut boxes = vec![Rect::default(); count];
    let mut x = 0.;
    for column in &columns {
        let mut y = ((tallest - height(column)) / 2.).floor();
        let width = column.iter().map(|node| sizes[*node].0).fold(0., f32::max);
        for node in column.iter() {
            let (w, h) = sizes[*node];
            boxes[*node] = Rect { x, y, w, h };
            y += h + STACK_GAP;
        }
        x += width + LAYER_GAP;
    }

    // The tables without a link, in a grid under the rest.
    let alone: Vec<usize> = (0..count).filter(|node| linked[*node].is_empty()).collect();
    if !alone.is_empty() {
        let top = if columns.is_empty() {
            0.
        } else {
            tallest + 2. * STACK_GAP
        };
        let per_row = (alone.len() as f32).sqrt().ceil() as usize;
        let widths: Vec<f32> = (0..per_row)
            .map(|column| {
                alone
                    .iter()
                    .skip(column)
                    .step_by(per_row)
                    .map(|node| sizes[*node].0)
                    .fold(0., f32::max)
            })
            .collect();
        let mut y = top;
        for row in alone.chunks(per_row) {
            let mut x = 0.;
            for (column, node) in row.iter().enumerate() {
                let (w, h) = sizes[*node];
                boxes[*node] = Rect { x, y, w, h };
                x += widths[column] + STACK_GAP;
            }
            y += row.iter().map(|node| sizes[*node].1).fold(0., f32::max) + STACK_GAP;
        }
    }
    boxes
}

/// The rank of `node` (see [`layout`]), memoised in `rank`.
fn rank_of(
    node: usize,
    parents: &[Vec<usize>],
    rank: &mut [Option<usize>],
    on_path: &mut [bool],
) -> usize {
    if let Some(known) = rank[node] {
        return known;
    }
    if on_path[node] {
        return 0;
    }
    on_path[node] = true;
    let mut own = 0;
    for parent in parents[node].clone() {
        if on_path[parent] {
            continue;
        }
        own = own.max(rank_of(parent, parents, rank, on_path) + 1);
    }
    on_path[node] = false;
    rank[node] = Some(own);
    own
}

/// The middle height of the row of `column` in a node (of its header when
/// the column is not shown).
fn row_middle(node: &Node, rect: &Rect, column: Option<&String>) -> f32 {
    match column.and_then(|name| node.columns.iter().position(|column| &column.name == name)) {
        Some(row) => rect.y + HEADER_HEIGHT + (row as f32 + 0.5) * ROW_HEIGHT,
        None => rect.y + HEADER_HEIGHT / 2.,
    }
}

/// The polyline of an edge, from the referencing table's first key column
/// to the referenced table's first key column: horizontal and vertical
/// segments only, never across another table. Between two tables side by
/// side it turns in the gap between them; a table that references itself,
/// or two tables of one column, are joined round their right side. When
/// that way would cross a table (a key that skips a layer), the edge leaves
/// by the left side, rises in the gap left of its table's column, runs
/// above every table and comes down the gap left of the referenced table:
/// columns are left-aligned, so these gaps are free. `edge` picks a lane,
/// so edges that would share a segment run side by side.
pub fn route(diagram: &Diagram, layout: &[Rect], edge: usize) -> Vec<(f32, f32)> {
    let edge_ref = &diagram.edges[edge];
    let (from, to) = (layout[edge_ref.from], layout[edge_ref.to]);
    let y1 = row_middle(
        &diagram.nodes[edge_ref.from],
        &from,
        edge_ref.from_columns.first(),
    );
    let mut y2 = row_middle(
        &diagram.nodes[edge_ref.to],
        &to,
        edge_ref.to_columns.first(),
    );
    let lane = (edge % 5) as f32 * 6.;
    let direct = if from.right() + 2. * STUB <= to.x {
        let middle = ((from.right() + to.x) / 2. + lane - 12.).round();
        vec![(from.right(), y1), (middle, y1), (middle, y2), (to.x, y2)]
    } else if to.right() + 2. * STUB <= from.x {
        let middle = ((to.right() + from.x) / 2. + lane - 12.).round();
        vec![(from.x, y1), (middle, y1), (middle, y2), (to.right(), y2)]
    } else {
        if edge_ref.from == edge_ref.to && (y2 - y1).abs() < 1. {
            y2 += ROW_HEIGHT / 3.;
        }
        let outside = from.right().max(to.right()) + STUB + lane;
        vec![
            (from.right(), y1),
            (outside, y1),
            (outside, y2),
            (to.right(), y2),
        ]
    };
    let others: Vec<&Rect> = layout
        .iter()
        .enumerate()
        .filter(|(node, _)| *node != edge_ref.from && *node != edge_ref.to)
        .map(|(_, rect)| rect)
        .collect();
    if !crosses(&direct, &others) {
        return direct;
    }
    let top = layout.iter().map(|rect| rect.y).fold(f32::MAX, f32::min) - STUB - lane;
    let (out, into) = (from.x - STUB - lane, to.x - STUB - lane);
    vec![
        (from.x, y1),
        (out, y1),
        (out, top),
        (into, top),
        (into, y2),
        (to.x, y2),
    ]
}

/// Whether a polyline of horizontal and vertical segments goes through one
/// of `boxes` (touching a border does not count).
fn crosses(points: &[(f32, f32)], boxes: &[&Rect]) -> bool {
    points.windows(2).any(|pair| {
        let (low_x, high_x) = (pair[0].0.min(pair[1].0), pair[0].0.max(pair[1].0));
        let (low_y, high_y) = (pair[0].1.min(pair[1].1), pair[0].1.max(pair[1].1));
        boxes.iter().any(|rect| {
            low_x < rect.right() - 0.5
                && high_x > rect.x + 0.5
                && low_y < rect.bottom() - 0.5
                && high_y > rect.y + 0.5
        })
    })
}

/// The space the tables and the edges take, with room for the edges' ends;
/// `None` when there is nothing.
pub fn extent(layout: &[Rect], routes: &[Vec<(f32, f32)>]) -> Option<Rect> {
    let points = routes.iter().flatten().map(|&(x, y)| Rect {
        x: x - 10.,
        y: y - 6.,
        w: 20.,
        h: 12.,
    });
    layout
        .iter()
        .copied()
        .chain(points)
        .reduce(|all, rect| all.union(&rect))
}

/// The ends of an edge: a crow's foot where it leaves the referencing
/// table ("many"), a bar where it reaches the referenced one ("one"); as
/// line segments.
pub fn ends(points: &[(f32, f32)]) -> Vec<[(f32, f32); 2]> {
    let (Some(&start), Some(&next), Some(&end), Some(&before)) = (
        points.first(),
        points.get(1),
        points.last(),
        points
            .len()
            .checked_sub(2)
            .and_then(|index| points.get(index)),
    ) else {
        return Vec::new();
    };
    let out = if next.0 >= start.0 { 1. } else { -1. };
    let foot = (start.0 + out * 10., start.1);
    let back = if before.0 >= end.0 { 1. } else { -1. };
    let bar = end.0 + back * 8.;
    vec![
        [foot, (start.0, start.1 - 5.)],
        [foot, (start.0, start.1 + 5.)],
        [(bar, end.1 - 5.), (bar, end.1 + 5.)],
    ]
}

// Exports ---------------------------------------------------------------------

/// The colours of an exported diagram, as `#rrggbb`: the app gives its
/// theme's, light or dark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub background: String,
    /// A table's body.
    pub surface: String,
    /// A table's header.
    pub header: String,
    pub border: String,
    pub text: String,
    /// Types and key marks.
    pub muted: String,
    pub edge: String,
}

/// Space around the tables in an export.
const MARGIN: f32 = 24.;

fn xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// The diagram as an SVG document: the boxes where `layout` puts them,
/// the edges as orthogonal paths with their ends, in `palette`'s colours,
/// the text in Inter and JetBrains Mono.
pub fn to_svg(diagram: &Diagram, layout: &[Rect], palette: &Palette) -> String {
    let routes: Vec<_> = (0..diagram.edges.len())
        .map(|edge| route(diagram, layout, edge))
        .collect();
    let area = extent(layout, &routes).unwrap_or_default();
    let (dx, dy) = (MARGIN - area.x, MARGIN - area.y);
    let (width, height) = ((area.w + 2. * MARGIN).ceil(), (area.h + 2. * MARGIN).ceil());
    let mut svg = String::new();
    let _ = writeln!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" font-family="Inter, sans-serif">"#
    );
    let _ = writeln!(
        svg,
        r#"<rect width="{width}" height="{height}" fill="{}"/>"#,
        palette.background
    );
    let _ = writeln!(
        svg,
        r#"<g fill="none" stroke="{}" stroke-width="1.25">"#,
        palette.edge
    );
    for (edge, points) in routes.iter().enumerate() {
        let mut path = String::new();
        for (index, (x, y)) in points.iter().enumerate() {
            let _ = write!(
                path,
                "{}{} {} ",
                if index == 0 { "M" } else { "L" },
                x + dx,
                y + dy
            );
        }
        for [(x1, y1), (x2, y2)] in ends(points) {
            let _ = write!(path, "M{} {} L{} {} ", x1 + dx, y1 + dy, x2 + dx, y2 + dy);
        }
        let _ = writeln!(
            svg,
            r#"<path d="{}"><title>{}</title></path>"#,
            path.trim_end(),
            xml(&diagram.edges[edge].name)
        );
    }
    svg.push_str("</g>\n");
    for (index, (node, rect)) in diagram.nodes.iter().zip(layout).enumerate() {
        let (x, y, w, h) = (rect.x + dx, rect.y + dy, rect.w, rect.h);
        let _ = writeln!(svg, "<g>");
        let _ = writeln!(
            svg,
            r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="6" fill="{}"/>"#,
            palette.surface
        );
        let _ = writeln!(
            svg,
            r#"<path d="M{x} {bottom} V{top} Q{x} {y} {left} {y} H{right} Q{edge} {y} {edge} {top} V{bottom} Z" fill="{}"/>"#,
            palette.header,
            bottom = y + HEADER_HEIGHT,
            top = y + 6.,
            left = x + 6.,
            right = x + w - 6.,
            edge = x + w,
        );
        let _ = writeln!(
            svg,
            r#"<line x1="{x}" y1="{line}" x2="{}" y2="{line}" stroke="{}"/>"#,
            x + w,
            palette.border,
            line = y + HEADER_HEIGHT,
        );
        let _ = writeln!(
            svg,
            r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="6" fill="none" stroke="{}"/>"#,
            palette.border
        );
        let _ = writeln!(
            svg,
            r#"<text x="{}" y="{}" font-size="13" font-weight="600" fill="{}">{}</text>"#,
            x + PADDING,
            y + HEADER_HEIGHT / 2. + 4.5,
            palette.text,
            xml(&diagram.title(index))
        );
        for (row, line) in rows(node).iter().enumerate() {
            let base = y + HEADER_HEIGHT + row as f32 * ROW_HEIGHT + ROW_HEIGHT / 2. + 4.;
            if !line.mark.is_empty() {
                let _ = writeln!(
                    svg,
                    r#"<text x="{}" y="{base}" font-size="9" font-weight="600" fill="{}">{}</text>"#,
                    x + PADDING,
                    palette.muted,
                    line.mark
                );
            }
            let _ = writeln!(
                svg,
                r#"<text x="{}" y="{base}" font-size="12" fill="{}">{}</text>"#,
                x + PADDING + MARK_WIDTH,
                palette.text,
                xml(&line.name)
            );
            let _ = writeln!(
                svg,
                r#"<text x="{}" y="{base}" font-size="11" font-family="JetBrains Mono, monospace" text-anchor="end" fill="{}">{}</text>"#,
                x + w - PADDING,
                palette.muted,
                xml(&line.data_type)
            );
        }
        svg.push_str("</g>\n");
    }
    svg.push_str("</svg>\n");
    svg
}

/// A word Mermaid takes as an attribute's type or name: a letter first,
/// then letters, digits, `-`, `_`, `(`, `)`, `[`, `]`; anything else
/// becomes `_`.
fn mermaid_word(text: &str, prefix: &str) -> String {
    let mut word: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_()[]".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !word.starts_with(|c: char| c.is_ascii_alphabetic()) {
        word.insert_str(0, prefix);
    }
    word
}

/// Text Mermaid takes between double quotes: on one line, without a double
/// quote (a line break would end the statement and start another, K31).
fn mermaid_quoted(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '"' => '\'',
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') => ' ',
            c => c,
        })
        .collect()
}

/// The diagram as a Mermaid `erDiagram`: one entity per table (named
/// `schema.table`, quoted), its columns with `PK` / `FK`, and one
/// relationship per foreign key, `|o` on the referenced side when the key
/// may be NULL.
pub fn to_mermaid(diagram: &Diagram) -> String {
    let quoted = |node: usize| {
        let table = &diagram.nodes[node].table;
        format!(
            "\"{}\"",
            mermaid_quoted(&format!("{}.{}", table.schema, table.name))
        )
    };
    let mut text = String::from("erDiagram\n");
    for (index, node) in diagram.nodes.iter().enumerate() {
        let _ = writeln!(text, "    {} {{", quoted(index));
        for column in &node.columns {
            let name = mermaid_word(&column.name, "c_");
            let keys = match (column.primary_key, column.foreign_key) {
                (true, true) => " PK, FK",
                (true, false) => " PK",
                (false, true) => " FK",
                (false, false) => "",
            };
            let comment = if name == column.name {
                String::new()
            } else {
                format!(" \"{}\"", mermaid_quoted(&column.name))
            };
            let _ = writeln!(
                text,
                "        {} {name}{keys}{comment}",
                mermaid_word(&column.data_type, "t_")
            );
        }
        text.push_str("    }\n");
    }
    for edge in &diagram.edges {
        let _ = writeln!(
            text,
            "    {} {}--o{{ {} : \"{}\"",
            quoted(edge.to),
            if edge.optional { "|o" } else { "||" },
            quoted(edge.from),
            mermaid_quoted(&edge.name)
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::TypeKind;
    use crate::schema::{ColumnInfo, ForeignKeyInfo};

    fn column(name: &str, data_type: &str, pk: Option<u32>, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            kind: TypeKind::Text,
            nullable,
            default: None,
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: pk,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    fn key(
        name: &str,
        from: &str,
        columns: &[&str],
        to: &str,
        to_columns: &[&str],
    ) -> ForeignKeyInfo {
        ForeignKeyInfo {
            name: name.into(),
            table: TableRef::new("shop", from),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            referenced_table: TableRef::new("shop", to),
            referenced_columns: to_columns.iter().map(|c| c.to_string()).collect(),
            on_update: "NO ACTION".into(),
            on_delete: "NO ACTION".into(),
        }
    }

    fn table(
        name: &str,
        columns: Vec<ColumnInfo>,
        keys: Vec<ForeignKeyInfo>,
    ) -> (TableRef, TableStructure) {
        (
            TableRef::new("shop", name),
            TableStructure {
                columns,
                foreign_keys: keys,
                ..Default::default()
            },
        )
    }

    /// A small shop: a self-referencing category, a composite key, a key
    /// to a table outside the diagram, a table without links, and names
    /// that need escaping.
    fn shop() -> Diagram {
        Diagram::new("shop", shop_tables())
    }

    fn shop_tables() -> Vec<(TableRef, TableStructure)> {
        vec![
            table(
                "products",
                vec![
                    column("id", "bigint", Some(1), false),
                    column("category_id", "integer", None, false),
                    column("price", "numeric(10, 2)", None, false),
                ],
                vec![key(
                    "products_category_fk",
                    "products",
                    &["category_id"],
                    "categories",
                    &["id"],
                )],
            ),
            table(
                "categories",
                vec![
                    column("id", "integer", Some(1), false),
                    column("parent_id", "integer", None, true),
                    column("tags", "text[]", None, false),
                ],
                vec![key(
                    "categories_parent_fk",
                    "categories",
                    &["parent_id"],
                    "categories",
                    &["id"],
                )],
            ),
            table(
                "order_items",
                vec![
                    column("order_id", "bigint", Some(1), false),
                    column("line_no", "smallint", Some(2), false),
                    column("product_id", "bigint", None, false),
                ],
                vec![
                    key(
                        "order_items_product_fk",
                        "order_items",
                        &["product_id"],
                        "products",
                        &["id"],
                    ),
                    key(
                        "order_items_order_fk",
                        "order_items",
                        &["order_id"],
                        "orders",
                        &["id"],
                    ),
                ],
            ),
            table(
                "orders",
                vec![
                    column("id", "bigint", Some(1), false),
                    column("customer_id", "bigint", None, false),
                ],
                vec![key(
                    "orders_customer_fk",
                    "orders",
                    &["customer_id"],
                    "customers",
                    &["id"],
                )],
            ),
            table(
                "shipments",
                vec![
                    column("order_id", "bigint", None, false),
                    column("line_no", "smallint", None, false),
                    column("Note <&>", "character varying(40)", None, true),
                ],
                vec![key(
                    "shipments_line_fk",
                    "shipments",
                    &["order_id", "line_no"],
                    "order_items",
                    &["order_id", "line_no"],
                )],
            ),
            table(
                "audit_log",
                vec![column("actor", "text", None, false)],
                vec![],
            ),
        ]
    }

    /// The first edge that goes through a table other than its own two.
    fn crossing(diagram: &Diagram, layout: &[Rect]) -> Option<String> {
        diagram.edges.iter().enumerate().find_map(|(index, edge)| {
            let others: Vec<&Rect> = layout
                .iter()
                .enumerate()
                .filter(|(node, _)| *node != edge.from && *node != edge.to)
                .map(|(_, rect)| rect)
                .collect();
            crosses(&route(diagram, layout, index), &others).then(|| edge.name.clone())
        })
    }

    fn overlapping(layout: &[Rect]) -> Option<(usize, usize)> {
        (0..layout.len()).find_map(|a| {
            (a + 1..layout.len())
                .find(|b| layout[a].intersects(&layout[*b]))
                .map(|b| (a, b))
        })
    }

    #[test]
    fn edges_are_the_foreign_keys_between_the_tables_shown() {
        let diagram = shop();
        let names: Vec<_> = diagram
            .nodes
            .iter()
            .map(|node| node.table.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "audit_log",
                "categories",
                "order_items",
                "orders",
                "products",
                "shipments"
            ]
        );
        let edges: Vec<_> = diagram
            .edges
            .iter()
            .map(|edge| {
                (
                    edge.name.as_str(),
                    names[edge.from],
                    names[edge.to],
                    edge.from_columns.len(),
                    edge.optional,
                )
            })
            .collect();
        // `orders_customer_fk` leads out of the diagram: left out. The
        // composite key is one edge.
        assert_eq!(
            edges,
            [
                ("categories_parent_fk", "categories", "categories", 1, true),
                ("order_items_order_fk", "order_items", "orders", 1, false),
                (
                    "order_items_product_fk",
                    "order_items",
                    "products",
                    1,
                    false
                ),
                ("products_category_fk", "products", "categories", 1, false),
                ("shipments_line_fk", "shipments", "order_items", 2, false),
            ]
        );
        let items = &diagram.nodes[2].columns;
        assert!(items[0].primary_key && items[0].foreign_key);
        assert!(items[1].primary_key && !items[1].foreign_key);
    }

    #[test]
    fn the_layout_is_deterministic_ranked_and_without_overlap() {
        let diagram = shop();
        let layout = layout(&diagram);
        assert_eq!(layout, super::layout(&diagram));
        assert_eq!(overlapping(&layout), None);
        let x = |name: &str| {
            let node = diagram
                .nodes
                .iter()
                .position(|node| node.table.name == name)
                .unwrap();
            layout[node].x
        };
        // Referenced tables left of the tables that reference them.
        assert!(x("categories") < x("products"));
        assert!(x("products") < x("order_items"));
        assert!(x("order_items") < x("shipments"));
        assert!(x("orders") < x("order_items"));
        // The table without links is under the others.
        let alone = layout[0];
        assert!(layout[1..].iter().all(|rect| rect.bottom() < alone.y));
        // The order the server lists the tables in does not matter.
        let mut reversed = shop_tables();
        reversed.reverse();
        let again = Diagram::new("shop", reversed);
        assert_eq!(again, diagram);
        assert_eq!(super::layout(&again), layout);
    }

    #[test]
    fn edges_are_orthogonal_and_a_self_reference_loops_on_the_right() {
        let diagram = shop();
        let layout = layout(&diagram);
        assert_eq!(crossing(&diagram, &layout), None);
        for edge in 0..diagram.edges.len() {
            let points = route(&diagram, &layout, edge);
            for pair in points.windows(2) {
                assert!(
                    pair[0].0 == pair[1].0 || pair[0].1 == pair[1].1,
                    "{}: {points:?}",
                    diagram.edges[edge].name
                );
            }
        }
        // categories.parent_id → categories.id: out of the right side and
        // back, from the parent_id row to the id row.
        let rect = layout[1];
        let points = route(&diagram, &layout, 0);
        assert_eq!(
            points[0],
            (rect.right(), rect.y + HEADER_HEIGHT + 1.5 * ROW_HEIGHT)
        );
        assert_eq!(
            points[3],
            (rect.right(), rect.y + HEADER_HEIGHT + 0.5 * ROW_HEIGHT)
        );
        assert!(points[1].0 > rect.right());
        assert_eq!(ends(&points).len(), 3);
    }

    #[test]
    fn mermaid_names_every_table_key_and_relationship() {
        // Types and names Mermaid would refuse (`numeric(10, 2)`, spaces,
        // `<&>`) are rewritten; the real name stays as a comment. A key
        // that may be NULL is `|o` on the referenced side.
        let expected = r#"erDiagram
    "shop.audit_log" {
        text actor
    }
    "shop.categories" {
        integer id PK
        integer parent_id FK
        text[] tags
    }
    "shop.order_items" {
        bigint order_id PK, FK
        smallint line_no PK
        bigint product_id FK
    }
    "shop.orders" {
        bigint id PK
        bigint customer_id FK
    }
    "shop.products" {
        bigint id PK
        integer category_id FK
        numeric(10__2) price
    }
    "shop.shipments" {
        bigint order_id FK
        smallint line_no FK
        character_varying(40) Note____ "Note <&>"
    }
    "shop.categories" |o--o{ "shop.categories" : "categories_parent_fk"
    "shop.orders" ||--o{ "shop.order_items" : "order_items_order_fk"
    "shop.products" ||--o{ "shop.order_items" : "order_items_product_fk"
    "shop.categories" ||--o{ "shop.products" : "products_category_fk"
    "shop.order_items" ||--o{ "shop.shipments" : "shipments_line_fk"
"#;
        assert_eq!(to_mermaid(&shop()), expected);
    }

    fn light() -> Palette {
        Palette {
            background: "#ffffff".into(),
            surface: "#ffffff".into(),
            header: "#f6f7f8".into(),
            border: "#dde1e6".into(),
            text: "#16191d".into(),
            muted: "#5b6470".into(),
            edge: "#5b6470".into(),
        }
    }

    #[test]
    fn the_svg_draws_each_table_and_edge_and_escapes_names() {
        let diagram = shop();
        let layout = layout(&diagram);
        let svg = to_svg(&diagram, &layout, &light());
        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert_eq!(
            svg.matches("<path d=").count(),
            diagram.edges.len() + diagram.nodes.len()
        );
        assert!(svg.contains(">Note &lt;&amp;&gt;</text>"));
        assert!(!svg.contains("<&>"));
        assert!(svg.contains(">PK FK</text>"));
        assert!(svg.contains(r##"fill="#f6f7f8""##));
        // Every box lies inside the picture.
        let routes: Vec<_> = (0..diagram.edges.len())
            .map(|edge| route(&diagram, &layout, edge))
            .collect();
        let area = extent(&layout, &routes).unwrap();
        assert!(svg.contains(&format!("width=\"{}\"", (area.w + 2. * MARGIN).ceil())));
        // Dark: the same drawing in other colours.
        let dark = Palette {
            background: "#14171a".into(),
            ..light()
        };
        let other = to_svg(&diagram, &layout, &dark);
        assert_eq!(other.replace("#14171a", "#ffffff"), svg);
    }

    #[test]
    fn long_names_are_cut_and_other_schemas_named() {
        let long = "a".repeat(60);
        let diagram = Diagram::new(
            "shop",
            vec![(
                TableRef::new("analytics", &long),
                TableStructure {
                    columns: vec![column(&long, &long, None, true)],
                    ..Default::default()
                },
            )],
        );
        let title = diagram.title(0);
        assert!(title.starts_with("analytics.aaa") && title.ends_with('…'));
        assert_eq!(title.chars().count(), HEADER_CHARS);
        let row = &rows(&diagram.nodes[0])[0];
        assert_eq!(row.name.chars().count(), NAME_CHARS);
        assert_eq!(row.data_type.chars().count(), TYPE_CHARS);
        let (w, _) = size(&diagram, 0);
        assert!(w < 2. * PADDING + MARK_WIDTH + 40. * NAME_ADVANCE + 30. * TYPE_ADVANCE);
    }

    /// 200 tables, 400 foreign keys: a schema competitors' users say their
    /// client cannot draw (DBeaver #36058).
    fn generated(tables: usize, keys_per_table: usize) -> Diagram {
        let name = |index: usize| format!("t{index:03}");
        Diagram::new(
            "big",
            (0..tables)
                .map(|index| {
                    let mut columns = vec![column("id", "bigint", Some(1), false)];
                    let mut keys = Vec::new();
                    for k in 0..keys_per_table {
                        // Mostly towards lower tables, some cycles.
                        let other = (index * 7 + k * 13 + 1) % tables;
                        let column_name = format!("ref_{k}");
                        columns.push(column(&column_name, "bigint", None, k % 2 == 0));
                        keys.push(ForeignKeyInfo {
                            table: TableRef::new("big", name(index)),
                            referenced_table: TableRef::new("big", name(other)),
                            ..key(
                                &format!("fk_{index}_{k}"),
                                "x",
                                &[&column_name],
                                "x",
                                &["id"],
                            )
                        });
                    }
                    for extra in 0..(index % 9) {
                        columns.push(column(&format!("value_{extra}"), "text", None, true));
                    }
                    (
                        TableRef::new("big", name(index)),
                        TableStructure {
                            columns,
                            foreign_keys: keys,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn two_hundred_tables_lay_out_quickly_without_overlap() {
        let diagram = generated(200, 2);
        assert_eq!(diagram.edges.len(), 400);
        let started = std::time::Instant::now();
        let layout = layout(&diagram);
        let routes: Vec<_> = (0..diagram.edges.len())
            .map(|edge| route(&diagram, &layout, edge))
            .collect();
        let elapsed = started.elapsed();
        eprintln!("layout and routes of 200 tables / 400 edges: {elapsed:?}");
        assert_eq!(routes.len(), 400);
        assert_eq!(overlapping(&layout), None);
        assert_eq!(crossing(&diagram, &layout), None);
        assert_eq!(layout, super::layout(&diagram));
        // CLAUDE.md §4 J18 asks for < 200 ms; a debug build of the tests
        // is far slower than the release app, so this bound is the same.
        assert!(elapsed.as_millis() < 200, "{elapsed:?}");
    }

    #[test]
    fn the_svg_of_one_table() {
        let diagram = Diagram::new(
            "shop",
            vec![table(
                "tags",
                vec![column("id", "integer", Some(1), false)],
                vec![],
            )],
        );
        let expected = r##"<svg xmlns="http://www.w3.org/2000/svg" width="208" height="106" viewBox="0 0 208 106" font-family="Inter, sans-serif">
<rect width="208" height="106" fill="#ffffff"/>
<g fill="none" stroke="#5b6470" stroke-width="1.25">
</g>
<g>
<rect x="24" y="24" width="160" height="58" rx="6" fill="#ffffff"/>
<path d="M24 54 V30 Q24 24 30 24 H178 Q184 24 184 30 V54 Z" fill="#f6f7f8"/>
<line x1="24" y1="54" x2="184" y2="54" stroke="#dde1e6"/>
<rect x="24" y="24" width="160" height="58" rx="6" fill="none" stroke="#dde1e6"/>
<text x="34" y="43.5" font-size="13" font-weight="600" fill="#16191d">tags</text>
<text x="34" y="69" font-size="9" font-weight="600" fill="#5b6470">PK</text>
<text x="68" y="69" font-size="12" fill="#16191d">id</text>
<text x="174" y="69" font-size="11" font-family="JetBrains Mono, monospace" text-anchor="end" fill="#5b6470">integer</text>
</g>
</svg>
"##;
        assert_eq!(to_svg(&diagram, &layout(&diagram), &light()), expected);
    }
}

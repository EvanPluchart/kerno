//! K18 (the owner's request of 2026-11-18): under the value of a cell that
//! holds a foreign key, the row it points to, as a short record: the
//! columns a person reads first (a name, an email, a title…), or the ones
//! the user picked for that table, then every column on demand.
//!
//! The row is read with the statement of the cell menu's jump (E3: the same
//! filter on the referenced key, written by `browse::page_sql`), limited to
//! one row, on a session of the panel's own (D-091): never the connection's
//! meta session, whose keep-alive a read waiting on a lock would hold (K15),
//! nor the tab's, which may hold the user's transaction or run their
//! statement. It sees committed rows only. A read starts once the selection
//! has rested for [`REST`], is stopped (its session closed, so the server
//! stops it too) when the selection moves to another key, and after
//! [`READ_LIMIT`]. What was read is kept for the panel's grid: the same key
//! shown again costs nothing; a grid read again (Refresh, Apply, another
//! page, a re-run) starts afresh.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{StyledExt as _, h_flex};
use gpui_kit::{Entity, StatefulInteractiveElement as _};
use kerno_core::browse::{Page, page_sql};
use kerno_core::exec::{self, CollectedResult};
use kerno_core::schema::ColumnInfo;
use kerno_core::{Session, TxState, describe};

use super::*;
use crate::bridge;
use crate::connections::manager::Connections;
use crate::data::tab::{Related, follow_at, table_key};
use crate::query::grid::{GridAction, Mark};
use crate::tabs::Target;

/// How long the selection rests on a key before its row is read: moving
/// through the cells reads nothing.
pub(super) const REST: Duration = Duration::from_millis(250);
/// A read still running this long after it started says "Reading…": a fast
/// answer shows no flicker.
const LOADING_SHOWN: Duration = Duration::from_millis(300);
/// A read not answered in this time is stopped.
pub(super) const READ_LIMIT: Duration = Duration::from_secs(10);
/// The columns shown before "Show all columns", at most.
const SHOWN_AT_MOST: usize = 8;
/// Rows kept for a grid; past it the cache starts again.
// ponytail: cleared when full, an LRU if people walk through more keys.
const CACHE_AT_MOST: usize = 256;
/// A value is cut after this many characters.
const VALUE_AT_MOST: usize = 200;

/// Where the panel reads the rows foreign keys point to: the tab's
/// connection and database.
#[derive(Clone, PartialEq)]
pub struct Lookup {
    pub connections: Entity<Connections>,
    pub target: Target,
}

/// Names a person reads first in a row, normalised (lower case, no `_`,
/// `-` or space), in the order they are shown.
const READ_FIRST: [&str; 19] = [
    "name",
    "fullname",
    "displayname",
    "firstname",
    "givenname",
    "lastname",
    "surname",
    "familyname",
    "title",
    "label",
    "email",
    "emailaddress",
    "mail",
    "username",
    "login",
    "nickname",
    "handle",
    "code",
    "slug",
];

/// Text types that hold long text: shown after the short ones.
const LONG_TEXT: [&str; 6] = [
    "text",
    "tinytext",
    "mediumtext",
    "longtext",
    "clob",
    "ntext",
];

fn normalised(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

/// The columns of a referenced row shown by default (D-091), as indexes
/// into `columns`, at most [`SHOWN_AT_MOST`]: those a person reads first by
/// their name (a name, first and last name, a title, a label, an email, a
/// user name, a code or a slug; then any `…name`, `…title`, `…email`); else
/// the first text columns, short ones first; else the first columns. The
/// key (`key`, the referenced columns) is not repeated, and binary and JSON
/// columns are never picked.
pub fn default_columns(columns: &[Column], key: &[String]) -> Vec<usize> {
    let candidates: Vec<usize> = (0..columns.len())
        .filter(|&index| {
            let column = &columns[index];
            !key.contains(&column.name) && !matches!(column.kind, TypeKind::Binary | TypeKind::Json)
        })
        .collect();
    let names: Vec<String> = columns
        .iter()
        .map(|column| normalised(&column.name))
        .collect();
    let mut picked: Vec<usize> = Vec::new();
    for word in READ_FIRST {
        picked.extend(candidates.iter().filter(|&&index| names[index] == word));
    }
    for &index in &candidates {
        let name = &names[index];
        if !picked.contains(&index)
            && ["name", "title", "email"]
                .iter()
                .any(|end| name.ends_with(end))
        {
            picked.push(index);
        }
    }
    if picked.is_empty() {
        let long = |index: &usize| {
            LONG_TEXT
                .iter()
                .any(|long| columns[*index].type_name.eq_ignore_ascii_case(long))
        };
        let mut text: Vec<usize> = candidates
            .iter()
            .copied()
            .filter(|&index| columns[index].kind == TypeKind::Text)
            .collect();
        text.sort_by_key(long);
        picked = text;
    }
    if picked.is_empty() {
        picked = candidates;
    }
    picked.truncate(SHOWN_AT_MOST);
    picked
}

/// The row a cell's foreign key points to.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Reference {
    /// The cell menu's jump: the referenced table and the filter on its key.
    related: Related,
    /// Its number among the row's jumps ([`GridAction::Follow`]), where the
    /// grid's owner jumps.
    follow: Option<usize>,
    /// The key's columns under their referenced names, typed as the columns
    /// that hold them here: what the filter's literals are written from.
    key: Vec<ColumnInfo>,
    /// The columns of this table that hold the key.
    by: Vec<String>,
    /// A column of the key holds a pending value.
    pending: bool,
}

impl Reference {
    fn heading(&self) -> String {
        format!("{}, by {}", self.related.table.name, self.by.join(", "))
    }

    /// What the cache knows it by.
    fn cache_key(&self) -> String {
        let values: Vec<(&str, &str)> = self
            .related
            .filter
            .conditions
            .iter()
            .map(|condition| (condition.column.as_str(), condition.value.as_str()))
            .collect();
        serde_json::to_string(&(&self.related.table.schema, &self.related.table.name, values))
            .unwrap_or_default()
    }

    /// "id = 42".
    fn key_text(&self) -> String {
        self.related
            .filter
            .conditions
            .iter()
            .map(|condition| format!("{} = {}", condition.column, condition.value))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The referenced key's columns.
    fn referenced(&self) -> Vec<String> {
        self.key.iter().map(|column| column.name.clone()).collect()
    }
}

/// What the selected cell has to do with a foreign key.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Wanted {
    Row(Reference),
    /// A column of the key gets its value from the server (a default, an
    /// expression) once applied: the heading.
    Later(String),
}

/// What the cell at `row` and the result's column `source` points to: none
/// when its column is in no foreign key of the table the rows are from, or
/// a column of the key is NULL (or binary, as the cell menu's jump).
pub(super) fn wanted_at(grid: &Grid, row: usize, source: usize) -> Option<Wanted> {
    let structure = grid.structure()?;
    let name = grid.table_column_name(source)?;
    let key = structure
        .foreign_keys
        .iter()
        .find(|key| key.columns.iter().any(|column| column == name))?;
    let sources: Vec<usize> = key
        .columns
        .iter()
        .map(|column| grid.source_of(column))
        .collect::<Option<_>>()?;
    let computed = sources.iter().any(|&source| {
        matches!(
            grid.pending()
                .and_then(|pending| pending.value(grid.row_count(), row, source)),
            Some(CellValue::Default | CellValue::Expression(_))
        )
    });
    if computed {
        return Some(Wanted::Later(format!(
            "{}, by {}",
            key.referenced_table.name,
            key.columns.join(", ")
        )));
    }
    let (index, related) = follow_at(structure, grid, row, name)?;
    let follow = grid.follows().then_some(index);
    let key_columns = key
        .columns
        .iter()
        .zip(&key.referenced_columns)
        .map(|(here, there)| {
            let mut column = structure
                .columns
                .iter()
                .find(|column| &column.name == here)
                .cloned()
                .unwrap_or_else(|| {
                    kerno_core::browse::described(&[Column::new(here, "", TypeKind::Text)])
                        .remove(0)
                });
            column.name = there.clone();
            column
        })
        .collect();
    Some(Wanted::Row(Reference {
        related,
        follow,
        key: key_columns,
        by: key.columns.clone(),
        pending: sources
            .iter()
            .any(|&source| matches!(grid.mark(row, source), Mark::Edited | Mark::New)),
    }))
}

/// A row as the preview shows it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Record {
    columns: Vec<Column>,
    /// One line each; `None` for NULL.
    values: Vec<Option<String>>,
}

/// What reading a key found.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Read {
    Row(Record),
    NoRow,
}

fn line_of(column: &Column, bytes: &[u8]) -> String {
    if column.kind == TypeKind::Binary {
        return format!("{} of binary data", byte_size(bytes.len() as u64));
    }
    let text = String::from_utf8_lossy(bytes);
    let mut line: String = text
        .chars()
        .take(VALUE_AT_MOST)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.chars().nth(VALUE_AT_MOST).is_some() {
        line.push('…');
    }
    line
}

fn read_of(result: CollectedResult) -> Read {
    let Some(batch) = result.batches.iter().find(|batch| batch.row_count() > 0) else {
        return Read::NoRow;
    };
    let values = result
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| batch.cell(0, index).map(|bytes| line_of(column, bytes)))
        .collect();
    Read::Row(Record {
        columns: result.columns.to_vec(),
        values,
    })
}

/// Where the preview is.
#[derive(Debug, Clone, Default, PartialEq)]
enum State {
    /// Nothing to show, or the selection has not rested yet.
    #[default]
    Resting,
    /// Read; "Reading…" once it takes long enough to be seen.
    Reading {
        shown: bool,
    },
    Done(Read),
    /// What went wrong, in the server's words.
    Failed(String),
    /// Why it was not read: cancelled, or too long.
    Stopped(&'static str),
}

const CANCELLED: &str = "Not read: cancelled.";
const TOO_LONG: &str = "Not read: the server took longer than 10 s.";

/// The panel's own session, closed when it is dropped (the panel hidden or
/// gone, its connection changed) or stopped.
struct Reader {
    session: Arc<dyn Session>,
    stop: bridge::Stop,
    _closes: tokio::sync::oneshot::Sender<()>,
}

impl Reader {
    fn new(session: Arc<dyn Session>, cx: &App) -> Self {
        let (closes, closed) = tokio::sync::oneshot::channel::<()>();
        let held = session.clone();
        bridge::spawn(cx, async move {
            let _ = closed.await;
            let _ = tokio::time::timeout(Duration::from_secs(2), held.close()).await;
        })
        .detach();
        Self {
            session,
            stop: bridge::Stop::default(),
            _closes: closes,
        }
    }
}

/// The preview of a panel.
#[derive(Default)]
pub(super) struct Preview {
    lookup: Option<Lookup>,
    wanted: Option<Wanted>,
    state: State,
    /// What was read, by [`Reference::cache_key`], for the grid followed.
    cache: HashMap<String, Read>,
    /// The columns picked per referenced table (by [`table_key`]); `None`:
    /// Kerno's choice. Read from the store once per table.
    choices: HashMap<String, Option<Vec<String>>>,
    /// Every column is shown.
    all: bool,
    reader: Option<Reader>,
    /// The selection resting before a read.
    rest: Option<Task<()>>,
    /// Counts the keys wanted: an answer for an earlier one is dropped.
    generation: u64,
}

#[cfg(test)]
thread_local! {
    /// Reads started on this thread (a test's), for the tests.
    pub(super) static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl ValuePanel {
    /// The tab's connection and database, given as it draws.
    pub(super) fn set_lookup(&mut self, lookup: Option<Lookup>, cx: &mut Context<Self>) {
        if self.preview.lookup != lookup {
            self.preview.lookup = lookup;
            self.stop_reading(cx);
            self.preview.reader = None;
            self.preview.cache.clear();
            self.preview.choices.clear();
        }
    }

    /// The grid followed is another: what was read for the last one may have
    /// changed since, the key shown too (rows read again keep their
    /// selection, K36: the same key is read again).
    pub(super) fn forget_reads(&mut self) {
        self.preview.cache.clear();
        self.preview.wanted = None;
    }

    /// Stops a read in flight; its session is closed with it.
    fn stop_reading(&mut self, cx: &mut Context<Self>) {
        if let Some(reader) = &self.preview.reader
            && reader.stop.busy()
        {
            reader.stop.stop(cx);
            self.preview.reader = None;
        }
    }

    /// Follows the cell shown: reads the row its foreign key points to once
    /// the selection rests, unless already read.
    pub(super) fn follow_reference(&mut self, cx: &mut Context<Self>) {
        let wanted = match (self.cell(), self.grid()) {
            (Some((row, source)), Some(grid)) => wanted_at(grid.read(cx).delegate(), row, source),
            _ => None,
        };
        if wanted == self.preview.wanted {
            return;
        }
        let same_key = match (&wanted, &self.preview.wanted) {
            (Some(Wanted::Row(new)), Some(Wanted::Row(old))) => new.cache_key() == old.cache_key(),
            _ => false,
        };
        self.preview.wanted = wanted;
        // Another column of the same key, the same value pending or not:
        // what is read or being read stays.
        if same_key {
            return;
        }
        self.stop_reading(cx);
        self.preview.generation += 1;
        self.preview.rest = None;
        self.preview.all = false;
        self.preview.state = State::Resting;
        let Some(Wanted::Row(reference)) = self.preview.wanted.clone() else {
            return;
        };
        self.load_choice(&reference, cx);
        if let Some(read) = self.preview.cache.get(&reference.cache_key()) {
            self.preview.state = State::Done(read.clone());
            return;
        }
        let generation = self.preview.generation;
        self.preview.rest = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REST).await;
            let _ = this.update(cx, |this, cx| this.read_reference(generation, cx));
        }));
    }

    /// Where the column choice of the referenced table is stored.
    fn table_key_of(&self, reference: &Reference, cx: &App) -> Option<(String, String)> {
        let lookup = self.preview.lookup.as_ref()?;
        Some((
            lookup.target.connection.clone(),
            table_key(
                lookup.connections.read(cx),
                &lookup.target,
                &reference.related.table,
            ),
        ))
    }

    /// Reads the columns picked for the referenced table, once.
    fn load_choice(&mut self, reference: &Reference, cx: &mut Context<Self>) {
        let Some((connection, key)) = self.table_key_of(reference, cx) else {
            return;
        };
        if self.preview.choices.contains_key(&key) {
            return;
        }
        self.preview.choices.insert(key.clone(), None);
        let storage = AppState::get(cx).storage.clone();
        let stored = key.clone();
        let reading =
            bridge::spawn_blocking(cx, move || storage.preview_columns(&connection, &stored));
        cx.spawn(async move |this, cx| {
            let choice = reading.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                this.preview.choices.insert(key, choice);
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads the row the key wanted points to, unless another is wanted now.
    fn read_reference(&mut self, generation: u64, cx: &mut Context<Self>) {
        if generation != self.preview.generation {
            return;
        }
        let (Some(Wanted::Row(reference)), Some(lookup)) =
            (self.preview.wanted.clone(), self.preview.lookup.clone())
        else {
            return;
        };
        #[cfg(test)]
        READS.with(|reads| reads.set(reads.get() + 1));
        self.preview.state = State::Reading { shown: false };
        cx.notify();
        let opening = match &self.preview.reader {
            Some(reader) => Task::ready(Ok(reader.session.clone())),
            None => lookup.connections.read(cx).open_session(
                &lookup.target.connection,
                lookup.target.database.clone(),
                cx,
            ),
        };
        let shown = cx.background_executor().timer(LOADING_SHOWN);
        cx.spawn(async move |this, cx| {
            shown.await;
            let _ = this.update(cx, |this, cx| {
                if this.preview.generation == generation
                    && this.preview.state == (State::Reading { shown: false })
                {
                    this.preview.state = State::Reading { shown: true };
                    cx.notify();
                }
            });
        })
        .detach();
        let limit = cx.background_executor().timer(READ_LIMIT);
        cx.spawn(async move |this, cx| {
            limit.await;
            let _ = this.update(cx, |this, cx| {
                if this.preview.generation == generation
                    && matches!(this.preview.state, State::Reading { .. })
                {
                    this.halt(TOO_LONG, cx);
                }
            });
        })
        .detach();
        // Not dropped with the panel: a session opened meanwhile must reach
        // a reader, which closes it.
        cx.spawn(async move |this, cx| {
            let session = match opening.await {
                Ok(session) => session,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        if this.preview.generation == generation {
                            this.preview.state = State::Failed(describe(&error).to_string());
                            cx.notify();
                        }
                    });
                    return;
                }
            };
            let running = this.update(cx, |this, cx| {
                let kept = match &this.preview.reader {
                    Some(reader) if Arc::ptr_eq(&reader.session, &session) => None,
                    Some(_) => Some(session.clone()),
                    None => {
                        this.preview.reader = Some(Reader::new(session.clone(), cx));
                        None
                    }
                };
                // Opened twice at once: the one not kept is closed.
                if let Some(extra) = kept {
                    drop(Reader::new(extra, cx));
                }
                let reader = this.preview.reader.as_ref()?;
                if this.preview.generation != generation {
                    return None;
                }
                let (key, table) = (reference.key.clone(), reference.related.clone());
                Some(
                    reader
                        .stop
                        .run(cx, reader.session.clone(), move |session| async move {
                            let sql = page_sql(
                                session.dialect(),
                                &key,
                                Page {
                                    table: &table.table,
                                    filter: &table.filter,
                                    sort: None,
                                    identity: &[],
                                    limit: 1,
                                    offset: 0,
                                },
                            )?;
                            exec::collect(session, sql).await
                        }),
                )
            });
            let Ok(Some(running)) = running else {
                return;
            };
            // `None`: stopped, and whoever stopped it said why.
            let Some(outcome) = running.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(mut collected) => {
                        let read = read_of(collected.results.pop().unwrap_or_default());
                        if this.preview.cache.len() >= CACHE_AT_MOST {
                            this.preview.cache.clear();
                        }
                        this.preview
                            .cache
                            .insert(reference.cache_key(), read.clone());
                        if this.preview.generation == generation {
                            this.preview.state = State::Done(read);
                        }
                    }
                    Err(error) => {
                        // The session may be gone with the error: the next
                        // read opens another.
                        this.preview.reader = None;
                        if this.preview.generation == generation {
                            this.preview.state = State::Failed(describe(&error).to_string());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Stops the read in flight, and says why.
    fn halt(&mut self, why: &'static str, cx: &mut Context<Self>) {
        self.stop_reading(cx);
        self.preview.state = State::Stopped(why);
        cx.notify();
    }

    /// Reads the row again, now.
    fn read_again(&mut self, cx: &mut Context<Self>) {
        self.preview.generation += 1;
        self.preview.rest = None;
        let generation = self.preview.generation;
        self.read_reference(generation, cx);
    }

    /// The columns shown of `record`, as indexes.
    fn shown_columns(&self, reference: &Reference, record: &Record, cx: &App) -> Vec<usize> {
        if self.preview.all {
            return (0..record.columns.len()).collect();
        }
        let picked = self
            .table_key_of(reference, cx)
            .and_then(|(_, key)| self.preview.choices.get(&key).cloned().flatten());
        if let Some(picked) = picked {
            let shown: Vec<usize> = (0..record.columns.len())
                .filter(|&index| picked.contains(&record.columns[index].name))
                .collect();
            if !shown.is_empty() {
                return shown;
            }
        }
        default_columns(&record.columns, &reference.referenced())
    }

    /// Shows the column `name` of the referenced table, or no longer shows
    /// it, from now on, for every key of that table.
    pub fn pick_preview_column(&mut self, name: &str, cx: &mut Context<Self>) {
        let (Some(Wanted::Row(reference)), State::Done(Read::Row(record))) =
            (&self.preview.wanted, &self.preview.state)
        else {
            return;
        };
        let shown = {
            let all = std::mem::take(&mut self.preview.all);
            let shown = self.shown_columns(reference, record, cx);
            self.preview.all = all;
            shown
        };
        let mut names: Vec<String> = shown
            .iter()
            .map(|&index| record.columns[index].name.clone())
            .collect();
        match names.iter().position(|shown| shown == name) {
            Some(at) => {
                names.remove(at);
            }
            None => names.push(name.to_string()),
        }
        let order: Vec<&str> = record.columns.iter().map(|c| c.name.as_str()).collect();
        names.sort_by_key(|name| order.iter().position(|known| known == name));
        let reference = reference.clone();
        self.choose(&reference, Some(names), cx);
    }

    /// Goes back to Kerno's choice of columns for the referenced table.
    pub fn automatic_preview_columns(&mut self, cx: &mut Context<Self>) {
        if let Some(Wanted::Row(reference)) = self.preview.wanted.clone() {
            self.choose(&reference, None, cx);
        }
    }

    fn choose(
        &mut self,
        reference: &Reference,
        names: Option<Vec<String>>,
        cx: &mut Context<Self>,
    ) {
        let Some((connection, key)) = self.table_key_of(reference, cx) else {
            return;
        };
        let names = names.filter(|names| !names.is_empty());
        self.preview.choices.insert(key.clone(), names.clone());
        self.preview.all = false;
        let storage = AppState::get(cx).storage.clone();
        let writing = bridge::spawn_blocking(cx, move || {
            storage.save_preview_columns(&connection, &key, names.as_deref())
        });
        cx.background_spawn(async move {
            if let Err(error) = writing.await {
                tracing::warn!(%error, "the columns of a preview could not be stored");
            }
        })
        .detach();
        cx.notify();
    }

    /// Every column of the row, or the chosen ones again.
    pub(super) fn toggle_all_preview_columns(&mut self, cx: &mut Context<Self>) {
        self.preview.all = !self.preview.all;
        cx.notify();
    }

    /// Opens the row in a tab of its own: the cell menu's jump (E3).
    pub(super) fn open_referenced_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(Wanted::Row(reference)), Some(grid), Some((row, _))) =
            (&self.preview.wanted, self.grid(), self.cell())
        else {
            return;
        };
        let Some(index) = reference.follow else {
            return;
        };
        let label = reference.related.label.clone();
        let pick = Grid::pick(&grid, Some(row), 0, cx);
        pick(GridAction::Follow(index, label), window, cx);
    }

    /// The section under the value: the row the key points to.
    pub(super) fn render_preview(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let (muted, danger, border) = (theme.muted_foreground, theme.danger, theme.border);
        let wanted = self.preview.wanted.as_ref()?;
        let reference = match wanted {
            Wanted::Row(reference) => reference,
            Wanted::Later(heading) => {
                return Some(
                    section(heading.clone(), border)
                        .child(note(
                            "fk-preview-later",
                            "Its pending value is computed by the server: the row is shown \
                             once the change is applied.",
                            muted,
                        ))
                        .into_any_element(),
                );
            }
        };
        let record = match &self.preview.state {
            State::Done(Read::Row(record)) => Some(record),
            _ => None,
        };
        let me = cx.entity();
        let mut heading = h_flex().gap_1().child(
            div()
                .id("fk-preview-heading")
                .test_support()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_semibold()
                .child(SharedString::from(reference.heading())),
        );
        if let Some(record) = record {
            let shown = self.shown_columns(reference, record, cx);
            let names: Vec<(String, bool)> = record
                .columns
                .iter()
                .enumerate()
                .map(|(index, column)| (column.name.clone(), shown.contains(&index)))
                .collect();
            let automatic = self
                .table_key_of(reference, cx)
                .is_none_or(|(_, key)| self.preview.choices.get(&key).is_none_or(Option::is_none));
            let me = me.clone();
            heading = heading.child(
                Button::new("fk-preview-columns")
                    .ghost()
                    .xsmall()
                    .label("Choose columns…")
                    .tooltip("Choose the columns shown for this table")
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        let picker = me.clone();
                        menu = menu.item(
                            PopupMenuItem::new("Kerno's choice")
                                .checked(automatic)
                                .on_click(move |_, _, cx| {
                                    picker
                                        .update(cx, |panel, cx| panel.automatic_preview_columns(cx))
                                }),
                        );
                        menu = menu.separator();
                        for (name, checked) in &names {
                            let (picker, name) = (me.clone(), name.clone());
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(name.clone()))
                                    .checked(*checked)
                                    .on_click(move |_, _, cx| {
                                        picker.update(cx, |panel, cx| {
                                            panel.pick_preview_column(&name, cx)
                                        })
                                    }),
                            );
                        }
                        menu
                    }),
            );
            if reference.follow.is_some() {
                heading = heading.child(
                    Button::new("fk-preview-open")
                        .ghost()
                        .xsmall()
                        .label("Open the row")
                        .tooltip("Show this row in a tab of its own")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_referenced_row(window, cx)),
                        ),
                );
            }
        }
        let mut body = section(String::new(), border).child(heading);
        if reference.pending {
            body = body.child(note(
                "fk-preview-pending",
                "The row of the pending value, not applied yet.",
                muted,
            ));
        }
        let retry = |id: &'static str| {
            Button::new(id)
                .ghost()
                .xsmall()
                .label("Try again")
                .on_click(cx.listener(|this, _, _, cx| this.read_again(cx)))
        };
        body = match &self.preview.state {
            State::Resting | State::Reading { shown: false } => body,
            State::Reading { shown: true } => body.child(
                h_flex()
                    .gap_1()
                    .child(note(
                        "fk-preview-reading",
                        &format!("Reading the row of {}…", reference.related.table.name),
                        muted,
                    ))
                    .child(
                        Button::new("fk-preview-cancel")
                            .ghost()
                            .xsmall()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.halt(CANCELLED, cx))),
                    ),
            ),
            State::Done(Read::NoRow) => {
                let mut text = format!(
                    "No row with this key: {} has no row where {}.",
                    reference.related.table.name,
                    reference.key_text()
                );
                let open = self.grid().is_some_and(|grid| {
                    grid.read(cx).delegate().session().tx_state() != TxState::Idle
                });
                if open {
                    text.push_str(
                        " Rows of this tab's open transaction are not seen here: only \
                         committed rows are.",
                    );
                }
                body.child(note("fk-preview-no-row", &text, muted))
            }
            State::Done(Read::Row(record)) => {
                let shown = self.shown_columns(reference, record, cx);
                let rows = shown.iter().map(|&index| {
                    let value = &record.values[index];
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(
                            div()
                                .w(px(120.))
                                .flex_none()
                                .truncate()
                                .text_color(muted)
                                .child(SharedString::from(record.columns[index].name.clone())),
                        )
                        .child(match value {
                            Some(value) => div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(value.clone())),
                            None => div().flex_1().italic().text_color(muted).child("NULL"),
                        })
                });
                let total = record.columns.len();
                let toggle = (self.preview.all || shown.len() < total).then(|| {
                    Button::new("fk-preview-all")
                        .ghost()
                        .xsmall()
                        .label(if self.preview.all {
                            "Show fewer columns".to_string()
                        } else {
                            format!("Show all {total} columns")
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_all_preview_columns(cx)))
                });
                body.child(
                    v_flex()
                        .id("fk-preview-row")
                        .test_support()
                        .max_h(px(240.))
                        .overflow_y_scroll()
                        .children(rows),
                )
                .children(toggle)
            }
            State::Failed(message) => body.child(
                v_flex()
                    .gap_1()
                    .child(note(
                        "fk-preview-failed",
                        &format!("The row could not be read.\n{message}"),
                        danger,
                    ))
                    .child(retry("fk-preview-retry")),
            ),
            State::Stopped(why) => body.child(
                h_flex()
                    .gap_1()
                    .child(note("fk-preview-stopped", why, muted))
                    .child(retry("fk-preview-retry")),
            ),
        };
        Some(body.into_any_element())
    }
}

/// The frame of the section, under a line.
fn section(heading: String, border: gpui_kit::Hsla) -> gpui_kit::Div {
    let frame = v_flex()
        .flex_none()
        .gap_1()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(border);
    if heading.is_empty() {
        frame
    } else {
        frame.child(
            div()
                .id("fk-preview-heading")
                .test_support()
                .font_semibold()
                .truncate()
                .child(SharedString::from(heading)),
        )
    }
}

/// A line of text of the section.
fn note(id: &'static str, text: &str, color: gpui_kit::Hsla) -> impl IntoElement {
    div()
        .id(id)
        .test_support()
        .test_text(text.to_string())
        .text_sm()
        .text_color(color)
        .child(SharedString::from(text.to_string()))
}

/// What the tests read of the preview.
#[cfg(test)]
impl ValuePanel {
    /// The heading: the referenced table and the columns holding its key.
    pub fn preview_heading(&self) -> Option<String> {
        match self.preview.wanted.as_ref()? {
            Wanted::Row(reference) => Some(reference.heading()),
            Wanted::Later(heading) => Some(heading.clone()),
        }
    }

    /// Where the preview is, in words: "resting", "reading", "row", "no
    /// row", "failed: …", or why it was not read.
    pub fn preview_state(&self) -> String {
        match &self.preview.state {
            State::Resting => "resting".into(),
            State::Reading { .. } => "reading".into(),
            State::Done(Read::Row(_)) => "row".into(),
            State::Done(Read::NoRow) => "no row".into(),
            State::Failed(message) => format!("failed: {message}"),
            State::Stopped(why) => (*why).into(),
        }
    }

    /// The fields shown: names and values (`None`: NULL).
    pub fn preview_fields(&self, cx: &App) -> Vec<(String, Option<String>)> {
        let (Some(Wanted::Row(reference)), State::Done(Read::Row(record))) =
            (&self.preview.wanted, &self.preview.state)
        else {
            return Vec::new();
        };
        self.shown_columns(reference, record, cx)
            .into_iter()
            .map(|index| {
                (
                    record.columns[index].name.clone(),
                    record.values[index].clone(),
                )
            })
            .collect()
    }

    /// Whether the key shown holds a pending value.
    pub fn preview_pending(&self) -> bool {
        matches!(&self.preview.wanted, Some(Wanted::Row(reference)) if reference.pending)
    }
}

/// Reads started on this test's thread.
#[cfg(test)]
pub fn reads_started() -> usize {
    READS.with(std::cell::Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(spec: &[(&str, &str, TypeKind)]) -> Vec<Column> {
        spec.iter()
            .map(|(name, type_name, kind)| Column::new(*name, *type_name, *kind))
            .collect()
    }

    fn names(columns: &[Column], picked: &[usize]) -> Vec<String> {
        picked
            .iter()
            .map(|&index| columns[index].name.clone())
            .collect()
    }

    /// K18: the columns a person reads first, matched whatever their case
    /// and spelling, in a set order; the key never repeated; binary and JSON
    /// never picked.
    #[test]
    fn the_default_columns_are_the_ones_a_person_reads_first() {
        use TypeKind::*;
        let customers = columns(&[
            ("id", "int8", Integer),
            ("email", "email", Text),
            ("full_name", "text", Text),
            ("phone", "varchar", Text),
            ("preferences", "jsonb", Json),
            ("created_at", "timestamptz", DateTimeTz),
        ]);
        let picked = default_columns(&customers, &["id".into()]);
        assert_eq!(names(&customers, &picked), ["full_name", "email"]);

        let people = columns(&[
            ("ID", "int", Integer),
            ("LastName", "varchar", Text),
            ("FIRST-NAME", "varchar", Text),
            ("E_Mail", "varchar", Text),
            ("company_name", "varchar", Text),
            ("Code", "char", Text),
            ("avatar_title", "bytea", Binary),
        ]);
        assert_eq!(
            names(&people, &default_columns(&people, &["ID".into()])),
            ["FIRST-NAME", "LastName", "E_Mail", "Code", "company_name"]
        );

        // The key is not repeated, even when it is a name.
        let codes = columns(&[("code", "varchar", Text), ("name", "text", Text)]);
        assert_eq!(
            names(&codes, &default_columns(&codes, &["code".into()])),
            ["name"]
        );

        // No known name: the first text columns, short ones first.
        let notes = columns(&[
            ("id", "int", Integer),
            ("body", "text", Text),
            ("kind", "varchar", Text),
            ("blob", "bytea", Binary),
        ]);
        assert_eq!(
            names(&notes, &default_columns(&notes, &["id".into()])),
            ["kind", "body"]
        );

        // No text at all: the first columns other than the key.
        let numbers = columns(&[
            ("a", "int", Integer),
            ("b", "int", Integer),
            ("doc", "json", Json),
        ]);
        assert_eq!(
            names(&numbers, &default_columns(&numbers, &["a".into()])),
            ["b"]
        );

        // At most eight.
        let wide: Vec<Column> = (0..12)
            .map(|index| Column::new(format!("c{index}_name"), "text", Text))
            .collect();
        assert_eq!(default_columns(&wide, &[]).len(), SHOWN_AT_MOST);
    }

    /// K18: a row read is shown one line per value; NULL stays NULL, binary
    /// says its size, long text is cut; no row is no row.
    #[test]
    fn a_read_row_is_shown_one_line_per_value() {
        let text = Column::new("t", "text", TypeKind::Text);
        let bytes = Column::new("b", "bytea", TypeKind::Binary);
        assert_eq!(line_of(&text, b"a\nb"), "a b");
        assert_eq!(line_of(&bytes, &[0, 1, 2]), "3 B of binary data");
        let long = "é".repeat(VALUE_AT_MOST + 5);
        let cut = line_of(&text, long.as_bytes());
        assert_eq!(cut.chars().count(), VALUE_AT_MOST + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(read_of(CollectedResult::default()), Read::NoRow);
    }
}

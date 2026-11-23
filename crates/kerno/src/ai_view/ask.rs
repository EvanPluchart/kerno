//! The assistant asked from a right-click (K53, D-122): on a table, a view or a materialized view
//! (the tree's AI ▸, the AI button of a data or structure tab), on columns, on a result grid and
//! on a selection of its cells. Each entry opens the panel on a query tab's conversation with its
//! first message ready (its subject as a chip, the whole message in "What is sent…"); nothing is
//! sent before the user sends it, as K51's `open`. On a table, a data tab or a structure tab the
//! conversation is a new query tab's, on the table's connection and schema; on a query's result
//! it is that query tab's. What a message says is `kerno_core::ai::ask`.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::table::TableState;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, Subscription, TestSupportExt as _, Window, div,
    px,
};
use kerno_core::TypeKind;
use kerno_core::ai::ask::{
    self as asking, Ask, MAX_RESULT_ROWS, MAX_SELECTED_CELLS, MAX_VALUE_COLUMNS, Shown, Subject,
    Values,
};
use kerno_core::schema::{RelationKind, TableRef, TableStructure};

use super::{AiPanel, Draft, Facts, OFF, OFF_NOTICE, chip, notify, show};
use crate::connections::manager::Connections;
use crate::query::grid::{self, Grid, GridAction};
use crate::query::tab::{QueryTab, QueryTabEvent};
use crate::state::AppState;

/// An entry of the AI ▸ menus, and its command of the palette ("AI: " and its words).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Entry {
    AskTable,
    ExplainTable,
    WriteQuery,
    TestData,
    ExplainColumns,
    ExplainResult,
    AskSelection,
}

impl Entry {
    #[cfg(test)]
    pub const ALL: [Entry; 7] = [
        Entry::AskTable,
        Entry::ExplainTable,
        Entry::WriteQuery,
        Entry::TestData,
        Entry::ExplainColumns,
        Entry::ExplainResult,
        Entry::AskSelection,
    ];
    /// The entries of a table, a view or a materialized view.
    pub const TABLE: [Entry; 4] = [
        Entry::AskTable,
        Entry::ExplainTable,
        Entry::WriteQuery,
        Entry::TestData,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::AskTable => "Ask about this table…",
            Self::ExplainTable => "Explain this table",
            Self::WriteQuery => "Write a query on this table…",
            Self::TestData => "Generate test data…",
            Self::ExplainColumns => "Explain these columns",
            Self::ExplainResult => "Explain this result",
            Self::AskSelection => "Ask about the selection…",
        }
    }

    /// The id of its command in `actions::COMMANDS` (the window binds each to its action).
    #[cfg(test)]
    pub fn command(self) -> &'static str {
        match self {
            Self::AskTable => "ai_ask_about_table",
            Self::ExplainTable => "ai_explain_table",
            Self::WriteQuery => "ai_write_query_on_table",
            Self::TestData => "ai_generate_test_data",
            Self::ExplainColumns => "ai_explain_columns",
            Self::ExplainResult => "ai_explain_result",
            Self::AskSelection => "ai_ask_about_selection",
        }
    }
}

/// An entry as a menu shows it, and whether it can be used. Off: greyed, saying how to turn the
/// assistant on. "Generate test data…" is greyed on a view, and on a read-only connection or a
/// production environment says that it only writes text (`caution` names which).
pub fn shown(entry: Entry, on: bool, view: bool, caution: Option<&str>) -> (String, bool) {
    let label = entry.label();
    match entry {
        _ if !on => (format!("{label} ({OFF})"), false),
        Entry::TestData if view => (format!("{label} (not for a view)"), false),
        Entry::TestData => match caution {
            Some(caution) => (
                format!("{label} ({caution}: writes text, runs nothing)"),
                true,
            ),
            None => (label.to_string(), true),
        },
        _ => (label.to_string(), true),
    }
}

/// Why "Generate test data…" says it only writes text on connection `id`: a read-only
/// connection, or an environment that asks twice before a write (production).
pub fn caution(connections: &Connections, id: &str, cx: &App) -> Option<String> {
    let saved = connections.get(id)?;
    if saved.config.read_only {
        return Some("read-only connection".into());
    }
    let environments = &AppState::get(cx).settings.environments;
    let environment = kerno_core::environment::environment(environments, &saved.config.environment);
    (environment.rules.confirm_writes_twice).then(|| environment.name.clone())
}

/// What `entry` asks of `table` (its structure as read), or of its columns `columns` (names) for
/// "Explain these columns". `None` for an entry about a result.
pub fn of_table(
    entry: Entry,
    table: TableRef,
    kind: RelationKind,
    structure: &TableStructure,
    columns: &[String],
) -> Option<Ask> {
    let subject = Subject::of(table.clone(), kind, structure);
    Some(match entry {
        Entry::AskTable => Ask::Table(subject),
        Entry::ExplainTable => Ask::ExplainTable(subject),
        Entry::WriteQuery => Ask::WriteQuery(subject),
        Entry::TestData => Ask::TestData {
            subject,
            rows: asking::TEST_ROWS,
        },
        Entry::ExplainColumns => Ask::Columns {
            table: Some(table),
            statement: None,
            columns: (columns.iter())
                .filter_map(|name| {
                    let column = structure.columns.iter().find(|c| c.name == *name)?;
                    Some((column.name.clone(), column.data_type.clone()))
                })
                .collect(),
        },
        Entry::ExplainResult | Entry::AskSelection => return None,
    })
}

/// What `entry` asks about the result the grid `table` shows, read from what was fetched, on the
/// UI thread (no I/O): `statement` produced it, `of` is its table when it is one table's (a data
/// tab), `header` the grid column of a header right-clicked (else the selection's columns).
pub fn of_grid(
    entry: Entry,
    table: &Entity<TableState<Grid>>,
    statement: &str,
    of: Option<TableRef>,
    header: Option<usize>,
    cx: &App,
) -> Option<Ask> {
    let state = table.read(cx);
    let grid = state.delegate();
    let all = grid.store().columns();
    let typed = |source: usize| (all[source].name.clone(), all[source].type_name.clone());
    let selected = grid.selected(state.selection());
    let layout: Vec<usize> = grid.layout().to_vec();
    let fetched = grid.store().row_count();
    let shown = Shown {
        statement: statement.to_string(),
        columns: layout.iter().map(|&source| typed(source)).collect(),
        rows: fetched,
        more: grid.has_more(),
        table: of.clone(),
    };
    // The text of a fetched cell: binary values by their size only.
    let value = |row: usize, source: usize| -> Option<String> {
        let bytes = grid.store().cell(grid.source_row(row), source)?;
        Some(match all[source].kind {
            TypeKind::Binary => format!("(binary, {} bytes)", bytes.len()),
            _ => String::from_utf8_lossy(bytes).into_owned(),
        })
    };
    let values = |rows: std::ops::Range<usize>, columns: &[usize], of: usize| Values {
        columns: columns
            .iter()
            .map(|&source| all[source].name.clone())
            .collect(),
        rows: rows
            .filter(|&row| row < fetched)
            .map(|row| columns.iter().map(|&source| value(row, source)).collect())
            .collect(),
        of,
    };
    match entry {
        Entry::ExplainColumns => {
            let header = header.and_then(|column| grid.source_column(column));
            let sources = match (&selected, header) {
                (Some(selected), Some(header)) if selected.columns.contains(&header) => {
                    selected.columns.clone()
                }
                (_, Some(header)) => vec![header],
                (Some(selected), None) => selected.columns.clone(),
                (None, None) => return None,
            };
            Some(Ask::Columns {
                statement: of.is_none().then(|| statement.to_string()),
                table: of,
                columns: sources.into_iter().map(typed).collect(),
            })
        }
        Entry::ExplainResult => {
            let columns = &layout[..layout.len().min(MAX_VALUE_COLUMNS)];
            let values = values(0..MAX_RESULT_ROWS, columns, fetched * layout.len());
            Some(Ask::Result { shown, values })
        }
        Entry::AskSelection => {
            let selected = selected?;
            let columns = &selected.columns[..selected.columns.len().min(MAX_VALUE_COLUMNS)];
            let (first, last) = (*selected.rows.start(), *selected.rows.end());
            let taken = (MAX_SELECTED_CELLS / columns.len().max(1)).max(1);
            let cells = (last - first + 1) * selected.columns.len();
            let values = values(first..(last + 1).min(first + taken), columns, cells);
            Some(Ask::Selection { shown, values })
        }
        _ => None,
    }
}

/// What `entry` asks on a data tab: of its table (`structure` as read), or of the rows its grid
/// shows, read with its statement; `column` is the header's or the current cell's.
pub fn of_data(
    entry: Entry,
    table: &TableRef,
    kind: RelationKind,
    structure: &TableStructure,
    grid: Option<(&Entity<TableState<Grid>>, &str)>,
    column: usize,
    cx: &App,
) -> Option<Ask> {
    match entry {
        Entry::ExplainColumns | Entry::ExplainResult | Entry::AskSelection => {
            let (grid, statement) = grid?;
            of_grid(
                entry,
                grid,
                statement,
                Some(table.clone()),
                Some(column),
                cx,
            )
        }
        _ => of_table(entry, table.clone(), kind, structure, &[]),
    }
}

/// AI ▸ of a grid's menus: on a cell or a row number, the result and the selection; on a column
/// header, its columns. Off, each entry says how to turn the assistant on.
pub fn grid_menu(header: bool, on: bool) -> grid::Entry {
    let entries: &[Entry] = if header {
        &[Entry::ExplainColumns]
    } else {
        &[Entry::ExplainResult, Entry::AskSelection]
    };
    grid::Entry::Submenu(
        "AI",
        (entries.iter())
            .map(|&entry| grid::Entry::Item(GridAction::Ai(entry), (!on).then_some(OFF), false))
            .collect(),
    )
}

/// `entries` with `ai` after the menu's own entries, before its closing Refresh.
pub fn with_ai(mut entries: Vec<grid::Entry>, ai: grid::Entry) -> Vec<grid::Entry> {
    let refresh = matches!(
        entries.as_slice(),
        [
            ..,
            grid::Entry::Separator,
            grid::Entry::Item(GridAction::Refresh, ..)
        ]
    );
    let at = entries.len() - if refresh { 2 } else { 0 };
    entries.splice(at..at, [grid::Entry::Separator, ai]);
    entries
}

/// Runs an entry of an AI button's menu.
pub type Pick = Rc<dyn Fn(Entry, &mut Window, &mut App)>;

/// The "AI" button of a data tab's footer (`xsmall`) or a structure tab's bar: the table's
/// entries ([`caution`] given), then "Explain these columns" when `columns` is given (why it
/// cannot be used, when it cannot). `pick` runs the entry chosen.
pub fn bar_button(
    id: &'static str,
    xsmall: bool,
    view: bool,
    caution: Option<String>,
    columns: Option<Option<&'static str>>,
    pick: Pick,
    cx: &App,
) -> impl IntoElement + use<> {
    let on = AppState::get(cx).settings.ai.enabled;
    let button = Button::new(id).ghost();
    let button = if xsmall {
        button.xsmall()
    } else {
        button.small()
    };
    button
        .label("AI")
        .dropdown_caret(true)
        .tooltip("Ask the AI assistant about this table")
        .dropdown_menu(move |mut menu: PopupMenu, _, _| {
            for entry in Entry::TABLE {
                let (label, enabled) = shown(entry, on, view, caution.as_deref());
                let pick = pick.clone();
                menu = menu.item(
                    PopupMenuItem::new(SharedString::from(label))
                        .disabled(!enabled)
                        .on_click(move |_, window, cx| pick(entry, window, cx)),
                );
            }
            if let Some(why_not) = columns {
                let entry = Entry::ExplainColumns;
                let (label, enabled) = match why_not.filter(|_| on) {
                    Some(why_not) => (format!("{} ({why_not})", entry.label()), false),
                    None => shown(entry, on, view, None),
                };
                let pick = pick.clone();
                menu = menu.separator().item(
                    PopupMenuItem::new(SharedString::from(label))
                        .disabled(!enabled)
                        .on_click(move |_, window, cx| pick(entry, window, cx)),
                );
            }
            menu
        })
}

/// Opens the AI panel on `tab`'s conversation with `ask` ready: its subject as a chip, the
/// keyboard in the message field; nothing is sent until the user sends it. Off, it says so and
/// opens nothing. Not from inside `tab`'s own update: defer, as `ai_view::open` asks.
pub fn open(
    tab: &Entity<QueryTab>,
    ask: Ask,
    caution: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    if !AppState::get(cx).settings.ai.enabled {
        notify(OFF_NOTICE, window, cx);
        return;
    }
    let panel = tab.update(cx, |tab, cx| tab.ai_chat(window, cx));
    panel.update(cx, |panel, cx| {
        panel.prepare(Draft::default(), window, cx);
        panel.set_ask(ask, caution, window, cx);
    });
    show(true, cx);
}

/// What a right-click asked, while the message is being written: the message's subject.
pub struct Asked {
    pub(super) ask: Ask,
    /// "Generate test data…" on a read-only connection or a production environment.
    caution: Option<String>,
    /// The values' tick (a result's first rows, a selection's cells): off at first.
    values: bool,
    /// The number of rows of "Generate test data…".
    rows: Option<Entity<InputState>>,
    /// The tab's catalog is being read: Send waits, so the tables linked to the subject go too.
    catalog: bool,
    _rows_changed: Option<Subscription>,
}

impl Asked {
    /// The message as sent, from the user's words; `None` while words are needed.
    pub(super) fn message(&self, typed: &str, engine: &str) -> Option<String> {
        self.ask.message(typed, engine, self.values)
    }

    /// The chips the thread keeps under the message sent.
    pub(super) fn sent_chips(&self) -> Vec<String> {
        let mut chips = vec![self.ask.chip()];
        if let Some(values) = self.ask.values().filter(|_| self.values) {
            let selection = matches!(self.ask, Ask::Selection { .. });
            chips.push(values.label(selection).replacen("Include", "With", 1));
        }
        chips
    }

    /// Its answers' SQL is offered in a new query tab.
    pub(super) fn test_data(&self) -> bool {
        matches!(self.ask, Ask::TestData { .. })
    }
}

impl AiPanel {
    /// Makes `ask` the subject of the message being written (after [`AiPanel::prepare`]).
    pub fn set_ask(
        &mut self,
        ask: Ask,
        caution: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (rows, subscription) = match &ask {
            Ask::TestData { rows, .. } => {
                let field = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(asking::TEST_ROWS.to_string())
                        .default_value(rows.to_string())
                });
                let subscription =
                    cx.subscribe(&field, |this: &mut Self, field, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            let wanted = asking::test_rows(&field.read(cx).value());
                            if let Some(Asked {
                                ask: Ask::TestData { rows, .. },
                                ..
                            }) = &mut this.asked
                            {
                                *rows = wanted;
                            }
                            this.refresh(cx);
                        }
                    });
                (Some(field), Some(subscription))
            }
            _ => (None, None),
        };
        // A fresh query tab reads its catalog after it opens: Send waits for it, so the tables
        // the subject is linked to are described too (`QueryTab::load_catalog` tells the panel).
        let catalog = self.facts.catalog.schemas.is_empty() && self.facts.session.is_some();
        let test_data = matches!(ask, Ask::TestData { .. });
        self.asked = Some(Asked {
            ask,
            caution: caution.filter(|_| test_data),
            values: false,
            rows,
            catalog,
            _rows_changed: subscription,
        });
        self.refresh(cx);
    }

    /// The tab's catalog was read (or could not be): the context is put together again from it.
    pub fn catalog_read(&mut self, facts: Facts, cx: &mut Context<Self>) {
        self.facts = facts;
        if let Some(asked) = &mut self.asked {
            asked.catalog = false;
        }
        self.refresh(cx);
    }

    /// Send waits for the tab's catalog.
    pub(super) fn waiting_catalog(&self) -> bool {
        let empty = self.facts.catalog.schemas.is_empty();
        empty && self.asked.as_ref().is_some_and(|asked| asked.catalog)
    }

    /// Ticks or unticks the values of the result or the selection.
    pub fn set_ask_values(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(asked) = &mut self.asked {
            asked.values = on;
        }
        self.refresh(cx);
    }

    /// What the message being written is about; for tests.
    #[cfg(test)]
    pub fn asked(&self) -> Option<&Ask> {
        self.asked.as_ref().map(|asked| &asked.ask)
    }

    /// Opens `sql` in a new query tab on this conversation's connection, database and schema;
    /// nothing runs.
    fn open_in_new_tab(&self, sql: &str, cx: &mut App) {
        if let Some(tab) = self.tab.upgrade() {
            let sql = sql.to_string();
            tab.update(cx, |_, cx| {
                cx.emit(QueryTabEvent::OpenQuery {
                    title: "Test data".into(),
                    sql,
                })
            });
        }
    }

    /// The button under an answer's SQL that opens it in a new tab ("Generate test data…").
    pub(super) fn new_tab_button(
        &self,
        id: gpui_kit::ElementId,
        sql: String,
        cx: &Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        self.new_tab.then(|| {
            Button::new(id)
                .outline()
                .xsmall()
                .label("Open in a new tab")
                .tooltip("In a new query tab on this connection: nothing runs")
                .on_click(cx.listener(move |this, _, _, cx| this.open_in_new_tab(&sql, cx)))
        })
    }

    /// The chips of what a right-click asked: its subject (× takes it off), what "Generate test
    /// data…" only writes, the number of rows it asks for, and the tick of the values.
    pub(super) fn asked_chips(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let Some(asked) = &self.asked else {
            return Vec::new();
        };
        let muted = cx.theme().muted_foreground;
        let mut chips = vec![
            chip("ai-ask-subject", asked.ask.chip(), cx, |this, cx| {
                this.asked = None;
                this.refresh(cx);
            })
            .into_any_element(),
        ];
        if let Some(caution) = &asked.caution {
            chips.push(
                div()
                    .id("ai-ask-caution")
                    .test_support()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(format!(
                        "{caution}: Kerno only writes the SQL, nothing runs."
                    )))
                    .into_any_element(),
            );
        }
        if let Some(rows) = &asked.rows {
            chips.push(
                h_flex()
                    .id("ai-ask-rows")
                    .test_support()
                    .gap_2()
                    .text_xs()
                    .child("Rows to generate")
                    .child(
                        div()
                            .w(px(64.))
                            .child(Input::new(rows).xsmall().id("ai-ask-rows-input")),
                    )
                    .child(div().text_color(muted).child(SharedString::from(format!(
                        "1 to {}",
                        asking::MAX_TEST_ROWS
                    ))))
                    .into_any_element(),
            );
        }
        if let Some(values) = asked.ask.values() {
            let selection = matches!(asked.ask, Ask::Selection { .. });
            chips.push(
                Checkbox::new("ai-ask-values")
                    .label(SharedString::from(values.label(selection)))
                    .checked(asked.values)
                    .on_click(
                        cx.listener(|this, checked: &bool, _, cx| {
                            this.set_ask_values(*checked, cx)
                        }),
                    )
                    .into_any_element(),
            );
        }
        chips
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(entries: &[grid::Entry]) -> Vec<String> {
        let mut labels = Vec::new();
        for entry in entries {
            match entry {
                grid::Entry::Item(action, why_not, _) => labels.push(match why_not {
                    Some(why) => format!("{} ({why})", action.label()),
                    None => action.label(),
                }),
                grid::Entry::Submenu(label, items) => {
                    labels.push(format!("{label} ▸"));
                    labels.extend(labels_of(items).into_iter().map(|l| format!("  {l}")));
                }
                grid::Entry::Separator => labels.push("—".into()),
            }
        }
        labels
    }

    fn labels_of(entries: &[grid::Entry]) -> Vec<String> {
        labels(entries)
    }

    /// AI ▸ goes after the menu's own entries and before its closing Refresh; nothing of the
    /// menu is moved or left out.
    #[test]
    fn ai_goes_before_refresh_and_moves_nothing() {
        let refresh = grid::Entry::Item(GridAction::Refresh, None, false);
        let export = grid::Entry::Item(GridAction::Export, None, false);
        let data = vec![export.clone(), grid::Entry::Separator, refresh.clone()];
        assert_eq!(
            labels(&with_ai(data, grid_menu(false, true))),
            [
                "Export…",
                "—",
                "AI ▸",
                "  Explain this result",
                "  Ask about the selection…",
                "—",
                "Refresh"
            ]
        );
        assert_eq!(
            labels(&with_ai(vec![export], grid_menu(true, false))),
            [
                "Export…",
                "—",
                "AI ▸",
                "  Explain these columns (turn on in Settings → AI)"
            ]
        );
    }

    #[test]
    fn the_entries_say_when_they_cannot_be_used_or_only_write_text() {
        assert_eq!(
            shown(Entry::TestData, false, false, None),
            (
                "Generate test data… (turn on in Settings → AI)".to_string(),
                false
            )
        );
        assert_eq!(
            shown(Entry::TestData, true, true, None),
            ("Generate test data… (not for a view)".to_string(), false)
        );
        assert_eq!(
            shown(Entry::TestData, true, false, Some("Production")),
            (
                "Generate test data… (Production: writes text, runs nothing)".to_string(),
                true
            )
        );
        assert_eq!(
            shown(Entry::ExplainTable, true, true, Some("Production")),
            ("Explain this table".to_string(), true)
        );
        // Each entry is a command of the palette, named as the menu names it.
        for entry in Entry::ALL {
            let command = crate::actions::COMMANDS
                .iter()
                .find(|command| command.id == entry.command())
                .unwrap_or_else(|| panic!("no command {}", entry.command()));
            // Title case, as every command of the palette.
            assert_eq!(
                command.label.to_lowercase(),
                format!("ai: {}", entry.label()).to_lowercase()
            );
        }
    }
}

//! The value panel (K17): the cell under the grid's cursor, whole, at the
//! right of a query's results and of a table's data, as DBeaver's "Value"
//! panel (`ValueViewerPanel`), with JSON always laid out and highlighted. It
//! follows the selection, reads the grid's store (nothing is fetched again),
//! and saves through the same pending changes as the value editor dialog
//! (`super::viewer`), whose functions draw and check the value.
//!
//! One panel per tab, made when the setting shows it and dropped when it
//! hides it: hidden, it does no work. Text typed and not set is never lost
//! silently: the panel stays on its cell while the selection moves on, and
//! when its rows go (read again, the panel hidden) it goes to the clipboard,
//! and a notification says so.

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::empty::{Empty, EmptyDescription, EmptyHeader, EmptyTitle};
use gpui_kit::component::input::{self, Editor, EditorState, InputEvent};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{
    ResizablePanelEvent, ResizableState, h_resizable, resizable_panel,
};
use gpui_kit::component::table::TableState;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, SharedString,
    Styled as _, Subscription, Task, TestSupportExt as _, WeakEntity, Window, div, px,
};
use kerno_core::format::byte_size;
use kerno_core::sql::CellValue;
use kerno_core::{Column, TypeKind};

use super::grid::{Grid, NOT_EDITABLE};
use super::viewer::{self, MAX_SHOWN_BYTES, View};
use crate::actions;
use crate::settings::VALUE_PANEL_WIDTHS;
use crate::state::AppState;

/// The narrowest the grid gets beside the panel; in a narrower window the
/// panel is not drawn.
const GRID_MIN: Pixels = px(240.);
/// Values up to this size are laid out on the UI thread; larger ones off it.
const INLINE_BYTES: usize = 64 * 1024;

/// Shows the value panel beside every grid, or hides it (F7, the grids'
/// "Value" button, the palette).
pub fn toggle(window: &mut Window, cx: &mut App) {
    AppState::update_settings(cx, |settings| settings.value_panel = !settings.value_panel);
    window.refresh();
}

/// The grids' "Value" button.
pub fn button(id: &'static str, cx: &App) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .label("Value")
        .selected(AppState::get(cx).settings.value_panel)
        .tooltip_with_action(
            "Show the selected cell's whole value beside the grid",
            &actions::ToggleValuePanel,
            None,
        )
        .on_click(|_, window, cx| toggle(window, cx))
}

/// Keeps a tab's panel in step with the setting, the grid on screen and the
/// tab's reason for read-only rows (`why`): made when the setting shows it,
/// dropped (its unsaved text kept) when it hides it. Called as the tab
/// draws; the panel follows its grid right after.
pub fn sync(
    slot: &mut Option<Entity<ValuePanel>>,
    grid: Option<&Entity<TableState<Grid>>>,
    why: Option<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<ValuePanel>> {
    if !AppState::get(cx).settings.value_panel {
        if let Some(panel) = slot.take() {
            window.defer(cx, move |window, cx| {
                panel.update(cx, |panel, cx| panel.keep_draft(window, cx))
            });
        }
        return None;
    }
    let panel = slot
        .get_or_insert_with(|| cx.new(|cx| ValuePanel::new(window, cx)))
        .clone();
    let (current, known) = {
        let panel = panel.read(cx);
        (panel.grid(), panel.why.clone())
    };
    if current.as_ref() != grid || known != why {
        let (grid, follower) = (grid.cloned(), panel.clone());
        window.defer(cx, move |window, cx| {
            follower.update(cx, |panel, cx| panel.watch(grid, why, window, cx))
        });
    }
    Some(panel)
}

/// `rows` with the panel at its right, when there is one, and room for it.
pub fn beside(rows: AnyElement, panel: Option<&Entity<ValuePanel>>, cx: &mut App) -> AnyElement {
    let Some(panel) = panel else {
        return rows;
    };
    let split = panel.read(cx).split.clone();
    let container = split.read(cx).container_size();
    // Before the first layout the width is not known: room is assumed.
    let room = container <= px(0.) || container >= GRID_MIN + px(*VALUE_PANEL_WIDTHS.start());
    panel.update(cx, |panel, _| panel.collapsed = !room);
    let width = AppState::get(cx).settings.value_panel_width;
    h_resizable("value-panel-split")
        .with_state(&split)
        .child(
            resizable_panel()
                .size_range(GRID_MIN..Pixels::MAX)
                .child(rows),
        )
        .child(
            resizable_panel()
                .size(px(width))
                .size_range(px(*VALUE_PANEL_WIDTHS.start())..px(*VALUE_PANEL_WIDTHS.end()))
                .fixed()
                .visible(room)
                .child(panel.clone()),
        )
        .into_any_element()
}

/// What the selection of the grid is, for the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Nothing,
    Several,
    /// A row of the grid and a column of the result.
    Cell(usize, usize),
}

/// A value laid out for the panel: off the UI thread when large.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Prepared {
    view: View,
    /// What the editor holds.
    text: String,
    /// The value as text, what an edit is compared with (empty when the
    /// value cannot be edited here).
    fetched: String,
    /// `None` for NULL.
    size: Option<u64>,
    /// Only the start of the value is shown: it is not edited here.
    partial: bool,
    /// What time an integer that looks like a Unix time is.
    hint: Option<String>,
}

/// Lays a value of `column` out as the viewer does: JSON indented, text
/// whole, binary as a hex dump, at most [`MAX_SHOWN_BYTES`] of it. A text
/// column that is `editable` is shown as stored: re-indenting a user's text
/// and saving it would change it.
pub(super) fn prepare(column: &Column, value: Option<&[u8]>, editable: bool) -> Prepared {
    let (views, json) = viewer::views_for(column, value);
    let mut view = views.first().copied().unwrap_or(View::Text);
    if editable && view == View::Json && column.kind != TypeKind::Json {
        view = View::Text;
    }
    let hint = viewer::time_hint(column, value);
    let Some(bytes) = value else {
        return Prepared {
            view: View::Text,
            text: String::new(),
            fetched: String::new(),
            size: None,
            partial: false,
            hint,
        };
    };
    let mut text = viewer::view_text(bytes, view, json.as_deref());
    let mut partial = view == View::Hex && bytes.len() > MAX_SHOWN_BYTES / 4;
    if let Some(end) = viewer::cut_at(&text) {
        text.truncate(end);
        partial = true;
    }
    let fetched = if partial || view == View::Hex {
        String::new()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    Prepared {
        view,
        text,
        fetched,
        size: Some(bytes.len() as u64),
        partial,
        hint,
    }
}

/// The line beside the header: NULL, an empty value, or its size.
pub(super) fn status(prepared: &Prepared) -> String {
    match prepared.size {
        None => "NULL".into(),
        Some(0) if prepared.view == View::Hex => "Empty: 0 bytes".into(),
        Some(0) => "Empty string".into(),
        Some(size) if prepared.partial => format!(
            "{}; the first {} are shown",
            byte_size(size),
            byte_size(MAX_SHOWN_BYTES as u64)
        ),
        Some(size) => byte_size(size),
    }
}

/// The cell the panel shows.
struct Shown {
    row: usize,
    /// Column of the result.
    source: usize,
    name: String,
    type_name: String,
    kind: TypeKind,
    /// `None` while it is laid out.
    prepared: Option<Prepared>,
    /// Whether it takes a value here, and in plain words why not.
    editable: Result<(), String>,
    nullable: bool,
}

/// The value panel of a tab.
pub struct ValuePanel {
    table: Option<WeakEntity<TableState<Grid>>>,
    /// The widths of the grid and the panel.
    split: Entity<ResizableState>,
    editor: Entity<EditorState>,
    /// What the selection is, as last shown, and the grid's generation then.
    seen: Option<(Target, u64)>,
    target: Target,
    shown: Option<Shown>,
    /// The text was changed and not set: the panel stays on this cell.
    draft: Option<(usize, usize)>,
    /// The selection is on another cell than the draft's.
    moved: bool,
    /// Why the text cannot be set.
    problem: Option<String>,
    /// The tab's words for rows that take no change.
    why: Option<SharedString>,
    /// Not drawn: the window is too narrow.
    collapsed: bool,
    formatting: Option<Task<()>>,
    _follow: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl ValuePanel {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("text")
                .soft_wrap(true)
                .line_number(false)
        });
        let split = cx.new(|_| ResizableState::default());
        let subscriptions = vec![
            cx.subscribe_in(
                &editor,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.changed(window, cx);
                    }
                },
            ),
            // The width is kept once a drag ends.
            cx.subscribe(&split, |_, split, _: &ResizablePanelEvent, cx| {
                if let Some(width) = split.read(cx).sizes().get(1) {
                    let width = f32::from(*width)
                        .clamp(*VALUE_PANEL_WIDTHS.start(), *VALUE_PANEL_WIDTHS.end());
                    AppState::update_settings(cx, |settings| settings.value_panel_width = width);
                }
            }),
        ];
        Self {
            table: None,
            split,
            editor,
            seen: None,
            target: Target::Nothing,
            shown: None,
            draft: None,
            moved: false,
            problem: None,
            why: None,
            collapsed: false,
            formatting: None,
            _follow: None,
            _subscriptions: subscriptions,
        }
    }

    fn grid(&self) -> Option<Entity<TableState<Grid>>> {
        self.table.as_ref().and_then(WeakEntity::upgrade)
    }

    /// The cell shown, as a row of the grid and a column of the result: what
    /// a section under the value (K18's referenced row) is about.
    pub fn cell(&self) -> Option<(usize, usize)> {
        self.shown.as_ref().map(|shown| (shown.row, shown.source))
    }

    /// Follows `grid` from now on (none: the tab shows no rows), with the
    /// tab's words for rows that take no change.
    fn watch(
        &mut self,
        grid: Option<Entity<TableState<Grid>>>,
        why: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.why != why {
            self.why = why;
            self.seen = None;
        }
        if self.grid() != grid {
            self.keep_draft(window, cx);
            if let Some(old) = self.grid() {
                old.update(cx, |table, _| table.delegate_mut().value_panel = None);
            }
            let me = cx.entity().downgrade();
            self._follow = grid.as_ref().map(|grid| {
                grid.update(cx, |table, _| table.delegate_mut().value_panel = Some(me));
                cx.observe_in(grid, window, |this, _, window, cx| this.follow(window, cx))
            });
            self.table = grid.as_ref().map(Entity::downgrade);
            self.seen = None;
        }
        self.follow(window, cx);
    }

    /// Shows the cell the grid's selection is on, unless typed text waits
    /// on another one. Cheap when nothing changed: the grid calls it on
    /// every change, scrolling included.
    fn follow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (target, generation) = match self.grid() {
            Some(grid) => {
                let state = grid.read(cx);
                let grid = state.delegate();
                let target = match grid.selected(state.selection()) {
                    None => Target::Nothing,
                    Some(selected)
                        if selected.rows.start() == selected.rows.end()
                            && selected.columns.len() == 1 =>
                    {
                        Target::Cell(*selected.rows.start(), selected.columns[0])
                    }
                    Some(_) => Target::Several,
                };
                (target, grid.generation())
            }
            None => (Target::Nothing, 0),
        };
        self.target = target;
        if let Some((row, source)) = self.draft {
            let moved = target != Target::Cell(row, source);
            if moved != self.moved {
                self.moved = moved;
                cx.notify();
            }
            return;
        }
        if self.seen == Some((target, generation)) {
            return;
        }
        self.seen = Some((target, generation));
        self.moved = false;
        self.problem = None;
        self.formatting = None;
        self.shown = None;
        if let (Target::Cell(row, source), Some(grid)) = (target, self.grid()) {
            self.show(&grid, row, source, window, cx);
        }
        cx.notify();
    }

    /// Reads a cell from the grid and lays it out: here when small, off the
    /// UI thread when large.
    fn show(
        &mut self,
        grid: &Entity<TableState<Grid>>,
        row: usize,
        source: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = grid.read(cx);
        let grid = state.delegate();
        let column = grid.store().columns()[source].clone();
        let shown_at = grid.layout().iter().position(|&shown| shown == source);
        let pending = grid
            .pending()
            .and_then(|pending| pending.value(grid.row_count(), row, source));
        let editable = match (pending, shown_at.map(|index| grid.editable(row, index + 1))) {
            (Some(CellValue::Default | CellValue::Expression(_)), _) => Err(
                "Its pending value is the column's default or a SQL expression: change it in \
                 the cell (Enter)."
                    .to_string(),
            ),
            (_, Some(Ok(()))) => Ok(()),
            (_, Some(Err(why))) if why != NOT_EDITABLE => Err(why.to_string()),
            _ => Err(self.why.as_deref().unwrap_or(NOT_EDITABLE).to_string()),
        };
        let value = grid.cell(row, source).map(<[u8]>::to_vec);
        let nullable = grid.nullable(source);
        let large = value
            .as_ref()
            .is_some_and(|value| value.len() > INLINE_BYTES);
        let takes = editable.is_ok();
        self.shown = Some(Shown {
            row,
            source,
            name: column.name.clone(),
            type_name: column.type_name.clone(),
            kind: column.kind,
            prepared: None,
            editable,
            nullable,
        });
        if !large {
            let prepared = prepare(&column, value.as_deref(), takes);
            self.prepared(prepared, window, cx);
            return;
        }
        let laying_out = cx
            .background_executor()
            .spawn(async move { prepare(&column, value.as_deref(), takes) });
        self.formatting = Some(cx.spawn_in(window, async move |this, cx| {
            let prepared = laying_out.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.formatting = None;
                this.prepared(prepared, window, cx);
            });
        }));
    }

    /// Puts a value laid out in the editor.
    fn prepared(&mut self, prepared: Prepared, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = self.shown.as_mut() else {
            return;
        };
        let language = if prepared.view == View::Json {
            "json"
        } else {
            "text"
        };
        let (text, wrap, null) = (
            prepared.text.clone(),
            prepared.view != View::Hex,
            prepared.size.is_none(),
        );
        shown.prepared = Some(prepared);
        self.editor.update(cx, |editor, cx| {
            if editor.language_name() != language {
                editor.set_highlighter(language, cx);
            }
            editor.set_soft_wrap(wrap, window, cx);
            editor.set_placeholder(if null { "NULL" } else { "" }, window, cx);
            editor.set_value(text, window, cx);
        });
        cx.notify();
    }

    /// The text was typed in: it is a draft while it differs from the value.
    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = &self.shown else {
            return;
        };
        let Some(prepared) = &shown.prepared else {
            return;
        };
        let differs = self.editor.read(cx).value().as_ref() != prepared.text;
        self.draft = differs.then_some((shown.row, shown.source));
        self.problem = None;
        if !differs {
            // Typed back to the value: the panel follows the selection again.
            self.moved = false;
            self.follow(window, cx);
        }
        cx.notify();
    }

    /// Keeps the text as the cell's pending value (Set, Cmd+Enter), unless
    /// it is JSON the parser refuses: then it says why and writes nothing.
    fn set(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(grid), Some(shown), Some((row, source))) =
            (self.grid(), &self.shown, self.cell())
        else {
            return;
        };
        let Some(prepared) = shown.prepared.as_ref().filter(|_| shown.editable.is_ok()) else {
            return;
        };
        let typed = self.editor.read(cx).value().to_string();
        match viewer::saved_value(&typed, &prepared.text, &prepared.fetched, shown.kind) {
            Err(problem) => {
                self.problem = Some(problem);
                cx.notify();
            }
            Ok(value) => {
                self.forget_draft();
                if let Some(value) = value {
                    Grid::set_value(&grid, row, source, value, cx);
                }
                self.follow(window, cx);
            }
        }
    }

    /// Makes the cell NULL.
    fn set_null(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(grid), Some(shown)) = (self.grid(), &self.shown) else {
            return;
        };
        if shown.editable.is_ok() && shown.nullable {
            let (row, source) = (shown.row, shown.source);
            self.forget_draft();
            Grid::set_value(&grid, row, source, CellValue::Null, cx);
            self.follow(window, cx);
        }
    }

    /// Drops the typed text: the panel shows the selected cell's value.
    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.forget_draft();
        self.follow(window, cx);
    }

    fn forget_draft(&mut self) {
        self.draft = None;
        self.moved = false;
        self.problem = None;
        self.seen = None;
    }

    /// Escape in the editor: drops the typed text, and the keyboard goes
    /// back to the grid.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.discard(window, cx);
        if let Some(grid) = self.grid() {
            grid.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    /// Lays the JSON typed out on lines (`pretty`) or on one line.
    fn reformat(&mut self, pretty: bool, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.editor.read(cx).value().to_string();
        let rewritten = if pretty {
            kerno_core::format::pretty_json(&typed)
        } else {
            kerno_core::format::compact_json(&typed)
        };
        match rewritten {
            Some(text) => {
                self.editor
                    .update(cx, |editor, cx| editor.set_value(text, window, cx));
                self.changed(window, cx);
            }
            None => {
                self.problem = Some(viewer::json_problem(&typed));
                cx.notify();
            }
        }
    }

    /// The rows the text was typed for are gone (read again, another
    /// result, the panel hidden): the text goes to the clipboard, and a
    /// notification says so.
    fn keep_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft.take().is_none() {
            return;
        }
        let text = self.editor.read(cx).value().to_string();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        window.push_notification(
            Notification::warning(
                "The text typed in the value panel was not set: the rows it was for are no \
                 longer shown. It is on the clipboard.",
            ),
            cx,
        );
        self.moved = false;
        self.seen = None;
    }

    /// Gives the keyboard to the editor (a double click or Shift+Enter on
    /// the grid). Not for a value shown in part, which the dialog shows
    /// whole, nor while the panel is not drawn.
    pub(super) fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let whole = self.shown.as_ref().is_some_and(|shown| {
            shown
                .prepared
                .as_ref()
                .is_none_or(|prepared| !prepared.partial)
        });
        if self.collapsed || !whole {
            return false;
        }
        self.editor
            .update(cx, |editor, cx| editor.focus(window, cx));
        true
    }

    fn render_empty(&self, cx: &App) -> AnyElement {
        let (title, description) = match (self.grid().is_some(), self.target) {
            (false, _) => (
                "No rows",
                "Once rows are shown, the value of the selected cell is shown here.",
            ),
            (true, Target::Several) => (
                "Several cells are selected",
                "Select one cell to see its whole value here.",
            ),
            (true, _) => (
                "No cell selected",
                "Select a cell of the grid to see its whole value here.",
            ),
        };
        div()
            .id("value-panel-empty")
            .test_support()
            .test_text(format!("{title} {description}"))
            .size_full()
            .flex()
            .text_color(cx.theme().muted_foreground)
            .child(
                Empty::new().header(
                    EmptyHeader::new()
                        .title(EmptyTitle::new().child(title))
                        .description(EmptyDescription::new().child(description)),
                ),
            )
            .into_any_element()
    }

    fn render_cell(&self, shown: &Shown, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(prepared) = &shown.prepared else {
            return div()
                .id("value-panel-laying-out")
                .test_support()
                .p_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Laying the value out…")
                .into_any_element();
        };
        let editable = shown.editable.is_ok() && !prepared.partial;
        let body = if prepared.size.is_none() && !editable {
            div()
                .id("value-panel-null")
                .test_support()
                .p_3()
                .italic()
                .text_color(theme.muted_foreground)
                .child("NULL")
                .into_any_element()
        } else {
            Editor::new(&self.editor)
                .h_full()
                .bordered(false)
                .appearance(false)
                .readonly(!editable)
                .into_any_element()
        };
        let read_only = match (&shown.editable, prepared.partial) {
            (Err(why), _) => Some(why.clone()),
            (Ok(()), true) => Some(
                "Too long to edit here: only its start is shown. “Edit value” in the cell's \
                 menu shows it whole."
                    .to_string(),
            ),
            (Ok(()), false) => None,
        };
        let json = shown.kind == TypeKind::Json;
        v_flex()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .id("value-panel-body")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .px_1()
                    .child(body),
            )
            .children(
                viewer::hint_line(prepared.hint.clone(), cx)
                    .map(|hint| div().px_3().pt_1().child(hint)),
            )
            .children(self.problem.clone().map(|problem| {
                div()
                    .id("value-panel-problem")
                    .test_support()
                    .px_3()
                    .pt_1()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(SharedString::from(problem))
            }))
            .children(read_only.map(|why| {
                div()
                    .id("value-panel-why")
                    .test_support()
                    .px_3()
                    .pt_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(why))
            }))
            .when(editable, |panel| {
                panel.child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .px_2()
                        .py_2()
                        .when(json, |bar| {
                            bar.child(
                                Button::new("value-panel-pretty")
                                    .ghost()
                                    .xsmall()
                                    .label("Format")
                                    .tooltip("Lay the JSON out on lines")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.reformat(true, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("value-panel-compact")
                                    .ghost()
                                    .xsmall()
                                    .label("Compact")
                                    .tooltip("Put the JSON on one line")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.reformat(false, window, cx)
                                    })),
                            )
                        })
                        .child(div().flex_1())
                        .child(
                            Button::new("value-panel-null-button")
                                .outline()
                                .xsmall()
                                .label("Set NULL")
                                .disabled(!shown.nullable)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.set_null(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("value-panel-discard")
                                .outline()
                                .xsmall()
                                .label("Discard")
                                .tooltip("Drop the typed text (Escape)")
                                .disabled(self.draft.is_none())
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.discard(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("value-panel-set")
                                .primary()
                                .xsmall()
                                .label("Set")
                                .tooltip("Keep the text as a pending change (⌘Enter)")
                                .on_click(cx.listener(|this, _, window, cx| this.set(window, cx))),
                        ),
                )
            })
            // K18: sections about the cell (the row a foreign key points to)
            // go here, under the value.
            .into_any_element()
    }
}

impl Render for ValuePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, warning, border) = (theme.muted_foreground, theme.warning, theme.border);
        let header = self.shown.as_ref().map(|shown| {
            let status = shown.prepared.as_ref().map(status);
            h_flex()
                .flex_none()
                .gap_2()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(border)
                .text_sm()
                .child(
                    div()
                        .id("value-panel-header")
                        .test_support()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(format!(
                            "{} · {}",
                            shown.name, shown.type_name
                        ))),
                )
                .children(status.map(|status| {
                    div()
                        .id("value-panel-status")
                        .test_support()
                        .flex_none()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(status))
                }))
        });
        let unsaved = self.draft.map(|(row, _)| {
            let line = if self.moved {
                format!(
                    "Not set yet: this text is for row {}, not the selected cell. Set it, or \
                     press Escape to drop it.",
                    row + 1
                )
            } else {
                "Not set yet: Set (⌘Enter) keeps it as a pending change; Escape drops it."
                    .to_string()
            };
            div()
                .id("value-panel-unsaved")
                .test_support()
                .test_text(line.clone())
                .flex_none()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(warning)
                .child(SharedString::from(line))
        });
        let body = match &self.shown {
            Some(shown) => self.render_cell(shown, cx),
            None => self.render_empty(cx),
        };
        v_flex()
            .id("value-panel")
            .test_support()
            .size_full()
            .min_w_0()
            .bg(cx.theme().background)
            // The editor's keys: Cmd+Enter sets the text (it is "Run
            // statement" everywhere else), Escape drops it and goes back to
            // the grid.
            .capture_action(cx.listener(|this, _: &actions::RunStatement, window, cx| {
                this.set(window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, enter: &input::Enter, window, cx| {
                if enter.secondary {
                    this.set(window, cx);
                    cx.stop_propagation();
                }
            }))
            // Only from the editor itself: Escape in its find field closes
            // the find field, and keeps the text.
            .capture_action(cx.listener(|this, _: &input::Escape, window, cx| {
                if this.editor.read(cx).focus_handle(cx).is_focused(window) {
                    this.escape(window, cx);
                    cx.stop_propagation();
                }
            }))
            .children(header)
            .children(unsaved)
            .child(body)
    }
}

/// What the tests read of a panel.
#[cfg(test)]
impl ValuePanel {
    /// The header: the column's name and type.
    pub fn header(&self) -> Option<String> {
        let shown = self.shown.as_ref()?;
        Some(format!("{} · {}", shown.name, shown.type_name))
    }

    /// NULL, an empty value, or its size.
    pub fn status(&self) -> Option<String> {
        self.shown.as_ref()?.prepared.as_ref().map(status)
    }

    /// What the editor holds.
    pub fn text(&self, cx: &App) -> String {
        self.editor.read(cx).value().to_string()
    }

    /// Whether the editor holds JSON, highlighted as such.
    pub fn highlights_json(&self, cx: &App) -> bool {
        self.editor.read(cx).language_name().as_ref() == "json"
    }

    /// Why the text was not set.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Why the cell takes no value here.
    pub fn why_read_only(&self) -> Option<&str> {
        self.shown
            .as_ref()?
            .editable
            .as_ref()
            .err()
            .map(String::as_str)
    }

    /// Whether typed text waits to be set.
    pub fn has_draft(&self) -> bool {
        self.draft.is_some()
    }

    /// Whether the editor has the keyboard.
    pub fn editor_focused(&self, window: &Window, cx: &App) -> bool {
        self.editor.read(cx).focus_handle(cx).is_focused(window)
    }

    /// The grid's and the panel's widths.
    pub fn split(&self) -> Entity<ResizableState> {
        self.split.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(kind: TypeKind) -> Column {
        Column::new("c", "t", kind)
    }

    /// K17: each kind of value as the panel lays it out.
    #[test]
    fn values_are_laid_out_as_the_viewer_lays_them_out() {
        // JSON indented, for a JSON column, editable or not.
        let json = prepare(&column(TypeKind::Json), Some(br#"{"a":[1,2]}"#), true);
        assert_eq!(json.view, View::Json);
        assert_eq!(json.text, "{\n  \"a\": [\n    1,\n    2\n  ]\n}");
        assert_eq!(json.fetched, r#"{"a":[1,2]}"#);
        assert_eq!(status(&json), "11 B");
        // A text holding a document: indented when only shown, as stored
        // when it can be edited.
        let doc = br#"{"a":1}"#;
        assert_eq!(
            prepare(&column(TypeKind::Text), Some(doc), false).view,
            View::Json
        );
        let typed = prepare(&column(TypeKind::Text), Some(doc), true);
        assert_eq!(
            (typed.view, typed.text.as_str()),
            (View::Text, r#"{"a":1}"#)
        );
        // NULL is not the empty string.
        let null = prepare(&column(TypeKind::Text), None, true);
        assert_eq!((status(&null), null.text.as_str()), ("NULL".into(), ""));
        let empty = prepare(&column(TypeKind::Text), Some(b""), true);
        assert_eq!(status(&empty), "Empty string");
        // Binary: a hex dump, never edited here.
        let binary = prepare(&column(TypeKind::Binary), Some(&[0x00, 0xff]), false);
        assert_eq!(binary.view, View::Hex);
        assert!(
            binary.text.starts_with("00000000  00 ff"),
            "{}",
            binary.text
        );
        assert_eq!(binary.fetched, "");
        // A Unix time says when.
        let time = prepare(&column(TypeKind::Integer), Some(b"1794619425"), true);
        assert_eq!(
            time.hint.as_deref(),
            Some("Unix seconds \u{2192} 2026-11-14 01:23:45 UTC")
        );
        // Too long: its start, said in the status, and not editable.
        let long = "x".repeat(MAX_SHOWN_BYTES + 10);
        let cut = prepare(&column(TypeKind::Text), Some(long.as_bytes()), true);
        assert!(cut.partial && cut.text.len() == MAX_SHOWN_BYTES);
        assert_eq!(status(&cut), "256 kB; the first 256 kB are shown");
        let bytes = vec![7u8; MAX_SHOWN_BYTES];
        assert!(prepare(&column(TypeKind::Binary), Some(&bytes), false).partial);
    }
}

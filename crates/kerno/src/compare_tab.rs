//! The schema compare (K30; NAV-57, CM-21, CM-59, TOOL-22): a tab that
//! compares a source schema (the model) with a target schema (what the
//! script changes), lists the objects with their state, says what differs
//! in plain words with both definitions side by side, and opens the script
//! that brings the target to the source in a query tab on the target.
//! Nothing runs from here: the script is read and run there, under the
//! connection's rules. The diff is `kerno_core::compare` (D-103).

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    ParentElement as _, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, Window, div, px,
};
use kerno_core::compare::{self, Item, Kind, Progress, State as ItemState};
use kerno_core::schema::ObjectRef;
use kerno_core::{Error, TxState, describe};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::engine_logo::engine_logo;
use crate::query::tab::TabStatus;
use crate::tabs::Target;

/// The two sides of a comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sides {
    /// The model.
    pub source: Target,
    /// What the script changes.
    pub target: Target,
}

/// The schema a side compares: its schema, or its database on engines
/// without schemas.
pub fn compared_schema(target: &Target) -> String {
    target
        .schema
        .clone()
        .or_else(|| target.database.clone())
        .unwrap_or_default()
}

/// What the tab asks of the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareTabEvent {
    /// Open `sql` in a new query tab on `target`; nothing runs.
    OpenScript {
        target: Target,
        title: String,
        sql: String,
    },
    /// Show `object` of the side `target`: its structure for a table or a
    /// view, its DDL in a query tab otherwise.
    Open {
        target: Target,
        object: ObjectRef,
        ddl: String,
    },
}

/// Which objects the list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Differences,
    OnlyInSource,
    OnlyInTarget,
    Different,
    Same,
}

impl Filter {
    pub const ALL: [Filter; 5] = [
        Filter::Differences,
        Filter::OnlyInSource,
        Filter::OnlyInTarget,
        Filter::Different,
        Filter::Same,
    ];

    fn label(self) -> &'static str {
        match self {
            Filter::Differences => "All differences",
            Filter::OnlyInSource => "Only in source",
            Filter::OnlyInTarget => "Only in target",
            Filter::Different => "Different",
            Filter::Same => "Same",
        }
    }

    pub fn lets(self, state: ItemState) -> bool {
        match self {
            Filter::Differences => state != ItemState::Same,
            Filter::OnlyInSource => state == ItemState::OnlyInSource,
            Filter::OnlyInTarget => state == ItemState::OnlyInTarget,
            Filter::Different => state == ItemState::Different,
            Filter::Same => state == ItemState::Same,
        }
    }
}

/// The items `filter` and the search `query` (lower case) let through.
pub fn shown(items: &[Item], filter: Filter, query: &str) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| filter.lets(item.state))
        .filter(|(_, item)| query.is_empty() || item.name.to_lowercase().contains(query))
        .map(|(index, _)| index)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// A connection is not open, or the tab is not shown yet.
    Waiting,
    Loading,
    Ready,
    Failed(String),
}

/// An item's identity across refreshes.
type Key = (Kind, String);

fn key(item: &Item) -> Key {
    (item.kind, item.name.clone())
}

/// The compare of two schemas.
pub struct CompareTab {
    focus_handle: FocusHandle,
    connections: Entity<Connections>,
    sides: Sides,
    hidden: bool,
    state: State,
    /// Per side: objects read, out of how many.
    progress: [Progress; 2],
    items: Vec<Item>,
    /// Items left out of the script, by kind and name.
    excluded: HashSet<Key>,
    filter: Filter,
    search: Entity<InputState>,
    query: String,
    selected: Option<Key>,
    /// The selected item's two definitions, read-only.
    ddl: Option<(Entity<EditorState>, Entity<EditorState>)>,
    /// Write the drops of what only the target has.
    drops: bool,
    stops: [bridge::Stop; 2],
    cancel: Arc<AtomicBool>,
    generation: u64,
    load: Vec<Task<()>>,
    list_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CompareTabEvent> for CompareTab {}

impl CompareTab {
    pub fn new(
        sides: Sides,
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search names"));
        let typed = cx.subscribe(&search, |this, input, event, cx| {
            if let InputEvent::Change = event {
                this.query = input.read(cx).value().to_lowercase();
                cx.notify();
            }
        });
        let follow = cx.observe_in(&connections, window, |this, _, window, cx| {
            match (&this.state, this.connected(cx)) {
                (State::Waiting, true) => this.reload(window, cx),
                (State::Waiting, false) | (_, true) => {}
                (_, false) => {
                    this.stop_loading(cx);
                    this.state = State::Waiting;
                    cx.notify();
                }
            }
        });
        let mut tab = Self {
            focus_handle: cx.focus_handle(),
            connections,
            sides,
            hidden: false,
            state: State::Waiting,
            progress: [Progress::default(); 2],
            items: Vec::new(),
            excluded: HashSet::new(),
            filter: Filter::Differences,
            search,
            query: String::new(),
            selected: None,
            ddl: None,
            drops: false,
            stops: Default::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            generation: 0,
            load: Vec::new(),
            list_scroll: ScrollHandle::new(),
            _subscriptions: vec![typed, follow],
        };
        tab.reload(window, cx);
        tab
    }

    pub fn sides(&self) -> &Sides {
        &self.sides
    }

    /// The target: what the tab acts on (its environment, its status).
    pub fn target(&self) -> &Target {
        &self.sides.target
    }

    pub fn title(&self) -> SharedString {
        SharedString::from(format!(
            "{} ⇄ {} · compare",
            compared_schema(&self.sides.source),
            compared_schema(&self.sides.target)
        ))
    }

    fn side(&self, index: usize) -> &Target {
        if index == 0 {
            &self.sides.source
        } else {
            &self.sides.target
        }
    }

    fn connected(&self, cx: &App) -> bool {
        [&self.sides.source, &self.sides.target].iter().all(|side| {
            matches!(
                self.connections.read(cx).status(&side.connection),
                Status::Connected { .. }
            )
        })
    }

    fn connection_name(&self, side: &Target, cx: &App) -> String {
        self.connections
            .read(cx)
            .get(&side.connection)
            .map(|saved| saved.config.name.clone())
            .unwrap_or_default()
    }

    /// `connection · database.schema` (or `connection · database`).
    pub fn side_label(&self, index: usize, cx: &App) -> String {
        let side = self.side(index);
        let place = match (&side.database, &side.schema) {
            (Some(database), Some(schema)) => format!("{database}.{schema}"),
            _ => compared_schema(side),
        };
        format!("{} · {place}", self.connection_name(side, cx))
    }

    fn read_only(&self, cx: &App) -> bool {
        self.connections
            .read(cx)
            .get(&self.sides.target.connection)
            .is_some_and(|saved| saved.config.read_only)
    }

    pub fn status(&self, cx: &App) -> TabStatus {
        TabStatus {
            connection: Some(self.connection_name(&self.sides.target, cx))
                .filter(|name| !name.is_empty()),
            read_only: self.read_only(cx),
            database: self.sides.target.database.clone(),
            transaction: TxState::Idle,
            running: self.state == State::Loading,
            rows: None,
            elapsed: None,
            notices: 0,
            pending: 0,
        }
    }

    /// `waiting`, `loading`, `ready` or the error; for tests.
    #[cfg(test)]
    pub fn state_text(&self) -> String {
        match &self.state {
            State::Waiting => "waiting".into(),
            State::Loading => "loading".into(),
            State::Ready => "ready".into(),
            State::Failed(message) => message.clone(),
        }
    }

    #[cfg(test)]
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    // Reading -----------------------------------------------------------------

    pub fn defer_until_shown(&mut self, cx: &mut Context<Self>) {
        self.hidden = true;
        self.stop_loading(cx);
        self.state = State::Waiting;
        cx.notify();
    }

    pub fn reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.hidden) {
            self.reload(window, cx);
        }
    }

    fn stop_loading(&mut self, cx: &App) {
        self.generation += 1;
        self.cancel.store(true, Ordering::Release);
        for stop in &self.stops {
            stop.stop(cx);
        }
        self.stops = Default::default();
        self.load.clear();
    }

    /// Stops both reads (Cmd+., Cancel, the tab closed), on the servers too.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.state == State::Loading {
            self.stop_loading(cx);
            self.state = State::Failed("Cancelled: the schemas were not compared.".into());
            cx.notify();
        }
    }

    /// Reads both sides again, at the same time, each on a session of its
    /// own, then compares them off the UI thread.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_loading(cx);
        let generation = self.generation;
        if self.hidden || !self.connected(cx) {
            self.state = State::Waiting;
            cx.notify();
            return;
        }
        self.state = State::Loading;
        self.progress = [Progress::default(); 2];
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let mut reads = Vec::new();
        for index in 0..2 {
            let side = self.side(index).clone();
            let schema = compared_schema(&side);
            let stop = self.stops[index].clone();
            let opening =
                self.connections
                    .read(cx)
                    .open_session(&side.connection, side.database.clone(), cx);
            let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            self.load.push(cx.spawn(async move |this, cx| {
                while let Some(progress) = receiver.recv().await {
                    let _ = this.update(cx, |this, cx| {
                        if this.generation == generation {
                            this.progress[index] = progress;
                            cx.notify();
                        }
                    });
                }
            }));
            let cancel = cancel.clone();
            reads.push(cx.spawn(async move |_, cx| {
                let session = opening.await?;
                let reading = cx.update(|cx| {
                    stop.run(cx, session, move |session| async move {
                        let tell = move |progress| {
                            let _ = sender.send(progress);
                        };
                        compare::load(session.clone(), &schema, tell, &cancel)
                            .await
                            .map(|side| (side, session))
                    })
                });
                reading.await.unwrap_or(Err(Error::Cancelled))
            }));
        }
        let target = reads.pop().expect("two reads");
        let source = reads.pop().expect("two reads");
        self.load.push(cx.spawn_in(window, async move |this, cx| {
            let failed = |this: &mut Self, which: &str, error: Error, cx: &mut Context<Self>| {
                if this.generation != generation || matches!(error, Error::Cancelled) {
                    return;
                }
                this.stop_loading(cx);
                this.state = State::Failed(format!(
                    "The {which} could not be read. Refresh to try again.\n{}",
                    describe(&error)
                ));
                cx.notify();
            };
            let source = match source.await {
                Ok(read) => read,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| failed(this, "source", error, cx));
                    return;
                }
            };
            let target = match target.await {
                Ok(read) => read,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| failed(this, "target", error, cx));
                    let _ = cx.update(|_, cx| {
                        bridge::spawn(cx, async move { source.1.close().await }).detach()
                    });
                    return;
                }
            };
            let computing = cx.update(|_, cx| {
                bridge::spawn(cx, async move {
                    let ((a, source), (b, target)) = (source, target);
                    let (from, to) = (source.clone(), target.clone());
                    let items = tokio::task::spawn_blocking(move || {
                        compare::compare(&a, &b, from.dialect(), to.dialect())
                    })
                    .await
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic.into_panic()));
                    source.close().await;
                    target.close().await;
                    items
                })
            });
            let Ok(computing) = computing else {
                return;
            };
            let items = computing.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation == generation {
                    this.show(items, window, cx);
                }
            });
        }));
        cx.notify();
    }

    fn show(&mut self, items: Vec<Item>, window: &mut Window, cx: &mut Context<Self>) {
        self.items = items;
        self.state = State::Ready;
        let known: HashSet<Key> = self.items.iter().map(key).collect();
        self.excluded.retain(|key| known.contains(key));
        let selected = self.selected.take().filter(|key| known.contains(key));
        match selected {
            Some((kind, name)) => self.select_key((kind, name), window, cx),
            None => self.ddl = None,
        }
        cx.notify();
    }

    /// Exchanges the source and the target and compares again.
    pub fn swap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sides = &mut self.sides;
        std::mem::swap(&mut sides.source, &mut sides.target);
        self.reload(window, cx);
    }

    // The list ----------------------------------------------------------------

    pub fn shown(&self) -> Vec<usize> {
        shown(&self.items, self.filter, &self.query)
    }

    /// How many objects the search lets through under `filter`.
    pub fn count(&self, filter: Filter) -> usize {
        shown(&self.items, filter, &self.query).len()
    }

    pub fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        cx.notify();
    }

    #[cfg(test)]
    pub fn search_for(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.set_value(text, window, cx));
        self.query = text.to_lowercase();
        cx.notify();
    }

    fn index_of(&self, key: &Key) -> Option<usize> {
        self.items.iter().position(|item| &self::key(item) == key)
    }

    pub fn selected(&self) -> Option<&Item> {
        self.selected
            .as_ref()
            .and_then(|key| self.index_of(key))
            .map(|index| &self.items[index])
    }

    fn select_key(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.index_of(&key) else {
            return;
        };
        if self.selected.as_ref() != Some(&key) || self.ddl.is_none() {
            let item = &self.items[index];
            let editor = |text: &str, window: &mut Window, cx: &mut Context<Self>| {
                let text = text.to_string();
                cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language("sql")
                        .line_number(true)
                        .soft_wrap(false)
                        .default_value(text)
                })
            };
            let (source, target) = (item.source_ddl.clone(), item.target_ddl.clone());
            self.ddl = Some((editor(&source, window, cx), editor(&target, window, cx)));
        }
        self.selected = Some(key);
        if let Some(at) = self.shown().iter().position(|&shown| shown == index) {
            self.list_scroll.scroll_to_item(at);
        }
        cx.notify();
    }

    fn step_selection(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.shown();
        if shown.is_empty() {
            return;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|key| self.index_of(key))
            .and_then(|index| shown.iter().position(|&i| i == index));
        let next = match at {
            None if step > 0 => 0,
            None => shown.len() - 1,
            Some(at) => (at as isize + step).clamp(0, shown.len() as isize - 1) as usize,
        };
        let key = key(&self.items[shown[next]]);
        self.select_key(key, window, cx);
    }

    pub fn is_included(&self, index: usize) -> bool {
        self.items
            .get(index)
            .is_some_and(|item| !self.excluded.contains(&key(item)))
    }

    /// Puts the object in the script, or leaves it out.
    pub fn set_included(&mut self, name: &str, included: bool, cx: &mut Context<Self>) {
        if let Some(item) = self.items.iter().find(|item| item.name == name) {
            if included {
                self.excluded.remove(&key(item));
            } else {
                self.excluded.insert(key(item));
            }
            cx.notify();
        }
    }

    /// All (or none) of the objects shown go into the script.
    pub fn include_shown(&mut self, included: bool, cx: &mut Context<Self>) {
        for index in self.shown() {
            let key = key(&self.items[index]);
            if included {
                self.excluded.remove(&key);
            } else {
                self.excluded.insert(key);
            }
        }
        cx.notify();
    }

    pub fn set_drops(&mut self, drops: bool, cx: &mut Context<Self>) {
        self.drops = drops;
        cx.notify();
    }

    /// Whether any object differs: an empty comparison has no script.
    fn differs(&self) -> bool {
        self.items.iter().any(|item| item.state != ItemState::Same)
    }

    /// The script of the objects ticked, with what it is about.
    pub fn script(&self, cx: &App) -> String {
        let body = compare::script(&self.items, |index| self.is_included(index), self.drops);
        // The connections' names are the user's: commented line by line.
        let heading = compare::commented(&format!(
            "Schema compare: brings {} to {}.\nNothing has run: read the script, then run it \
             here, on the target.",
            self.side_label(1, cx),
            self.side_label(0, cx)
        ));
        format!("{heading}\n\n{body}")
    }

    /// Opens the script in a new query tab on the target; nothing runs.
    pub fn open_script(&mut self, cx: &mut Context<Self>) {
        if self.state != State::Ready || !self.differs() {
            return;
        }
        let sql = self.script(cx);
        let target = self.sides.target.clone();
        cx.emit(CompareTabEvent::OpenScript {
            title: format!(
                "Bring {} to {}",
                compared_schema(&target),
                compared_schema(&self.sides.source)
            ),
            target,
            sql,
        });
    }

    /// Shows the object `name` on the side `index` (0: source).
    pub fn open_object(&mut self, name: &str, index: usize, cx: &mut Context<Self>) {
        let Some(item) = self.items.iter().find(|item| item.name == name) else {
            return;
        };
        let (object, ddl) = if index == 0 {
            (item.source.clone(), item.source_ddl.clone())
        } else {
            (item.target.clone(), item.target_ddl.clone())
        };
        if let Some(object) = object {
            cx.emit(CompareTabEvent::Open {
                target: self.side(index).clone(),
                object,
                ddl,
            });
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "down" => self.step_selection(1, window, cx),
            "up" => self.step_selection(-1, window, cx),
            "f" if keystroke.modifiers.secondary(cx) => {
                self.search
                    .update(cx, |search, cx| search.focus(window, cx));
            }
            "space" if self.focus_handle.is_focused(window) => {
                if let Some(item) = self.selected().cloned() {
                    let included = !self.excluded.contains(&key(&item));
                    self.set_included(&item.name, !included, cx);
                }
            }
            "escape" => self.focus_handle.focus(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    // View --------------------------------------------------------------------

    fn render_side(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let side = self.side(index);
        let engine = self
            .connections
            .read(cx)
            .get(&side.connection)
            .map(|saved| saved.config.engine);
        let label = self.side_label(index, cx);
        let (role, id) = if index == 0 {
            ("Source", "compare-source")
        } else {
            ("Target", "compare-target")
        };
        h_flex()
            .gap_1()
            .min_w_0()
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(role),
            )
            .children(
                engine.map(|engine| div().flex_none().child(engine_logo(engine, px(14.), cx))),
            )
            .child(
                div()
                    .id(id)
                    .test_support()
                    .text_sm()
                    .font_semibold()
                    .truncate()
                    .test_text(label.clone())
                    .child(SharedString::from(label)),
            )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ready = self.state == State::Ready;
        h_flex()
            .flex_none()
            .flex_wrap()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(self.render_side(0, cx))
            .child(
                Button::new("compare-swap")
                    .ghost()
                    .small()
                    .label("⇄ Swap")
                    .tooltip("Make the target the model, and compare again")
                    .on_click(cx.listener(|this, _, window, cx| this.swap(window, cx))),
            )
            .child(self.render_side(1, cx))
            .child(div().flex_1())
            .child(
                Button::new("compare-refresh")
                    .ghost()
                    .small()
                    .label("Refresh")
                    .tooltip("Read both schemas again")
                    .disabled(self.state == State::Loading)
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
            )
            .child(
                Button::new("compare-open-script")
                    .primary()
                    .small()
                    .label("Open script")
                    .tooltip("Open the script in a query tab on the target: nothing runs")
                    .disabled(!ready || !self.differs())
                    .on_click(cx.listener(|this, _, _, cx| this.open_script(cx))),
            )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .flex_wrap()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .children(Filter::ALL.into_iter().enumerate().map(|(index, filter)| {
                let label = format!("{} {}", filter.label(), self.count(filter));
                Button::new(("compare-filter", index))
                    .small()
                    .map(|button| {
                        if self.filter == filter {
                            button.primary()
                        } else {
                            button.ghost()
                        }
                    })
                    .label(SharedString::from(label))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx)))
            }))
            .child(div().flex_1())
            .child(
                div().w(px(200.)).min_w(px(120.)).child(
                    Input::new(&self.search)
                        .small()
                        .id("compare-search")
                        .cleanable(true),
                ),
            )
            .child(
                Button::new("compare-all")
                    .ghost()
                    .small()
                    .label("All")
                    .tooltip("Put every object shown in the script")
                    .on_click(cx.listener(|this, _, _, cx| this.include_shown(true, cx))),
            )
            .child(
                Button::new("compare-none")
                    .ghost()
                    .small()
                    .label("None")
                    .tooltip("Leave every object shown out of the script")
                    .on_click(cx.listener(|this, _, _, cx| this.include_shown(false, cx))),
            )
            .child(
                Checkbox::new("compare-drops")
                    .label("Also drop what only the target has")
                    .checked(self.drops)
                    .on_click(
                        cx.listener(|this, checked: &bool, _, cx| this.set_drops(*checked, cx)),
                    ),
            )
    }

    /// Progress, errors and the read-only target, under the toolbar.
    fn render_notes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let loading = self.state == State::Loading;
        let reason = "The target connection is read-only: the script opens, and its statements \
                      will be refused there.";
        v_flex()
            .flex_none()
            .px_3()
            .gap_1()
            .text_xs()
            .when(loading, |this| {
                this.child(
                    h_flex()
                        .py_1()
                        .gap_3()
                        .children((0..2).map(|index| {
                            let progress = self.progress[index];
                            let text = format!(
                                "{}: {}",
                                if index == 0 { "Source" } else { "Target" },
                                if progress.total == 0 {
                                    "listing the objects…".to_string()
                                } else {
                                    format!(
                                        "{} of {} objects read",
                                        crate::query::plan_view::number(progress.done as f64),
                                        crate::query::plan_view::number(progress.total as f64)
                                    )
                                }
                            );
                            div()
                                .id(("compare-progress", index))
                                .test_support()
                                .test_text(text.clone())
                                .child(SharedString::from(text))
                        }))
                        .child(
                            Button::new("compare-cancel")
                                .ghost()
                                .xsmall()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                        ),
                )
            })
            .when(self.read_only(cx), |this| {
                this.child(
                    div()
                        .id("compare-read-only")
                        .test_support()
                        .py_1()
                        .text_color(theme.warning)
                        .test_text(reason)
                        .child(reason),
                )
            })
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let item = &self.items[index];
        let selected = self.selected.as_ref() == Some(&key(item));
        let (mark, words) = item.state.mark();
        let color = match item.state {
            ItemState::OnlyInSource => theme.success,
            ItemState::OnlyInTarget => theme.danger,
            ItemState::Different => theme.warning,
            ItemState::Same => theme.muted_foreground,
        };
        let name = item.name.clone();
        let (click_key, press_key, menu_name, tick_name) =
            (key(item), key(item), name.clone(), name.clone());
        let view = cx.entity();
        let hover = theme.list_hover;
        let included = self.is_included(index);
        h_flex()
            .id(("compare-row", index))
            .test_support()
            .test_text(format!("{name} {words}"))
            .gap_2()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .cursor_pointer()
            .map(|row| {
                if selected {
                    row.bg(theme.list_active)
                } else {
                    row.hover(move |row| row.bg(hover))
                }
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_key(click_key.clone(), window, cx);
                this.focus_handle.focus(window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    this.select_key(press_key.clone(), window, cx)
                }),
            )
            .context_menu(move |menu, _, cx| view.read(cx).row_menu(&menu_name, menu, &view))
            .when(item.state != ItemState::Same, |row| {
                row.child(
                    Checkbox::new(("compare-include", index))
                        .checked(included)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.set_included(&tick_name, *checked, cx)
                        })),
                )
            })
            .child(
                div()
                    .flex_none()
                    .w(px(14.))
                    .text_color(color)
                    .font_weight(FontWeight::BOLD)
                    .child(mark),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_sm()
                    .child(SharedString::from(name)),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(words),
            )
            .into_any_element()
    }

    fn row_menu(&self, name: &str, menu: PopupMenu, view: &Entity<Self>) -> PopupMenu {
        let Some(item) = self.items.iter().find(|item| item.name == name).cloned() else {
            return menu;
        };
        let open = |menu: PopupMenu, label: &'static str, index: usize, there: bool| {
            let (view, name) = (view.clone(), item.name.clone());
            menu.item(
                PopupMenuItem::new(label)
                    .disabled(!there)
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| this.open_object(&name, index, cx))
                    }),
            )
        };
        let menu = open(menu, "Open in source", 0, item.source.is_some());
        let menu = open(menu, "Open in target", 1, item.target.is_some());
        let copied = item.name.clone();
        let menu = menu
            .separator()
            .item(PopupMenuItem::new("Copy name").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
            }));
        if item.state == ItemState::Same {
            return menu;
        }
        let included = !self.excluded.contains(&key(&item));
        let (view, name) = (view.clone(), item.name.clone());
        menu.item(
            PopupMenuItem::new(if included {
                "Leave out of the script"
            } else {
                "Put in the script"
            })
            .on_click(move |_, _, cx| {
                view.update(cx, |this, cx| this.set_included(&name, !included, cx))
            }),
        )
    }

    fn notice(id: &'static str, text: String, cx: &App) -> AnyElement {
        div()
            .id(id)
            .test_support()
            .p_4()
            .text_sm()
            .whitespace_normal()
            .text_color(cx.theme().muted_foreground)
            .test_text(text.clone())
            .child(SharedString::from(text))
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.state {
            State::Waiting => {
                return Self::notice(
                    "compare-waiting",
                    "Connect both connections to compare their schemas.".into(),
                    cx,
                );
            }
            State::Loading => {
                return Self::notice("compare-loading", "Reading both schemas…".into(), cx);
            }
            State::Failed(message) => {
                return Self::notice("compare-error", message.clone(), cx);
            }
            State::Ready => {}
        }
        if !self.differs() && self.filter != Filter::Same {
            return Self::notice(
                "compare-identical",
                "The two schemas are identical.".into(),
                cx,
            );
        }
        let shown = self.shown();
        if shown.is_empty() {
            let text = if self.query.is_empty() {
                "No object is in this state.".to_string()
            } else {
                format!("No object matches “{}”.", self.search.read(cx).value())
            };
            return Self::notice("compare-empty", text, cx);
        }
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let mut rows = Vec::new();
        for kind in Kind::ALL {
            let of_kind: Vec<usize> = shown
                .iter()
                .copied()
                .filter(|&index| self.items[index].kind == kind)
                .collect();
            if of_kind.is_empty() {
                continue;
            }
            rows.push(
                div()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .border_b_1()
                    .border_color(border)
                    .text_xs()
                    .font_semibold()
                    .text_color(muted)
                    .child(SharedString::from(format!(
                        "{} {}",
                        kind.plural(),
                        of_kind.len()
                    )))
                    .into_any_element(),
            );
            for index in of_kind {
                rows.push(self.render_row(index, cx));
            }
        }
        div()
            .id("compare-list")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .children(rows)
            .into_any_element()
    }

    fn render_details(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(item) = self.selected().cloned() else {
            return Self::notice(
                "compare-details-empty",
                "Select an object to see what differs.".into(),
                cx,
            );
        };
        let renamed = matches!(
            item.state,
            ItemState::OnlyInSource | ItemState::OnlyInTarget
        )
        .then_some(
            "If it was renamed, the compare cannot tell: the script creates it under its new \
             name and drops the old one (when drops are asked for).",
        );
        let differences = item.differences.join("\n");
        let pane = |title: &'static str,
                    editor: Option<&Entity<EditorState>>,
                    empty: &'static str,
                    id: &'static str| {
            v_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme.muted_foreground)
                        .child(title),
                )
                .child(div().id(id).test_support().flex_1().min_h_0().map(|this| {
                    match editor {
                        Some(editor) => this.child(
                            Editor::new(editor)
                                .readonly(true)
                                .h_full()
                                .bordered(false)
                                .appearance(false),
                        ),
                        None => this
                            .p_2()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(empty),
                    }
                }))
        };
        let (source, target) = match &self.ddl {
            Some((source, target)) => (Some(source), Some(target)),
            None => (None, None),
        };
        let source = source.filter(|_| item.source.is_some());
        let target = target.filter(|_| item.target.is_some());
        v_flex()
            .id("compare-details")
            .size_full()
            .child(
                v_flex()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().font_semibold().child(SharedString::from(format!(
                        "{} {} · {}",
                        item.kind.label(),
                        item.name,
                        item.state.mark().1
                    ))))
                    .when(!differences.is_empty(), |this| {
                        this.child(
                            div()
                                .id("compare-differences")
                                .test_support()
                                .max_h(px(160.))
                                .overflow_y_scroll()
                                .text_sm()
                                .whitespace_normal()
                                .test_text(differences.clone())
                                .children(item.differences.iter().map(|difference| {
                                    div().child(SharedString::from(format!("• {difference}")))
                                })),
                        )
                    })
                    .children(renamed.map(|text| {
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .text_color(theme.muted_foreground)
                            .child(text)
                    })),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(pane(
                        "Source",
                        source,
                        "Not in the source.",
                        "compare-source-ddl",
                    ))
                    .child(div().w(px(1.)).h_full().bg(theme.border))
                    .child(pane(
                        "Target",
                        target,
                        "Not in the target.",
                        "compare-target-ddl",
                    )),
            )
            .into_any_element()
    }
}

impl Focusable for CompareTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CompareTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let list = self.render_list(cx);
        let details = self.render_details(cx);
        v_flex()
            .id("compare-tab")
            .track_focus(&self.focus_handle)
            .size_full()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.key_down(event, window, cx)
            }))
            .child(self.render_header(cx))
            .child(self.render_toolbar(cx))
            .child(self.render_notes(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().w(px(340.)).flex_none().h_full().child(list))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .border_l_1()
                            .border_color(border)
                            .child(details),
                    ),
            )
    }
}

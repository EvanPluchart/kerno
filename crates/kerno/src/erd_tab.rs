//! The ER diagram tab (CLAUDE.md §4 J18; DBeaver #36058, Beekeeper #492,
//! #1160): the tables of a schema, or a table and its neighbours, drawn
//! with their keys and the foreign keys between them. Pan by dragging the
//! background or scrolling, zoom with ⌘-scroll, a pinch or the toolbar;
//! a table can be dragged (for this session only); a double click or Enter
//! opens its data. Exports to SVG, PNG and Mermaid. Read-only: nothing
//! here writes to the database.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, PathBuilder, PinchEvent,
    Pixels, Render, ScrollEvent, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, TestSupportExt as _, Window, canvas, div, point, px,
};
use kerno_core::erd::{self, Diagram, Layout, Palette, Progress, Rect, Scope};
use kerno_core::schema::TableRef;
use kerno_core::{Error, TxState};
use resvg::{tiny_skia, usvg};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::engine_logo::engine_logo;
use crate::query::tab::TabStatus;
use crate::tabs::{TabSnapshot, Target};

/// What the diagram asks of the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagramTabEvent {
    OpenData { table: TableRef },
    OpenStructure { table: TableRef },
}

/// A file the diagram is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Svg,
    Png,
    Mermaid,
}

impl ExportFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Svg => "svg",
            Self::Png => "png",
            Self::Mermaid => "mmd",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Svg => "Export SVG…",
            Self::Png => "Export PNG…",
            Self::Mermaid => "Export Mermaid…",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// The connection is not open, or the tab is not shown yet.
    Waiting,
    Loading(Progress),
    Ready,
    Failed(String),
}

/// What a press of the left button started.
#[derive(Debug, Clone, Copy)]
enum Drag {
    Pan { last: (f32, f32) },
    Table { node: usize, last: (f32, f32) },
}

const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 3.;
/// Under this zoom the rows are not drawn, only the boxes and names.
const ROWS_ZOOM: f32 = 0.4;

/// An ER diagram.
pub struct DiagramTab {
    focus_handle: FocusHandle,
    connections: Entity<Connections>,
    target: Target,
    scope: Scope,
    /// Restored and not shown yet: nothing is read, no session is open.
    hidden: bool,
    state: State,
    diagram: Rc<Diagram>,
    layout: Layout,
    routes: Rc<Vec<Vec<(f32, f32)>>>,
    /// Tables whose structure could not be read, and why.
    skipped: Vec<(TableRef, String)>,
    zoom: f32,
    /// Where the diagram's origin is drawn, in the canvas.
    pan: (f32, f32),
    hovered: Option<usize>,
    selected: Option<usize>,
    drag: Option<Drag>,
    /// The canvas's bounds in the window, at the last frame.
    canvas: Rc<Cell<Bounds<Pixels>>>,
    /// Fit the diagram to the canvas once its size is known.
    fit_pending: Rc<Cell<bool>>,
    /// Set to stop the load in flight (another load, the tab closed).
    cancel: Arc<AtomicBool>,
    generation: u64,
    load: Vec<Task<()>>,
    /// The last export, for tests: its file or why it failed.
    last_export: Option<Result<PathBuf, String>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DiagramTabEvent> for DiagramTab {}

impl Drop for DiagramTab {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl DiagramTab {
    pub fn new(
        target: Target,
        scope: Scope,
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let follow = cx.observe_in(&connections, window, |this, _, window, cx| {
            match (&this.state, this.connected(cx)) {
                (State::Waiting, true) => this.reload(window, cx),
                (State::Waiting, false) | (_, true) => {}
                (_, false) => {
                    this.state = State::Waiting;
                    cx.notify();
                }
            }
        });
        let mut tab = Self {
            focus_handle: cx.focus_handle(),
            connections,
            target,
            scope,
            hidden: false,
            state: State::Waiting,
            diagram: Rc::default(),
            layout: Vec::new(),
            routes: Rc::default(),
            skipped: Vec::new(),
            zoom: 1.,
            pan: (0., 0.),
            hovered: None,
            selected: None,
            drag: None,
            canvas: Rc::default(),
            fit_pending: Rc::default(),
            cancel: Arc::default(),
            generation: 0,
            load: Vec::new(),
            last_export: None,
            _subscriptions: vec![follow],
        };
        tab.reload(window, cx);
        tab
    }

    pub fn title(&self) -> SharedString {
        let subject = match &self.scope {
            Scope::Schema(schema) => schema,
            Scope::Around(table) => &table.name,
        };
        SharedString::from(format!("{subject} · diagram"))
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    pub fn snapshot(&self) -> TabSnapshot {
        TabSnapshot::Diagram {
            target: self.target.clone(),
            schema: self.scope.schema().to_string(),
            table: match &self.scope {
                Scope::Schema(_) => None,
                Scope::Around(table) => Some(table.name.clone()),
            },
        }
    }

    fn connected(&self, cx: &App) -> bool {
        matches!(
            self.connections.read(cx).status(&self.target.connection),
            Status::Connected { .. }
        )
    }

    /// What the status bar says about this tab.
    pub fn status(&self, cx: &App) -> TabStatus {
        let connections = self.connections.read(cx);
        let saved = connections.get(&self.target.connection);
        TabStatus {
            connection: saved.map(|saved| saved.config.name.clone()),
            read_only: saved.is_some_and(|saved| saved.config.read_only),
            database: self.target.database.clone().or_else(|| {
                connections
                    .meta_session(&self.target.connection)
                    .and_then(|meta| meta.database())
            }),
            transaction: TxState::Idle,
            running: matches!(self.state, State::Loading(_)),
            rows: None,
            elapsed: None,
            notices: 0,
            pending: 0,
        }
    }

    // Loading -----------------------------------------------------------------

    /// Reads nothing until shown (a restored tab): see `reveal`.
    pub fn defer_until_shown(&mut self, cx: &mut Context<Self>) {
        self.hidden = true;
        self.stop_loading();
        self.state = State::Waiting;
        cx.notify();
    }

    /// The tab is on screen: a deferred tab reads the diagram now.
    pub fn reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.hidden) {
            self.reload(window, cx);
        }
    }

    fn stop_loading(&mut self) {
        self.generation += 1;
        self.cancel.store(true, Ordering::Release);
        self.load.clear();
    }

    /// Reads the tables and lays them out again, on a session of its own
    /// that closes once they are read. Positions changed by dragging are
    /// lost.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_loading();
        let generation = self.generation;
        if self.hidden || !self.connected(cx) {
            self.state = State::Waiting;
            cx.notify();
            return;
        }
        self.state = State::Loading(Progress::default());
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let opening = self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        );
        let scope = self.scope.clone();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let progress = cx.spawn(async move |this, cx| {
            while let Some(progress) = receiver.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation && matches!(this.state, State::Loading(_)) {
                        this.state = State::Loading(progress);
                        cx.notify();
                    }
                });
            }
        });
        let reading = cx.spawn_in(window, async move |this, cx| {
            let session = match opening.await {
                Ok(session) => session,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| this.failed(generation, &error, cx));
                    return;
                }
            };
            let Ok(reading) = cx.update(|_, cx| {
                bridge::spawn(cx, async move {
                    let tell = move |progress| {
                        let _ = sender.send(progress);
                    };
                    let loaded = erd::load(session.clone(), &scope, tell, &cancel).await;
                    session.close().await;
                    // The layout too runs off the UI thread.
                    loaded.map(|loaded| {
                        let layout = erd::layout(&loaded.diagram);
                        let routes = routes_of(&loaded.diagram, &layout);
                        (loaded, layout, routes)
                    })
                })
            }) else {
                return;
            };
            let outcome = reading.await;
            let _ = this.update(cx, |this, cx| match outcome {
                Ok(_) if this.generation != generation => {}
                Ok((loaded, layout, routes)) => {
                    this.diagram = Rc::new(loaded.diagram);
                    this.layout = layout;
                    this.routes = Rc::new(routes);
                    this.skipped = loaded.skipped;
                    this.selected = None;
                    this.hovered = None;
                    this.state = State::Ready;
                    this.fit_pending.set(true);
                    cx.notify();
                }
                Err(error) => this.failed(generation, &error, cx),
            });
        });
        self.load = vec![progress, reading];
        cx.notify();
    }

    fn failed(&mut self, generation: u64, error: &Error, cx: &mut Context<Self>) {
        if self.generation != generation || matches!(error, Error::Cancelled) {
            return;
        }
        self.state = State::Failed(kerno_core::describe(error).to_string());
        cx.notify();
    }

    // View --------------------------------------------------------------------

    /// The canvas's size, once drawn.
    fn canvas_size(&self) -> Option<(f32, f32)> {
        let size = self.canvas.get().size;
        let (w, h) = (size.width.as_f32(), size.height.as_f32());
        (w > 0. && h > 0.).then_some((w, h))
    }

    /// Zooms so that the whole diagram shows (never past 100 %), centred.
    pub fn fit(&mut self, cx: &mut Context<Self>) {
        self.fit_pending.set(false);
        let (Some((width, height)), Some(area)) =
            (self.canvas_size(), erd::extent(&self.layout, &self.routes))
        else {
            return;
        };
        let margin = 32.;
        self.zoom = ((width - 2. * margin) / area.w)
            .min((height - 2. * margin) / area.h)
            .clamp(MIN_ZOOM, 1.);
        self.pan = (
            (width - area.w * self.zoom) / 2. - area.x * self.zoom,
            (height - area.h * self.zoom) / 2. - area.y * self.zoom,
        );
        cx.notify();
    }

    /// Zooms to `zoom` keeping the point `at` of the canvas (its centre
    /// when `None`) where it is.
    pub fn zoom_to(&mut self, zoom: f32, at: Option<(f32, f32)>, cx: &mut Context<Self>) {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let (x, y) = at
            .or_else(|| self.canvas_size().map(|(w, h)| (w / 2., h / 2.)))
            .unwrap_or_default();
        let ratio = zoom / self.zoom;
        self.pan = (x - (x - self.pan.0) * ratio, y - (y - self.pan.1) * ratio);
        self.zoom = zoom;
        cx.notify();
    }

    /// The zoom; for tests.
    #[cfg(test)]
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// A window position as a point of the canvas.
    fn in_canvas(&self, position: gpui_kit::Point<Pixels>) -> (f32, f32) {
        let origin = self.canvas.get().origin;
        (
            (position.x - origin.x).as_f32(),
            (position.y - origin.y).as_f32(),
        )
    }

    /// A node's box on the canvas.
    fn on_canvas(&self, rect: &Rect) -> Rect {
        Rect {
            x: self.pan.0 + rect.x * self.zoom,
            y: self.pan.1 + rect.y * self.zoom,
            w: rect.w * self.zoom,
            h: rect.h * self.zoom,
        }
    }

    fn press_background(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        self.selected = None;
        self.drag = Some(Drag::Pan {
            last: self.in_canvas(event.position),
        });
        cx.notify();
    }

    fn press_table(
        &mut self,
        node: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        self.selected = Some(node);
        self.drag = Some(Drag::Table {
            node,
            last: self.in_canvas(event.position),
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let now = self.in_canvas(event.position);
        match drag {
            Drag::Pan { last } => {
                self.pan = (self.pan.0 + now.0 - last.0, self.pan.1 + now.1 - last.1);
                self.drag = Some(Drag::Pan { last: now });
            }
            Drag::Table { node, last } => {
                self.move_table(
                    node,
                    (now.0 - last.0) / self.zoom,
                    (now.1 - last.1) / self.zoom,
                );
                self.drag = Some(Drag::Table { node, last: now });
            }
        }
        cx.notify();
    }

    /// Moves a table by `(dx, dy)` diagram units and routes the edges again.
    pub fn move_table(&mut self, node: usize, dx: f32, dy: f32) {
        let Some(rect) = self.layout.get_mut(node) else {
            return;
        };
        rect.x += dx;
        rect.y += dy;
        // Every edge: one may now go round the moved table.
        self.routes = Rc::new(routes_of(&self.diagram, &self.layout));
    }

    fn scroll(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(20.));
        let (dx, dy) = (delta.x.as_f32(), delta.y.as_f32());
        if event.modifiers.secondary(cx) {
            let at = self.in_canvas(event.position);
            self.zoom_to(self.zoom * (1. + dy / 200.), Some(at), cx);
        } else {
            self.pan = (self.pan.0 + dx, self.pan.1 + dy);
            cx.notify();
        }
    }

    fn pinch(&mut self, event: &PinchEvent, cx: &mut Context<Self>) {
        let at = self.in_canvas(event.position);
        self.zoom_to(self.zoom * (1. + event.delta), Some(at), cx);
    }

    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                if let Some(node) = self.selected {
                    self.open(node, false, cx);
                }
            }
            "escape" => {
                self.selected = None;
                cx.notify();
            }
            _ => {}
        }
    }

    /// Opens a table's data, or its structure.
    pub fn open(&mut self, node: usize, structure: bool, cx: &mut Context<Self>) {
        let Some(table) = self.diagram.nodes.get(node).map(|node| node.table.clone()) else {
            return;
        };
        cx.emit(if structure {
            DiagramTabEvent::OpenStructure { table }
        } else {
            DiagramTabEvent::OpenData { table }
        });
    }

    /// The node of the table named `name`; for tests.
    #[cfg(test)]
    pub fn node_of(&self, name: &str) -> Option<usize> {
        self.diagram
            .nodes
            .iter()
            .position(|node| node.table.name == name)
    }

    /// The edges as `(foreign key, referencing table, referenced table)`; for tests.
    #[cfg(test)]
    pub fn edges(&self) -> Vec<(String, String, String)> {
        let name = |node: usize| self.diagram.nodes[node].table.name.clone();
        self.diagram
            .edges
            .iter()
            .map(|edge| (edge.name.clone(), name(edge.from), name(edge.to)))
            .collect()
    }

    /// The selected table, if any; for tests.
    #[cfg(test)]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The box of a table; for tests.
    #[cfg(test)]
    pub fn rect(&self, node: usize) -> Rect {
        self.layout[node]
    }

    /// Where the tab is, in words; for tests.
    #[cfg(test)]
    pub fn state(&self) -> String {
        match &self.state {
            State::Waiting => "waiting".into(),
            State::Loading(_) => "loading".into(),
            State::Ready => format!("ready: {} tables", self.diagram.nodes.len()),
            State::Failed(error) => format!("failed: {error}"),
        }
    }

    /// The last export's file, or why it failed; for tests.
    #[cfg(test)]
    pub fn last_export(&self) -> Option<&Result<PathBuf, String>> {
        self.last_export.as_ref()
    }

    // Exports -----------------------------------------------------------------

    /// The theme's colours, for an export.
    fn palette(cx: &App) -> Palette {
        let theme = cx.theme();
        Palette {
            background: hex(theme.background),
            surface: hex(theme.background),
            header: hex(theme.table_head),
            border: hex(theme.border),
            text: hex(theme.foreground),
            muted: hex(theme.muted_foreground),
            edge: hex(theme.muted_foreground),
        }
    }

    /// Asks where to write the file, then writes it.
    pub fn choose_export(
        &mut self,
        format: ExportFormat,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state != State::Ready || self.diagram.nodes.is_empty() {
            return;
        }
        let name = format!(
            "{}.{}",
            self.title().replace(" · ", "-"),
            format.extension()
        );
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let picking = cx.prompt_for_new_path(&home, Some(&name));
        self.load.push(cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = picking.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.export_to(format, path, window, cx)
                });
            }
        }));
    }

    /// Writes the diagram, as on screen (dragged tables where they were
    /// left), to `path`, off the UI thread.
    pub fn export_to(
        &mut self,
        format: ExportFormat,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = match format {
            ExportFormat::Mermaid => erd::to_mermaid(&self.diagram),
            ExportFormat::Svg | ExportFormat::Png => {
                erd::to_svg(&self.diagram, &self.layout, &Self::palette(cx))
            }
        };
        let file = path.clone();
        let writing = bridge::spawn_blocking(cx, move || {
            let bytes = match format {
                ExportFormat::Png => rasterise(&text)?,
                ExportFormat::Svg | ExportFormat::Mermaid => text.into_bytes(),
            };
            std::fs::write(&file, bytes).map_err(|error| error.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let written = writing.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let notification = match &written {
                    Ok(()) => Notification::success(SharedString::from(format!("Saved {name}."))),
                    Err(error) => Notification::error(SharedString::from(format!(
                        "{name} could not be written: {error}"
                    ))),
                };
                window.push_notification(notification, cx);
                this.last_export = Some(written.map(|()| path));
            });
        })
        .detach();
    }

    // Rendering ---------------------------------------------------------------

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let engine = self
            .connections
            .read(cx)
            .get(&self.target.connection)
            .map(|saved| saved.config.engine);
        let ready = self.state == State::Ready && !self.diagram.nodes.is_empty();
        let summary = match &self.scope {
            Scope::Schema(schema) => schema.clone(),
            Scope::Around(table) => format!("{}.{} and its neighbours", table.schema, table.name),
        };
        let counts = (self.state == State::Ready).then(|| {
            let (tables, keys) = (self.diagram.nodes.len(), self.diagram.edges.len());
            format!(
                "{tables} {} · {keys} {}",
                if tables == 1 { "table" } else { "tables" },
                if keys == 1 {
                    "foreign key"
                } else {
                    "foreign keys"
                }
            )
        });
        let export = |format: ExportFormat, id: &'static str, cx: &mut Context<Self>| {
            Button::new(id)
                .ghost()
                .small()
                .label(format.label())
                .disabled(!ready)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.choose_export(format, window, cx)),
                )
        };
        h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .children(
                engine.map(|engine| div().flex_none().child(engine_logo(engine, px(14.), cx))),
            )
            .child(
                div()
                    .id("erd-subject")
                    .test_support()
                    .px_1()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .child(SharedString::from(summary)),
            )
            .children(counts.map(|counts| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(counts))
            }))
            .when(!self.skipped.is_empty(), |bar| {
                let first = &self.skipped[0];
                bar.child(
                    div()
                        .id("erd-skipped")
                        .test_support()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(SharedString::from(format!(
                            "{} {} could not be read ({}: {})",
                            self.skipped.len(),
                            if self.skipped.len() == 1 {
                                "table"
                            } else {
                                "tables"
                            },
                            first.0.name,
                            first.1
                        ))),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("erd-zoom-out")
                    .ghost()
                    .small()
                    .label("−")
                    .tooltip("Zoom out")
                    .disabled(!ready)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.zoom_to(this.zoom / 1.25, None, cx)),
                    ),
            )
            .child(
                Button::new("erd-zoom-reset")
                    .ghost()
                    .small()
                    .label(SharedString::from(format!("{:.0} %", self.zoom * 100.)))
                    .tooltip("Zoom to 100 %")
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, _, cx| this.zoom_to(1., None, cx))),
            )
            .child(
                Button::new("erd-zoom-in")
                    .ghost()
                    .small()
                    .label("+")
                    .tooltip("Zoom in")
                    .disabled(!ready)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.zoom_to(this.zoom * 1.25, None, cx)),
                    ),
            )
            .child(
                Button::new("erd-fit")
                    .ghost()
                    .small()
                    .label("Fit")
                    .tooltip("Show the whole diagram")
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, _, cx| this.fit(cx))),
            )
            .child(export(ExportFormat::Svg, "erd-export-svg", cx))
            .child(export(ExportFormat::Png, "erd-export-png", cx))
            .child(export(ExportFormat::Mermaid, "erd-export-mermaid", cx))
            .child(
                Button::new("erd-refresh")
                    .ghost()
                    .small()
                    .label("Refresh")
                    .tooltip("Read the tables again and lay them out anew")
                    .disabled(matches!(self.state, State::Loading(_)))
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
            )
    }

    /// A message in place of the diagram, with what to do next.
    fn render_notice(
        id: &'static str,
        title: &str,
        detail: String,
        action: Option<AnyElement>,
        cx: &App,
    ) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .id(id)
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_6()
            .child(
                div()
                    .font_semibold()
                    .child(SharedString::from(title.to_string())),
            )
            .child(
                div()
                    .max_w(px(640.))
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(detail)),
            )
            .children(action)
            .into_any_element()
    }

    fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let schema = self.scope.schema().to_string();
        match &self.state {
            State::Waiting => {
                let name = self
                    .connections
                    .read(cx)
                    .get(&self.target.connection)
                    .map_or_else(
                        || "This connection".into(),
                        |saved| saved.config.name.clone(),
                    );
                let connect = Button::new("erd-connect")
                    .primary()
                    .label("Connect")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let id = this.target.connection.clone();
                        this.connections
                            .update(cx, |connections, cx| connections.connect(&id, cx));
                    }))
                    .into_any_element();
                Self::render_notice(
                    "erd-waiting",
                    "Not connected",
                    format!(
                        "“{name}” is not connected. Connect it to draw the diagram of {schema}."
                    ),
                    Some(connect),
                    cx,
                )
            }
            State::Loading(progress) => Self::render_notice(
                "erd-loading",
                "Loading the diagram…",
                if progress.total == 0 {
                    format!("Listing the tables of {schema}.")
                } else {
                    format!(
                        "Loading {} of {} {}…",
                        progress.done,
                        progress.total,
                        if progress.total == 1 {
                            "table"
                        } else {
                            "tables"
                        }
                    )
                },
                None,
                cx,
            ),
            State::Failed(error) => {
                let retry = Button::new("erd-retry")
                    .outline()
                    .label("Try again")
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx)))
                    .into_any_element();
                Self::render_notice(
                    "erd-error",
                    "The diagram could not be read",
                    crate::advice_after(
                        error,
                        &format!(
                            "Check that {schema} still exists and that this account can read its catalog, then try again."
                        ),
                    ),
                    Some(retry),
                    cx,
                )
            }
            State::Ready if self.diagram.nodes.is_empty() => {
                let refresh = Button::new("erd-empty-refresh")
                    .outline()
                    .label("Refresh")
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx)))
                    .into_any_element();
                Self::render_notice(
                    "erd-empty",
                    "This schema has no tables",
                    format!("Create a table in {schema}, then refresh the diagram."),
                    Some(refresh),
                    cx,
                )
            }
            State::Ready => self.render_canvas(window, cx),
        }
    }

    fn render_canvas(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let view = cx.entity();
        // The tables of the selected or hovered table are drawn on top, in
        // the accent colour.
        let focus = self.selected.or(self.hovered);
        let lit: Vec<bool> = self
            .diagram
            .edges
            .iter()
            .map(|edge| focus.is_some_and(|node| edge.from == node || edge.to == node))
            .collect();
        let (routes, zoom, pan) = (self.routes.clone(), self.zoom, self.pan);
        let (bounds_cell, fit_pending) = (self.canvas.clone(), self.fit_pending.clone());
        let weak = view.downgrade();
        let (edge_color, lit_color) = (theme.muted_foreground, theme.ring);
        let edges = canvas(
            move |bounds, _, cx| {
                bounds_cell.set(bounds);
                if fit_pending.get() {
                    cx.defer(move |cx| {
                        let _ = weak.update(cx, |this, cx| this.fit(cx));
                    });
                }
            },
            move |bounds, _, window, _| {
                for highlighted in [false, true] {
                    for (route, _) in routes
                        .iter()
                        .zip(&lit)
                        .filter(|(_, lit)| **lit == highlighted)
                    {
                        paint_edge(
                            route,
                            bounds,
                            pan,
                            zoom,
                            highlighted,
                            if highlighted { lit_color } else { edge_color },
                            window,
                        );
                    }
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let visible = self.canvas_size().map(|(w, h)| Rect { x: 0., y: 0., w, h });
        let nodes = (0..self.diagram.nodes.len())
            .filter_map(|node| {
                let rect = self.on_canvas(&self.layout[node]);
                if visible.is_some_and(|view| !view.intersects(&rect)) {
                    return None;
                }
                Some(self.render_node(node, rect, &theme, cx))
            })
            .collect::<Vec<_>>();
        let menu_view = view.clone();
        div()
            .id("erd-canvas")
            .test_support()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(theme.background)
            .cursor_grab()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::press_background))
            .on_mouse_move(
                cx.listener(|this, event: &MouseMoveEvent, _, cx| this.mouse_move(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.drag = None),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.drag = None),
            )
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| this.scroll(event, cx)))
            .on_pinch(cx.listener(|this, event: &PinchEvent, _, cx| this.pinch(event, cx)))
            .child(edges)
            .children(nodes)
            .context_menu(move |menu, _, _| canvas_menu(menu, &menu_view))
            .into_any_element()
    }

    fn render_node(
        &self,
        node: usize,
        rect: Rect,
        theme: &gpui_kit::component::Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let zoom = self.zoom;
        let selected = self.selected == Some(node);
        let lit = selected || self.hovered == Some(node);
        let view = cx.entity();
        let rows = (zoom >= ROWS_ZOOM).then(|| erd::rows(&self.diagram.nodes[node]));
        let scaled = |value: f32| px(value * zoom);
        div()
            .id(("erd-table", node))
            .test_support()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(scaled(6.))
            .bg(theme.background)
            .border_1()
            .when(selected, |this| this.border_2())
            .border_color(if lit { theme.ring } else { theme.border })
            .cursor_pointer()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                let now = hovered.then_some(node);
                if *hovered || this.hovered == Some(node) {
                    this.hovered = now;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.press_table(node, event, window, cx)
                }),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.open(node, false, cx);
                }
            }))
            .child(
                div()
                    .flex_none()
                    .h(scaled(erd::HEADER_HEIGHT))
                    .px(scaled(erd::PADDING))
                    .flex()
                    .items_center()
                    .bg(theme.table_head)
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(scaled(13.))
                    .font_semibold()
                    .text_color(theme.foreground)
                    .whitespace_nowrap()
                    .child(SharedString::from(self.diagram.title(node))),
            )
            .children(rows.into_iter().flatten().map(|row| {
                h_flex()
                    .flex_none()
                    .h(scaled(erd::ROW_HEIGHT))
                    .px(scaled(erd::PADDING))
                    .whitespace_nowrap()
                    .child(
                        div()
                            .flex_none()
                            .w(scaled(erd::MARK_WIDTH))
                            .text_size(scaled(9.))
                            .font_semibold()
                            .text_color(theme.muted_foreground)
                            .child(row.mark),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_size(scaled(12.))
                            .text_color(theme.foreground)
                            .child(SharedString::from(row.name)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(scaled(11.))
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(row.data_type)),
                    )
            }))
            .context_menu(move |menu, _, _| node_menu(menu, &view, node))
            .into_any_element()
    }
}

/// The menu of a table of the diagram.
fn node_menu(menu: PopupMenu, view: &Entity<DiagramTab>, node: usize) -> PopupMenu {
    let entry = |label: &'static str, structure: Option<bool>| {
        let view = view.clone();
        PopupMenuItem::new(label).on_click(move |_, _, cx| {
            view.update(cx, |this, cx| match structure {
                Some(structure) => this.open(node, structure, cx),
                None => {
                    if let Some(table) = this.diagram.nodes.get(node) {
                        cx.write_to_clipboard(ClipboardItem::new_string(table.table.name.clone()));
                    }
                }
            })
        })
    };
    menu.item(entry("Open data", Some(false)))
        .item(entry("Open structure", Some(true)))
        .separator()
        .item(entry("Copy name", None))
}

/// The menu of the diagram's background.
fn canvas_menu(menu: PopupMenu, view: &Entity<DiagramTab>) -> PopupMenu {
    let item = |label: &'static str,
                run: fn(&mut DiagramTab, &mut Window, &mut Context<DiagramTab>)| {
        let view = view.clone();
        PopupMenuItem::new(label)
            .on_click(move |_, window, cx| view.update(cx, |this, cx| run(this, window, cx)))
    };
    menu.item(item("Fit to window", |this, _, cx| this.fit(cx)))
        .item(item("Zoom to 100 %", |this, _, cx| {
            this.zoom_to(1., None, cx)
        }))
        .separator()
        .item(item(ExportFormat::Svg.label(), |this, window, cx| {
            this.choose_export(ExportFormat::Svg, window, cx)
        }))
        .item(item(ExportFormat::Png.label(), |this, window, cx| {
            this.choose_export(ExportFormat::Png, window, cx)
        }))
        .item(item(ExportFormat::Mermaid.label(), |this, window, cx| {
            this.choose_export(ExportFormat::Mermaid, window, cx)
        }))
        .separator()
        .item(item("Refresh", |this, window, cx| this.reload(window, cx)))
}

/// Paints one edge with its ends, unless it is out of view.
fn paint_edge(
    route: &[(f32, f32)],
    bounds: Bounds<Pixels>,
    pan: (f32, f32),
    zoom: f32,
    highlighted: bool,
    color: Hsla,
    window: &mut Window,
) {
    let place = |(x, y): (f32, f32)| {
        point(
            bounds.origin.x + px(pan.0 + x * zoom),
            bounds.origin.y + px(pan.1 + y * zoom),
        )
    };
    let (mut low, mut high) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
    for (x, y) in route {
        low = (low.0.min(*x), low.1.min(*y));
        high = (high.0.max(*x), high.1.max(*y));
    }
    let area = Rect {
        x: pan.0 + low.0 * zoom - 12.,
        y: pan.1 + low.1 * zoom - 12.,
        w: (high.0 - low.0) * zoom + 24.,
        h: (high.1 - low.1) * zoom + 24.,
    };
    let view = Rect {
        x: 0.,
        y: 0.,
        w: bounds.size.width.as_f32(),
        h: bounds.size.height.as_f32(),
    };
    if !area.intersects(&view) {
        return;
    }
    let width = if highlighted { 2. } else { 1.25 };
    let mut path = PathBuilder::stroke(px(width));
    for (index, at) in route.iter().enumerate() {
        if index == 0 {
            path.move_to(place(*at));
        } else {
            path.line_to(place(*at));
        }
    }
    for [from, to] in erd::ends(route) {
        path.move_to(place(from));
        path.line_to(place(to));
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// The routes of every edge.
fn routes_of(diagram: &Diagram, layout: &[Rect]) -> Vec<Vec<(f32, f32)>> {
    (0..diagram.edges.len())
        .map(|edge| erd::route(diagram, layout, edge))
        .collect()
}

/// `color` as `#rrggbb`.
fn hex(color: Hsla) -> String {
    let rgb = color.to_rgb();
    let channel = |value: f32| (value * 255.).round().clamp(0., 255.) as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(rgb.r),
        channel(rgb.g),
        channel(rgb.b)
    )
}

/// An SVG drawn as a PNG at twice its size (less for a huge diagram), its
/// text in the embedded Inter and JetBrains Mono.
fn rasterise(svg: &str) -> Result<Vec<u8>, String> {
    let mut options = usvg::Options {
        font_family: "Inter".into(),
        ..usvg::Options::default()
    };
    let fonts = options.fontdb_mut();
    fonts.load_font_data(include_bytes!("../assets/fonts/inter/Inter-Regular.ttf").to_vec());
    fonts.load_font_data(include_bytes!("../assets/fonts/inter/Inter-SemiBold.ttf").to_vec());
    fonts.load_font_data(
        include_bytes!("../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf").to_vec(),
    );
    let tree = usvg::Tree::from_str(svg, &options).map_err(|error| error.to_string())?;
    let size = tree.size();
    // At most 8,192 pixels a side.
    let scale = (8192. / size.width().max(size.height())).min(2.);
    let (width, height) = (
        (size.width() * scale).ceil() as u32,
        (size.height() * scale).ceil() as u32,
    );
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| "the diagram is too large for a picture".to_string())?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let pixels = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    let picture = image::RgbaImage::from_raw(width, height, pixels)
        .ok_or_else(|| "the picture could not be made".to_string())?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    picture
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(bytes.into_inner())
}

impl Focusable for DiagramTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DiagramTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("erd-tab")
            .track_focus(&self.focus_handle)
            .size_full()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.key_down(event, cx)))
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(self.render_body(window, cx)))
    }
}

//! "Export schema…" (K31): the structure of a schema, all of it or the
//! tables and views ticked, written as a document: HTML, Markdown, DBML,
//! SQL or Mermaid (kerno-core's `schema_doc`). The dialog lists the
//! schema's tables and views, takes the format and its options and the
//! file (chosen up front in the save panel), then reads the structure on a
//! session of its own, off the UI thread, with progress and Cancel. It only
//! reads: nothing is written to the database, no row is read.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, Window, div, px, relative, rgb,
};
use kerno_core::erd::Progress;
use kerno_core::format::byte_size;
use kerno_core::schema::RelationKind;
use kerno_core::schema_doc::{self, Document, Format, Options, PageSize, Pick, Style};
use kerno_core::{Error, describe};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::settings_ui::OptionCards;
use crate::tabs::Target;

/// The tables and views of the schema, once read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Listing {
    Loading,
    Failed(String),
    Ready(Vec<(String, RelationKind)>),
}

/// Where the export is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Options,
    Running(Progress),
    Done { path: PathBuf, bytes: u64 },
    Cancelled,
    Failed(String),
}

pub struct SchemaExportDialog {
    connections: Entity<Connections>,
    target: Target,
    database: String,
    /// The schema read: the target's schema, or its database on engines
    /// without schemas.
    schema: String,
    has_schemas: bool,
    listing: Listing,
    /// Ticked tables and views.
    picked: BTreeSet<String>,
    /// Ticked when the list arrives: all of them when empty.
    preselect: Vec<String>,
    code: bool,
    search: Entity<InputState>,
    query: String,
    format: Format,
    style: Style,
    diagram: bool,
    page: PageSize,
    title: Entity<InputState>,
    output: Option<PathBuf>,
    /// The file chosen exists; the save panel of macOS already asked.
    replaces: bool,
    state: State,
    stop: bridge::Stop,
    cancel: Arc<AtomicBool>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// Opens the dialog on the schema of `target` (its database on engines
/// without schemas), `tables` ticked alone when given; or says why not.
pub fn open(
    connections: Entity<Connections>,
    target: Target,
    tables: Vec<String>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<SchemaExportDialog>> {
    let place = {
        let manager = connections.read(cx);
        let connected = matches!(manager.status(&target.connection), Status::Connected { .. });
        manager
            .meta_session(&target.connection)
            .filter(|_| connected)
            .and_then(|meta| {
                let has_schemas = meta.dialect().has_schemas();
                let database = target
                    .database
                    .clone()
                    .or_else(|| meta.database())
                    .filter(|name| !name.is_empty())?;
                let schema = if has_schemas {
                    target.schema.clone().or_else(|| meta.schema())?
                } else {
                    database.clone()
                };
                Some((database, schema, has_schemas))
            })
    };
    let Some((database, schema, has_schemas)) = place else {
        window.push_notification(
            Notification::warning(
                "Export schema: connect first, or right-click a schema (a database on MySQL, \
                 MariaDB and SQLite) in the tree.",
            ),
            cx,
        );
        return None;
    };
    let target = Target {
        database: Some(database.clone()),
        schema: has_schemas.then(|| schema.clone()),
        ..target
    };
    let view = cx.new(|cx| {
        SchemaExportDialog::new(
            connections,
            target,
            (database, schema, has_schemas),
            tables,
            window,
            cx,
        )
    });
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        let entered = shown.clone();
        dialog
            .title("Export schema")
            .w(px(860.))
            .on_ok(move |_, window, cx| {
                entered.update(cx, |this, cx| this.enter(window, cx));
                false
            })
            .content(move |content, _, _| content.child(view.clone()))
    });
    Some(view)
}

/// Writes `text` beside `path` as `<name>.part`, then puts it in place
/// unless the export was cancelled meanwhile: a cancelled export leaves no
/// file, and never half of one.
fn write_file(bytes: &[u8], path: &Path, cancel: &AtomicBool) -> kerno_core::Result<u64> {
    let failed = |error| Error::io(format!("{} could not be written", path.display()), error);
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    std::fs::write(&part, bytes).map_err(failed)?;
    if cancel.load(Ordering::Acquire) {
        let _ = std::fs::remove_file(&part);
        return Err(Error::Cancelled);
    }
    std::fs::rename(&part, path).map_err(|error| {
        let _ = std::fs::remove_file(&part);
        failed(error)
    })?;
    Ok(bytes.len() as u64)
}

/// `#rrggbb` as GPUI's colour.
fn hex(colour: &str) -> gpui_kit::Rgba {
    rgb(u32::from_str_radix(colour.trim_start_matches('#'), 16).unwrap_or_default())
}

fn bar(width: f32, colour: impl Into<gpui_kit::Hsla>) -> Div {
    div()
        .h(px(3.))
        .w(relative(width))
        .rounded_full()
        .bg(colour.into())
}

impl SchemaExportDialog {
    fn new(
        connections: Entity<Connections>,
        target: Target,
        (database, schema, has_schemas): (String, String, bool),
        preselect: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search tables and views"));
        let default_title = schema_doc::default_title(&database, has_schemas.then_some(&*schema));
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(default_title.clone())
                .placeholder(default_title)
        });
        let typed = cx.subscribe(&search, |this: &mut Self, input, event, cx| {
            if let InputEvent::Change = event {
                this.query = input.read(cx).value().to_lowercase();
                cx.notify();
            }
        });
        let mut dialog = Self {
            connections,
            target,
            database,
            schema,
            has_schemas,
            listing: Listing::Loading,
            picked: BTreeSet::new(),
            preselect,
            code: false,
            search,
            query: String::new(),
            format: Format::Html,
            style: Style::KernoLight,
            diagram: true,
            page: PageSize::A4,
            title,
            output: None,
            replaces: false,
            state: State::Options,
            stop: bridge::Stop::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            tasks: Vec::new(),
            _subscriptions: vec![typed],
        };
        dialog.read_list(cx);
        dialog
    }

    // The list -------------------------------------------------------------------

    /// Reads the schema's tables and views on a session of its own.
    fn read_list(&mut self, cx: &mut Context<Self>) {
        self.listing = Listing::Loading;
        let opening = self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        );
        let (schema, stop) = (self.schema.clone(), self.stop.clone());
        self.tasks.push(cx.spawn(async move |this, cx| {
            let listed = match opening.await {
                Err(error) => Err(error),
                Ok(session) => {
                    let reading = cx.update(|cx| {
                        stop.run(cx, session, move |session| async move {
                            let listed = session.objects(&schema).await;
                            session.close().await;
                            listed
                        })
                    });
                    reading.await.unwrap_or(Err(Error::Cancelled))
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.listing = match listed {
                    Ok(objects) => {
                        let relations = schema_doc::pickable(&objects);
                        let wanted: BTreeSet<&str> =
                            this.preselect.iter().map(String::as_str).collect();
                        this.picked = relations
                            .iter()
                            .map(|(name, _)| name)
                            .filter(|name| wanted.is_empty() || wanted.contains(name.as_str()))
                            .cloned()
                            .collect();
                        Listing::Ready(relations)
                    }
                    Err(Error::Cancelled) => return,
                    Err(error) => Listing::Failed(format!(
                        "The tables of “{}” could not be read.\n{}",
                        this.schema,
                        describe(&error)
                    )),
                };
                cx.notify();
            });
        }));
    }

    /// The tables and views the search lets through, in server order.
    fn shown(&self) -> Vec<(String, RelationKind)> {
        match &self.listing {
            Listing::Ready(relations) => relations
                .iter()
                .filter(|(name, _)| {
                    self.query.is_empty() || name.to_lowercase().contains(&self.query)
                })
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn tick(&mut self, name: &str, ticked: bool, cx: &mut Context<Self>) {
        if ticked {
            self.picked.insert(name.to_string());
        } else {
            self.picked.remove(name);
        }
        cx.notify();
    }

    /// All / None: on the tables and views the search shows.
    pub fn tick_shown(&mut self, ticked: bool, cx: &mut Context<Self>) {
        for (name, _) in self.shown() {
            self.tick(&name, ticked, cx);
        }
    }

    pub fn set_code(&mut self, code: bool, cx: &mut Context<Self>) {
        self.code = code;
        cx.notify();
    }

    pub fn set_format(&mut self, format: Format, cx: &mut Context<Self>) {
        // A chosen file follows the format when it still has the last one's
        // extension.
        if let Some(path) = &self.output
            && path.extension().and_then(|ext| ext.to_str()) == Some(self.format.extension())
        {
            self.output = Some(path.with_extension(format.extension()));
            self.replaces = false;
        }
        self.format = format;
        cx.notify();
    }

    pub fn set_style(&mut self, style: Style, cx: &mut Context<Self>) {
        self.style = style;
        cx.notify();
    }

    pub fn set_page(&mut self, page: PageSize, cx: &mut Context<Self>) {
        self.page = page;
        cx.notify();
    }

    pub fn set_diagram(&mut self, diagram: bool, cx: &mut Context<Self>) {
        self.diagram = diagram;
        cx.notify();
    }

    #[cfg(test)]
    pub fn search_for(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
        self.query = text.to_lowercase();
        cx.notify();
    }

    #[cfg(test)]
    pub fn picked(&self) -> Vec<String> {
        self.picked.iter().cloned().collect()
    }

    /// `loading`, `ready: N` or the error; for tests.
    #[cfg(test)]
    pub fn listing_text(&self) -> String {
        match &self.listing {
            Listing::Loading => "loading".into(),
            Listing::Ready(relations) => format!("ready: {}", relations.len()),
            Listing::Failed(error) => error.clone(),
        }
    }

    /// Where the export is, in words; for tests.
    #[cfg(test)]
    pub fn state_text(&self) -> String {
        match &self.state {
            State::Options => "options".into(),
            State::Running(_) => "running".into(),
            State::Done { path, .. } => format!("done: {}", path.display()),
            State::Cancelled => "cancelled".into(),
            State::Failed(error) => format!("failed: {error}"),
        }
    }

    #[cfg(test)]
    pub fn output(&self) -> Option<&Path> {
        self.output.as_deref()
    }

    #[cfg(test)]
    pub fn format(&self) -> Format {
        self.format
    }

    /// Back to the options after an export, as reopening the dialog would.
    #[cfg(test)]
    pub fn again(&mut self, cx: &mut Context<Self>) {
        self.state = State::Options;
        cx.notify();
    }

    // The export -----------------------------------------------------------------

    /// What stops Export, in plain words.
    pub fn problem(&self) -> Option<&'static str> {
        match &self.listing {
            Listing::Loading => return Some("Reading the tables…"),
            Listing::Failed(_) => return Some("The tables could not be read."),
            Listing::Ready(_) => {}
        }
        if self.picked.is_empty() && !self.code {
            return Some("Tick at least one table or view.");
        }
        if self.output.is_none() {
            return Some("Choose the file to write.");
        }
        None
    }

    /// Enter: chooses the file when there is none yet, else exports.
    fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state != State::Options {
            return;
        }
        if self.output.is_none() && matches!(self.listing, Listing::Ready(_)) {
            self.choose_file(window, cx);
        } else {
            self.export(window, cx);
        }
    }

    /// "Choose…": the save panel, on `<schema>-schema.<ext>`.
    pub fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (folder, name) = match &self.output {
            Some(path) => (
                path.parent().map(Path::to_path_buf).unwrap_or_default(),
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            None => (
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default(),
                schema_doc::file_name(&self.schema, self.format),
            ),
        };
        let picking = cx.prompt_for_new_path(&folder, Some(&name));
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = picking.await {
                let probe = path.clone();
                let exists = cx
                    .background_executor()
                    .spawn(async move { probe.exists() })
                    .await;
                let _ = this.update(cx, |this, cx| this.set_output(path, exists, cx));
            }
        }));
    }

    /// Takes `path` as the file; a name ending in another format's
    /// extension chooses that format.
    fn set_output(&mut self, path: PathBuf, exists: bool, cx: &mut Context<Self>) {
        if let Some(format) = Format::ALL.into_iter().find(|format| {
            path.extension().and_then(|ext| ext.to_str()) == Some(format.extension())
        }) {
            self.format = format;
        }
        self.output = Some(path);
        self.replaces = exists;
        cx.notify();
    }

    /// Reads what is ticked, writes the document, says how it went.
    pub fn export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.problem().is_some() || matches!(self.state, State::Running(_)) {
            return;
        }
        let Some(path) = self.output.clone() else {
            return;
        };
        let pick = Pick {
            relations: self.picked.clone(),
            code: self.code,
        };
        let format = self.format;
        let options = Options {
            style: self.style,
            diagram: self.diagram,
            page: self.page,
        };
        let title = Some(self.title.read(cx).value().trim().to_string())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| {
                schema_doc::default_title(&self.database, self.has_schemas.then_some(&*self.schema))
            });
        let (database, schema) = (self.database.clone(), self.schema.clone());
        let in_schema = self.has_schemas.then(|| self.schema.clone());
        self.stop = bridge::Stop::default();
        self.cancel = Arc::new(AtomicBool::new(false));
        self.state = State::Running(Progress::default());
        let (stop, cancel) = (self.stop.clone(), self.cancel.clone());
        let opening = self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        );
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        self.tasks.push(cx.spawn(async move |this, cx| {
            while let Some(progress) = receiver.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if let State::Running(shown) = &mut this.state {
                        *shown = progress;
                        cx.notify();
                    }
                });
            }
        }));
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let outcome = match opening.await {
                Err(error) => Err(error),
                Ok(session) => {
                    let Ok(running) = cx.update(|_, cx| {
                        stop.run(cx, session, move |session| async move {
                            let tell = move |progress| {
                                let _ = sender.send(progress);
                            };
                            let read =
                                schema_doc::load(session.clone(), &schema, &pick, tell, &cancel)
                                    .await;
                            let written = match read {
                                Err(error) => Err(error),
                                Ok(side) => {
                                    let document = Document {
                                        title,
                                        engine: session.engine(),
                                        server: schema_doc::server_label(
                                            session.engine(),
                                            session.server(),
                                        ),
                                        database,
                                        schema: in_schema,
                                        date: schema_doc::today(),
                                        side,
                                    };
                                    let (writer, stopped) = (session.clone(), cancel.clone());
                                    tokio::task::spawn_blocking(move || {
                                        let bytes = schema_doc::render(
                                            &document,
                                            format,
                                            &options,
                                            writer.dialect(),
                                            &crate::theme::document_fonts(),
                                        )
                                        .map_err(|why| {
                                            Error::Unsupported(format!(
                                                "the document could not be written: {why}"
                                            ))
                                        })?;
                                        write_file(&bytes, &path, &stopped)
                                    })
                                    .await
                                    .unwrap_or_else(|panic| {
                                        std::panic::resume_unwind(panic.into_panic())
                                    })
                                }
                            };
                            session.close().await;
                            written
                        })
                    }) else {
                        return;
                    };
                    running.await.unwrap_or(Err(Error::Cancelled))
                }
            };
            let _ = this.update(cx, |this, cx| {
                if !matches!(this.state, State::Running(_)) {
                    return;
                }
                this.state = match outcome {
                    Ok(bytes) => State::Done {
                        path: this.output.clone().unwrap_or_default(),
                        bytes,
                    },
                    Err(Error::Cancelled) => State::Cancelled,
                    Err(error) => State::Failed(describe(&error).to_string()),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Stops the reading, on the server too; no file is written.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if matches!(self.state, State::Running(_)) {
            self.cancel.store(true, Ordering::Release);
            self.stop.stop(cx);
            self.state = State::Cancelled;
            cx.notify();
        }
    }

    // Drawing --------------------------------------------------------------------

    fn row(label: &'static str, content: impl IntoElement, cx: &App) -> AnyElement {
        h_flex()
            .gap_3()
            .items_start()
            .child(
                div()
                    .flex_none()
                    .w(px(96.))
                    .pt(px(2.))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(content))
            .into_any_element()
    }

    /// A small page as a format looks: lines, a table, boxes, code.
    fn format_preview(format: Format, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let (border, muted, accent) = (
            theme.border,
            theme.muted_foreground.opacity(0.55),
            theme.primary,
        );
        let page = div()
            .size_full()
            .rounded(theme.radius)
            .border_1()
            .border_color(border)
            .bg(theme.background)
            .overflow_hidden()
            .p(px(6.));
        let boxes = |vertical: bool| {
            let card = || {
                v_flex()
                    .w(px(30.))
                    .gap(px(2.))
                    .p(px(3.))
                    .rounded_sm()
                    .border_1()
                    .border_color(border)
                    .child(bar(0.8, accent))
                    .child(bar(0.6, muted))
                    .child(bar(0.7, muted))
            };
            let link = div().bg(muted);
            let link = if vertical {
                link.w(px(1.)).h(px(6.))
            } else {
                link.h(px(1.)).w(px(10.))
            };
            let group = if vertical { v_flex() } else { h_flex() };
            group
                .size_full()
                .items_center()
                .justify_center()
                .child(card())
                .child(link)
                .child(card())
        };
        match format {
            Format::Html => page.child(
                v_flex()
                    .gap(px(4.))
                    .child(div().h(px(5.)).w(px(18.)).rounded_sm().bg(accent))
                    .child(bar(0.6, theme.foreground.opacity(0.6)))
                    .children((0..3).map(|_| {
                        h_flex()
                            .gap(px(3.))
                            .child(bar(0.3, muted))
                            .child(bar(0.3, muted))
                            .child(bar(0.2, muted))
                    })),
            ),
            // A page: the lockup's place, a heading and lines.
            Format::Pdf => page.child(
                h_flex().size_full().justify_center().child(
                    v_flex()
                        .w(px(34.))
                        .h_full()
                        .gap(px(3.))
                        .p(px(4.))
                        .rounded_sm()
                        .border_1()
                        .border_color(border)
                        .child(div().h(px(4.)).w(px(12.)).rounded_sm().bg(accent))
                        .child(bar(0.9, theme.foreground.opacity(0.6)))
                        .child(bar(0.7, muted))
                        .child(bar(0.8, muted))
                        .child(bar(0.6, muted)),
                ),
            ),
            Format::Markdown => page.font_family(theme.mono_font_family.clone()).child(
                v_flex()
                    .gap(px(2.))
                    .text_size(px(7.))
                    .text_color(muted)
                    .child("# Tables")
                    .child("| id | type |")
                    .child("|---|---|")
                    .child("| `email` | text |"),
            ),
            Format::Dbml => page.child(boxes(false)),
            Format::Mermaid => page.child(boxes(true)),
            Format::Sql => page.font_family(theme.mono_font_family.clone()).child(
                v_flex()
                    .gap(px(2.))
                    .text_size(px(7.))
                    .text_color(muted)
                    .child("CREATE TABLE t (")
                    .child("  id bigint,")
                    .child(");"),
            ),
        }
        .into_any_element()
    }

    /// A page of a style, in its own colours: the logo's place, a title,
    /// a table header and rows.
    fn style_preview(style: Style, cx: &App) -> AnyElement {
        let colours = schema_doc::colours(style);
        let mark = match style {
            Style::Plain => None,
            Style::KernoLight | Style::KernoDark => Some(hex(&colours.accent)),
        };
        div()
            .size_full()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(hex(&colours.background))
            .overflow_hidden()
            .p(px(6.))
            .child(
                v_flex()
                    .gap(px(4.))
                    .children(mark.map(|mark| div().h(px(5.)).w(px(14.)).rounded_sm().bg(mark)))
                    .child(bar(0.55, hex(&colours.text)))
                    .child(
                        div()
                            .h(px(6.))
                            .w_full()
                            .rounded_sm()
                            .bg(hex(&colours.surface))
                            .border_1()
                            .border_color(hex(&colours.border)),
                    )
                    .child(bar(0.8, hex(&colours.muted)))
                    .child(bar(0.7, hex(&colours.muted))),
            )
            .into_any_element()
    }

    fn render_objects(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let notice = |id: &'static str, text: String| {
            div()
                .id(id)
                .test_support()
                .p_3()
                .text_sm()
                .whitespace_normal()
                .text_color(muted)
                .test_text(text.clone())
                .child(SharedString::from(text))
                .into_any_element()
        };
        let list = match &self.listing {
            Listing::Loading => notice(
                "schema-export-loading",
                format!("Reading the tables and views of “{}”…", self.schema),
            ),
            Listing::Failed(error) => v_flex()
                .gap_2()
                .child(notice("schema-export-error", error.clone()))
                .child(
                    div().px_3().child(
                        Button::new("schema-export-retry")
                            .outline()
                            .small()
                            .label("Try again")
                            .on_click(cx.listener(|this, _, _, cx| this.read_list(cx))),
                    ),
                )
                .into_any_element(),
            Listing::Ready(relations) if relations.is_empty() => notice(
                "schema-export-empty",
                format!(
                    "“{}” holds no table or view. Tick “Also routines, sequences, types and \
                     triggers” to export its other objects.",
                    self.schema
                ),
            ),
            Listing::Ready(_) => {
                let shown = self.shown();
                if shown.is_empty() {
                    notice(
                        "schema-export-no-match",
                        format!(
                            "No table or view matches “{}”.",
                            self.search.read(cx).value()
                        ),
                    )
                } else {
                    v_flex()
                        .children(shown.into_iter().enumerate().map(|(index, (name, kind))| {
                            let ticked = self.picked.contains(&name);
                            let label = match kind {
                                RelationKind::View => format!("{name}  · view"),
                                RelationKind::MaterializedView => {
                                    format!("{name}  · materialized view")
                                }
                                RelationKind::PartitionedTable => {
                                    format!("{name}  · partitioned table")
                                }
                                _ => name.clone(),
                            };
                            div()
                                .px_2()
                                .py(px(3.))
                                .child(
                                    Checkbox::new(("schema-export-object", index))
                                        .label(SharedString::from(label))
                                        .checked(ticked)
                                        .on_click(cx.listener(
                                            move |this, checked: &bool, _, cx| {
                                                this.tick(&name, *checked, cx)
                                            },
                                        )),
                                )
                                .into_any_element()
                        }))
                        .into_any_element()
                }
            }
        };
        let total = match &self.listing {
            Listing::Ready(relations) => relations.len(),
            _ => 0,
        };
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.search).small().id("schema-export-search")),
                    )
                    .child(
                        Button::new("schema-export-all")
                            .ghost()
                            .small()
                            .label("All")
                            .on_click(cx.listener(|this, _, _, cx| this.tick_shown(true, cx))),
                    )
                    .child(
                        Button::new("schema-export-none")
                            .ghost()
                            .small()
                            .label("None")
                            .on_click(cx.listener(|this, _, _, cx| this.tick_shown(false, cx))),
                    )
                    .child(
                        div()
                            .id("schema-export-count")
                            .test_support()
                            .flex_none()
                            .text_xs()
                            .text_color(muted)
                            .test_text(format!("{} of {total} ticked", self.picked.len()))
                            .child(SharedString::from(format!(
                                "{} of {total} ticked",
                                self.picked.len()
                            ))),
                    ),
            )
            .child(
                div()
                    .id("schema-export-objects")
                    .h(px(140.))
                    .overflow_y_scroll()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .child(list),
            )
            .child(
                Checkbox::new("schema-export-code")
                    .label("Also routines, sequences, types and triggers")
                    .checked(self.code)
                    .on_click(
                        cx.listener(|this, checked: &bool, _, cx| this.set_code(*checked, cx)),
                    ),
            )
            .into_any_element()
    }

    /// The options, which scroll in a short window, and the file's row, which
    /// stays with the buttons.
    fn render_options(&self, cx: &mut Context<Self>) -> (Vec<AnyElement>, AnyElement) {
        let objects = self.render_objects(cx);
        let (muted, warning) = (cx.theme().muted_foreground, cx.theme().warning);
        let view = cx.entity();
        let formats = {
            let view = view.clone();
            let selected = Format::ALL.iter().position(|format| *format == self.format);
            Format::ALL.into_iter().fold(
                OptionCards::new("schema-export-format", selected, move |index, _, cx| {
                    view.update(cx, |this, cx| this.set_format(Format::ALL[index], cx))
                }),
                |cards, format| cards.option(format.label(), Self::format_preview(format, cx)),
            )
        };
        let mut rows = vec![
            Self::row("Objects", objects, cx),
            Self::row(
                "Format",
                v_flex().gap_1().child(formats).child(
                    div()
                        .id("schema-export-purpose")
                        .test_support()
                        .text_xs()
                        .whitespace_normal()
                        .text_color(muted)
                        .child(self.format.purpose()),
                ),
                cx,
            ),
        ];
        if matches!(self.format, Format::Html | Format::Pdf) {
            let selected = Style::ALL.iter().position(|style| *style == self.style);
            let styles = Style::ALL.into_iter().fold(
                OptionCards::new("schema-export-style", selected, move |index, _, cx| {
                    view.update(cx, |this, cx| this.set_style(Style::ALL[index], cx))
                }),
                |cards, style| cards.option(style.label(), Self::style_preview(style, cx)),
            );
            rows.push(Self::row(
                "Style",
                v_flex()
                    .gap_2()
                    .child(div().w(px(420.)).child(styles))
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                Checkbox::new("schema-export-diagram")
                                    .label("Include the diagram")
                                    .checked(self.diagram)
                                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                        this.set_diagram(*checked, cx)
                                    })),
                            )
                            // The paper, for the PDF only.
                            .when(self.format == Format::Pdf, |row| {
                                row.child(div().ml_4().text_sm().text_color(muted).child("Paper"))
                                    .children(PageSize::ALL.into_iter().enumerate().map(
                                        |(index, page)| {
                                            crate::theme::chip(
                                                ("schema-export-page", index),
                                                self.page == page,
                                            )
                                            .xsmall()
                                            .label(page.label())
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.set_page(page, cx)
                                                }),
                                            )
                                        },
                                    ))
                            }),
                    ),
                cx,
            ));
        }
        if self.format != Format::Mermaid {
            rows.push(Self::row(
                "Title",
                div()
                    .w(px(360.))
                    .child(Input::new(&self.title).small().id("schema-export-title")),
                cx,
            ));
        }
        let file: SharedString = match &self.output {
            Some(path) => path.display().to_string().into(),
            None => "Not chosen yet".into(),
        };
        let file_row = Self::row(
            "File",
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .id("schema-export-file")
                                .test_support()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .when(self.output.is_none(), |this| this.text_color(muted))
                                .child(file),
                        )
                        .child(
                            Button::new("schema-export-choose")
                                .outline()
                                .small()
                                .label("Choose…")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.choose_file(window, cx)),
                                ),
                        ),
                )
                // The macOS save panel asked already.
                .when(self.replaces && !cfg!(target_os = "macos"), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(warning)
                            .child("This file exists: the export replaces it."),
                    )
                }),
            cx,
        );
        (rows, file_row)
    }
}

impl Drop for SchemaExportDialog {
    /// Closing the dialog stops a reading that still runs.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

/// What the dialog draws around its options (its title, the place, the
/// intro, the file and the footer, paddings and the kit's margins), with room to spare: a
/// short window shortens the options, which scroll, and never cuts Export.
const DIALOG_CHROME: f32 = 330.;

impl Render for SchemaExportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let options_height = (window.viewport_size().height - px(DIALOG_CHROME)).max(px(160.));
        let theme = cx.theme();
        let (muted, danger, success) = (theme.muted_foreground, theme.danger, theme.success);
        let place = if self.has_schemas {
            format!("{}.{}", self.database, self.schema)
        } else {
            self.database.clone()
        };
        let connection = self
            .connections
            .read(cx)
            .get(&self.target.connection)
            .map(|saved| saved.config.name.clone())
            .unwrap_or_default();
        let message = |id: &'static str, text: String| {
            div()
                .id(id)
                .test_support()
                .text_sm()
                .whitespace_normal()
                .test_text(text.clone())
                .child(SharedString::from(text))
        };
        let close = || {
            Button::new("schema-export-close")
                .ghost()
                .label("Close")
                .on_click(|_, window, cx| window.close_dialog(cx))
        };
        let back = Button::new("schema-export-back")
            .outline()
            .label("Back")
            .on_click(cx.listener(|this, _, _, cx| {
                this.state = State::Options;
                cx.notify();
            }));
        let footer = h_flex().justify_end().gap_2();
        let body: Vec<AnyElement> = match &self.state {
            State::Options => {
                let problem = self.problem();
                let (rows, file_row) = self.render_options(cx);
                let mut body = vec![
                    div()
                        .id("schema-export-options")
                        .max_h(options_height)
                        .overflow_y_scroll()
                        .child(v_flex().gap_3().pr_2().children(rows))
                        .into_any_element(),
                    file_row,
                ];
                body.push(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .id("schema-export-problem")
                                .test_support()
                                .flex_1()
                                .text_sm()
                                .text_color(muted)
                                .test_text(problem.unwrap_or_default().to_string())
                                .children(problem),
                        )
                        .child(close().label("Cancel"))
                        .child(
                            Button::new("schema-export-start")
                                .primary()
                                .label("Export")
                                .disabled(problem.is_some())
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.export(window, cx)),
                                ),
                        )
                        .into_any_element(),
                );
                body
            }
            State::Running(progress) => vec![
                message(
                    "schema-export-progress",
                    if progress.total == 0 {
                        "Reading the structure…".into()
                    } else {
                        format!(
                            "Reading the structure: {} of {} objects…",
                            progress.done, progress.total
                        )
                    },
                )
                .into_any_element(),
                footer
                    .child(
                        Button::new("schema-export-cancel")
                            .outline()
                            .label("Cancel export")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
                    .into_any_element(),
            ],
            State::Done { path, bytes } => {
                let (reveal, open) = (path.clone(), path.clone());
                vec![
                    message(
                        "schema-export-done",
                        format!("Wrote {} ({}).", path.display(), byte_size(*bytes)),
                    )
                    .text_color(success)
                    .into_any_element(),
                    footer
                        .child(
                            Button::new("schema-export-reveal")
                                .outline()
                                .label(if cfg!(target_os = "macos") {
                                    "Show in Finder"
                                } else {
                                    "Show in folder"
                                })
                                .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
                        )
                        .child(
                            Button::new("schema-export-open")
                                .outline()
                                .label("Open")
                                .on_click(move |_, _, cx| cx.open_with_system(&open)),
                        )
                        .child(close().primary())
                        .into_any_element(),
                ]
            }
            State::Cancelled => vec![
                message(
                    "schema-export-cancelled",
                    "The export was cancelled. No file was written.".into(),
                )
                .into_any_element(),
                footer.child(back).child(close()).into_any_element(),
            ],
            State::Failed(error) => vec![
                message(
                    "schema-export-failed",
                    format!("The export failed. No file was written.\n{error}"),
                )
                .text_color(danger)
                .into_any_element(),
                footer.child(back).child(close()).into_any_element(),
            ],
        };
        v_flex()
            .id("schema-export-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(place)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(SharedString::from(format!("on {connection}"))),
                    ),
            )
            .child(div().text_sm().text_color(muted).whitespace_normal().child(
                "The structure only: tables with their columns, keys, constraints and indexes, \
                 views, and if you tick them the routines, sequences, types and triggers. No row \
                 is read, nothing is changed.",
            ))
            .children(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_write_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shop-schema.md");
        assert!(matches!(
            write_file(b"# shop", &path, &AtomicBool::new(true)),
            Err(Error::Cancelled)
        ));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert_eq!(
            write_file(b"# shop", &path, &AtomicBool::new(false)).unwrap(),
            6
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# shop");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

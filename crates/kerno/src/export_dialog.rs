//! Writing a table's rows or a query's result to a file, as they arrive,
//! on a session of its own: the tab it came from stays usable, and the
//! export shows its progress and can be cancelled.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Task, TestSupportExt as _, Window, div,
    px,
};
use kerno_core::browse::{Filter, select_sql};
use kerno_core::export::{DelimitedOptions, ExportFormat, LineEnding};
use kerno_core::export_file::{Encoding, ExportJob, ExportProgress, export_to_file};
use kerno_core::format::byte_size;
use kerno_core::schema::TableRef;
use kerno_core::xlsx::XlsxOptions;
use kerno_core::{Error, Session, exec};
use tokio::sync::mpsc::UnboundedSender;

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::query::plan_view::number;
use crate::tabs::Target;
use crate::theme;

/// What is exported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The rows of a table or view that `filter` keeps, in the order of
    /// `sort` (column, descending), all of them.
    Table {
        table: TableRef,
        filter: Filter,
        sort: Option<(String, bool)>,
    },
    /// What a statement returns. It runs again.
    Query { sql: String },
}

/// The formats offered, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Tsv,
    JsonArray,
    NdJson,
    SqlInsert,
    Markdown,
    /// An Excel workbook (J12).
    Xlsx,
}

impl Format {
    pub const ALL: [Format; 7] = [
        Format::Csv,
        Format::Tsv,
        Format::JsonArray,
        Format::NdJson,
        Format::SqlInsert,
        Format::Markdown,
        Format::Xlsx,
    ];

    fn label(self) -> &'static str {
        match self {
            Format::Csv => "CSV",
            Format::Tsv => "TSV",
            Format::JsonArray => "JSON array",
            Format::NdJson => "NDJSON",
            Format::SqlInsert => "SQL INSERT",
            Format::Markdown => "Markdown",
            Format::Xlsx => "Excel (XLSX)",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Tsv => "tsv",
            Format::JsonArray => "json",
            Format::NdJson => "ndjson",
            Format::SqlInsert => "sql",
            Format::Markdown => "md",
            Format::Xlsx => "xlsx",
        }
    }
}

const ENCODINGS: [(Encoding, &str); 4] = [
    (Encoding::Utf8, "UTF-8"),
    (Encoding::Utf8Bom, "UTF-8 with BOM"),
    (Encoding::Utf16Le, "UTF-16 LE"),
    (Encoding::Latin1, "Latin-1"),
];

/// Where the export is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// Choosing the format and the file.
    Options,
    Running {
        path: PathBuf,
        progress: ExportProgress,
    },
    Done {
        path: PathBuf,
        progress: ExportProgress,
    },
    Cancelled,
    Failed(String),
}

pub struct ExportDialog {
    connections: Entity<Connections>,
    target: Target,
    source: Source,
    format: Format,
    delimiter: Entity<InputState>,
    header: bool,
    null_text: Entity<InputState>,
    line_ending: LineEnding,
    encoding: Encoding,
    /// The table SQL INSERT statements write to, as `schema.table`.
    insert_table: Entity<InputState>,
    /// The workbook's first sheet.
    sheet_name: Entity<InputState>,
    /// Why the options cannot be used, after a try.
    problem: Option<String>,
    state: State,
    cancel: Arc<AtomicBool>,
    tasks: Vec<Task<()>>,
}

/// Opens the export dialog for `source` on the connection of `target`, or
/// says why it cannot be exported.
pub fn open(
    connections: Entity<Connections>,
    target: Target,
    source: Source,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<ExportDialog>> {
    if !matches!(
        connections.read(cx).status(&target.connection),
        Status::Connected { .. }
    ) {
        window.push_notification(
            Notification::warning("Connect first: the export runs on the connection."),
            cx,
        );
        return None;
    }
    let title = match &source {
        Source::Table { table, .. } => format!("Export {}", table.name),
        Source::Query { .. } => "Export the result".to_string(),
    };
    let view = cx.new(|cx| ExportDialog::new(connections, target, source, window, cx));
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        dialog
            .title(SharedString::from(title.clone()))
            .w(px(640.))
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    Some(view)
}

impl ExportDialog {
    fn new(
        connections: Entity<Connections>,
        target: Target,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (table, sheet) = match &source {
            Source::Table { table, .. } => (
                format!("{}.{}", table.schema, table.name),
                table.name.clone(),
            ),
            Source::Query { .. } => ("schema_name.table_name".into(), "Result".into()),
        };
        let mut input = |value: &str, placeholder: &'static str, cx: &mut Context<Self>| {
            let value = value.to_string();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value)
                    .placeholder(placeholder)
            })
        };
        Self {
            delimiter: input(",", "", cx),
            // An empty box writes NULL as nothing: the placeholder says so.
            null_text: input("", "empty", cx),
            insert_table: input(&table, "", cx),
            sheet_name: input(&sheet, "Sheet1", cx),
            connections,
            target,
            source,
            format: Format::Csv,
            header: true,
            line_ending: LineEnding::CrLf,
            encoding: Encoding::Utf8,
            problem: None,
            state: State::Options,
            cancel: Arc::new(AtomicBool::new(false)),
            tasks: Vec::new(),
        }
    }

    pub fn set_format(&mut self, format: Format, cx: &mut Context<Self>) {
        self.format = format;
        // Each delimited format starts with its usual line ending.
        self.line_ending = match format {
            Format::Csv => LineEnding::CrLf,
            _ => LineEnding::Lf,
        };
        self.problem = None;
        cx.notify();
    }

    /// The format with its options, or what is wrong with them.
    fn export_format(&self, cx: &App) -> Result<ExportFormat, String> {
        let delimited = |delimiter| {
            ExportFormat::Delimited(DelimitedOptions {
                delimiter,
                header: self.header,
                null_text: self.null_text.read(cx).value().to_string(),
                line_ending: self.line_ending,
            })
        };
        Ok(match self.format {
            Format::Csv => {
                let typed = self.delimiter.read(cx).value().to_string();
                // `\t` stands for a tab, which cannot be typed in the box.
                let typed = if typed == "\\t" { "\t".into() } else { typed };
                let mut chars = typed.chars();
                match (chars.next(), chars.next()) {
                    (Some(delimiter), None) if delimiter != '"' && delimiter != '\n' => {
                        delimited(delimiter)
                    }
                    _ => {
                        return Err(
                            "The delimiter is one character other than a quote, or \\t for a tab."
                                .into(),
                        );
                    }
                }
            }
            Format::Tsv => delimited('\t'),
            Format::JsonArray => ExportFormat::JsonArray,
            Format::NdJson => ExportFormat::NdJson,
            Format::Markdown => ExportFormat::Markdown,
            Format::Xlsx => ExportFormat::Xlsx(XlsxOptions {
                header: self.header,
                sheet_name: self.sheet_name.read(cx).value().trim().to_string(),
            }),
            Format::SqlInsert => {
                let typed = self.insert_table.read(cx).value().trim().to_string();
                match typed.split_once('.') {
                    Some((schema, name)) if !schema.is_empty() && !name.is_empty() => {
                        ExportFormat::SqlInsert {
                            table: TableRef::new(schema, name),
                        }
                    }
                    _ => {
                        return Err(
                            "Write the table the statements insert into as schema.table.".into(),
                        );
                    }
                }
            }
        })
    }

    /// Asks where to write the file, then exports to it.
    fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(problem) = self.export_format(cx) {
            self.problem = Some(problem);
            cx.notify();
            return;
        }
        let stem = match &self.source {
            Source::Table { table, .. } => table.name.clone(),
            Source::Query { .. } => "result".into(),
        };
        let name = format!("{stem}.{}", self.format.extension());
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let picking = cx.prompt_for_new_path(&home, Some(&name));
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = picking.await {
                let _ = this.update_in(cx, |this, window, cx| this.export_to(path, window, cx));
            }
        }));
    }

    /// Writes the rows to `path`.
    pub fn export_to(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let format = match self.export_format(cx) {
            Ok(format) => format,
            Err(problem) => {
                self.problem = Some(problem);
                cx.notify();
                return;
            }
        };
        let job = ExportJob {
            sql: String::new(),
            format,
            encoding: self.encoding,
            path: path.clone(),
        };
        self.problem = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.state = State::Running {
            path,
            progress: ExportProgress::default(),
        };
        let cancel = self.cancel.clone();
        let (source, schema) = (self.source.clone(), self.target.schema.clone());
        let opening = self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        );
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        self.tasks = vec![
            cx.spawn(async move |this, cx| {
                while let Some(progress) = receiver.recv().await {
                    let _ = this.update(cx, |this, cx| {
                        if let State::Running {
                            progress: shown, ..
                        } = &mut this.state
                        {
                            *shown = progress;
                            cx.notify();
                        }
                    });
                }
            }),
            cx.spawn_in(window, async move |this, cx| {
                let outcome = match opening.await {
                    Err(error) => Err(error),
                    Ok(session) => {
                        let Ok(running) = cx.update(|_, cx| {
                            bridge::spawn(cx, run(session, source, schema, job, sender, cancel))
                        }) else {
                            return;
                        };
                        running.await
                    }
                };
                let _ = this.update(cx, |this, cx| {
                    let State::Running { path, .. } = &this.state else {
                        return;
                    };
                    this.state = match outcome {
                        Ok(progress) => State::Done {
                            path: path.clone(),
                            progress,
                        },
                        Err(Error::Cancelled) => State::Cancelled,
                        Err(error) => State::Failed(kerno_core::describe(&error).to_string()),
                    };
                    cx.notify();
                });
            }),
        ];
        cx.notify();
    }

    /// Stops the export; the partial file is removed.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Release);
        cx.notify();
    }

    /// Where the export is, in words; for tests.
    #[cfg(test)]
    pub fn state(&self) -> String {
        match &self.state {
            State::Options => "options".into(),
            State::Running { .. } => "running".into(),
            State::Done { progress, .. } => format!("done: {} rows", progress.rows),
            State::Cancelled => "cancelled".into(),
            State::Failed(error) => format!("failed: {error}"),
        }
    }

    /// One line about what is exported.
    fn source_text(&self) -> String {
        match &self.source {
            Source::Table {
                table,
                filter,
                sort,
            } => {
                let mut text = format!("All rows of {}.{}", table.schema, table.name);
                if !filter.is_empty() {
                    text.push_str(" matching the filter");
                }
                if let Some((column, descending)) = sort {
                    text.push_str(&format!(
                        ", sorted by {column}{}",
                        if *descending { " descending" } else { "" }
                    ));
                }
                text.push('.');
                text
            }
            Source::Query { .. } => {
                "The statement of this result runs again, on a session of its own, and every \
                 row it returns is written:"
                    .into()
            }
        }
    }

    fn render_options(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let field = |label: &'static str, field: AnyElement| {
            h_flex()
                .gap_3()
                .child(
                    div()
                        .flex_none()
                        .w(px(96.))
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(field)
                .into_any_element()
        };
        let mut rows = vec![field(
            "Format",
            h_flex()
                .gap_1()
                .flex_wrap()
                .children(Format::ALL.into_iter().enumerate().map(|(index, format)| {
                    theme::chip(("export-format", index), self.format == format)
                        .small()
                        .label(format.label())
                        .on_click(cx.listener(move |this, _, _, cx| this.set_format(format, cx)))
                }))
                .into_any_element(),
        )];
        if matches!(self.format, Format::Csv | Format::Tsv) {
            if self.format == Format::Csv {
                rows.push(field(
                    "Delimiter",
                    // As wide as the box below it: one column of inputs.
                    div()
                        .w(px(160.))
                        .child(Input::new(&self.delimiter).small().id("export-delimiter"))
                        .into_any_element(),
                ));
            }
            rows.push(field(
                "NULL as",
                div()
                    .w(px(160.))
                    .child(Input::new(&self.null_text).small().id("export-null-text"))
                    .into_any_element(),
            ));
            rows.push(field(
                "Lines",
                h_flex()
                    .gap_3()
                    .child(
                        Checkbox::new("export-header")
                            .label("Header with the column names")
                            .checked(self.header)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.header = *checked;
                                cx.notify();
                            })),
                    )
                    .children(
                        [(LineEnding::Lf, "LF"), (LineEnding::CrLf, "CRLF")]
                            .into_iter()
                            .enumerate()
                            .map(|(index, (ending, label))| {
                                theme::chip(
                                    ("export-line-ending", index),
                                    self.line_ending == ending,
                                )
                                .xsmall()
                                .label(label)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.line_ending = ending;
                                        cx.notify();
                                    },
                                ))
                            }),
                    )
                    .into_any_element(),
            ));
        }
        if self.format == Format::Xlsx {
            rows.push(field(
                "Sheet",
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .w(px(160.))
                            .child(Input::new(&self.sheet_name).small().id("export-sheet-name")),
                    )
                    .child(
                        Checkbox::new("export-header")
                            .label("Header with the column names")
                            .checked(self.header)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.header = *checked;
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            ));
            rows.push(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        "Numbers, booleans and dates keep their type when Excel holds them \
                         exactly; times with a zone are written in UTC. A sheet holds 1,048,575 \
                         rows: more go on the next sheets. Text is cut at 32,767 characters.",
                    )
                    .into_any_element(),
            );
        }
        if self.format == Format::SqlInsert {
            rows.push(field(
                "Into table",
                div()
                    .w(px(280.))
                    .child(
                        Input::new(&self.insert_table)
                            .small()
                            .id("export-insert-table"),
                    )
                    .into_any_element(),
            ));
        }
        if self.format == Format::Xlsx {
            return rows;
        }
        rows.push(field(
            "Encoding",
            h_flex()
                .gap_1()
                .children(
                    ENCODINGS
                        .into_iter()
                        .enumerate()
                        .map(|(index, (encoding, label))| {
                            theme::chip(("export-encoding", index), self.encoding == encoding)
                                .xsmall()
                                .label(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.encoding = encoding;
                                    cx.notify();
                                }))
                        }),
                )
                .into_any_element(),
        ));
        rows
    }
}

impl Drop for ExportDialog {
    /// Closing the dialog stops an export that still runs.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

/// The work of one export, on the Tokio runtime: the statement, the file,
/// then the session closed whatever happened.
async fn run(
    session: Arc<dyn Session>,
    source: Source,
    schema: Option<String>,
    mut job: ExportJob,
    progress: UnboundedSender<ExportProgress>,
    cancel: Arc<AtomicBool>,
) -> kerno_core::Result<ExportProgress> {
    let outcome = async {
        job.sql = match source {
            Source::Table {
                table,
                filter,
                sort,
            } => {
                // Every row the filter keeps, in the order the tab shows them.
                let structure = session.table_structure(&table).await?;
                select_sql(
                    session.dialect(),
                    &structure.columns,
                    &table,
                    &filter,
                    sort.as_ref()
                        .map(|(column, descending)| (column.as_str(), *descending)),
                    &structure.row_identity().unwrap_or_default(),
                )?
            }
            Source::Query { sql } => {
                // The statement ran in the tab's schema; so does its export.
                if let Some(schema) = schema.filter(|_| session.dialect().has_schemas()) {
                    let entering = format!(
                        "SET search_path TO {}, public",
                        session.dialect().quote_ident(&schema)
                    );
                    exec::collect(session.clone(), entering).await?;
                }
                sql
            }
        };
        export_to_file(
            session.clone(),
            job,
            move |done| {
                let _ = progress.send(done);
            },
            cancel,
        )
        .await
    }
    .await;
    session.close().await;
    outcome
}

/// "123,000 rows · 14 MB", or the rows alone while the size is unknown.
fn progress_text(progress: ExportProgress) -> String {
    let rows = format!(
        "{} {}",
        number(progress.rows as f64),
        if progress.rows == 1 { "row" } else { "rows" }
    );
    // A workbook's size is known once it is saved.
    match progress.bytes {
        0 => rows,
        bytes => format!("{rows} · {}", byte_size(bytes)),
    }
}

/// What a workbook's report adds: its sheets beyond the first, and the
/// cells Excel's limit cut.
fn workbook_note(progress: ExportProgress) -> String {
    let mut note = String::new();
    if progress.sheets > 1 {
        note.push_str(&format!(
            " {} sheets: a sheet holds 1,048,575 rows.",
            progress.sheets
        ));
    }
    if progress.truncated > 0 {
        note.push_str(&format!(
            " {} {} cut at Excel's 32,767 characters.",
            number(progress.truncated as f64),
            if progress.truncated == 1 {
                "cell was"
            } else {
                "cells were"
            }
        ));
    }
    note
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

impl Render for ExportDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger, mono) = (
            theme.muted_foreground,
            theme.danger,
            theme.mono_font_family.clone(),
        );
        let close = || {
            Button::new("export-close")
                .ghost()
                .label("Close")
                .on_click(|_, window, cx| window.close_dialog(cx))
        };
        let message = |id: &'static str, text: String| {
            div()
                .id(id)
                .test_support()
                .text_sm()
                .child(SharedString::from(text))
        };
        let footer = h_flex().justify_end().gap_2();
        let body: Vec<AnyElement> = match &self.state {
            State::Options => {
                let mut body = self.render_options(cx);
                if let Some(problem) = &self.problem {
                    body.push(
                        message("export-problem", problem.clone())
                            .text_color(danger)
                            .into_any_element(),
                    );
                }
                body.push(
                    footer
                        .child(close().label("Cancel"))
                        .child(
                            Button::new("export-choose-file")
                                .primary()
                                .label("Choose file…")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.choose_file(window, cx)),
                                ),
                        )
                        .into_any_element(),
                );
                body
            }
            State::Running { path, progress } => vec![
                div()
                    .text_sm()
                    .text_color(muted)
                    .truncate()
                    .child(SharedString::from(format!("Writing {}", path_text(path))))
                    .into_any_element(),
                message(
                    "export-progress",
                    if progress.rows == 0 {
                        "Waiting for the first rows…".into()
                    } else {
                        progress_text(*progress)
                    },
                )
                .into_any_element(),
                footer
                    .child(
                        Button::new("export-cancel")
                            .outline()
                            .label("Cancel export")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
                    .into_any_element(),
            ],
            State::Done { path, progress } => {
                let reveal = path.clone();
                vec![
                    message(
                        "export-done",
                        format!(
                            "Exported {} {} to {} ({}).{}",
                            number(progress.rows as f64),
                            if progress.rows == 1 { "row" } else { "rows" },
                            path_text(path),
                            byte_size(progress.bytes),
                            workbook_note(*progress)
                        ),
                    )
                    .text_color(theme.success)
                    .into_any_element(),
                    footer
                        .child(
                            Button::new("export-reveal")
                                .outline()
                                .label(if cfg!(target_os = "macos") {
                                    "Show in Finder"
                                } else {
                                    "Show in folder"
                                })
                                .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
                        )
                        .child(close().primary())
                        .into_any_element(),
                ]
            }
            State::Cancelled | State::Failed(_) => {
                let (id, text) = match &self.state {
                    State::Failed(error) => (
                        "export-failed",
                        // Plain words first, then the server's (C14).
                        format!("The export failed. No file was written.\n{error}"),
                    ),
                    _ => (
                        "export-cancelled",
                        "The export was cancelled. No file was written.".to_string(),
                    ),
                };
                vec![
                    message(id, text)
                        .when(id == "export-failed", |this| this.text_color(danger))
                        .into_any_element(),
                    footer
                        .child(Button::new("export-back").outline().label("Back").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.state = State::Options;
                                cx.notify();
                            }),
                        ))
                        .child(close())
                        .into_any_element(),
                ]
            }
        };
        v_flex()
            .id("export-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .child(
                div()
                    .text_sm()
                    .child(SharedString::from(self.source_text())),
            )
            .when_some(
                match &self.source {
                    Source::Query { sql } => Some(sql.clone()),
                    Source::Table { .. } => None,
                },
                |this, sql| {
                    this.child(
                        div()
                            .p_2()
                            .max_h(px(96.))
                            .overflow_hidden()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.muted)
                            .font_family(mono)
                            .text_xs()
                            .child(SharedString::from(sql)),
                    )
                },
            )
            .children(body)
    }
}

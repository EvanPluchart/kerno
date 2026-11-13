//! Importing a CSV file into a table, existing or new: the file and how it
//! is written, with a preview; which column goes where; the SQL, confirmed;
//! then the import, in one transaction on a session of its own.

use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, Window, div, px,
};
use kerno_core::config::Engine;
use kerno_core::export_file::Encoding;
use kerno_core::import::{
    CsvOptions, CsvPreview, ImportJob, Target as ImportTarget, count_records, create_table_sql,
    import_csv, preview,
};
use kerno_core::schema::{ColumnInfo, TableRef};
use kerno_core::sql::CellValue;
use kerno_core::{Dialect, Error, Session, TypeKind};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::query::plan_view::number;
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::tabs::Target;

/// Records of the file shown before importing it.
const PREVIEW_ROWS: usize = 50;
/// Rows of the first `INSERT` shown in the confirmation.
const SHOWN_ROWS: usize = 3;
/// Rows per `INSERT`.
const BATCH_ROWS: usize = 500;

/// The delimiters offered, and tried in this order when guessing.
const DELIMITERS: [(char, &str); 4] = [
    (',', "Comma ,"),
    (';', "Semicolon ;"),
    ('\t', "Tab"),
    ('|', "Pipe |"),
];

/// UTF-8 also reads a file starting with a byte order mark.
const ENCODINGS: [(Encoding, &str); 3] = [
    (Encoding::Utf8, "UTF-8"),
    (Encoding::Utf16Le, "UTF-16 LE"),
    (Encoding::Latin1, "Latin-1"),
];

/// Called with the table once rows went in.
type Imported = dyn Fn(&TableRef, &mut Window, &mut App);

/// What the preview read.
struct Read {
    preview: CsvPreview,
    /// Records of the whole file, header left out.
    records: u64,
}

/// Where one column of the file goes.
struct MapRow {
    /// Existing table: the column it goes into, or `None` to skip it.
    target: Option<String>,
    /// New table: the column's name and SQL type, and whether it is left out.
    name: Entity<InputState>,
    data_type: Entity<InputState>,
    skip: bool,
    /// The kind its values look like.
    kind: TypeKind,
    _changes: [Subscription; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// The file and how it is written.
    Options,
    /// Which column goes where.
    Mapping,
    Running {
        inserted: u64,
    },
    Done {
        inserted: u64,
        table: TableRef,
    },
    Cancelled,
    /// Why nothing was imported; `dropped` when the import created a table
    /// on its own (MySQL, MariaDB) and dropped it again.
    Failed {
        report: String,
        dropped: bool,
    },
}

pub struct ImportDialog {
    connections: Entity<Connections>,
    target: Target,
    /// The table rows go into; `None` for a new table of `schema`.
    existing: Option<TableRef>,
    schema: String,
    /// The columns of the existing table, once read.
    columns: Option<Result<Vec<ColumnInfo>, String>>,
    path: Option<PathBuf>,
    delimiter: char,
    header: bool,
    null_text: Entity<InputState>,
    encoding: Encoding,
    read: Option<Result<Read, String>>,
    /// Bumped at each read, so a late one is dropped.
    generation: u64,
    /// The name of a new table.
    table_name: Entity<InputState>,
    mapping: Vec<MapRow>,
    step: Step,
    cancel: Arc<AtomicBool>,
    /// The session of the import while it runs, to cancel its statement.
    session: Option<Arc<dyn Session>>,
    on_imported: Rc<Imported>,
    reading: Option<Task<()>>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// Opens the import of a CSV file into `existing`, or into a new table of
/// `schema`; or says why nothing can be imported there.
pub fn open(
    connections: Entity<Connections>,
    target: Target,
    schema: String,
    existing: Option<TableRef>,
    window: &mut Window,
    cx: &mut App,
    on_imported: impl Fn(&TableRef, &mut Window, &mut App) + 'static,
) -> Option<Entity<ImportDialog>> {
    let refusal = {
        let connections = connections.read(cx);
        match connections.get(&target.connection) {
            Some(saved) if saved.config.read_only => Some(format!(
                "“{}” is a read-only connection: nothing can be imported.",
                saved.config.name
            )),
            _ if !matches!(
                connections.status(&target.connection),
                Status::Connected { .. }
            ) =>
            {
                Some("Connect first: the import runs on the connection.".into())
            }
            _ => None,
        }
    };
    if let Some(refusal) = refusal {
        window.push_notification(Notification::warning(SharedString::from(refusal)), cx);
        return None;
    }
    let title = match &existing {
        Some(table) => format!("Import CSV into {}", table.name),
        None => format!("Import CSV into a new table of {schema}"),
    };
    let on_imported: Rc<Imported> = Rc::new(on_imported);
    let view = cx.new(|cx| {
        ImportDialog::new(
            connections,
            target,
            schema,
            existing,
            on_imported,
            window,
            cx,
        )
    });
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        dialog
            .title(SharedString::from(title.clone()))
            .w(px(780.))
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    Some(view)
}

impl ImportDialog {
    fn new(
        connections: Entity<Connections>,
        target: Target,
        schema: String,
        existing: Option<TableRef>,
        on_imported: Rc<Imported>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let null_text = cx.new(|cx| InputState::new(window, cx).placeholder("empty"));
        let table_name = cx.new(|cx| InputState::new(window, cx).placeholder("table name"));
        let subscriptions = vec![
            cx.subscribe(&null_text, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.reload(cx);
                }
            }),
            cx.subscribe(&table_name, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        let mut dialog = Self {
            connections,
            target,
            existing,
            schema,
            columns: None,
            path: None,
            delimiter: ',',
            header: true,
            null_text,
            encoding: Encoding::Utf8,
            read: None,
            generation: 0,
            table_name,
            mapping: Vec::new(),
            step: Step::Options,
            cancel: Arc::new(AtomicBool::new(false)),
            session: None,
            on_imported,
            reading: None,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        dialog.read_columns(cx);
        dialog
    }

    fn engine(&self, cx: &App) -> Engine {
        self.connections
            .read(cx)
            .get(&self.target.connection)
            .map_or(Engine::Postgres, |saved| saved.config.engine)
    }

    /// The columns of the existing table, on a session opened for that.
    fn read_columns(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.existing.clone() else {
            return;
        };
        let opening = self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        );
        self.tasks.push(cx.spawn(async move |this, cx| {
            let outcome = match opening.await {
                Err(error) => Err(error),
                Ok(session) => {
                    let reading = cx.update(|cx| {
                        bridge::spawn(cx, async move {
                            let structure = session.table_structure(&table).await;
                            session.close().await;
                            structure
                        })
                    });
                    reading.await
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.columns = Some(
                    outcome
                        .map(|structure| structure.columns)
                        .map_err(|error| error.to_string()),
                );
                cx.notify();
            });
        }));
    }

    fn options(&self, cx: &App) -> CsvOptions {
        CsvOptions {
            delimiter: self.delimiter,
            header: self.header,
            null_text: self.null_text.read(cx).value().to_string(),
            encoding: self.encoding,
        }
    }

    fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picking = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picking.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |this, window, cx| this.load_file(path, window, cx));
            }
        }));
    }

    /// Takes `path` as the file to import: guesses its encoding and
    /// delimiter, then reads its first records.
    pub fn load_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.existing.is_none() {
            let name = table_name_for(&path);
            self.table_name
                .update(cx, |input, cx| input.set_value(name, window, cx));
        }
        self.path = Some(path.clone());
        self.read = None;
        self.generation += 1;
        let generation = self.generation;
        let sniffing = bridge::spawn_blocking(cx, move || sniff(&path));
        self.reading = Some(cx.spawn(async move |this, cx| {
            let (encoding, delimiter) = sniffing.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.encoding = encoding;
                    this.delimiter = delimiter;
                    this.reload(cx);
                }
            });
        }));
        cx.notify();
    }

    /// Reads the first records again, with the options as they are.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        let options = self.options(cx);
        let reading = bridge::spawn_blocking(cx, move || {
            let preview =
                preview(&path, &options, PREVIEW_ROWS).map_err(|error| error.to_string())?;
            let records = count_records(&path, &options).map_err(|error| error.to_string())?;
            Ok(Read { preview, records })
        });
        self.reading = Some(cx.spawn(async move |this, cx| {
            let read = reading.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.read = Some(read);
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn preview(&self) -> Option<&Read> {
        self.read.as_ref()?.as_ref().ok()
    }

    /// Goes to the mapping, which starts from the file's header: columns
    /// of the same name for an existing table, the header's names and the
    /// guessed types for a new one.
    pub fn map_columns(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(read) = self.preview() else {
            return;
        };
        if self.existing.is_some() && self.columns.is_none() {
            return;
        }
        let dialect = self
            .connections
            .read(cx)
            .meta_session(&self.target.connection);
        let names: Vec<String> = match &self.columns {
            Some(Ok(columns)) => columns.iter().map(|column| column.name.clone()).collect(),
            _ => Vec::new(),
        };
        let headers: Vec<(String, TypeKind)> = read
            .preview
            .headers
            .iter()
            .cloned()
            .zip(read.preview.guessed.iter().copied())
            .collect();
        self.mapping = headers
            .into_iter()
            .map(|(header, kind)| {
                let data_type = dialect
                    .as_ref()
                    .map_or("text", |meta| meta.dialect().type_for(kind));
                let mut input = |value: String, cx: &mut Context<Self>| {
                    cx.new(|cx| InputState::new(window, cx).default_value(value))
                };
                let name = input(header.clone(), cx);
                let data_type = input(data_type.to_string(), cx);
                let changed = |_: &mut Self, _, event: &InputEvent, cx: &mut Context<Self>| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                };
                MapRow {
                    target: names
                        .iter()
                        .find(|name| name.eq_ignore_ascii_case(&header))
                        .cloned(),
                    _changes: [
                        cx.subscribe(&name, changed),
                        cx.subscribe(&data_type, changed),
                    ],
                    name,
                    data_type,
                    skip: false,
                    kind,
                }
            })
            .collect();
        self.step = Step::Mapping;
        cx.notify();
    }

    /// Sets where a column of the file goes in the existing table.
    pub fn set_target(&mut self, column: usize, target: Option<String>, cx: &mut Context<Self>) {
        if let Some(row) = self.mapping.get_mut(column) {
            row.target = target;
            cx.notify();
        }
    }

    /// The table, what to import, and the columns of an existing table; or
    /// what is missing.
    fn job(&self, cx: &App) -> Result<(ImportJob, Vec<ColumnInfo>), String> {
        let (Some(path), Some(read)) = (&self.path, self.preview()) else {
            return Err("Choose a file.".into());
        };
        if read.records == 0 {
            return Err("The file has no rows to import.".into());
        }
        let (target, columns, mapping) = match &self.existing {
            Some(table) => {
                let columns = match &self.columns {
                    Some(Ok(columns)) => columns.clone(),
                    Some(Err(error)) => {
                        return Err(format!(
                            "The columns of the table could not be read: {error}"
                        ));
                    }
                    None => return Err("Reading the columns of the table…".into()),
                };
                let mapping: Vec<Option<String>> =
                    self.mapping.iter().map(|row| row.target.clone()).collect();
                (ImportTarget::Existing(table.clone()), columns, mapping)
            }
            None => {
                let name = self.table_name.read(cx).value().trim().to_string();
                if name.is_empty() {
                    return Err("Name the new table.".into());
                }
                let mut columns = Vec::new();
                let mut mapping = Vec::new();
                for row in &self.mapping {
                    if row.skip {
                        mapping.push(None);
                        continue;
                    }
                    let column = row.name.read(cx).value().trim().to_string();
                    let data_type = row.data_type.read(cx).value().trim().to_string();
                    if column.is_empty() {
                        return Err("Give every imported column a name.".into());
                    }
                    if data_type.is_empty() {
                        return Err(format!("Give “{column}” a type."));
                    }
                    mapping.push(Some(column.clone()));
                    columns.push((column, data_type));
                }
                let table = TableRef::new(self.schema.clone(), name);
                (ImportTarget::New { table, columns }, Vec::new(), mapping)
            }
        };
        let mapped: Vec<&String> = mapping.iter().flatten().collect();
        if mapped.is_empty() {
            return Err("Import at least one column of the file.".into());
        }
        for (index, name) in mapped.iter().enumerate() {
            if mapped[..index]
                .iter()
                .any(|other| other.eq_ignore_ascii_case(name))
            {
                return Err(format!("Two columns of the file go into “{name}”."));
            }
        }
        let job = ImportJob {
            path: path.clone(),
            options: self.options(cx),
            target,
            mapping,
            batch_rows: BATCH_ROWS,
        };
        Ok((job, columns))
    }

    /// The statements the import starts with, as the confirmation shows
    /// them: the `CREATE TABLE` of a new table and the first `INSERT`.
    fn statements(
        &self,
        dialect: &dyn Dialect,
        job: &ImportJob,
        columns: &[ColumnInfo],
    ) -> Result<String, String> {
        let read = self.preview().ok_or("Choose a file.")?;
        let mut parts = Vec::new();
        let (table, columns): (&TableRef, Vec<ColumnInfo>) = match &job.target {
            ImportTarget::Existing(table) => (table, columns.to_vec()),
            ImportTarget::New { table, columns } => {
                parts.extend(
                    create_table_sql(dialect, table, columns).map_err(|error| error.to_string())?,
                );
                // The kinds the values were guessed as decide how they are
                // written, as for the table's own columns.
                let kinds: Vec<TypeKind> = self
                    .mapping
                    .iter()
                    .filter(|row| !row.skip)
                    .map(|row| row.kind)
                    .collect();
                let infos = columns
                    .iter()
                    .zip(kinds)
                    .map(|((name, data_type), kind)| ColumnInfo {
                        name: name.clone(),
                        kind: if dialect.type_for(kind).eq_ignore_ascii_case(data_type) {
                            kind
                        } else {
                            TypeKind::Text
                        },
                        data_type: data_type.clone(),
                        nullable: true,
                        default: None,
                        generation: None,
                        on_update: None,
                        invisible: false,
                        primary_key_position: None,
                        comment: None,
                        collation: None,
                        enum_values: Vec::new(),
                    })
                    .collect();
                (table, infos)
            }
        };
        let mut targets = Vec::new();
        for (index, name) in job.mapping.iter().enumerate() {
            if let Some(name) = name {
                let column = columns
                    .iter()
                    .find(|column| column.name == *name)
                    .ok_or_else(|| format!("The table has no column “{name}”."))?;
                targets.push((index, column));
            }
        }
        let quoted: Vec<String> = targets
            .iter()
            .map(|(_, column)| dialect.quote_ident(&column.name))
            .collect();
        let rows: Vec<String> = read
            .preview
            .rows
            .iter()
            .take(SHOWN_ROWS)
            .map(|row| row_sql(dialect, row, &targets))
            .collect();
        let shown = rows.len() as u64;
        parts.push(format!(
            "INSERT INTO {} ({}) VALUES\n  {}",
            dialect.table_name(table),
            quoted.join(", "),
            rows.join(",\n  ")
        ));
        let mut sql = parts.join(";\n\n") + ";";
        if read.records > shown {
            let more = read.records - shown;
            sql.push_str(&format!(
                "\n-- … and {} more {}",
                number(more as f64),
                if more == 1 { "row" } else { "rows" }
            ));
        }
        Ok(sql)
    }

    /// Shows the SQL the import starts with and, once confirmed, imports.
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok((job, columns)) = self.job(cx) else {
            return;
        };
        let connections = self.connections.read(cx);
        let (Some(saved), Some(meta)) = (
            connections.get(&self.target.connection),
            connections.meta_session(&self.target.connection),
        ) else {
            return;
        };
        let (name, environment) = (saved.config.name.clone(), saved.config.environment.clone());
        let sql = match self.statements(meta.dialect(), &job, &columns) {
            Ok(sql) => sql,
            Err(problem) => {
                window.push_notification(Notification::error(SharedString::from(problem)), cx);
                return;
            }
        };
        let records = self.preview().map_or(0, |read| read.records);
        let (table, new) = match &job.target {
            ImportTarget::Existing(table) => (table.clone(), false),
            ImportTarget::New { table, .. } => (table.clone(), true),
        };
        let explanation = match (new, self.engine(cx)) {
            (false, _) => "The rows are inserted in one transaction: all of them, or none.".into(),
            (true, Engine::Postgres) => "The table is created and the rows inserted in one \
                                         transaction: all of them, or none."
                .into(),
            (true, _) => "The table is created first, on its own: MySQL and MariaDB commit a \
                          CREATE TABLE at once. Then the rows are inserted in one transaction. \
                          If a row fails, none is kept and the new table is dropped."
                .to_string(),
        };
        let view = cx.entity();
        sql_confirm::open(
            Confirmation {
                title: format!(
                    "Import {} {} into {}.{}",
                    number(records as f64),
                    if records == 1 { "row" } else { "rows" },
                    table.schema,
                    table.name
                ),
                explanation,
                statement: Statement::Fixed(sql),
                action: "Import",
                destructive: false,
                connection: name,
                environment,
                writes: true,
            },
            window,
            cx,
            move |_, window, cx| {
                let (job, columns) = (job.clone(), columns.clone());
                view.update(cx, |this, cx| this.start(job, columns, window, cx));
            },
        );
    }

    /// Runs the import on a session of its own.
    fn start(
        &mut self,
        job: ImportJob,
        columns: Vec<ColumnInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let table = match &job.target {
            ImportTarget::Existing(table) | ImportTarget::New { table, .. } => table.clone(),
        };
        self.cancel = Arc::new(AtomicBool::new(false));
        self.step = Step::Running { inserted: 0 };
        let cancel = self.cancel.clone();
        let opening = self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        );
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        self.tasks = vec![
            cx.spawn(async move |this, cx| {
                while let Some(inserted) = receiver.recv().await {
                    let _ = this.update(cx, |this, cx| {
                        if let Step::Running { inserted: shown } = &mut this.step {
                            *shown = inserted;
                            cx.notify();
                        }
                    });
                }
            }),
            cx.spawn_in(window, async move |this, cx| {
                let outcome = match opening.await {
                    Err(error) => Err(error),
                    Ok(session) => {
                        let _ = this.update(cx, |this, _| this.session = Some(session.clone()));
                        let Ok(running) = cx.update(|_, cx| {
                            bridge::spawn(cx, async move {
                                let progress = move |inserted| {
                                    let _ = sender.send(inserted);
                                };
                                let report =
                                    import_csv(session.clone(), columns, job, progress, cancel)
                                        .await;
                                session.close().await;
                                report
                            })
                        }) else {
                            return;
                        };
                        running.await
                    }
                };
                let _ = this.update_in(cx, |this, window, cx| {
                    this.session = None;
                    let cancelled = this.cancel.load(Ordering::Acquire);
                    // A row refused after the table was created on its own:
                    // the core dropped it. An error (the CREATE itself, a
                    // lost connection) says nothing of the table.
                    let created_alone =
                        this.existing.is_none() && this.engine(cx) != Engine::Postgres;
                    this.step = match outcome {
                        Ok(report) if report.failed.is_none() => {
                            (this.on_imported)(&table, window, cx);
                            Step::Done {
                                inserted: report.inserted,
                                table: table.clone(),
                            }
                        }
                        // However it ended, a cancelled import kept nothing.
                        _ if cancelled => Step::Cancelled,
                        Err(Error::Cancelled) => Step::Cancelled,
                        Ok(report) => Step::Failed {
                            report: report
                                .failed
                                .map(|failure| failure.to_string())
                                .unwrap_or_default(),
                            dropped: created_alone,
                        },
                        Err(error) => Step::Failed {
                            report: error.to_string(),
                            dropped: false,
                        },
                    };
                    cx.notify();
                });
            }),
        ];
        cx.notify();
    }

    /// Stops the import: everything is rolled back.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Release);
        if let Some(session) = self.session.clone() {
            bridge::spawn(cx, async move {
                if let Err(error) = session.cancel().await {
                    tracing::debug!(%error, "the import statement could not be cancelled");
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// Where the import is, in words; for tests.
    #[cfg(test)]
    pub fn state(&self, cx: &App) -> String {
        match &self.step {
            Step::Options => match &self.read {
                None => "options".into(),
                Some(Ok(read)) => format!(
                    "options: {} columns, {} rows, delimiter {:?}",
                    read.preview.headers.len(),
                    read.records,
                    self.delimiter
                ),
                Some(Err(error)) => format!("options: {error}"),
            },
            Step::Mapping => match self.job(cx) {
                Ok(_) => "mapping".into(),
                Err(problem) => format!("mapping: {problem}"),
            },
            Step::Running { .. } => "running".into(),
            Step::Done { inserted, .. } => format!("done: {inserted} rows"),
            Step::Cancelled => "cancelled".into(),
            Step::Failed { report, dropped } => format!(
                "failed: {report}{}",
                if *dropped { " (table dropped)" } else { "" }
            ),
        }
    }

    // Rendering ---------------------------------------------------------------

    fn render_options(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let field = |label: &'static str, field: AnyElement| {
            h_flex()
                .gap_3()
                .child(
                    div()
                        .flex_none()
                        .w(px(80.))
                        .text_sm()
                        .text_color(muted)
                        .child(label),
                )
                .child(field)
                .into_any_element()
        };
        let mut rows = vec![field(
            "File",
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .when(self.path.is_none(), |this| this.text_color(muted))
                        .child(SharedString::from(self.path.as_ref().map_or_else(
                            || "No file chosen yet.".to_string(),
                            |path| path.display().to_string(),
                        ))),
                )
                .child(
                    Button::new("import-choose-file")
                        .small()
                        .outline()
                        .label("Choose file…")
                        .on_click(cx.listener(|this, _, window, cx| this.choose_file(window, cx))),
                )
                .into_any_element(),
        )];
        if self.path.is_none() {
            return rows;
        }
        rows.push(field(
            "Delimiter",
            h_flex()
                .gap_1()
                .children(
                    DELIMITERS
                        .into_iter()
                        .enumerate()
                        .map(|(index, (delimiter, label))| {
                            Button::new(("import-delimiter", index))
                                .xsmall()
                                .outline()
                                .label(label)
                                .selected(self.delimiter == delimiter)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delimiter = delimiter;
                                    this.reload(cx);
                                }))
                        }),
                )
                .into_any_element(),
        ));
        rows.push(field(
            "Read as",
            h_flex()
                .gap_3()
                .child(
                    Checkbox::new("import-header")
                        .label("First line is a header")
                        .checked(self.header)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.header = *checked;
                            this.reload(cx);
                        })),
                )
                .child(div().text_sm().text_color(muted).child("NULL is"))
                .child(
                    div()
                        .w(px(100.))
                        .child(Input::new(&self.null_text).small().id("import-null-text")),
                )
                .children(
                    ENCODINGS
                        .into_iter()
                        .enumerate()
                        .map(|(index, (encoding, label))| {
                            Button::new(("import-encoding", index))
                                .xsmall()
                                .outline()
                                .label(label)
                                .selected(self.encoding == encoding)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.encoding = encoding;
                                    this.reload(cx);
                                }))
                        }),
                )
                .into_any_element(),
        ));
        rows.push(self.render_preview(cx));
        rows
    }

    /// The first records of the file, as a grid.
    fn render_preview(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let read = match &self.read {
            None => {
                return div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Reading the file…")
                    .into_any_element();
            }
            Some(Err(error)) => {
                return div()
                    .id("import-preview-error")
                    .test_support()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(SharedString::from(format!(
                        "{error}. Check the delimiter and the encoding."
                    )))
                    .into_any_element();
            }
            Some(Ok(read)) => read,
        };
        let cell = |text: &str| {
            div()
                .flex_none()
                .w(px(130.))
                .px_2()
                .py_0p5()
                .truncate()
                .child(SharedString::from(text.to_string()))
        };
        let shown = read.preview.rows.len() as u64;
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(if read.records > shown {
                        format!(
                            "The first {} of {} rows",
                            shown,
                            number(read.records as f64)
                        )
                    } else {
                        format!(
                            "{} {}",
                            number(read.records as f64),
                            if read.records == 1 { "row" } else { "rows" }
                        )
                    })),
            )
            .child(
                div()
                    .id("import-preview")
                    .test_support()
                    .h(px(280.))
                    .overflow_scroll()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .child(
                        h_flex()
                            .bg(theme.muted)
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .children(read.preview.headers.iter().map(|name| cell(name))),
                    )
                    .children(read.preview.rows.iter().map(|row| {
                        h_flex()
                            .border_t_1()
                            .border_color(theme.border)
                            .children(row.iter().map(|value| match value {
                                Some(text) => cell(text),
                                None => cell("NULL").text_color(theme.muted_foreground),
                            }))
                    })),
            )
            .into_any_element()
    }

    fn render_mapping(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let read = self.preview();
        let sample = |index: usize| {
            read.and_then(|read| read.preview.rows.first()?.get(index).cloned())
                .map(|value| value.unwrap_or_else(|| "NULL".into()))
                .unwrap_or_default()
        };
        let headers: Vec<String> = read.map_or_else(Vec::new, |read| read.preview.headers.clone());
        let names: Vec<String> = match &self.columns {
            Some(Ok(columns)) => columns.iter().map(|column| column.name.clone()).collect(),
            _ => Vec::new(),
        };
        let view = cx.entity();
        let heading = match &self.existing {
            Some(table) => div()
                .text_sm()
                .child(SharedString::from(format!(
                    "Into {}.{}. Columns of the file without a match are skipped.",
                    table.schema, table.name
                )))
                .into_any_element(),
            None => h_flex()
                .gap_2()
                .text_sm()
                .child(SharedString::from(format!("New table in {}:", self.schema)))
                .child(
                    div()
                        .w(px(240.))
                        .child(Input::new(&self.table_name).small().id("import-table-name")),
                )
                .into_any_element(),
        };
        let rows = self.mapping.iter().enumerate().map(|(index, row)| {
            let line = h_flex()
                .gap_2()
                .py_0p5()
                .child(
                    div()
                        .flex_none()
                        .w(px(160.))
                        .truncate()
                        .text_sm()
                        .font_family(theme.mono_font_family.clone())
                        .child(SharedString::from(
                            headers.get(index).cloned().unwrap_or_default(),
                        )),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(150.))
                        .truncate()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(sample(index))),
                )
                .child(div().flex_none().text_color(muted).child("→"));
            match &self.existing {
                Some(_) => {
                    let menu = {
                        let (view, names) = (view.clone(), names.clone());
                        move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                            for choice in
                                std::iter::once(None).chain(names.iter().cloned().map(Some))
                            {
                                let view = view.clone();
                                let label = choice.clone().unwrap_or_else(|| "Skip".into());
                                menu = menu.item(
                                    PopupMenuItem::new(SharedString::from(label)).on_click(
                                        move |_, _, cx| {
                                            let choice = choice.clone();
                                            view.update(cx, |this, cx| {
                                                this.set_target(index, choice, cx)
                                            });
                                        },
                                    ),
                                );
                            }
                            menu
                        }
                    };
                    line.child(
                        Button::new(("import-target", index))
                            .small()
                            .outline()
                            .label(SharedString::from(
                                row.target.clone().unwrap_or_else(|| "Skip".into()),
                            ))
                            .dropdown_caret(true)
                            .dropdown_menu(menu),
                    )
                }
                None => line
                    .child(
                        Checkbox::new(("import-column", index))
                            .label("Import")
                            .checked(!row.skip)
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                if let Some(row) = this.mapping.get_mut(index) {
                                    row.skip = !*checked;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        div().w(px(150.)).child(
                            Input::new(&row.name)
                                .small()
                                .id(("import-name", index))
                                .disabled(row.skip),
                        ),
                    )
                    .child(
                        div().w(px(130.)).child(
                            Input::new(&row.data_type)
                                .small()
                                .id(("import-type", index))
                                .disabled(row.skip),
                        ),
                    ),
            }
            .into_any_element()
        });
        vec![
            heading,
            v_flex()
                .id("import-mapping")
                .test_support()
                .max_h(px(380.))
                .overflow_y_scroll()
                .children(rows)
                .into_any_element(),
        ]
    }
}

/// Encoding and delimiter of a file, guessed from its first records: UTF-16
/// when it starts with its byte order mark, and the delimiter that splits
/// every record into the same number of fields, the most of them; a comma
/// when none does.
fn sniff(path: &Path) -> (Encoding, char) {
    let mut start = [0u8; 2];
    let encoding = match File::open(path).and_then(|mut file| file.read_exact(&mut start)) {
        Ok(()) if start == [0xff, 0xfe] => Encoding::Utf16Le,
        _ => Encoding::Utf8,
    };
    let fields = |delimiter: char| {
        let options = CsvOptions {
            delimiter,
            header: false,
            null_text: String::new(),
            encoding,
        };
        let read = preview(path, &options, 10).ok()?;
        let width = read.headers.len();
        (width > 1 && read.rows.iter().all(|row| row.len() == width)).then_some(width)
    };
    // The first delimiter wins a tie.
    let delimiter = DELIMITERS
        .iter()
        .rev()
        .filter_map(|(delimiter, _)| Some((fields(*delimiter)?, *delimiter)))
        .max_by_key(|(width, _)| *width)
        .map_or(',', |(_, delimiter)| delimiter);
    (encoding, delimiter)
}

/// A table name from a file name: `Orders 2024.csv` gives `orders_2024`.
fn table_name_for(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let name: String = stem
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    name.trim_matches('_').to_string()
}

/// `(v1, v2, …)` for the mapped fields of a previewed record, as the import
/// writes them: NULL where the reader read NULL, so a quoted `""` stays the
/// empty string.
fn row_sql(
    dialect: &dyn Dialect,
    row: &[Option<String>],
    targets: &[(usize, &ColumnInfo)],
) -> String {
    let values: Vec<String> = targets
        .iter()
        .map(|(index, column)| {
            let value = match row.get(*index) {
                Some(Some(text)) => CellValue::Text(text.clone()),
                _ => CellValue::Null,
            };
            dialect.literal(&value, column)
        })
        .collect();
    format!("({})", values.join(", "))
}

impl Drop for ImportDialog {
    /// Closing the dialog stops an import that still runs.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Render for ImportDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger, success) = (theme.muted_foreground, theme.danger, theme.success);
        let close = |label: &'static str| {
            Button::new("import-close")
                .ghost()
                .label(label)
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
        let body: Vec<AnyElement> = match &self.step {
            Step::Options => {
                let mut body = self.render_options(cx);
                body.push(
                    footer
                        .child(close("Cancel"))
                        .child(
                            Button::new("import-next")
                                .primary()
                                .label("Map columns…")
                                // The mapping starts from the table's columns.
                                .disabled(
                                    self.preview().is_none()
                                        || (self.existing.is_some() && self.columns.is_none()),
                                )
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.map_columns(window, cx)),
                                ),
                        )
                        .into_any_element(),
                );
                body
            }
            Step::Mapping => {
                let problem = self.job(cx).err();
                let mut body = self.render_mapping(cx);
                if let Some(problem) = &problem {
                    body.push(
                        message("import-problem", problem.clone())
                            .text_color(muted)
                            .into_any_element(),
                    );
                }
                body.push(
                    footer
                        .child(Button::new("import-back").outline().label("Back").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.step = Step::Options;
                                cx.notify();
                            }),
                        ))
                        .child(
                            Button::new("import-start")
                                .primary()
                                .label("Import…")
                                .disabled(problem.is_some())
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.confirm(window, cx)),
                                ),
                        )
                        .into_any_element(),
                );
                body
            }
            Step::Running { inserted } => vec![
                message(
                    "import-progress",
                    format!(
                        "{} {} sent. Nothing is kept until every row is in: the import is one \
                         transaction.",
                        number(*inserted as f64),
                        if *inserted == 1 { "row" } else { "rows" }
                    ),
                )
                .into_any_element(),
                footer
                    .child(
                        Button::new("import-cancel")
                            .outline()
                            .label("Cancel import")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
                    .into_any_element(),
            ],
            Step::Done { inserted, table } => vec![
                message(
                    "import-done",
                    format!(
                        "{} {} imported into {}.{}.",
                        number(*inserted as f64),
                        if *inserted == 1 { "row" } else { "rows" },
                        table.schema,
                        table.name
                    ),
                )
                .text_color(success)
                .into_any_element(),
                footer.child(close("Close").primary()).into_any_element(),
            ],
            Step::Cancelled | Step::Failed { .. } => {
                let text = match &self.step {
                    Step::Failed { report, dropped } => {
                        format!(
                            "Nothing was imported: {report}.{}",
                            if *dropped {
                                " The new table was dropped."
                            } else {
                                ""
                            }
                        )
                    }
                    _ => "The import was cancelled. Nothing was imported.".into(),
                };
                let failed = matches!(self.step, Step::Failed { .. });
                vec![
                    message(
                        if failed {
                            "import-failed"
                        } else {
                            "import-cancelled"
                        },
                        text,
                    )
                    .when(failed, |this| this.text_color(danger))
                    .into_any_element(),
                    footer
                        .child(Button::new("import-back").outline().label("Back").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.step = Step::Mapping;
                                cx.notify();
                            }),
                        ))
                        .child(close("Close"))
                        .into_any_element(),
                ]
            }
        };
        v_flex()
            .id("import-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .children(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(header: bool, encoding: Encoding) -> CsvOptions {
        CsvOptions {
            delimiter: ',',
            header,
            null_text: String::new(),
            encoding,
        }
    }

    fn file(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn the_confirmation_writes_null_where_the_reader_reads_null() {
        let column = |name: &str| ColumnInfo {
            name: name.into(),
            data_type: "text".into(),
            kind: TypeKind::Text,
            nullable: true,
            default: None,
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: None,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "n.csv", b"a,b,c,d\n\"\",,NULL,\"NULL\"\n");
        let (a, b, c, d) = (column("a"), column("b"), column("c"), column("d"));
        let targets = [(0, &a), (1, &b), (2, &c), (3, &d)];
        let dialect = kerno_postgres::PgDialect;
        let rows = |null_text: &str| {
            let options = CsvOptions {
                null_text: null_text.into(),
                ..options(true, Encoding::Utf8)
            };
            let read = preview(&path, &options, 10).unwrap();
            row_sql(&dialect, &read.rows[0], &targets)
        };
        // `""` is the empty string; a bare field that reads the NULL text
        // is NULL; a quoted one is text.
        assert_eq!(rows(""), "('', NULL, 'NULL', 'NULL')");
        assert_eq!(rows("NULL"), "('', '', NULL, 'NULL')");
    }

    #[test]
    fn the_delimiter_and_the_encoding_are_guessed() {
        let dir = tempfile::tempdir().unwrap();
        for (text, delimiter) in [
            ("a;b;c\n1;\"x;y\";3\n", ';'),
            ("a\tb\n1\t2\n", '\t'),
            ("a|b\n1|2\n", '|'),
            // Commas inside quotes do not count; semicolons split every line.
            ("a;b\n\"1,5\";2\n", ';'),
            ("a,b\n1,2\n", ','),
            // One column: nothing to guess from.
            ("name\nx\n", ','),
        ] {
            let path = file(&dir, "d.csv", text.as_bytes());
            assert_eq!(sniff(&path), (Encoding::Utf8, delimiter), "{text:?}");
        }
        let wide: Vec<u8> = "\u{feff}a;b\n1;2\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let path = file(&dir, "e.csv", &wide);
        assert_eq!(sniff(&path), (Encoding::Utf16Le, ';'));
    }

    #[test]
    fn a_table_is_named_after_its_file() {
        assert_eq!(
            table_name_for(Path::new("/x/Orders 2024.csv")),
            "orders_2024"
        );
        assert_eq!(table_name_for(Path::new("-data-.csv")), "data");
    }
}

//! The engine's maintenance tools on one object of the tree (DBeaver's
//! Tools ▸, `tasks.sql.ui` `SQLToolTaskWizard`): the object's tools, the
//! options of the chosen one at DBeaver's defaults (a refresh keeps the
//! data), the statement as it stands, then — once confirmed through
//! [`sql_confirm`] — the run on a session of its own, its cancel, and what
//! the server answered: MySQL's status rows, PostgreSQL's notices.

use std::mem::discriminant;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task,
    TestSupportExt as _, Window, div, px,
};
use kerno_core::exec::{self, ExecEvent, RowLimit};
use kerno_core::maintenance::{CheckOption, Maintenance};
use kerno_core::schema::ObjectRef;
use kerno_core::{Error, Session};
use tokio::sync::mpsc::UnboundedSender;

use crate::bridge;
use crate::connections::manager::Connections;
use crate::query::tab::duration_text;
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::theme;

/// What the dialog is opened on.
pub struct Opening {
    pub connection: String,
    /// The database the object is in: the session runs there (a tool on a
    /// database acts on the one the session is connected to).
    pub database: String,
    pub object: ObjectRef,
    /// The object in a sentence: "table", "trigger"…
    pub kind: &'static str,
    pub name: String,
    /// The object's tools at their default options, in menu order.
    pub tools: Vec<Maintenance>,
    /// The tool shown first.
    pub selected: usize,
}

/// Called once a tool ran without error.
type Ran = dyn Fn(&mut Window, &mut App);

/// The name of a tool, as menus and buttons show it.
pub fn label(tool: &Maintenance) -> &'static str {
    match tool {
        Maintenance::Vacuum(_) => "Vacuum",
        Maintenance::Analyze => "Analyze",
        Maintenance::RefreshMaterializedView(_) => "Refresh",
        Maintenance::Truncate(_) => "Truncate",
        Maintenance::EnableTrigger => "Enable trigger",
        Maintenance::DisableTrigger => "Disable trigger",
        Maintenance::Check(_) => "Check",
        Maintenance::Optimize => "Optimize",
        Maintenance::Repair(_) => "Repair",
    }
}

/// The on/off options of a tool, in DBeaver's order.
fn flags(tool: &mut Maintenance) -> Vec<(&'static str, &mut bool)> {
    match tool {
        Maintenance::Vacuum(o) => vec![
            ("Full", &mut o.full),
            ("Freeze", &mut o.freeze),
            ("Analyze", &mut o.analyze),
            ("Disable page skipping", &mut o.disable_page_skipping),
            ("Skip locked", &mut o.skip_locked),
            ("Index cleanup", &mut o.index_cleanup),
        ],
        Maintenance::RefreshMaterializedView(o) => vec![
            ("With data", &mut o.with_data),
            ("Concurrently", &mut o.concurrently),
        ],
        Maintenance::Truncate(o) => vec![
            ("Only", &mut o.only),
            ("Restart identity", &mut o.restart_identity),
            ("Cascade", &mut o.cascade),
        ],
        Maintenance::Repair(o) => vec![
            ("Quick", &mut o.quick),
            ("Extended", &mut o.extended),
            ("Use .frm", &mut o.use_frm),
        ],
        _ => Vec::new(),
    }
}

/// Whether option `index` of `tool` cannot be turned on now: the server
/// refuses DISABLE_PAGE_SKIPPING with FULL, and CONCURRENTLY without data.
fn blocked(tool: &Maintenance, index: usize) -> bool {
    match tool {
        Maintenance::Vacuum(o) => index == 3 && o.full,
        Maintenance::RefreshMaterializedView(o) => index == 1 && !o.with_data,
        _ => false,
    }
}

/// What running `tool` does, in sentences: what the confirmation says.
fn explanation(tool: &Maintenance, kind: &str, name: &str) -> String {
    let whole = if kind == "database" {
        format!("every table of the database “{name}”")
    } else {
        format!("the {kind}")
    };
    match tool {
        Maintenance::Vacuum(o) => {
            let mut text = format!(
                "Reclaims the space of the dead rows of {whole}{}.",
                if o.analyze {
                    " and updates the planner statistics"
                } else {
                    ""
                }
            );
            if o.full {
                text.push_str(
                    " FULL rewrites it into new files under an exclusive lock: every read and \
                     write of it waits until the vacuum ends, and it needs room for a full copy.",
                );
            }
            text
        }
        Maintenance::Analyze => format!("Reads a sample of the rows of {whole} and updates the statistics the planner uses."),
        Maintenance::RefreshMaterializedView(o) if !o.with_data => "Empties the view: it cannot be read until it is refreshed with data.".into(),
        Maintenance::RefreshMaterializedView(o) => format!(
            "Runs the view's query again and replaces its rows{}.",
            if o.concurrently {
                ", without blocking the readers (the view needs a unique index)"
            } else {
                "; readers wait until it ends"
            }
        ),
        Maintenance::Truncate(o) => format!(
            "Every row of the table is deleted{}{}. This cannot be undone.",
            if o.cascade {
                ", and every row of the tables that reference it"
            } else {
                ""
            },
            if o.restart_identity {
                "; the sequences its columns own start again"
            } else {
                ""
            }
        ),
        Maintenance::EnableTrigger => "The trigger fires again on the changes it is defined for.".into(),
        Maintenance::DisableTrigger => "The trigger stops firing until it is enabled again.".into(),
        Maintenance::Check(_) => "Checks the table and its indexes for errors and reports what it found.".into(),
        Maintenance::Optimize => "Rebuilds the table and its indexes to reclaim unused space; writes to it wait while it runs.".into(),
        Maintenance::Repair(_) => "Repairs a damaged table. Only some storage engines (MyISAM, Aria, Archive, CSV) can; InnoDB answers that it cannot.".into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Options,
    /// Opening the session, then running.
    Running,
    Done(Duration),
    Cancelled,
    Failed(String),
}

/// What the server sends while a tool runs.
enum Event {
    Columns(Vec<String>),
    Rows(Vec<Vec<String>>),
    Notice(String),
}

pub struct MaintenanceDialog {
    connections: Entity<Connections>,
    opening: Opening,
    /// The connection's name and environment, as the confirmation shows them.
    connection_name: String,
    environment: String,
    /// The session whose dialect writes the statements (the connection's own).
    dialect_of: Option<Arc<dyn Session>>,
    step: Step,
    /// The statement that runs or ran.
    ran: String,
    /// The session running it, for Cancel.
    session: Option<Arc<dyn Session>>,
    cancel_requested: bool,
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    notices: Vec<String>,
    on_ran: Rc<Ran>,
    running: Option<Task<()>>,
}

/// Opens the maintenance tools of `opening.object`.
pub fn open(
    connections: Entity<Connections>,
    opening: Opening,
    window: &mut Window,
    cx: &mut App,
    on_ran: impl Fn(&mut Window, &mut App) + 'static,
) -> Entity<MaintenanceDialog> {
    let view = cx.new(|cx| MaintenanceDialog::new(connections, opening, Rc::new(on_ran), cx));
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let view = shown.clone();
        dialog
            .title(SharedString::from(view.read(cx).title()))
            .w(px(680.))
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    view
}

impl MaintenanceDialog {
    fn new(
        connections: Entity<Connections>,
        opening: Opening,
        on_ran: Rc<Ran>,
        cx: &mut Context<Self>,
    ) -> Self {
        let manager = connections.read(cx);
        let (connection_name, environment) = manager
            .get(&opening.connection)
            .map(|saved| (saved.config.name.clone(), saved.config.environment.clone()))
            .unwrap_or_default();
        let dialect_of = manager.meta_session(&opening.connection);
        Self {
            connections,
            opening,
            connection_name,
            environment,
            dialect_of,
            step: Step::Options,
            ran: String::new(),
            session: None,
            cancel_requested: false,
            columns: Vec::new(),
            rows: Vec::new(),
            notices: Vec::new(),
            on_ran,
            running: None,
        }
    }

    fn tool(&self) -> &Maintenance {
        &self.opening.tools[self.opening.selected]
    }

    fn title(&self) -> String {
        format!(
            "{} {} “{}”",
            label(self.tool()),
            self.opening.kind,
            self.opening.name
        )
    }

    /// The statement of the chosen tool, or why there is none.
    fn sql(&self) -> Result<kerno_core::maintenance::MaintenanceSql, String> {
        let Some(meta) = &self.dialect_of else {
            return Err("The connection is not open.".into());
        };
        meta.dialect()
            .maintenance_sql(self.tool(), &self.opening.object)
            .map_err(|error| error.to_string())
    }

    /// Chooses tool `index` of the object.
    pub fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.opening.tools.len() && self.step == Step::Options {
            self.opening.selected = index;
            cx.notify();
        }
    }

    /// Turns option `index` of the chosen tool on or off.
    pub fn set_option(&mut self, index: usize, on: bool, cx: &mut Context<Self>) {
        let selected = self.opening.selected;
        let tool = &mut self.opening.tools[selected];
        if on && blocked(tool, index) {
            return;
        }
        if let Some((_, flag)) = flags(tool).into_iter().nth(index) {
            *flag = on;
        }
        // What the server refuses together is undone.
        match tool {
            Maintenance::Vacuum(o) if o.full => o.disable_page_skipping = false,
            Maintenance::RefreshMaterializedView(o) if !o.with_data => o.concurrently = false,
            _ => {}
        }
        cx.notify();
    }

    /// The option of CHECK TABLE; `None` for the server's default.
    pub fn set_check(&mut self, option: Option<CheckOption>, cx: &mut Context<Self>) {
        let selected = self.opening.selected;
        if let Maintenance::Check(chosen) = &mut self.opening.tools[selected] {
            *chosen = option;
            cx.notify();
        }
    }

    /// Shows the statement and, once confirmed, runs it.
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(sql) = self.sql() else {
            return;
        };
        let tool = self.tool();
        let destructive = match tool {
            Maintenance::Truncate(_) => true,
            Maintenance::Vacuum(o) => o.full,
            _ => false,
        };
        let view = cx.entity();
        sql_confirm::open(
            Confirmation {
                title: format!("{}?", self.title()),
                explanation: explanation(tool, self.opening.kind, &self.opening.name),
                statement: Statement::Fixed(sql.sql),
                action: label(tool),
                destructive,
                connection: self.connection_name.clone(),
                environment: self.environment.clone(),
                writes: true,
            },
            window,
            cx,
            move |sql, window, cx| view.update(cx, |this, cx| this.run(sql, window, cx)),
        );
    }

    /// Runs `sql` on a session of its own, opened on the object's database
    /// and closed once it ends. A fresh session is in auto-commit: what
    /// `MaintenanceSql::autocommit` asks for.
    fn run(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        self.ran = sql.clone();
        self.step = Step::Running;
        self.cancel_requested = false;
        self.columns.clear();
        self.rows.clear();
        self.notices.clear();
        let opening = self.connections.read(cx).open_session(
            &self.opening.connection,
            Some(self.opening.database.clone()),
            cx,
        );
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        self.running = Some(cx.spawn_in(window, async move |this, cx| {
            let started = Instant::now();
            let outcome = match opening.await {
                Err(error) => Err(error),
                Ok(session) => {
                    // A dialog closed meanwhile counts as a cancel: the
                    // session is closed without running anything.
                    let cancelled = this
                        .update(cx, |this, _| {
                            this.session = Some(session.clone());
                            this.cancel_requested
                        })
                        .unwrap_or(true);
                    let Ok(running) = cx.update(|_, cx| {
                        bridge::spawn(cx, execute(session, sql, cancelled, sender))
                    }) else {
                        return;
                    };
                    // The channel closes once the statement ended.
                    while let Some(event) = receiver.recv().await {
                        let mut events = vec![event];
                        while let Ok(event) = receiver.try_recv() {
                            events.push(event);
                        }
                        if this.update(cx, |this, cx| this.take(events, cx)).is_err() {
                            return;
                        }
                    }
                    running.await
                }
            };
            let elapsed = started.elapsed();
            let _ = this.update_in(cx, |this, window, cx| {
                this.session = None;
                this.step = match outcome {
                    // MySQL may end a killed statement quietly.
                    Ok(()) if this.cancel_requested => Step::Cancelled,
                    Ok(()) => {
                        (this.on_ran.clone())(window, cx);
                        Step::Done(elapsed)
                    }
                    Err(_) if this.cancel_requested => Step::Cancelled,
                    Err(error) => Step::Failed(error_text(&error)),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn take(&mut self, events: Vec<Event>, cx: &mut Context<Self>) {
        for event in events {
            match event {
                Event::Columns(columns) => self.columns = columns,
                Event::Rows(rows) => self.rows.extend(rows),
                Event::Notice(notice) => self.notices.push(notice),
            }
        }
        cx.notify();
    }

    /// Asks the server to stop the statement.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.step != Step::Running {
            return;
        }
        self.cancel_requested = true;
        if let Some(session) = self.session.clone() {
            bridge::spawn(cx, async move { session.cancel().await }).detach();
        }
        cx.notify();
    }

    /// Where the dialog is, in words; for tests.
    #[cfg(test)]
    pub fn state(&self) -> String {
        match &self.step {
            Step::Options => match self.sql() {
                Ok(sql) => format!("options: {}", sql.sql),
                Err(why) => format!("no statement: {why}"),
            },
            Step::Running => "running".into(),
            Step::Done(_) => "done".into(),
            Step::Cancelled => "cancelled".into(),
            Step::Failed(error) => format!("failed: {error}"),
        }
    }

    /// The tools offered, by label; for tests.
    #[cfg(test)]
    pub fn tools(&self) -> Vec<&'static str> {
        self.opening.tools.iter().map(label).collect()
    }

    /// The status rows and the notices of the last run; for tests.
    #[cfg(test)]
    pub fn report(&self) -> (Vec<Vec<String>>, Vec<String>) {
        (self.rows.clone(), self.notices.clone())
    }
}

/// An error with the server's detail and hint, when it gave them.
fn error_text(error: &Error) -> String {
    let mut text = error.to_string();
    if let Error::Db(db) = error {
        for extra in [&db.detail, &db.hint].into_iter().flatten() {
            text.push_str(&format!(" {extra}"));
        }
    }
    text
}

/// The work of one run, on the Tokio runtime: the statement, its events
/// sent as they come, then the session closed whatever happened. When the
/// dialog is gone the statement is cancelled and waited for.
async fn execute(
    session: Arc<dyn Session>,
    sql: String,
    cancelled: bool,
    events: UnboundedSender<Event>,
) -> kerno_core::Result<()> {
    if cancelled {
        session.close().await;
        return Err(Error::Cancelled);
    }
    let mut execution = exec::run(session.clone(), sql, RowLimit::All);
    let mut outcome = Ok(());
    let mut gone = false;
    loop {
        // A dialog closed while the statement waits (a lock, a long vacuum)
        // sends nothing more: its end of the channel closing is the signal.
        let event = tokio::select! {
            event = execution.next() => event,
            () = events.closed(), if !gone => {
                gone = true;
                let _ = session.cancel().await;
                continue;
            }
        };
        let Some(event) = event else {
            break;
        };
        let event = match event {
            ExecEvent::Columns(columns) => {
                Event::Columns(columns.iter().map(|column| column.name.clone()).collect())
            }
            ExecEvent::Rows(batch) => Event::Rows(
                (0..batch.row_count())
                    .map(|row| {
                        (0..batch.column_count())
                            .map(|column| {
                                batch.cell(row, column).map_or_else(
                                    || "NULL".to_string(),
                                    |bytes| String::from_utf8_lossy(bytes).into_owned(),
                                )
                            })
                            .collect()
                    })
                    .collect(),
            ),
            ExecEvent::Notice(notice) => {
                Event::Notice(format!("{}: {}", notice.severity, notice.message))
            }
            ExecEvent::Complete { .. } => continue,
            ExecEvent::Finished(result) => {
                outcome = result;
                break;
            }
        };
        if !gone && events.send(event).is_err() {
            gone = true;
            let _ = session.cancel().await;
        }
    }
    drop(execution);
    session.close().await;
    outcome
}

impl MaintenanceDialog {
    /// A labelled row of the options.
    fn field(label: &'static str, field: AnyElement, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .gap_3()
            .items_start()
            .child(
                div()
                    .flex_none()
                    .w(px(96.))
                    .pt_0p5()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(field))
            .into_any_element()
    }

    fn render_options(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let chip = |index: usize, tool: &Maintenance| {
            let selected = index == self.opening.selected;
            // Truncate empties the table: shown in the danger colour once
            // chosen.
            let chip = if selected && matches!(tool, Maintenance::Truncate(_)) {
                Button::new(("maintenance-tool", index)).danger()
            } else {
                theme::chip(("maintenance-tool", index), selected)
            };
            chip.small()
                .label(label(tool))
                .on_click(cx.listener(move |this, _, _, cx| this.select(index, cx)))
        };
        let (destructive, safe): (Vec<_>, Vec<_>) = self
            .opening
            .tools
            .iter()
            .enumerate()
            .partition(|(_, tool)| matches!(tool, Maintenance::Truncate(_)));
        let mut rows = vec![Self::field(
            "Tool",
            h_flex()
                .gap_1()
                .flex_wrap()
                .children(safe.into_iter().map(|(index, tool)| chip(index, tool)))
                // Set apart, last, after a gap.
                .when(!destructive.is_empty(), |row| {
                    row.child(div().w(px(12.))).children(
                        destructive
                            .into_iter()
                            .map(|(index, tool)| chip(index, tool)),
                    )
                })
                .into_any_element(),
            cx,
        )];
        // Only the options this engine writes (MySQL's TRUNCATE has none).
        let tool = self.tool().clone();
        let dialect = self.dialect_of.clone();
        let mut probe = tool.clone();
        let shown: Vec<(usize, &'static str, bool)> = flags(&mut probe)
            .into_iter()
            .enumerate()
            .map(|(index, (label, on))| (index, label, *on))
            .filter(|(index, _, _)| {
                let mut with = tool.clone();
                if let Some((_, flag)) = flags(&mut with).into_iter().nth(*index) {
                    *flag = true;
                }
                dialect.as_ref().is_some_and(|meta| {
                    meta.dialect()
                        .maintenance_sql(&with, &self.opening.object)
                        .is_ok()
                })
            })
            .collect();
        if !shown.is_empty() {
            rows.push(Self::field(
                "Options",
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children(shown.into_iter().map(|(index, name, on)| {
                        theme::chip(("maintenance-option", index), on)
                            .small()
                            .label(name)
                            .disabled(!on && blocked(&tool, index))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.set_option(index, !on, cx)),
                            )
                    }))
                    .into_any_element(),
                cx,
            ));
        }
        if let Maintenance::Check(chosen) = tool {
            let choices = [None].into_iter().chain(CheckOption::ALL.map(Some));
            rows.push(Self::field(
                "Option",
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children(choices.enumerate().map(|(index, option)| {
                        theme::chip(("maintenance-check", index), chosen == option)
                            .small()
                            .label(option.map_or("Default", CheckOption::sql))
                            .on_click(cx.listener(move |this, _, _, cx| this.set_check(option, cx)))
                    }))
                    .into_any_element(),
                cx,
            ));
        }
        rows
    }

    fn render_sql(&self, text: Result<String, String>, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        div()
            .id("maintenance-sql")
            .test_support()
            .p_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_sm()
            .map(|this| match text {
                Ok(sql) => this.child(SharedString::from(sql)),
                Err(reason) => this
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(reason)),
            })
            .into_any_element()
    }

    /// The status rows, as a small table.
    fn render_rows(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let line = |cells: &[String], header: bool| {
            h_flex()
                .gap_2()
                .children(cells.iter().enumerate().map(|(index, cell)| {
                    // ponytail: fixed widths for Table, Op, Msg_type, Msg_text;
                    // a real grid if other engines answer with wider rows.
                    div()
                        .when(index + 1 < cells.len(), |this| this.flex_none().w(px(120.)))
                        .when(index + 1 == cells.len(), |this| this.flex_1().min_w_0())
                        .truncate()
                        .when(header, |this| this.text_color(theme.muted_foreground))
                        .child(SharedString::from(cell.clone()))
                }))
        };
        v_flex()
            .id("maintenance-rows")
            .test_support()
            .p_2()
            .gap_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .child(line(&self.columns, true))
            .children(self.rows.iter().enumerate().map(|(index, row)| {
                line(row, false)
                    .id(("maintenance-row", index))
                    .test_support()
            }))
            .into_any_element()
    }

    /// The server's notices.
    fn render_log(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        div()
            .id("maintenance-log")
            .test_support()
            .max_h(px(220.))
            .overflow_y_scroll()
            .p_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .map(|this| {
                if self.notices.is_empty() {
                    this.text_color(theme.muted_foreground)
                        .child("The server reported nothing.")
                } else {
                    this.children(
                        self.notices
                            .iter()
                            .map(|notice| div().child(SharedString::from(notice.clone()))),
                    )
                }
            })
            .into_any_element()
    }
}

impl Render for MaintenanceDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let close = |label: &'static str| {
            Button::new("maintenance-close")
                .ghost()
                .label(label)
                .on_click(|_, window, cx| window.close_dialog(cx))
        };
        let footer = h_flex().justify_end().gap_2();
        let mut body: Vec<AnyElement> = Vec::new();
        match &self.step {
            Step::Options => {
                body.extend(self.render_options(cx));
                let sql = self.sql();
                let ready = sql.is_ok();
                body.push(Self::field(
                    "Statement",
                    self.render_sql(sql.map(|sql| sql.sql), cx),
                    cx,
                ));
                body.push(
                    footer
                        .child(close("Cancel"))
                        .child(
                            Button::new("maintenance-run")
                                .primary()
                                .label(SharedString::from(format!("{}…", label(self.tool()))))
                                .disabled(!ready)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.confirm(window, cx)),
                                ),
                        )
                        .into_any_element(),
                );
            }
            step => {
                let (text, color) = match step {
                    Step::Running if self.cancel_requested => {
                        ("Cancelling…".to_string(), theme.muted_foreground)
                    }
                    Step::Running => ("Running…".to_string(), theme.muted_foreground),
                    Step::Done(elapsed) => (
                        format!("Done in {}.", duration_text(*elapsed)),
                        theme.success,
                    ),
                    Step::Cancelled => (
                        "Cancelled: the server stopped the statement.".to_string(),
                        theme.danger,
                    ),
                    Step::Failed(error) => (format!("Failed: {error}"), theme.danger),
                    Step::Options => unreachable!(),
                };
                body.push(
                    div()
                        .id("maintenance-outcome")
                        .test_support()
                        .text_sm()
                        .text_color(color)
                        .child(SharedString::from(text))
                        .into_any_element(),
                );
                body.push(self.render_sql(Ok(self.ran.clone()), cx));
                if self.sql().is_ok_and(|sql| sql.returns_rows) {
                    body.push(self.render_rows(cx));
                } else {
                    body.push(self.render_log(cx));
                }
                let running = *step == Step::Running;
                body.push(
                    footer
                        .when(running, |this| {
                            this.child(
                                Button::new("maintenance-cancel")
                                    .outline()
                                    .label("Cancel")
                                    .disabled(self.cancel_requested)
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                            )
                        })
                        .when(!running, |this| {
                            this.child(
                                Button::new("maintenance-back")
                                    .outline()
                                    .label("Back")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.step = Step::Options;
                                        cx.notify();
                                    })),
                            )
                            .child(close("Close").primary())
                        })
                        .into_any_element(),
                );
            }
        }
        v_flex()
            .id("maintenance-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .child(div().text_sm().child(SharedString::from(format!(
                "On the {} “{}” of the database “{}”, on “{}”.",
                self.opening.kind, self.opening.name, self.opening.database, self.connection_name
            ))))
            .children(body)
    }
}

/// The tool of `tools` like `wanted` (whatever its options), or the first.
pub fn position(tools: &[Maintenance], wanted: Option<&Maintenance>) -> usize {
    wanted
        .and_then(|wanted| {
            tools
                .iter()
                .position(|tool| discriminant(tool) == discriminant(wanted))
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::Dialect as _;
    use kerno_core::maintenance::{RefreshOptions, VacuumOptions};
    use kerno_core::schema::{RelationKind, TableRef};

    #[test]
    fn options_are_dbeavers_and_what_the_server_refuses_together_is_blocked() {
        let mut vacuum = Maintenance::Vacuum(VacuumOptions::default());
        let labels: Vec<&str> = flags(&mut vacuum).into_iter().map(|(l, _)| l).collect();
        assert_eq!(
            labels,
            [
                "Full",
                "Freeze",
                "Analyze",
                "Disable page skipping",
                "Skip locked",
                "Index cleanup"
            ]
        );
        assert!(flags(&mut vacuum).iter().all(|(_, on)| !**on));
        let full = Maintenance::Vacuum(VacuumOptions {
            full: true,
            ..VacuumOptions::default()
        });
        assert!(blocked(&full, 3) && !blocked(&full, 1) && !blocked(&vacuum, 3));
        // A refresh keeps the data by default; CONCURRENTLY needs it.
        let mut refresh = Maintenance::RefreshMaterializedView(RefreshOptions::default());
        assert_eq!(
            flags(&mut refresh)
                .into_iter()
                .map(|(l, on)| (l, *on))
                .collect::<Vec<_>>(),
            [("With data", true), ("Concurrently", false)]
        );
        let empty = Maintenance::RefreshMaterializedView(RefreshOptions {
            with_data: false,
            concurrently: false,
        });
        assert!(blocked(&empty, 1) && !blocked(&refresh, 1));
        assert!(flags(&mut Maintenance::Analyze).is_empty());
    }

    #[test]
    fn the_confirmation_says_what_full_and_cascade_do() {
        let full = Maintenance::Vacuum(VacuumOptions {
            full: true,
            ..VacuumOptions::default()
        });
        let text = explanation(&full, "table", "orders");
        assert!(
            text.contains("exclusive lock") && text.contains("waits"),
            "{text}"
        );
        assert!(
            explanation(&Maintenance::Analyze, "database", "kerno")
                .contains("every table of the database “kerno”")
        );
        let cascade = Maintenance::Truncate(kerno_core::maintenance::TruncateOptions {
            cascade: true,
            ..Default::default()
        });
        assert!(explanation(&cascade, "table", "t").contains("tables that reference it"));
    }

    #[test]
    fn a_tool_is_found_whatever_its_options() {
        let pg = kerno_postgres::PgDialect;
        let table = ObjectRef::Relation(TableRef::new("s", "t"), RelationKind::Table);
        let tools = pg.maintenance_tools(&table);
        let truncate = Maintenance::Truncate(kerno_core::maintenance::TruncateOptions {
            cascade: true,
            ..Default::default()
        });
        assert_eq!(label(&tools[position(&tools, Some(&truncate))]), "Truncate");
        assert_eq!(position(&tools, None), 0);
        assert_eq!(position(&tools, Some(&Maintenance::Optimize)), 0);
    }
}

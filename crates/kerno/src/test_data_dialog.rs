//! "Generate test data…" (K72, D-141): rows made for a table on this machine
//! from a seed, exact about its rules. The dialog reads the table once (its
//! structure, its row count, the largest value of each integer key, the keys
//! of each referenced table) on a session opened for that; one line per
//! column holds the kind of values it gets, guessed from its type, its keys
//! and its name, and that kind's two settings; a preview of the first rows
//! follows the settings, made off the UI thread. "Open script" writes the
//! `INSERT` statements into a query tab on the table's connection, and nothing
//! runs; "Insert" shows its SQL, asks as the environment's rules say
//! (production twice), and inserts every row in one transaction, with
//! progress and Cancel, or none.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, WeakEntity, Window, div, px,
};
use kerno_core::schema::TableRef;
use kerno_core::test_data::{
    self, ColumnSettings, Facts, Kind, MAX_SCRIPT_ROWS, PREVIEW_ROWS, Plan, Settings, grouped,
};
use kerno_core::{Error, Session, describe};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::tabs::Target;
use crate::workspace::Workspace;

/// Rows of the first `INSERT` the confirmation shows.
const SHOWN_ROWS: usize = 3;

/// A column line's share for the column's name, and the widths of its kind
/// (the longest, "A key of the referenced table", whole) and of its NULL %.
const NAME_SHARE: f32 = 0.2;
const KIND_WIDTH: f32 = 220.;
const NULLS_WIDTH: f32 = 48.;

/// What the dialog says the seed does (K76: values that must be unique are
/// drawn after the rows already there, so a second run does not repeat them).
const SAME_ROWS: &str = "The same settings give the same rows, except values that must be \
                         unique, which continue after the rows already in the table.";

/// The last settings of each table, for the run of the app (K76, in memory
/// only): the dialog opens on them again, its seed included; New seed is the
/// way to other rows.
#[derive(Default)]
struct Kept(HashMap<(String, Option<String>, TableRef), Last>);

impl gpui_kit::Global for Kept {}

#[derive(Clone)]
struct Last {
    settings: Settings,
    /// The columns' names when kept: a table changed since gets the guess.
    names: Vec<String>,
    /// Each column's settings as its kind started then: a setting left as it
    /// was takes the one it starts with now (a sequence from the largest key
    /// there now).
    started: Vec<ColumnSettings>,
}

/// The key of a table's kept settings: its connection, its database (the
/// connection's own when the place names none, so that the tree, a data tab
/// and the palette find the same settings) and the table.
fn kept_key(
    connections: &Entity<Connections>,
    target: &Target,
    table: &TableRef,
    cx: &App,
) -> (String, Option<String>, TableRef) {
    let database = target.database.clone().or_else(|| {
        let saved = connections.read(cx).get(&target.connection)?;
        saved.config.database.clone()
    });
    (target.connection.clone(), database, table.clone())
}

/// One column's line.
struct Line {
    kind: Kind,
    from: Entity<InputState>,
    to: Entity<InputState>,
    nulls: Entity<InputState>,
}

/// The settings checked, with the preview of their first rows.
struct Checked {
    plan: Plan,
    names: Vec<String>,
    rows: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Settings,
    Running { inserted: u64 },
    Done { inserted: u64 },
    Cancelled,
    Failed(String),
}

pub struct TestDataDialog {
    connections: Entity<Connections>,
    workspace: WeakEntity<Workspace>,
    target: Target,
    table: TableRef,
    facts: Option<Result<Arc<Facts>, String>>,
    rows: Entity<InputState>,
    seed: Entity<InputState>,
    lines: Vec<Line>,
    /// What the settings give: the plan and its preview, or why there is none.
    checked: Option<Result<Arc<Checked>, String>>,
    /// Bumped at each change, so a late preview is dropped.
    generation: u64,
    checking: Option<Task<()>>,
    /// "Open script" is writing it.
    writing: Option<Task<()>>,
    step: Step,
    cancel: Arc<AtomicBool>,
    /// The session of the insert while it runs, to cancel its statement.
    session: Option<Arc<dyn Session>>,
    /// The table's last valid settings in this run of the app, until the
    /// table is read.
    last: Option<Last>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// Opens "Generate test data…" on `table` of `target` (its database, and its
/// schema where the engine has schemas); or says why it cannot.
pub fn open(
    connections: Entity<Connections>,
    workspace: WeakEntity<Workspace>,
    target: Target,
    table: TableRef,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<TestDataDialog>> {
    if !matches!(
        connections.read(cx).status(&target.connection),
        Status::Connected { .. }
    ) {
        window.push_notification(
            Notification::warning("Generate test data: connect first, the table is read there."),
            cx,
        );
        return None;
    }
    let title = format!("Generate test data for {}.{}", table.schema, table.name);
    let view = cx.new(|cx| TestDataDialog::new(connections, workspace, target, table, window, cx));
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        dialog
            .title(SharedString::from(title.clone()))
            .w(px(940.))
            .on_ok(|_, _, _| false)
            // The body scrolls in a short window; the buttons stay in view (K76).
            .content(move |content, _, _| content.min_h_0().child(view.clone()))
    });
    Some(view)
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

/// A number as typed: `1,000`, `1 000` and `1_000` are 1000.
fn number(text: &str) -> Option<u64> {
    let digits: String = text
        .chars()
        .filter(|c| !matches!(c, ',' | ' ' | '_'))
        .collect();
    digits.parse().ok()
}

impl TestDataDialog {
    fn new(
        connections: Entity<Connections>,
        workspace: WeakEntity<Workspace>,
        target: Target,
        table: TableRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let key = kept_key(&connections, &target, &table, cx);
        let last = cx
            .try_global::<Kept>()
            .and_then(|kept| kept.0.get(&key))
            .cloned();
        let (rows, seed) = match &last {
            Some(last) => (last.settings.rows, last.settings.seed),
            None => (100, test_data::new_seed()),
        };
        let rows = cx.new(|cx| InputState::new(window, cx).default_value(rows.to_string()));
        let seed = cx.new(|cx| InputState::new(window, cx).default_value(seed.to_string()));
        let subscriptions = [&rows, &seed]
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.check(cx);
                    }
                })
            })
            .into();
        let mut dialog = Self {
            connections,
            workspace,
            target,
            table,
            facts: None,
            rows,
            seed,
            lines: Vec::new(),
            checked: None,
            generation: 0,
            checking: None,
            writing: None,
            step: Step::Settings,
            cancel: Arc::new(AtomicBool::new(false)),
            session: None,
            last,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        dialog.read(window, cx);
        dialog
    }

    /// Reads the table on a session opened for that.
    fn read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let opening = self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        );
        let table = self.table.clone();
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let outcome = match opening.await {
                Err(error) => Err(error),
                Ok(session) => {
                    let Ok(reading) = cx.update(|_, cx| {
                        bridge::spawn(cx, async move {
                            let facts = test_data::read(session.clone(), &table).await;
                            session.close().await;
                            facts
                        })
                    }) else {
                        return;
                    };
                    reading.await
                }
            };
            let _ = this.update_in(cx, |this, window, cx| match outcome {
                Ok(facts) => this.take_facts(facts, window, cx),
                Err(error) => {
                    this.facts = Some(Err(describe(&error).to_string()));
                    cx.notify();
                }
            });
        }));
    }

    /// The key of the table's kept settings.
    fn key(&self, cx: &App) -> (String, Option<String>, TableRef) {
        kept_key(&self.connections, &self.target, &self.table, cx)
    }

    /// The columns' kept settings, if the table still has those columns and
    /// they can still get those kinds.
    fn kept_columns(&mut self, facts: &Facts) -> Option<Vec<ColumnSettings>> {
        let last = self.last.take()?;
        let names: Vec<String> = facts
            .structure
            .columns
            .iter()
            .map(|c| c.name.clone())
            .collect();
        let fits = last.names == names
            && (last.settings.columns.iter().enumerate())
                .all(|(index, line)| test_data::kinds(facts, index).contains(&line.kind));
        fits.then(|| {
            (last.settings.columns.iter().zip(&last.started).enumerate())
                .map(|(index, (line, then))| {
                    let now = test_data::defaults(facts, index, line.kind);
                    ColumnSettings {
                        from: if line.from == then.from {
                            now.from
                        } else {
                            line.from.clone()
                        },
                        to: if line.to == then.to {
                            now.to
                        } else {
                            line.to.clone()
                        },
                        ..line.clone()
                    }
                })
                .collect()
        })
    }

    /// One line per column: as kept from the last time, or as guessed.
    fn take_facts(&mut self, facts: Facts, window: &mut Window, cx: &mut Context<Self>) {
        let mut lines = Vec::new();
        let columns = self
            .kept_columns(&facts)
            .unwrap_or_else(|| test_data::guess(&facts));
        for line in columns {
            let [from, to, nulls] = [line.from, line.to, line.nulls.to_string()].map(|text| {
                let input = cx.new(|cx| InputState::new(window, cx).default_value(text));
                self._subscriptions.push(cx.subscribe(
                    &input,
                    |this, _, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::Change) {
                            this.check(cx);
                        }
                    },
                ));
                input
            });
            lines.push(Line {
                kind: line.kind,
                from,
                to,
                nulls,
            });
        }
        self.lines = lines;
        self.facts = Some(Ok(Arc::new(facts)));
        self.check(cx);
    }

    /// The settings as typed, or why they are not settings yet.
    fn settings(&self, cx: &App) -> Result<Settings, String> {
        let rows = number(&value(&self.rows, cx))
            .ok_or_else(|| "Type how many rows to make.".to_string())?;
        let seed = number(&value(&self.seed, cx)).ok_or_else(|| {
            "The seed is a whole number: type one, or click New seed.".to_string()
        })?;
        let columns = self
            .lines
            .iter()
            .map(|line| {
                let nulls = value(&line.nulls, cx);
                let nulls = if nulls.is_empty() {
                    0
                } else {
                    nulls
                        .trim_end_matches('%')
                        .trim()
                        .parse::<u8>()
                        .map_err(|_| "A share of NULL is a percentage: 0 to 100.".to_string())?
                };
                Ok(ColumnSettings {
                    kind: line.kind,
                    from: value(&line.from, cx),
                    to: value(&line.to, cx),
                    nulls,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Settings {
            rows,
            seed,
            columns,
        })
    }

    /// Checks the settings and makes the preview off the UI thread.
    fn check(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let Some(Ok(facts)) = self.facts.clone() else {
            return;
        };
        let settings = match self.settings(cx) {
            Ok(settings) => settings,
            Err(problem) => {
                self.checked = Some(Err(problem));
                self.checking = None;
                cx.notify();
                return;
            }
        };
        let last = Last {
            names: (facts.structure.columns.iter())
                .map(|c| c.name.clone())
                .collect(),
            started: (settings.columns.iter().enumerate())
                .map(|(index, line)| test_data::defaults(&facts, index, line.kind))
                .collect(),
            settings: settings.clone(),
        };
        let key = self.key(cx);
        cx.default_global::<Kept>().0.insert(key, last);
        let checking = cx.background_spawn(async move {
            let plan = Plan::new(&facts, &settings)?;
            let rows = plan
                .preview(PREVIEW_ROWS)?
                .iter()
                .map(|row| row.iter().map(test_data::shown).collect())
                .collect();
            Ok(Arc::new(Checked {
                names: plan.names(),
                plan,
                rows,
            }))
        });
        self.checking = Some(cx.spawn(async move |this, cx| {
            let checked = checking.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.checked = Some(checked);
                    this.checking = None;
                    cx.notify();
                }
            });
        }));
    }

    /// Gives column `index` values of `kind`, with that kind's settings.
    pub fn set_kind(
        &mut self,
        index: usize,
        kind: Kind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Ok(facts)) = self.facts.clone() else {
            return;
        };
        let defaults = test_data::defaults(&facts, index, kind);
        let Some(line) = self.lines.get_mut(index) else {
            return;
        };
        line.kind = kind;
        let (from, to) = (line.from.clone(), line.to.clone());
        from.update(cx, |input, cx| input.set_value(defaults.from, window, cx));
        to.update(cx, |input, cx| input.set_value(defaults.to, window, cx));
        if matches!(kind, Kind::Null | Kind::Default | Kind::Server) {
            let nulls = line.nulls.clone();
            nulls.update(cx, |input, cx| input.set_value("0", window, cx));
        }
        self.check(cx);
    }

    /// A new seed: other rows.
    fn new_seed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let seed = test_data::new_seed().to_string();
        self.seed
            .update(cx, |input, cx| input.set_value(seed, window, cx));
        self.check(cx);
    }

    fn plan(&self) -> Option<&Plan> {
        match &self.checked {
            Some(Ok(checked)) if self.checking.is_none() => Some(&checked.plan),
            _ => None,
        }
    }

    /// Why nothing can be made yet, in words.
    pub fn problem(&self) -> Option<String> {
        match (&self.facts, &self.checked) {
            (None, _) => Some(format!("Reading {}…", self.table.name)),
            (Some(Err(error)), _) => Some(format!("The table could not be read: {error}")),
            (_, Some(Err(problem))) => Some(problem.clone()),
            _ => None,
        }
    }

    /// Why Insert cannot run on this connection, if it cannot.
    fn read_only(&self, cx: &App) -> Option<String> {
        let connections = self.connections.read(cx);
        let saved = connections.get(&self.target.connection)?;
        saved.config.read_only.then(|| {
            format!(
                "“{}” is a read-only connection: Insert is off. Open script writes the \
                 statements without running them.",
                saved.config.name
            )
        })
    }

    fn heading(&self, plan: &Plan, cx: &App) -> String {
        let connection = self
            .connections
            .read(cx)
            .get(&self.target.connection)
            .map(|saved| saved.config.name.clone())
            .unwrap_or_default();
        let seed = number(&value(&self.seed, cx)).unwrap_or_default();
        format!(
            "{} rows of test data for {}.{} on {connection}, seed {seed}.\nThe same settings \
             give the same rows, except values that must be unique,\nwhich continue after the \
             rows already in the table.\nWritten by Kerno's Generate test data; nothing ran. Run \
             the script to insert them;\nswitch the tab to manual commit first to keep all of \
             them or none.",
            grouped(plan.rows()),
            self.table.schema,
            self.table.name,
        )
    }

    /// Writes the statements into a new query tab on the table's connection,
    /// off the UI thread; nothing runs.
    pub fn open_script(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.writing.is_some() {
            return;
        }
        let Some(plan) = self.plan().cloned() else {
            return;
        };
        let Some(meta) = self
            .connections
            .read(cx)
            .meta_session(&self.target.connection)
        else {
            return;
        };
        let heading = self.heading(&plan, cx);
        let title = format!("Test data for {}", self.table.name);
        let target = self.target.clone();
        let writing = cx.background_spawn(async move { plan.script(meta.dialect(), &heading) });
        self.writing = Some(cx.spawn_in(window, async move |this, cx| {
            let script = writing.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.writing = None;
                match script {
                    Ok(sql) => {
                        let workspace = this.workspace.clone();
                        window.close_dialog(cx);
                        let _ = workspace.update(cx, |workspace, cx| {
                            workspace.open_query(title, &sql, Some(target), window, cx)
                        });
                    }
                    Err(problem) => {
                        this.checked = Some(Err(problem));
                        cx.notify();
                    }
                }
            });
        }));
        cx.notify();
    }

    /// Shows the first statement and, once confirmed as the environment's
    /// rules ask, inserts.
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only(cx).is_some() {
            return;
        }
        let Some(plan) = self.plan().cloned() else {
            return;
        };
        let connections = self.connections.read(cx);
        let (Some(saved), Some(meta)) = (
            connections.get(&self.target.connection),
            connections.meta_session(&self.target.connection),
        ) else {
            return;
        };
        let sql = match plan.first_statement(meta.dialect(), SHOWN_ROWS) {
            Ok(sql) => sql,
            Err(problem) => {
                self.checked = Some(Err(problem));
                cx.notify();
                return;
            }
        };
        let rows = plan.rows();
        let view = cx.entity();
        sql_confirm::open(
            Confirmation {
                title: format!(
                    "Insert {} {} into {}.{}",
                    grouped(rows),
                    if rows == 1 { "row" } else { "rows" },
                    self.table.schema,
                    self.table.name
                ),
                explanation: "The rows are inserted in one transaction: all of them, or none. \
                              A row the server refuses stops it, and nothing is kept."
                    .into(),
                statement: Statement::Fixed(sql),
                action: "Insert",
                destructive: false,
                connection: saved.config.name.clone(),
                environment: saved.config.environment.clone(),
                writes: true,
            },
            window,
            cx,
            move |_, window, cx| {
                let plan = plan.clone();
                view.update(cx, |this, cx| this.start(plan, window, cx));
            },
        );
    }

    /// Inserts on a session of its own.
    fn start(&mut self, plan: Plan, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel = Arc::new(AtomicBool::new(false));
        self.step = Step::Running { inserted: 0 };
        let cancel = self.cancel.clone();
        let opening = self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        );
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        self.tasks.push(cx.spawn(async move |this, cx| {
            while let Some(inserted) = receiver.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if let Step::Running { inserted: shown } = &mut this.step {
                        *shown = inserted;
                        cx.notify();
                    }
                });
            }
        }));
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
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
                                test_data::insert(session.clone(), plan, progress, cancel).await;
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
                this.step = match outcome {
                    Ok(report) if report.failed.is_none() => {
                        let (target, table) = (this.target.clone(), this.table.clone());
                        let _ = this.workspace.update(cx, |workspace, cx| {
                            workspace.imported(&target, &table, window, cx)
                        });
                        Step::Done {
                            inserted: report.inserted,
                        }
                    }
                    // However it ended, a cancelled insert kept nothing.
                    _ if cancelled => Step::Cancelled,
                    Err(Error::Cancelled) => Step::Cancelled,
                    Ok(report) => Step::Failed(
                        report
                            .failed
                            .map(|failure| failure.to_string())
                            .unwrap_or_default(),
                    ),
                    Err(error) => Step::Failed(describe(&error).to_string()),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Stops the insert: everything is rolled back.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Release);
        if let Some(session) = self.session.clone() {
            bridge::spawn(cx, async move {
                if let Err(error) = session.cancel().await {
                    tracing::debug!(%error, "the insert of test data could not be cancelled");
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// Where the dialog is, in words; for tests.
    #[cfg(test)]
    pub fn state(&self) -> String {
        match &self.step {
            Step::Settings => match (&self.facts, self.plan(), self.problem()) {
                (None, ..) => "reading".into(),
                (_, Some(plan), _) => format!("ready: {} rows", plan.rows()),
                (_, None, Some(problem)) => format!("refused: {problem}"),
                _ => "checking".into(),
            },
            Step::Running { inserted } => format!("running: {inserted}"),
            Step::Done { inserted } => format!("done: {inserted} rows"),
            Step::Cancelled => "cancelled".into(),
            Step::Failed(report) => format!("failed: {report}"),
        }
    }

    /// The preview as drawn: its header, then its rows; for tests.
    #[cfg(test)]
    pub fn preview(&self) -> Option<(Vec<String>, Vec<Vec<String>>)> {
        match &self.checked {
            Some(Ok(checked)) => Some((checked.names.clone(), checked.rows.clone())),
            _ => None,
        }
    }

    /// Sets the inputs of column `index` (and the rows, the seed), as typed;
    /// for tests.
    #[cfg(test)]
    pub fn type_into(
        &mut self,
        field: &str,
        index: usize,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = match field {
            "rows" => self.rows.clone(),
            "seed" => self.seed.clone(),
            "from" => self.lines[index].from.clone(),
            "to" => self.lines[index].to.clone(),
            _ => self.lines[index].nulls.clone(),
        };
        input.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
        self.check(cx);
    }

    /// The rows and the seed as typed, and each column's kind; for tests.
    #[cfg(test)]
    pub fn typed(&self, cx: &App) -> (String, String, Vec<Kind>) {
        (
            value(&self.rows, cx),
            value(&self.seed, cx),
            self.lines.iter().map(|line| line.kind).collect(),
        )
    }

    /// The index of the column `name`; for tests.
    #[cfg(test)]
    pub fn column(&self, name: &str) -> usize {
        let Some(Ok(facts)) = &self.facts else {
            panic!("the table is not read");
        };
        facts
            .structure
            .columns
            .iter()
            .position(|column| column.name == name)
            .expect("a column of the table")
    }

    // Rendering -------------------------------------------------------------------

    /// One line per column. The column's name takes a share of the width and
    /// the settings what is left, so that a line fits the dialog at the
    /// window's smallest size with the longest kind and the widest settings
    /// (K76): only the column's name is cut, never a setting.
    fn render_lines(&self, facts: &Facts, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        // A disabled field as a disabled button is drawn: grey, its border
        // faint (the kit's disabled field looks enabled in the dark theme).
        let (off_background, off_border) = (theme.muted, theme.input.opacity(0.5));
        let view = cx.entity();
        let name_width = gpui_kit::relative(NAME_SHARE);
        let header = h_flex()
            .gap_2()
            .text_xs()
            .text_color(muted)
            .child(div().w(name_width).flex_none().child("Column"))
            .child(div().w(px(KIND_WIDTH)).flex_none().child("Values"))
            .child(div().flex_1().min_w_0().child("Settings"))
            .child(div().w(px(NULLS_WIDTH)).flex_none().child("NULL %"));
        let rows = self.lines.iter().enumerate().map(|(index, line)| {
            let column = &facts.structure.columns[index];
            let offered = test_data::kinds(facts, index);
            let menu = {
                let view = view.clone();
                let picked = line.kind;
                move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                    for kind in &offered {
                        let (view, kind) = (view.clone(), *kind);
                        menu = menu.item(
                            PopupMenuItem::new(kind.label())
                                .checked(kind == picked)
                                .on_click(move |_, window, cx| {
                                    view.update(cx, |this, cx| {
                                        this.set_kind(index, kind, window, cx)
                                    })
                                }),
                        );
                    }
                    menu
                }
            };
            let fixed = test_data::kinds(facts, index).len() < 2;
            let params = line.kind.params();
            let setting = |input: &Entity<InputState>, id: &'static str, name: &'static str| {
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().flex_none().text_xs().text_color(muted).child(name))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(input).small().id((id, index))),
                    )
            };
            let nulls_off =
                !column.nullable || matches!(line.kind, Kind::Null | Kind::Server | Kind::Default);
            h_flex()
                .gap_2()
                .py(px(2.))
                .child(
                    v_flex()
                        .w(name_width)
                        .flex_none()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(SharedString::from(column.name.clone())),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(SharedString::from(column.data_type.clone())),
                        ),
                )
                .child(
                    div().w(px(KIND_WIDTH)).flex_none().child(
                        Button::new(("test-data-kind", index))
                            .small()
                            .outline()
                            .w_full()
                            .label(line.kind.label())
                            .when(!fixed, |button| button.dropdown_caret(true))
                            .disabled(fixed)
                            .dropdown_menu(menu),
                    ),
                )
                .child(h_flex().flex_1().min_w_0().gap_2().when_some(
                    params,
                    |this, [first, second]| {
                        this.child(setting(&line.from, "test-data-from", first))
                            .child(setting(&line.to, "test-data-to", second))
                    },
                ))
                .child(
                    div().w(px(NULLS_WIDTH)).flex_none().child(
                        Input::new(&line.nulls)
                            .small()
                            .id(("test-data-nulls", index))
                            .disabled(nulls_off)
                            .when(nulls_off, |input| {
                                input.bg(off_background).border_color(off_border)
                            }),
                    ),
                )
                .into_any_element()
        });
        v_flex()
            .gap_1()
            .child(header)
            .child(
                v_flex()
                    .id("test-data-columns")
                    .test_support()
                    .max_h(px(250.))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_preview(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Ok(checked)) = &self.checked else {
            return None;
        };
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let mono = theme.mono_font_family.clone();
        let cell = |text: String, header: bool| {
            div()
                .w(px(150.))
                .flex_none()
                .px_2()
                .py(px(1.))
                .truncate()
                .border_r_1()
                .border_color(border)
                .when(header, |this| this.font_weight(FontWeight::MEDIUM))
                .when(!header && text == "NULL", |this| this.text_color(muted))
                .child(SharedString::from(text))
        };
        let header = h_flex()
            .border_b_1()
            .border_color(border)
            .children(checked.names.iter().map(|name| cell(name.clone(), true)));
        let rows = checked
            .rows
            .iter()
            .map(|row| h_flex().children(row.iter().map(|value| cell(value.clone(), false))));
        let text = checked
            .rows
            .iter()
            .map(|row| row.join("\t"))
            .collect::<Vec<_>>()
            .join("\n");
        Some(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(format!(
                            "The first {} rows, as they will be written:",
                            checked.rows.len()
                        ))),
                )
                .child(
                    v_flex()
                        .id("test-data-preview")
                        .test_support()
                        .test_text(text)
                        .max_h(px(200.))
                        .overflow_scroll()
                        .border_1()
                        .border_color(border)
                        .rounded(theme.radius)
                        .text_xs()
                        .font_family(mono)
                        .child(header)
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (muted, warning) = (theme.muted_foreground, theme.warning);
        let intro = format!(
            "Rows made on this computer from the seed. {SAME_ROWS} Keys stay unique, foreign \
             keys point at rows that exist. Open script writes the statements in a query tab and \
             runs nothing; Insert asks first, then inserts every row in one transaction, or none."
        );
        let mut body = vec![
            div()
                .id("test-data-intro")
                .test_support()
                .test_text(intro.clone())
                .text_sm()
                .text_color(muted)
                .whitespace_normal()
                .child(SharedString::from(intro))
                .into_any_element(),
            h_flex()
                .gap_3()
                .child(div().text_sm().child("Rows"))
                .child(
                    div()
                        .w(px(110.))
                        .child(Input::new(&self.rows).small().id("test-data-rows")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(format!(
                            "up to {} (a script up to {})",
                            grouped(test_data::MAX_ROWS),
                            grouped(MAX_SCRIPT_ROWS)
                        ))),
                )
                .child(div().w(px(16.)))
                .child(div().text_sm().child("Seed"))
                .child(
                    div()
                        .w(px(110.))
                        .child(Input::new(&self.seed).small().id("test-data-seed")),
                )
                .child(
                    Button::new("test-data-new-seed")
                        .ghost()
                        .small()
                        .label("New seed")
                        .tooltip("Other rows")
                        .on_click(cx.listener(|this, _, window, cx| this.new_seed(window, cx))),
                )
                .into_any_element(),
        ];
        if let Some(Ok(facts)) = self.facts.clone() {
            body.push(self.render_lines(&facts, cx));
            let notes = test_data::notes(&facts);
            if !notes.is_empty() {
                let text = notes.join("\n");
                body.push(
                    div()
                        .id("test-data-notes")
                        .test_support()
                        .test_text(text)
                        .text_xs()
                        .text_color(muted)
                        .whitespace_normal()
                        .children(
                            notes
                                .into_iter()
                                .map(|note| div().child(SharedString::from(note))),
                        )
                        .into_any_element(),
                );
            }
        }
        if let Some(problem) = self.problem() {
            body.push(
                div()
                    .id("test-data-problem")
                    .test_support()
                    .test_text(problem.clone())
                    .text_sm()
                    .whitespace_normal()
                    .text_color(muted)
                    .child(SharedString::from(problem))
                    .into_any_element(),
            );
        }
        body.extend(self.render_preview(cx));
        let read_only = self.read_only(cx);
        if let Some(read_only) = read_only.clone() {
            body.push(
                div()
                    .id("test-data-read-only")
                    .test_support()
                    .test_text(read_only.clone())
                    .text_xs()
                    .text_color(warning)
                    .whitespace_normal()
                    .child(SharedString::from(read_only))
                    .into_any_element(),
            );
        }
        let plan = self.plan();
        let too_long = plan.is_some_and(|plan| plan.rows() > MAX_SCRIPT_ROWS);
        body.push(
            h_flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("test-data-close")
                        .ghost()
                        .label("Cancel")
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .child(
                    Button::new("test-data-insert")
                        .outline()
                        .label("Insert…")
                        .tooltip("Insert the rows in one transaction, after showing the SQL")
                        .disabled(plan.is_none() || read_only.is_some())
                        .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                )
                .child(
                    Button::new("test-data-open-script")
                        .primary()
                        .label(if self.writing.is_some() {
                            "Writing the script…"
                        } else {
                            "Open script"
                        })
                        .tooltip(if too_long {
                            "Too many rows for a script: use Insert"
                        } else {
                            "Open the INSERT statements in a query tab: nothing runs"
                        })
                        .disabled(plan.is_none() || too_long || self.writing.is_some())
                        .on_click(cx.listener(|this, _, window, cx| this.open_script(window, cx))),
                )
                .into_any_element(),
        );
        body
    }
}

impl Drop for TestDataDialog {
    /// Closing the dialog stops an insert that still runs.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Render for TestDataDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (danger, success) = (theme.danger, theme.success);
        let message = |id: &'static str, text: String| {
            div()
                .id(id)
                .test_support()
                .test_text(text.clone())
                .text_sm()
                .whitespace_normal()
                .child(SharedString::from(text))
        };
        let close = |label: &'static str| {
            Button::new("test-data-close")
                .ghost()
                .label(label)
                .on_click(|_, window, cx| window.close_dialog(cx))
        };
        let footer = h_flex().justify_end().gap_2();
        let table = format!("{}.{}", self.table.schema, self.table.name);
        let mut body: Vec<AnyElement> = match &self.step {
            Step::Settings => self.render_settings(cx),
            Step::Running { inserted } => {
                let total = self.plan().map_or(0, Plan::rows);
                vec![
                    message(
                        "test-data-progress",
                        format!(
                            "{} of {} rows sent to {table}. Nothing is kept until every row is \
                             in: the insert is one transaction.",
                            grouped(*inserted),
                            grouped(total)
                        ),
                    )
                    .into_any_element(),
                    footer
                        .child(
                            Button::new("test-data-cancel")
                                .outline()
                                .label("Cancel insert")
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                        )
                        .into_any_element(),
                ]
            }
            Step::Done { inserted } => vec![
                message(
                    "test-data-done",
                    format!("{} rows inserted into {table}.", grouped(*inserted)),
                )
                .text_color(success)
                .into_any_element(),
                footer.child(close("Close").primary()).into_any_element(),
            ],
            Step::Cancelled | Step::Failed(_) => {
                let (id, text) = match &self.step {
                    // Plain words first, then the server's (C14).
                    Step::Failed(report) => (
                        "test-data-failed",
                        format!("Nothing was inserted: the server refused {report}"),
                    ),
                    _ => (
                        "test-data-cancelled",
                        "The insert was cancelled. Nothing was inserted.".to_string(),
                    ),
                };
                let failed = id == "test-data-failed";
                vec![
                    message(id, text)
                        .when(failed, |this| this.text_color(danger))
                        .into_any_element(),
                    footer
                        .child(
                            Button::new("test-data-back")
                                .outline()
                                .label("Back")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.step = Step::Settings;
                                    cx.notify();
                                })),
                        )
                        .child(close("Close"))
                        .into_any_element(),
                ]
            }
        };
        // Every step ends with its buttons: they stay below the body, which
        // scrolls when the window is too short for it.
        let footer = body.pop();
        v_flex()
            .id("test-data-dialog")
            .test_support()
            .test_text(format!("Generate test data for {table}"))
            .flex_1()
            .min_h_0()
            .gap_3()
            .pt_1()
            .child(
                v_flex()
                    .id("test-data-body")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .gap_3()
                    .children(body),
            )
            .children(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::number;

    #[test]
    fn row_counts_are_read_as_people_type_them() {
        assert_eq!(number("1,000"), Some(1000));
        assert_eq!(number("1 000 000"), Some(1_000_000));
        assert_eq!(number("12_5"), Some(125));
        assert_eq!(number("ten"), None);
        assert_eq!(number(""), None);
    }
}

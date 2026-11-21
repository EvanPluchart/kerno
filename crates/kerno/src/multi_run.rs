//! Run one script on several connections (CLAUDE.md §4 J14; DBeaver #3605,
//! #1804). A dialog lists the connections as the sidebar's tree, with a
//! checkbox each (a folder ticks the connections in it). Start opens one
//! query tab per ticked connection, holding the script, right after the
//! source tab, and runs them through each tab's own run path, at most
//! [`AT_ONCE`] at a time: its connection is opened when needed (password
//! asked as usual), its environment's confirmations are asked (one at a
//! time: the next target starts once the one before runs or ended), a
//! read-only connection refuses each write, and its history, notices and
//! cancel are the tab's. The dialog then lists each target with its
//! outcome; a click goes to its tab. Nothing runs on a connection that is
//! not ticked.

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, TestSupportExt as _, WeakEntity, Window, div, px,
};
use kerno_core::sql::{LexRules, find_params, split_statements};
use kerno_core::{Engine, TxState, environment};

use crate::connections::manager::{Connections, Status, folder_of};
use crate::engine_logo::engine_logo;
use crate::navigator::model::{Item, RowId, parse_row, sidebar_items};
use crate::query::tab::{QueryTab, duration_text};
use crate::state::AppState;
use crate::tabs::Target;
use crate::workspace::Workspace;

/// Targets that run at the same time, at most.
pub const AT_ONCE: usize = 4;

/// What an error does to the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnError {
    /// That connection stops; the others go on.
    #[default]
    StopConnection,
    /// Every connection stops: those running are cancelled, those waiting
    /// are not started.
    StopEverything,
    /// That connection runs the statements after the error.
    Continue,
}

impl OnError {
    pub const ALL: [OnError; 3] = [Self::StopConnection, Self::StopEverything, Self::Continue];

    fn label(self) -> &'static str {
        match self {
            Self::StopConnection => "Stop this connection",
            Self::StopEverything => "Stop every connection",
            Self::Continue => "Continue",
        }
    }
}

/// Where one target's run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stage {
    /// Opening its connection (and asking its password).
    #[default]
    Starting,
    /// Its environment's confirmation is open.
    Asking,
    Running,
    /// The confirmation was declined: nothing ran.
    Declined,
    /// It stopped at an error, or could not start.
    Failed,
    Cancelled,
    /// Every statement was run (some may have failed, with "Continue").
    Done,
}

/// What a query tab's part of a run on several connections did; kept by
/// the tab, read by the dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptRun {
    pub continue_on_error: bool,
    pub stage: Stage,
    /// Statements sent to the server.
    pub statements: usize,
    /// Writes refused on a read-only connection, not sent.
    pub refused: usize,
    /// Rows returned or changed.
    pub rows: u64,
    pub elapsed: Duration,
    pub first_error: Option<String>,
}

impl ScriptRun {
    pub fn new(continue_on_error: bool) -> Self {
        Self {
            continue_on_error,
            ..Self::default()
        }
    }

    /// Not ended yet.
    pub fn active(&self) -> bool {
        matches!(self.stage, Stage::Starting | Stage::Asking | Stage::Running)
    }

    /// A statement sent to the server ended.
    pub fn record(&mut self, elapsed: Duration, rows: Option<u64>, error: Option<&str>) {
        self.statements += 1;
        self.rows += rows.unwrap_or(0);
        self.elapsed += elapsed;
        if let Some(error) = error {
            self.first_error.get_or_insert_with(|| error.to_string());
        }
    }

    /// The run ended as `stage`, `error` being why when nothing said so yet.
    pub fn end(&mut self, stage: Stage, error: Option<String>) {
        self.stage = stage;
        if let Some(error) = error {
            self.first_error.get_or_insert(error);
        }
    }
}

/// A target, for planning what to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    /// Not started: the run was stopped first.
    Skipped,
    Started(Stage),
}

/// The next target to start, if one may start now: the first waiting one,
/// while fewer than [`AT_ONCE`] run and no started one is still opening its
/// connection or asking (so that two prompts are never open at once), and
/// the run is not halted.
pub fn next_to_start(phases: &[Phase], halted: bool) -> Option<usize> {
    if halted {
        return None;
    }
    let unsettled = phases.iter().any(|phase| {
        matches!(
            phase,
            Phase::Started(Stage::Starting) | Phase::Started(Stage::Asking)
        )
    });
    let running = phases
        .iter()
        .filter(|phase| matches!(phase, Phase::Started(Stage::Running)))
        .count();
    if unsettled || running >= AT_ONCE {
        return None;
    }
    phases.iter().position(|phase| *phase == Phase::Waiting)
}

/// Whether the run stops everywhere now: "Stop every connection" and a
/// target stopped at an error. A declined confirmation is not an error.
pub fn halts(on_error: OnError, phases: &[Phase]) -> bool {
    on_error == OnError::StopEverything && phases.contains(&Phase::Started(Stage::Failed))
}

/// Ticks every one of `ids`, or unticks them all when they all are (a
/// folder's checkbox).
pub fn toggle(ticked: &mut BTreeSet<String>, ids: &[String]) {
    if !ids.is_empty() && ids.iter().all(|id| ticked.contains(id)) {
        for id in ids {
            ticked.remove(id);
        }
    } else {
        ticked.extend(ids.iter().cloned());
    }
}

/// Why `script` cannot run on connections of these engines, if it cannot:
/// no statement, or parameters, which a run on several connections cannot
/// ask for.
pub fn script_problem(script: &str, engines: &[Engine]) -> Option<&'static str> {
    let rules = |engine: &Engine| LexRules::of(*engine);
    let all = [Engine::Postgres];
    let engines = if engines.is_empty() {
        &all[..]
    } else {
        engines
    };
    if engines
        .iter()
        .any(|engine| split_statements(script, rules(engine)).is_empty())
    {
        return Some("The script has no statement to run.");
    }
    if engines
        .iter()
        .any(|engine| !find_params(script, rules(engine)).is_empty())
    {
        return Some(
            "The script has parameters (:name, $1 or ?), which a run on several connections \
             cannot ask for: write their values in the script.",
        );
    }
    None
}

/// The first lines of the script, for the dialog.
fn preview(script: &str) -> String {
    const LINES: usize = 6;
    let lines: Vec<&str> = script.trim().lines().collect();
    let mut shown = lines[..lines.len().min(LINES)].join("\n");
    if lines.len() > LINES {
        shown.push_str(&format!("\n… {} more lines", lines.len() - LINES));
    }
    shown
}

/// One connection the script runs on.
struct RunTarget {
    connection: String,
    name: String,
    engine: Engine,
    tab: WeakEntity<QueryTab>,
    /// Its tab was closed.
    closed: bool,
    started: bool,
    skipped: bool,
    /// The tab's part of the run, as last read (kept once the tab closes).
    last: Option<ScriptRun>,
}

impl RunTarget {
    fn phase(&self) -> Phase {
        if self.skipped {
            Phase::Skipped
        } else if !self.started {
            Phase::Waiting
        } else {
            Phase::Started(self.last.as_ref().map_or(Stage::Starting, |run| run.stage))
        }
    }
}

/// The dialog, and the run it started; kept by the workspace so the run
/// goes on when the dialog is closed.
pub struct MultiRun {
    workspace: WeakEntity<Workspace>,
    connections: Entity<Connections>,
    source: WeakEntity<QueryTab>,
    title: String,
    script: String,
    ticked: BTreeSet<String>,
    on_error: OnError,
    /// Empty until Start.
    targets: Vec<RunTarget>,
    /// Cancel all, or an error with "Stop every connection".
    halted: bool,
    _subscriptions: Vec<Subscription>,
}

/// Opens the dialog for the script of `source`; returns its view, which
/// the workspace keeps.
pub fn open(
    workspace: &Entity<Workspace>,
    source: &Entity<QueryTab>,
    connections: Entity<Connections>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<MultiRun> {
    let (title, script) = {
        let tab = source.read(cx);
        (tab.title().to_string(), tab.sql(cx))
    };
    let view = cx.new(|_| MultiRun {
        workspace: workspace.downgrade(),
        connections,
        source: source.downgrade(),
        title,
        script,
        ticked: BTreeSet::new(),
        on_error: OnError::default(),
        targets: Vec::new(),
        halted: false,
        _subscriptions: Vec::new(),
    });
    show(&view, window, cx);
    view
}

/// Shows the dialog of `view`: its choices, or its run.
pub fn show(view: &Entity<MultiRun>, window: &mut Window, cx: &mut App) {
    let view = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = view.clone();
        dialog
            .title("Run script on connections")
            .w(px(680.))
            // Enter neither starts nor closes: Start takes a click.
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
}

impl MultiRun {
    /// Whether targets still wait or run.
    pub fn in_progress(&self) -> bool {
        self.targets.iter().any(|target| {
            matches!(
                target.phase(),
                Phase::Waiting | Phase::Started(Stage::Starting | Stage::Asking | Stage::Running)
            )
        })
    }

    /// Ticks or unticks one connection.
    pub fn toggle_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        toggle(&mut self.ticked, &[id.to_string()]);
        cx.notify();
    }

    /// Ticks every connection of `folder` (its sub-folders too), or unticks
    /// them all.
    pub fn toggle_folder(&mut self, folder: &str, cx: &mut Context<Self>) {
        let ids = self.folder_connections(folder, cx);
        toggle(&mut self.ticked, &ids);
        cx.notify();
    }

    pub fn set_on_error(&mut self, on_error: OnError, cx: &mut Context<Self>) {
        self.on_error = on_error;
        cx.notify();
    }

    fn folder_connections(&self, folder: &str, cx: &App) -> Vec<String> {
        self.connections
            .read(cx)
            .saved()
            .iter()
            .filter(|saved| {
                folder_of(&saved.config)
                    .is_some_and(|path| kerno_core::config::in_folder(&path, folder))
            })
            .map(|saved| saved.id.clone())
            .collect()
    }

    /// The engines of the ticked connections.
    fn engines(&self, cx: &App) -> Vec<Engine> {
        let connections = self.connections.read(cx);
        self.ticked
            .iter()
            .filter_map(|id| connections.get(id).map(|saved| saved.config.engine))
            .collect()
    }

    /// Why Start is not possible now.
    pub fn problem(&self, cx: &App) -> Option<&'static str> {
        if self.ticked.is_empty() {
            return Some("Tick the connections to run the script on.");
        }
        script_problem(&self.script, &self.engines(cx))
    }

    /// Opens one tab per ticked connection, in the sidebar's order, and
    /// starts the first runs.
    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.problem(cx).is_some() || !self.targets.is_empty() {
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let order: Vec<(String, String, Engine)> = {
            let connections = self.connections.read(cx);
            self.rows(cx)
                .into_iter()
                .filter_map(|(_, row)| match row {
                    Row::Connection(id) if self.ticked.contains(&id) => {
                        let saved = connections.get(&id)?;
                        Some((id, saved.config.name.clone(), saved.config.engine))
                    }
                    _ => None,
                })
                .collect()
        };
        let mut after = self.source.upgrade();
        for (connection, name, engine) in order {
            let target = Target {
                connection: connection.clone(),
                database: None,
                schema: None,
            };
            let title = format!("{} @ {name}", self.title);
            let tab = workspace.update(cx, |workspace, cx| {
                workspace.open_query_after(after.as_ref(), title, &self.script, target, window, cx)
            });
            self._subscriptions
                .push(cx.observe_in(&tab, window, |this, _, window, cx| this.tick(window, cx)));
            // A tab closed during the run: what was going on there is over.
            let index = self.targets.len();
            self._subscriptions.push(cx.observe_release_in(
                &tab,
                window,
                move |this: &mut Self, tab: &mut QueryTab, window, cx| {
                    let mut run = tab.script().cloned();
                    if let Some(run) = run.as_mut().filter(|run| run.active()) {
                        run.end(Stage::Cancelled, None);
                    }
                    if let Some(target) = this.targets.get_mut(index) {
                        target.closed = true;
                        target.last = run.or_else(|| target.last.take());
                    }
                    this.tick(window, cx)
                },
            ));
            self.targets.push(RunTarget {
                connection,
                name,
                engine,
                tab: tab.downgrade(),
                closed: false,
                started: false,
                skipped: false,
                last: None,
            });
            after = Some(tab);
        }
        self.tick(window, cx);
    }

    /// Reads where each target stands, stops everything when an error asks
    /// for it, and starts what may start.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for target in self.targets.iter_mut().filter(|target| !target.closed) {
            if let Some(tab) = target.tab.upgrade() {
                target.last = tab.read(cx).script().cloned();
            }
        }
        if !self.halted && halts(self.on_error, &self.phases()) {
            self.halt(cx);
        }
        while let Some(index) = next_to_start(&self.phases(), self.halted) {
            let target = &mut self.targets[index];
            target.started = true;
            let continue_on_error = self.on_error == OnError::Continue;
            match target.tab.upgrade().filter(|_| !target.closed) {
                Some(tab) => {
                    tab.update(cx, |tab, cx| tab.run_script(continue_on_error, window, cx));
                    target.last = tab.read(cx).script().cloned();
                }
                None => {
                    let mut run = ScriptRun::new(continue_on_error);
                    run.end(Stage::Cancelled, None);
                    target.last = Some(run);
                }
            }
        }
        cx.notify();
    }

    fn phases(&self) -> Vec<Phase> {
        self.targets.iter().map(RunTarget::phase).collect()
    }

    /// Stops every target: those waiting are not started, those running are
    /// cancelled (a cancel the server has not seen yet stops at the next
    /// statement).
    fn halt(&mut self, cx: &mut Context<Self>) {
        self.halted = true;
        for target in &mut self.targets {
            if !target.started {
                target.skipped = true;
            } else if target.last.as_ref().is_some_and(ScriptRun::active)
                && let Some(tab) = target.tab.upgrade()
            {
                tab.update(cx, |tab, cx| tab.cancel(cx));
            }
        }
        cx.notify();
    }

    /// Cancel all.
    pub fn cancel_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.halt(cx);
        self.tick(window, cx);
    }

    /// Activates the tab of target `index`, closing the dialog.
    pub fn go_to(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self
            .targets
            .get(index)
            .and_then(|target| target.tab.upgrade())
        else {
            return;
        };
        window.close_dialog(cx);
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.show_query_tab(&tab, window, cx)
            });
        }
    }

    /// The tabs of the targets, in order; for tests.
    #[cfg(test)]
    pub fn target_tabs(&self) -> Vec<Option<Entity<QueryTab>>> {
        self.targets
            .iter()
            .map(|target| target.tab.upgrade())
            .collect()
    }

    /// One line per target: name, status and what it did; for tests.
    #[cfg(test)]
    pub fn summary(&self, cx: &App) -> Vec<String> {
        (0..self.targets.len())
            .map(|index| {
                let (status, stats, error) = self.describe(index, cx);
                line(&self.targets[index].name, &status, &stats, error.as_deref())
            })
            .collect()
    }

    /// Status, numbers and first error of target `index`.
    fn describe(&self, index: usize, cx: &App) -> (String, String, Option<String>) {
        let target = &self.targets[index];
        let run = target.last.as_ref();
        let connecting = matches!(
            self.connections.read(cx).status(&target.connection),
            Status::Connecting | Status::WaitingForInput
        );
        let status = match target.phase() {
            Phase::Waiting => "Waiting".to_string(),
            Phase::Skipped => "Not run: the run was stopped".to_string(),
            Phase::Started(Stage::Starting) if connecting => "Connecting…".to_string(),
            Phase::Started(Stage::Starting) => "Starting…".to_string(),
            Phase::Started(Stage::Asking) => "Waiting for your confirmation".to_string(),
            Phase::Started(Stage::Running) => "Running…".to_string(),
            Phase::Started(Stage::Declined) => "Not run: confirmation declined".to_string(),
            Phase::Started(Stage::Failed) => "Stopped at an error".to_string(),
            Phase::Started(Stage::Cancelled) => "Cancelled".to_string(),
            Phase::Started(Stage::Done) if run.is_some_and(|r| r.first_error.is_some()) => {
                "Done, with errors".to_string()
            }
            Phase::Started(Stage::Done) => "Done".to_string(),
        };
        let mut stats = Vec::new();
        if let Some(run) = run.filter(|run| run.statements > 0 || run.refused > 0) {
            let plural = |count: usize, one: &str, many: &str| {
                format!("{count} {}", if count == 1 { one } else { many })
            };
            stats.push(plural(run.statements, "statement", "statements"));
            stats.push(format!(
                "{} {}",
                run.rows,
                if run.rows == 1 { "row" } else { "rows" }
            ));
            stats.push(duration_text(run.elapsed));
            if run.refused > 0 {
                stats.push(format!(
                    "{} refused (read-only)",
                    plural(run.refused, "write", "writes")
                ));
            }
        }
        // Production starts in manual commit: its tab holds the transaction.
        if let Some(tab) = target.tab.upgrade()
            && tab.read(cx).tx_state() != TxState::Idle
        {
            stats.push("transaction open: commit or roll back in its tab".into());
        }
        (
            status,
            stats.join(" · "),
            run.and_then(|run| run.first_error.clone()),
        )
    }

    /// The rows of the connection tree, with their depth.
    fn rows(&self, cx: &App) -> Vec<(usize, Row)> {
        let connections = self.connections.read(cx);
        let listed: Vec<(String, String, Option<String>)> = connections
            .saved()
            .iter()
            .map(|saved| {
                (
                    saved.id.clone(),
                    saved.config.name.clone(),
                    folder_of(&saved.config),
                )
            })
            .collect();
        let items = sidebar_items(
            &connections.folders(cx),
            &listed,
            &BTreeSet::new(),
            &HashSet::new(),
            "",
            |_| Vec::new(),
        );
        let mut rows = Vec::new();
        flatten(&items, 0, &mut rows);
        rows
    }

    fn render_choices(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let connections = self.connections.read(cx);
        let environments = &AppState::get(cx).settings.environments;
        let rows: Vec<AnyElement> = self
            .rows(cx)
            .into_iter()
            .enumerate()
            .map(|(index, (depth, row))| {
                let indent = px(depth as f32 * 18.);
                match row {
                    Row::Folder(path) => {
                        let ids = self.folder_connections(&path, cx);
                        let all = !ids.is_empty() && ids.iter().all(|id| self.ticked.contains(id));
                        let name = kerno_core::config::last_folder_name(&path).to_string();
                        h_flex()
                            .pl(indent)
                            .child(
                                Checkbox::new(("multi-folder", index))
                                    .label(SharedString::from(name))
                                    .checked(all)
                                    .disabled(ids.is_empty())
                                    .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                        this.toggle_folder(&path, cx)
                                    })),
                            )
                            .into_any_element()
                    }
                    Row::Connection(id) => {
                        let Some(saved) = connections.get(&id) else {
                            return div().into_any_element();
                        };
                        let config = &saved.config;
                        let env = environment::environment(environments, &config.environment);
                        let mut notes = Vec::new();
                        if config.environment != environment::LOCAL {
                            notes.push(env.name.clone());
                        }
                        if config.read_only {
                            notes.push("read-only".to_string());
                        }
                        let ticked = self.ticked.contains(&id);
                        h_flex()
                            .pl(indent)
                            .gap_1()
                            .child(
                                Checkbox::new(("multi-connection", index))
                                    .checked(ticked)
                                    .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                        this.toggle_connection(&id, cx)
                                    })),
                            )
                            .child(engine_logo(config.engine, px(14.), cx))
                            .child(
                                div()
                                    .text_sm()
                                    .child(SharedString::from(config.name.clone())),
                            )
                            .when(!notes.is_empty(), |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(SharedString::from(notes.join(" · "))),
                                )
                            })
                            .into_any_element()
                    }
                }
            })
            .collect();
        let empty = rows.is_empty();
        let problem = self.problem(cx);
        let selected = OnError::ALL.iter().position(|on| *on == self.on_error);
        v_flex()
            .gap_3()
            .child(div().text_sm().child(SharedString::from(format!(
                "Runs “{}” on each connection ticked below, in a query tab of its own. Each \
                 connection's rules apply: a read-only one refuses writes, and one whose \
                 environment confirms runs asks first.",
                self.title
            ))))
            .child(
                div()
                    .id("multi-connections")
                    .test_support()
                    .max_h(px(240.))
                    .overflow_y_scroll()
                    .p_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .children(rows)
                    .when(empty, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("No saved connection: add one first."),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(div().text_sm().child("On error:"))
                    .child(
                        RadioGroup::horizontal("multi-on-error")
                            .children(OnError::ALL.map(OnError::label))
                            .selected_index(selected)
                            .on_change(cx.listener(|this, index: &usize, _, cx| {
                                if let Some(on_error) = OnError::ALL.get(*index) {
                                    this.set_on_error(*on_error, cx);
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .id("multi-script")
                    .test_support()
                    .p_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .child(SharedString::from(preview(&self.script))),
            )
            .when_some(problem, |this, problem| {
                this.child(
                    div()
                        .id("multi-problem")
                        .test_support()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(problem),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("multi-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("multi-start")
                            .primary()
                            .label(SharedString::from(match self.ticked.len() {
                                0 | 1 => "Start".to_string(),
                                count => format!("Start on {count} connections"),
                            }))
                            .disabled(problem.is_some())
                            .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_run(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let progress = self.in_progress();
        let rows: Vec<AnyElement> = (0..self.targets.len())
            .map(|index| {
                let target = &self.targets[index];
                let (status, stats, error) = self.describe(index, cx);
                let line = line(&target.name, &status, &stats, error.as_deref());
                let failed = matches!(
                    target.phase(),
                    Phase::Started(Stage::Failed | Stage::Declined)
                ) || error.is_some();
                v_flex()
                    .id(("multi-target", index))
                    .test_support()
                    .aria_label(SharedString::from(line))
                    .px_2()
                    .py_1()
                    .gap_0p5()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.list_hover))
                    .on_click(cx.listener(move |this, _, window, cx| this.go_to(index, window, cx)))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(engine_logo(target.engine, px(14.), cx))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(SharedString::from(target.name.clone())),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(if failed {
                                        theme.danger
                                    } else {
                                        theme.foreground
                                    })
                                    .child(SharedString::from(status)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::from(stats)),
                            ),
                    )
                    .when_some(error, |this, error| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.danger)
                                .truncate()
                                .child(SharedString::from(error)),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_3()
            .child(div().text_sm().child(SharedString::from(if progress {
                format!(
                    "Running “{}” on {} connections, at most {AT_ONCE} at a time. Click one to \
                     see its tab.",
                    self.title,
                    self.targets.len()
                )
            } else {
                format!(
                    "“{}” ran on {} connections. Click one to see its tab.",
                    self.title,
                    self.targets.len()
                )
            })))
            .child(
                div()
                    .id("multi-targets")
                    .test_support()
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .p_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .children(rows),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("multi-cancel-all")
                            .danger()
                            .label("Cancel all")
                            .disabled(!progress)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.cancel_all(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("multi-close")
                            .ghost()
                            .label("Close")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
            .into_any_element()
    }
}

/// A target in one line: `name: status · numbers · first error`.
fn line(name: &str, status: &str, stats: &str, error: Option<&str>) -> String {
    let mut line = format!("{name}: {status}");
    for part in [Some(stats).filter(|stats| !stats.is_empty()), error]
        .into_iter()
        .flatten()
    {
        line.push_str(" · ");
        line.push_str(part);
    }
    line
}

/// A row of the connection tree.
enum Row {
    Folder(String),
    Connection(String),
}

/// The folders and connections of the sidebar's tree, depth first; the
/// lines under them ("Empty", a connection's status) are left out.
fn flatten(items: &[Item], depth: usize, rows: &mut Vec<(usize, Row)>) {
    for item in items {
        match parse_row(&item.id) {
            Some(RowId::Folder(path)) => {
                rows.push((depth, Row::Folder(path.to_string())));
                flatten(&item.children, depth + 1, rows);
            }
            Some(RowId::Connection(id)) => rows.push((depth, Row::Connection(id.to_string()))),
            _ => {}
        }
    }
}

impl Render for MultiRun {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.targets.is_empty() {
            self.render_choices(cx)
        } else {
            self.render_run(cx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use Phase::{Skipped, Started, Waiting};

    #[test]
    fn targets_start_one_at_a_time_until_four_run() {
        // Nothing has started: the first one starts.
        assert_eq!(next_to_start(&[Waiting, Waiting], false), Some(0));
        // One is opening its connection or asking: no other starts, so
        // that two prompts never open at once.
        for unsettled in [Stage::Starting, Stage::Asking] {
            assert_eq!(next_to_start(&[Started(unsettled), Waiting], false), None);
        }
        // Running or ended ones let the next start, up to four running.
        assert_eq!(
            next_to_start(
                &[
                    Started(Stage::Running),
                    Started(Stage::Done),
                    Started(Stage::Declined),
                    Waiting
                ],
                false
            ),
            Some(3)
        );
        let four = [Started(Stage::Running); AT_ONCE];
        let mut phases = four.to_vec();
        phases.push(Waiting);
        assert_eq!(next_to_start(&phases, false), None);
        phases[1] = Started(Stage::Failed);
        assert_eq!(next_to_start(&phases, false), Some(AT_ONCE));
        // A halted run starts nothing; a skipped one is never started.
        assert_eq!(next_to_start(&[Waiting], true), None);
        assert_eq!(next_to_start(&[Skipped, Started(Stage::Done)], false), None);
    }

    #[test]
    fn only_stop_everything_halts_and_only_on_an_error() {
        let failed = [Started(Stage::Failed), Started(Stage::Running), Waiting];
        assert!(halts(OnError::StopEverything, &failed));
        assert!(!halts(OnError::StopConnection, &failed));
        assert!(!halts(OnError::Continue, &failed));
        // A declined confirmation skips that target only (J14).
        let declined = [Started(Stage::Declined), Started(Stage::Cancelled), Waiting];
        assert!(!halts(OnError::StopEverything, &declined));
    }

    #[test]
    fn a_folder_ticks_its_connections_or_unticks_them_all() {
        let ids = ["a".to_string(), "b".to_string()];
        let mut ticked = BTreeSet::from(["a".to_string(), "z".to_string()]);
        toggle(&mut ticked, &ids);
        assert_eq!(ticked, BTreeSet::from(["a", "b", "z"].map(String::from)));
        toggle(&mut ticked, &ids);
        assert_eq!(ticked, BTreeSet::from(["z".to_string()]));
        // An empty folder ticks nothing.
        toggle(&mut ticked, &[]);
        assert_eq!(ticked, BTreeSet::from(["z".to_string()]));
    }

    #[test]
    fn a_script_without_statements_or_with_parameters_is_refused() {
        let all = [Engine::Postgres, Engine::MySql, Engine::MariaDb];
        assert_eq!(script_problem("SELECT 1; SELECT 2", &all), None);
        assert!(script_problem("  -- nothing\n", &all).is_some());
        for script in [
            "SELECT * FROM t WHERE id = :id",
            "SELECT * FROM t WHERE id = $1",
            "SELECT * FROM t WHERE id = ?",
        ] {
            assert!(
                script_problem(script, &all).is_some_and(|why| why.contains("parameters")),
                "{script}"
            );
        }
        // A `?` inside a string is no parameter.
        assert_eq!(script_problem("SELECT '?' AS mark", &all), None);
    }

    #[test]
    fn a_run_counts_its_statements_and_keeps_the_first_error() {
        let mut run = ScriptRun::new(true);
        assert!(run.active());
        run.record(Duration::from_millis(5), Some(3), None);
        run.record(Duration::from_millis(7), None, Some("no such table"));
        run.record(Duration::from_millis(1), Some(1), Some("later error"));
        run.end(Stage::Done, None);
        assert!(!run.active());
        assert_eq!(
            (run.statements, run.rows, run.elapsed),
            (3, 4, Duration::from_millis(13))
        );
        assert_eq!(run.first_error.as_deref(), Some("no such table"));
    }

    #[test]
    fn the_preview_shows_the_first_lines() {
        let script = (1..=9).map(|n| format!("SELECT {n};")).collect::<Vec<_>>();
        let shown = preview(&script.join("\n"));
        assert!(shown.starts_with("SELECT 1;\n"), "{shown}");
        assert!(shown.ends_with("… 3 more lines"), "{shown}");
    }
}

//! The simple view of a backup and of a restore (K26, D-099): one screen
//! that says what is backed up or restored, where, in which format and
//! with which client, and runs it. "Advanced settings…" is the wizard, on
//! the same state: both views build the command with the same code. Kerno's
//! own design.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input as TextInput, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, TestSupportExt as _, Window, div, px,
};
use kerno_core::config::Engine;
use kerno_core::tools::{
    BackupContent, BackupFormat, BackupOptions, ClientFamily, DumpMethod, EngineBackup,
    RestoreOptions, mysql_folder,
};

use super::{BackupDialog, Input, Loaded, Mode, Purpose, Step, View, is_view};
use crate::settings_ui::{self, OptionCards};

/// The formats the simple view offers, the default first: PostgreSQL's
/// custom archive (restored whole or in part, compressed) or a plain
/// script; MySQL and MariaDB write a script only.
fn simple_formats(family: ClientFamily) -> &'static [BackupFormat] {
    match family {
        ClientFamily::Postgres => &[BackupFormat::Custom, BackupFormat::Sql],
        ClientFamily::MySql => &[BackupFormat::Sql],
    }
}

/// How many connections "Faster" uses, for a backup (K48) as for a restore:
/// the machine's cores, 8 at most.
pub(crate) fn faster_jobs() -> u32 {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(8) as u32)
}

/// `n` things, in words.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "a, b and c".
pub(super) fn listed(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A row of the simple view's panel: its name, then what it holds.
fn row(label: &'static str, content: impl IntoElement, cx: &App) -> AnyElement {
    h_flex()
        .gap_3()
        .items_start()
        .px_3()
        .py_2()
        .child(
            div()
                .flex_none()
                .w(px(72.))
                .pt_0p5()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(v_flex().flex_1().min_w_0().gap_1().child(content))
        .into_any_element()
}

/// The preview of the archive card: a closed box of packed tables.
fn archive_preview(cx: &App) -> AnyElement {
    let theme = cx.theme();
    let block = |width: f32| {
        div()
            .h(px(7.))
            .w(px(width))
            .rounded_sm()
            .bg(theme.muted_foreground.opacity(0.45))
    };
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.muted)
        .child(
            v_flex()
                .gap(px(3.))
                .p(px(6.))
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .child(block(54.))
                .child(block(54.))
                .child(
                    h_flex()
                        .gap(px(3.))
                        .child(block(34.))
                        .child(div().h(px(7.)).w(px(17.)).rounded_sm().bg(theme.primary)),
                ),
        )
        .into_any_element()
}

/// The preview of the script card: lines of SQL text.
fn script_preview(cx: &App) -> AnyElement {
    let theme = cx.theme();
    let line = |keyword: f32, rest: f32| {
        h_flex()
            .gap(px(3.))
            .child(
                div()
                    .h(px(4.))
                    .w(px(keyword))
                    .rounded_sm()
                    .bg(theme.primary),
            )
            .child(
                div()
                    .h(px(4.))
                    .w(px(rest))
                    .rounded_sm()
                    .bg(theme.muted_foreground.opacity(0.45)),
            )
    };
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.muted)
        .child(
            v_flex()
                .gap(px(4.))
                .child(line(18., 48.))
                .child(line(12., 30.))
                .child(line(18., 40.))
                .child(line(12., 52.)),
        )
        .into_any_element()
}

impl BackupDialog {
    /// Whether the simple view's page shows now (not the client list, the
    /// progress, nor a preparation that failed).
    pub(super) fn simple_page_shown(&self) -> bool {
        matches!(self.step, Step::Options) && !self.showing_clients
    }

    /// Whether this dialog has a simple view: a backup or a restore of a
    /// run. A task's settings stay in the wizard.
    pub(super) fn simple_offered(&self) -> bool {
        self.purpose == Purpose::Run && matches!(self.mode, Mode::Backup | Mode::Restore)
    }

    /// "Advanced settings…": the wizard, on its first step, with what the
    /// simple view holds (the file, the format, the client).
    pub fn show_advanced(&mut self, cx: &mut Context<Self>) {
        self.view = View::Advanced;
        self.showing_clients = false;
        self.page = self.pages()[0];
        cx.notify();
    }

    /// The view shown, by name; for tests.
    #[cfg(test)]
    pub fn view_name(&self) -> &'static str {
        match self.view {
            View::Simple => "simple",
            View::Advanced => "advanced",
        }
    }

    /// The jobs field holds the number "Faster" uses.
    fn faster_jobs_typed(&self, cx: &App) -> bool {
        Self::text(&self.jobs, cx) == faster_jobs().to_string()
    }

    /// The simple backup's "Faster" is ticked (K48, D-117): a PostgreSQL
    /// directory archive written by pg_dump on [`faster_jobs`] connections,
    /// or a MySQL / MariaDB folder of one file per table written by that
    /// many mysqldump at once (K63, D-135).
    pub(super) fn faster_backup(&self, cx: &App) -> bool {
        self.backup.format == BackupFormat::Directory && self.faster_jobs_typed(cx)
    }

    /// The simple backup's "Block writes during the backup" is ticked (K63):
    /// the folder backup holds the server's read lock ("lock all tables").
    pub(super) fn blocks_writes(&self) -> bool {
        matches!(&self.backup.engine, EngineBackup::MySql(my) if my.method == DumpMethod::LockAllTables)
    }

    /// The simple restore's "Faster" is ticked: pg_restore on
    /// [`faster_jobs`] connections, outside one transaction.
    pub(super) fn faster_restore(&self, cx: &App) -> bool {
        !self.restore.single_transaction && self.faster_jobs_typed(cx)
    }

    /// Ticks or unticks the simple backup's "Faster": the directory format
    /// and its jobs, or the custom archive again (the Speed row shows only
    /// beside the archive, so that is what was chosen before).
    fn set_faster_backup(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (format, jobs) = if on {
            (BackupFormat::Directory, faster_jobs().to_string())
        } else {
            (simple_formats(self.family)[0], String::new())
        };
        self.backup.format = format;
        // The read lock is the faster backup's own choice in this view.
        if !on && let EngineBackup::MySql(my) = &mut self.backup.engine {
            my.method = DumpMethod::default();
        }
        self.jobs
            .update(cx, |input, cx| input.set_value(jobs, window, cx));
        cx.notify();
    }

    /// The wizard's settings the simple view neither shows nor sets that
    /// differ from its defaults, in words: what going back to it resets.
    /// The folder, the file name, a format the simple view offers and its
    /// "Faster" (format and jobs) are shown there, so they are kept.
    pub(super) fn advanced_changes(&self, cx: &App) -> Vec<&'static str> {
        let typed = |input: &Entity<InputState>| !Self::text(input, cx).is_empty();
        let mut changes = Vec::new();
        let mut note = |changed: bool, what: &'static str| {
            if changed {
                changes.push(what);
            }
        };
        match self.mode {
            Mode::Backup => {
                let defaults = BackupOptions::new(self.engine, self.database.clone());
                let faster = self.faster_backup(cx);
                note(!self.selection.complete, "the objects ticked");
                note(
                    !simple_formats(self.family).contains(&self.backup.format) && !faster,
                    "the format",
                );
                note(self.backup.content != defaults.content, "the content");
                // The jobs are the Jobs field's; "Faster" shows the read lock.
                let mut shown = self.backup.engine.clone();
                if let EngineBackup::MySql(my) = &mut shown {
                    my.jobs = None;
                    if faster && my.method == DumpMethod::LockAllTables {
                        my.method = DumpMethod::default();
                    }
                }
                note(shown != defaults.engine, "the dump tool's options");
                // The jobs count only with the directory format, the only
                // one pg_dump runs them for.
                let jobs =
                    self.backup.format == BackupFormat::Directory && typed(&self.jobs) && !faster;
                note(
                    typed(&self.compression) || typed(&self.encoding) || jobs,
                    "the compression, encoding or jobs",
                );
            }
            Mode::Restore => {
                let faster = self.faster_restore(cx);
                let mut defaults = RestoreOptions::new(
                    self.engine,
                    self.restore.database.clone(),
                    self.restore.input.clone(),
                    self.restore.format,
                );
                // The simple view's checkboxes set these (K35, "Faster").
                if self.restore.no_owner && self.restore.no_privileges {
                    (defaults.no_owner, defaults.no_privileges) = (true, true);
                }
                if faster {
                    defaults.single_transaction = false;
                }
                note(self.restore != defaults, "the restore options");
                note(typed(&self.jobs) && !faster, "the jobs");
                note(self.allow_commands, "the file's client commands allowed");
            }
            Mode::GlobalBackup | Mode::ExecuteScript => {}
        }
        note(typed(&self.extra), "the extra arguments");
        note(self.auth, "another user");
        changes
    }

    /// Puts every setting [`Self::advanced_changes`] lists back to the
    /// simple view's defaults; the folder, the file name, the file to
    /// restore, a format the simple view offers, "Faster" and the client
    /// stay.
    fn reset_advanced(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let clear = |input: &Entity<InputState>, window: &mut Window, cx: &mut App| {
            input.update(cx, |input, cx| input.set_value("", window, cx))
        };
        let faster = match self.mode {
            Mode::Backup => self.faster_backup(cx),
            Mode::Restore => self.faster_restore(cx),
            Mode::GlobalBackup | Mode::ExecuteScript => false,
        };
        match self.mode {
            Mode::Backup => {
                let mut defaults = BackupOptions::new(self.engine, self.database.clone());
                if simple_formats(self.family).contains(&self.backup.format) || faster {
                    defaults.format = self.backup.format;
                }
                if let EngineBackup::MySql(my) = &mut defaults.engine
                    && faster
                    && self.blocks_writes()
                {
                    my.method = DumpMethod::LockAllTables;
                }
                self.backup = defaults;
                self.selection.complete = true;
                self.selection.schemas.clear();
                self.selection.tables.clear();
                clear(&self.compression, window, cx);
                clear(&self.encoding, window, cx);
            }
            Mode::Restore => {
                let without_owners = self.without_owners();
                self.restore = RestoreOptions::new(
                    self.engine,
                    self.restore.database.clone(),
                    self.restore.input.clone(),
                    self.restore.format,
                );
                (self.restore.no_owner, self.restore.no_privileges) =
                    (without_owners, without_owners);
                self.restore.single_transaction &= !faster;
                self.allow_commands = false;
            }
            Mode::GlobalBackup | Mode::ExecuteScript => {}
        }
        if !faster {
            clear(&self.jobs, window, cx);
        }
        for input in [&self.extra, &self.auth_user, &self.auth_password] {
            clear(input, window, cx);
        }
        self.auth = false;
    }

    /// "Simple view" in the wizard: back at once when nothing the simple
    /// view hides was changed; otherwise it says what would be reset and
    /// asks, so no choice is dropped silently.
    pub(super) fn back_to_simple(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changes = self.advanced_changes(cx);
        if changes.is_empty() {
            self.view = View::Simple;
            self.showing_clients = false;
            cx.notify();
            return;
        }
        let words: Vec<String> = changes.iter().map(|change| change.to_string()).collect();
        let text = SharedString::from(format!(
            "The simple view does not show {}. Going back puts {} back to the simple view's \
             defaults; the file, the format and the client stay.",
            listed(&words),
            if changes.len() == 1 { "it" } else { "them" }
        ));
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let (view, text) = (view.clone(), text.clone());
            dialog
                .title("Back to the simple view?")
                .w(px(460.))
                .on_ok(|_, _, _| false)
                .content(move |content, _, _| {
                    let view = view.clone();
                    content.child(
                        v_flex()
                            .gap_3()
                            .child(
                                div()
                                    .id("simple-reset-text")
                                    .test_support()
                                    .text_sm()
                                    .child(text.clone())
                                    .test_text(text.clone()),
                            )
                            .child(
                                h_flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        Button::new("simple-stay")
                                            .outline()
                                            .label("Keep the advanced settings")
                                            .on_click(|_, window, cx| window.close_dialog(cx)),
                                    )
                                    .child(
                                        Button::new("simple-reset")
                                            .primary()
                                            .label("Reset and go back")
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                                view.update(cx, |this, cx| {
                                                    this.reset_advanced(window, cx);
                                                    this.view = View::Simple;
                                                    this.showing_clients = false;
                                                    cx.notify();
                                                });
                                            }),
                                    ),
                            ),
                    )
                })
        });
    }

    /// The simple restore's "Leave out the backup's owners and privileges"
    /// (K35): pg_restore's `--no-owner` and `--no-privileges`, the wizard's
    /// two checkboxes, both set.
    pub(super) fn without_owners(&self) -> bool {
        self.restore.no_owner && self.restore.no_privileges
    }

    /// Enter in the dialog: starts a simple backup that can run. A restore
    /// never starts on Enter, nor the wizard (its pages have no default).
    pub(super) fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.view == View::Simple
            && self.mode == Mode::Backup
            && self.simple_page_shown()
            && self.blocked(cx).is_none()
        {
            self.start(window, cx);
        }
    }

    /// "Choose…": the native save panel, in the folder and on the name the
    /// backup would write.
    fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let folder = PathBuf::from(Self::text(&self.folder, cx));
        let name = self.output_path(cx).ok().and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });
        let picking =
            crate::save_panel::ask(crate::save_panel::SaveKind::Backup, Some(folder), name, cx);
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Some(path) = picking.await {
                // The macOS panel asks before it returns a file that exists; a Linux
                // panel may not (it depends on the desktop's portal), so Kerno asks there.
                let probe = path.clone();
                let existed = cfg!(target_os = "macos")
                    && cx
                        .background_executor()
                        .spawn(async move { probe.exists() })
                        .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.agreed_replace = existed.then(|| path.clone());
                    this.set_file(&path, window, cx)
                });
            }
        }));
    }

    /// Takes `path` as the backup's file: its folder, and its name as the
    /// file name pattern. A name ending in the extension of a format the
    /// simple view offers chooses that format, the extension left to it
    /// (so another format later does not write `name.dump.sql`). With
    /// "Faster" ticked the backup is a folder, named as typed.
    fn set_file(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        let mut name = name.to_string_lossy().into_owned();
        let chosen = simple_formats(self.family).iter().find_map(|format| {
            name.strip_suffix(&format!(".{}", format.extension()))
                .filter(|stem| !stem.is_empty())
                .map(|stem| (*format, stem.to_string()))
        });
        if let Some((format, stem)) = chosen.filter(|_| !self.faster_backup(cx)) {
            self.backup.format = format;
            name = stem;
        }
        let folder = folder.display().to_string();
        self.folder
            .update(cx, |input, cx| input.set_value(folder, window, cx));
        self.pattern
            .update(cx, |input, cx| input.set_value(name, window, cx));
        cx.notify();
    }

    /// What the database holds, once read: "4 schemas, 37 tables and 6
    /// views" (the database's tables alone on MySQL).
    fn contents(&self) -> Option<String> {
        let Loaded::Ready(entries) = &self.objects else {
            return None;
        };
        let relations: Vec<_> = entries.iter().flat_map(|entry| &entry.relations).collect();
        let views = relations.iter().filter(|(_, kind)| is_view(kind)).count();
        let mut parts = Vec::new();
        if self.engine == Engine::Postgres {
            parts.push(count(entries.len(), "schema", "schemas"));
        }
        parts.push(count(relations.len() - views, "table", "tables"));
        if views > 0 {
            parts.push(count(views, "view", "views"));
        }
        Some(listed(&parts))
    }

    /// What is backed up, in one sentence.
    fn what_text(&self) -> String {
        let whole = format!("The whole database “{}”", self.database);
        match (&self.objects, self.contents()) {
            (_, Some(contents)) => format!("{whole}: {contents}."),
            (Loaded::Loading, None) => format!("{whole} (counting its tables…)."),
            _ => format!("{whole}."),
        }
    }

    /// What a format choice means, in the user's words.
    fn format_hint(&self) -> String {
        match &self.backup.engine {
            EngineBackup::Postgres(_) if self.backup.format == BackupFormat::Sql => {
                "A text file of SQL statements, run back by psql: readable and editable, \
                 larger, restored whole."
                    .into()
            }
            EngineBackup::Postgres(_) if self.backup.format == BackupFormat::Directory => {
                "pg_dump's directory archive: a folder of compressed files, one per table, \
                 restored by pg_restore whole or only some of its tables."
                    .into()
            }
            EngineBackup::Postgres(_) => {
                "pg_dump's custom archive: compressed, and restored by pg_restore whole or only \
                 some of its tables."
                    .into()
            }
            EngineBackup::MySql(_) if self.backup.format == BackupFormat::Directory => {
                "A folder of SQL scripts, one per table (a large table in several), written by \
                 several mysqldump at once and run back by several mysql clients. The procedures, \
                 functions, triggers and views come last, in their own files."
                    .into()
            }
            EngineBackup::MySql(my) => {
                let read = match my.method {
                    DumpMethod::SingleTransaction => {
                        "in one consistent snapshot (single transaction)".to_string()
                    }
                    other => format!("with “{}”", other.label().to_lowercase()),
                };
                let held: String = [
                    (my.routines, ", the procedures and functions"),
                    (my.triggers, ", the triggers"),
                    (my.events, ", the events"),
                ]
                .into_iter()
                .filter_map(|(on, what)| on.then_some(what))
                .collect();
                format!(
                    "A SQL script, run back by the mysql client and read {read}. It holds the \
                     tables and their rows{held}."
                )
            }
        }
    }

    /// What a restore does to the database, said before it runs.
    fn restore_warning(&self) -> String {
        let tail = match self.family {
            ClientFamily::Postgres if self.restore.single_transaction => {
                " It runs in one transaction: if a statement fails, nothing is kept."
            }
            ClientFamily::Postgres => " A failure keeps what ran before it.",
            ClientFamily::MySql => {
                " MySQL and MariaDB apply it statement by statement: a failure keeps what was \
                 already applied."
            }
        };
        let old = match &self.input {
            Some(Ok(input))
                if input.utf8mb4
                    && self.family == ClientFamily::MySql
                    && self.server_version < (5, 5, 3) =>
            {
                let (a, b, c) = self.server_version;
                format!(
                    " This backup was made in utf8mb4, which MySQL {a}.{b}.{c} does not have: the \
                     restore stops at its first statement (ERROR 1115)."
                )
            }
            _ => String::new(),
        };
        let base = if self.replaces() {
            format!(
                "A restore drops and replaces the backup's tables that “{}” holds and cannot be \
                 undone.",
                self.database
            )
        } else if self.plain_replaces() {
            format!(
                "A restore runs the dump in “{}” and cannot be undone.",
                self.database
            )
        } else {
            format!(
                "A restore writes into “{}” on top of what it holds and cannot be undone.",
                self.database
            )
        };
        format!(
            "{base} Kerno shows the command and asks before it runs.{tail}{}{old}",
            self.overlap_words()
        )
    }

    /// A folder backup's tables and views that the target database holds,
    /// once its tables are read (K74).
    fn held_by_target(&self) -> Option<(&mysql_folder::Manifest, Vec<String>, Vec<String>)> {
        let (
            Some(Ok(Input {
                manifest: Some(manifest),
                ..
            })),
            Some(held),
        ) = (&self.input, &self.target_tables)
        else {
            return None;
        };
        // Without case only where the server compares names so (L4).
        let fold = |name: &str| {
            if self.target_folds == Some(true) {
                name.to_lowercase()
            } else {
                name.to_string()
            }
        };
        let has = |name: &&str| held.iter().any(|have| fold(have) == fold(name));
        let quoted = |name: &str| format!("“{name}”");
        let tables = manifest
            .tables
            .iter()
            .map(|table| table.table.as_str())
            .filter(has)
            .map(quoted)
            .collect();
        let views = manifest
            .after
            .iter()
            .filter(|after| after.kind == mysql_folder::AfterKind::Views)
            .flat_map(|after| after.objects.iter().map(String::as_str))
            .filter(has)
            .map(quoted)
            .collect();
        Some((manifest, tables, views))
    }

    /// Whether the restore drops and replaces tables the target holds: a
    /// folder backup with its definitions and DROP TABLE, some of whose
    /// tables are there (K74).
    pub(super) fn replaces(&self) -> bool {
        let Some(Ok(Input {
            manifest: Some(manifest),
            ..
        })) = &self.input
        else {
            return false;
        };
        let drops = manifest.options.add_drop_table && manifest.content != BackupContent::DataOnly;
        drops
            && match self.held_by_target() {
                Some((_, tables, _)) => !tables.is_empty(),
                // Not read: every table of the backup there is replaced (L4).
                None => !self.target_reading,
            }
    }

    /// Whether a plain MySQL dump goes into a database that holds tables:
    /// its DROP TABLE statements, if it has any, replace those it names.
    fn plain_replaces(&self) -> bool {
        self.mode == Mode::Restore
            && self.family == ClientFamily::MySql
            && matches!(
                &self.input,
                Some(Ok(Input {
                    manifest: None,
                    format: BackupFormat::Sql,
                    ..
                }))
            )
            && self
                .target_tables
                .as_ref()
                .is_some_and(|held| !held.is_empty())
    }

    /// What the confirmation says the restore does to what the database
    /// holds (K74, L4): one sentence, true with the other words.
    pub(super) fn restore_clause(&self) -> &'static str {
        if self.replaces() {
            "dropping and replacing the backup's tables that the database holds"
        } else if self.plain_replaces() {
            "where the dump's DROP TABLE statements, if it has any, replace the tables they name"
        } else {
            "on top of what the database holds"
        }
    }

    /// A folder backup restored into a database that already holds some of
    /// its tables: what happens to them, how many there are, and what a
    /// stopped restore leaves (K63, K74). A plain dump: that its DROP TABLE
    /// statements replace tables, unnamed.
    pub(super) fn overlap_words(&self) -> String {
        let database = &self.database;
        let Some(Ok(input)) = &self.input else {
            return String::new();
        };
        if self.mode != Mode::Restore || self.family != ClientFamily::MySql {
            return String::new();
        }
        let Some(manifest) = &input.manifest else {
            // Which tables a dump drops is known only by reading all of it,
            // which can take minutes for a large one: not named (K74).
            return match &self.target_tables {
                Some(held) if !held.is_empty() => format!(
                    " “{database}” is not empty: a dump made with DROP TABLE statements, as \
                     mysqldump writes by default, drops each of its tables that “{database}” \
                     holds and creates it again from the dump; the other tables stay. Kerno does \
                     not read the whole file first to name them."
                ),
                _ => String::new(),
            };
        };
        let drops = manifest.options.add_drop_table && manifest.content != BackupContent::DataOnly;
        let Some((_, tables, views)) = self.held_by_target() else {
            return if drops && !self.target_reading {
                format!(
                    " Kerno could not read which tables “{database}” holds: every table of the \
                     backup that exists there is dropped and replaced."
                )
            } else {
                String::new()
            };
        };
        if tables.is_empty() && views.is_empty() {
            return String::new();
        }
        let shown = |names: &[String]| {
            if names.len() > 6 {
                format!("{} and {} more", names[..6].join(", "), names.len() - 6)
            } else {
                listed(names)
            }
        };
        if manifest.content == BackupContent::DataOnly || !manifest.options.add_drop_table {
            let both: Vec<String> = tables.iter().chain(&views).cloned().collect();
            let both = shown(&both);
            return if manifest.content == BackupContent::DataOnly {
                format!(
                    " “{database}” already holds {both}, which the backup has rows for: the \
                     backup holds rows only, so they are added to the tables already there; a \
                     table that already holds some of them stops the restore on a duplicate key, \
                     or ends with rows twice when it has no key."
                )
            } else {
                format!(
                    " “{database}” already holds {both}, which the backup has too, and the backup \
                     was made without DROP TABLE statements: the restore stops at the first of \
                     them."
                )
            };
        }
        let total = manifest.tables.len();
        let views_too = if views.is_empty() {
            String::new()
        } else {
            format!(", and its views {} are replaced too", shown(&views))
        };
        let schema = manifest.content == BackupContent::SchemaOnly;
        let mut text = if tables.is_empty() {
            format!(
                " “{database}” already holds the backup's views {}: they are replaced.",
                shown(&views)
            )
        } else {
            let count = match (tables.len(), total) {
                (_, 1) => "the backup's table".to_string(),
                (n, total) if n == total => format!("all {total} of the backup's tables"),
                (n, total) => format!("{n} of the backup's {total} tables"),
            };
            format!(
                " “{database}” already holds {count} ({}): {}{views_too}; the tables only in \
                 “{database}” stay.",
                shown(&tables),
                if schema {
                    "the backup holds definitions only, so each is dropped and created again \
                     empty, and the rows it holds now are gone"
                } else {
                    "each is dropped and replaced with the backup's table and rows"
                }
            )
        };
        if !schema && !tables.is_empty() {
            text.push_str(
                " Kerno creates the backup's tables first, one after the other, then loads their \
                 rows: if the restore stops, a table it has not finished is left empty or missing \
                 until you run it again.",
            );
        }
        text
    }

    /// A backup taken from another database than the one restored into, and
    /// its statements that name that database (K78): what they would do, and
    /// what Kerno does with them, said before the run. A plain dump: from
    /// its first lines only (the database its header names, or selects with
    /// `USE`).
    pub(super) fn source_words(&self) -> Option<String> {
        if self.mode != Mode::Restore || self.family != ClientFamily::MySql {
            return None;
        }
        let Some(Ok(input)) = &self.input else {
            return None;
        };
        let target = &self.database;
        let other = |source: &str| !mysql_folder::same_database(source, target, self.target_folds);
        if input.manifest.is_some() {
            let source = input.archive_database.as_deref()?;
            return other(source)
                .then(|| {
                    mysql_folder::naming_said(
                        self.naming(input),
                        source,
                        target,
                        self.restore.load_as_written,
                        false,
                    )
                })
                .flatten();
        }
        if let Some(uses) = input.dump_uses.as_deref().filter(|uses| other(uses)) {
            return Some(format!(
                "This dump selects the database “{uses}” itself (USE `{uses}`): restored, what it \
                 holds goes into “{uses}”, not into “{target}”."
            ));
        }
        let source = input
            .archive_database
            .as_deref()
            .filter(|source| other(source))?;
        Some(format!(
            "This dump was taken from the database “{source}”: its triggers, routines, events or \
             views that name “{source}” would act on “{source}”, not on “{target}”. Kerno reads \
             only the start of a dump, not every statement, so it cannot say which do."
        ))
    }

    /// Whether the folder backup holds statements that would write into the
    /// database it was taken from, which the restore leaves out unless
    /// "Load them as written" is ticked (K78).
    pub(super) fn source_writes(&self) -> bool {
        match &self.input {
            Some(Ok(
                input @ Input {
                    manifest: Some(_),
                    archive_database: Some(source),
                    ..
                },
            )) => {
                self.mode == Mode::Restore
                    && !mysql_folder::same_database(source, &self.database, self.target_folds)
                    && self
                        .naming(input)
                        .iter()
                        .any(mysql_folder::FileNaming::writes)
            }
            _ => false,
        }
    }

    /// The folder's statements that name its database, read by the target
    /// server's rule for names: with case only where it is known to keep
    /// case (K78, M4).
    fn naming<'a>(&self, input: &'a Input) -> &'a [mysql_folder::FileNaming] {
        &input.naming[usize::from(self.target_folds == Some(false))]
    }

    /// The client in use, small, with the way to another one.
    fn simple_client(&self, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .gap_2()
            .items_start()
            .child(div().flex_1().min_w_0().child(self.render_tool(true, cx)))
            .child(
                Button::new("backup-local-client")
                    .xsmall()
                    .ghost()
                    .label("Local client…")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.showing_clients = true;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn simple_backup_rows(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let what = self.what_text();
        let faster = self.faster_backup(cx);
        let file = match self.output_path(cx) {
            Ok(path) => {
                // A folder's path ends with a slash.
                let shown = format!("{}{}", path.display(), if faster { "/" } else { "" });
                div()
                    .id("simple-file")
                    .test_support()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .child(SharedString::from(shown.clone()))
                    .test_text(shown)
            }
            Err(why) => div()
                .id("simple-file-problem")
                .test_support()
                .text_sm()
                .text_color(theme.danger)
                .child(SharedString::from(why.clone()))
                .test_text(why),
        };
        let formats = simple_formats(self.family);
        let format = if formats.len() > 1 {
            let entity = cx.entity();
            // "Faster" writes the archive as a folder: still the archive.
            let shown = if faster {
                BackupFormat::Custom
            } else {
                self.backup.format
            };
            let selected = formats.iter().position(|format| *format == shown);
            v_flex()
                .gap_1()
                .child(
                    OptionCards::new("simple-format", selected, move |index, _, cx| {
                        // The archive's card is the chosen one while "Faster"
                        // is ticked (a chosen card is not chosen again); the
                        // script unticks it, its jobs then unused.
                        entity.update(cx, |this, cx| {
                            this.backup.format = formats[index];
                            cx.notify();
                        })
                    })
                    .option("Compressed archive", archive_preview(cx))
                    .option("Plain SQL script", script_preview(cx)),
                )
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let hint = self.format_hint();
        // pg_dump dumps several tables at once (`--jobs`) only into a
        // directory archive: offered beside PostgreSQL's archive (K48).
        // MySQL and MariaDB: several mysqldump of Kerno's own, into a folder
        // (K63).
        let speed = match self.family {
            ClientFamily::Postgres => matches!(
                self.backup.format,
                BackupFormat::Custom | BackupFormat::Directory
            ),
            ClientFamily::MySql => matches!(
                self.backup.format,
                BackupFormat::Sql | BackupFormat::Directory
            ),
        }
        .then(|| {
            // The "Writes" row shows only with "Faster" ticked (K74).
            let block = if faster {
                "“Block writes” below"
            } else {
                "“Block writes”, offered once this is ticked"
            };
            let hint = match self.family {
                ClientFamily::Postgres => {
                    "The backup is then a folder, one file per table, which Kerno restores like \
                     any backup. A single large table is still read by one connection."
                        .to_string()
                }
                ClientFamily::MySql if self.server_version < (5, 5, 3) => format!(
                    "The backup is then a folder, one file per table, which Kerno restores on \
                     several connections too. On this server, a large table with a single-column \
                     whole-number key is read in several parts at once only with {block}: only \
                     then is every part read at one moment. Otherwise each table is read by one \
                     connection."
                ),
                ClientFamily::MySql => format!(
                    "The backup is then a folder, one file per table, which Kerno restores on \
                     several connections too. A large InnoDB table with a single-column \
                     whole-number key is read in several parts at once, and a large table of any \
                     engine with {block}; any other is still read by one connection."
                ),
            };
            row(
                "Speed",
                v_flex()
                    .gap_1()
                    .child(
                        Checkbox::new("simple-faster")
                            .label(SharedString::from(format!(
                                "Faster: back up on {} connections at once",
                                faster_jobs()
                            )))
                            .checked(faster)
                            .on_click(cx.listener(|this, checked: &bool, window, cx| {
                                this.set_faster_backup(*checked, window, cx)
                            })),
                    )
                    .child(
                        div()
                            .id("simple-faster-hint")
                            .test_support()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(hint.clone()))
                            .test_text(hint),
                    ),
                cx,
            )
        });
        let consistency = (self.family == ClientFamily::MySql && faster).then(|| {
            let lacks = self.can_lock == Some(false);
            let on = self.blocks_writes();
            let hint = if lacks {
                "This account lacks the RELOAD privilege that the lock needs: each table is read \
                 at its own moment."
            } else if on {
                "Kerno holds a read lock on the whole server (FLUSH TABLES WITH READ LOCK) from \
                 before the first table until the last: every table is read at the same moment, \
                 and every write on the server waits until the backup ends. Kerno asks first, and \
                 the backup stops if a long statement keeps the lock from coming within 10 s."
            } else {
                "Each table is read at one moment, all its parts included; two tables are not read \
                 at the same instant (an InnoDB table in one snapshot, any other locked while it \
                 is read)."
            };
            row(
                "Writes",
                v_flex()
                    .gap_1()
                    .child(
                        Checkbox::new("simple-block-writes")
                            .label(
                                "Block writes during the backup, so that every table is read at \
                                 the same moment",
                            )
                            .checked(on)
                            .disabled(lacks && !on)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                if let EngineBackup::MySql(my) = &mut this.backup.engine {
                                    my.method = if *checked {
                                        DumpMethod::LockAllTables
                                    } else {
                                        DumpMethod::SingleTransaction
                                    };
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("simple-block-writes-hint")
                            .test_support()
                            .text_xs()
                            .text_color(if lacks {
                                theme.warning
                            } else {
                                theme.muted_foreground
                            })
                            .child(hint)
                            .test_text(hint),
                    ),
                cx,
            )
        });
        vec![
            row(
                "What",
                div()
                    .id("simple-what")
                    .test_support()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::from(what.clone()))
                    .test_text(what),
                cx,
            ),
            row(
                "To",
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(div().flex_1().min_w_0().child(file))
                    .child(
                        Button::new("simple-choose-file")
                            .small()
                            .outline()
                            .label("Choose…")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_file(window, cx)),
                            ),
                    ),
                cx,
            ),
            row(
                "Format",
                v_flex().gap_1().child(format).child(
                    div()
                        .id("simple-format-hint")
                        .test_support()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(hint.clone()))
                        .test_text(hint),
                ),
                cx,
            ),
        ]
        .into_iter()
        .chain(speed)
        .chain(consistency)
        .chain([row("Client", self.simple_client(cx), cx)])
        .collect()
    }

    fn simple_restore_rows(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let into = format!("The database “{}” of “{}”", self.database, self.name);
        let warning = self.restore_warning();
        let read = match &self.input {
            None => None,
            Some(Err(problem)) => Some(
                div()
                    .id("restore-problem")
                    .test_support()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(SharedString::from(problem.clone()))
                    .into_any_element(),
            ),
            Some(Ok(_)) => self.input_text().map(|kind| {
                div()
                    .id("simple-file-kind")
                    .test_support()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(kind.clone()))
                    .test_text(kind)
                    .into_any_element()
            }),
        };
        // A PostgreSQL archive can be restored without its owners and
        // privileges; a plain script sets them in its own text.
        let archive = self.family == ClientFamily::Postgres
            && matches!(&self.input, Some(Ok(input)) if input.format != BackupFormat::Sql);
        let roles = archive.then(|| {
            let user = self
                .params
                .as_ref()
                .map(|params| params.user.clone())
                .unwrap_or_default();
            let hint = format!(
                "For a backup from another server whose roles this one does not have: what is \
                 restored then belongs to “{user}”, without the backup's grants."
            );
            row(
                "Roles",
                v_flex()
                    .gap_1()
                    .child(
                        Checkbox::new("simple-no-owner")
                            .label("Leave out the backup's owners and privileges")
                            .checked(self.without_owners())
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.restore.no_owner = *checked;
                                this.restore.no_privileges = *checked;
                                this.owners_ticked = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("simple-no-owner-hint")
                            .test_support()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(hint.clone()))
                            .test_text(hint),
                    ),
                cx,
            )
        });
        // pg_restore reads a custom or a directory archive on several
        // connections at once (`--jobs`), which one transaction rules out:
        // the owner's restore of a large backup took 450 s on one.
        let parallel = archive
            && matches!(
                &self.input,
                Some(Ok(input))
                    if matches!(input.format, BackupFormat::Custom | BackupFormat::Directory)
            );
        // A MySQL / MariaDB folder backup loads its files on several clients
        // (K63).
        let folder = matches!(&self.input, Some(Ok(input)) if input.manifest.is_some());
        let speed = (parallel || folder).then(|| {
            let jobs = faster_jobs();
            let on = self.faster_restore(cx);
            let hint = if folder {
                "Several tables load at once, and the parts of a large InnoDB table side by side; \
                 the procedures, triggers and views come last. A failure stops the others and \
                 keeps what was loaded."
            } else {
                "Not all or nothing: if a step fails, pg_restore goes on and what it restored \
                 stays."
            };
            row(
                "Speed",
                v_flex()
                    .gap_1()
                    .child(
                        Checkbox::new("simple-parallel")
                            .label(SharedString::from(format!(
                                "Faster: restore on {jobs} connections at once"
                            )))
                            .checked(on)
                            .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                                // One transaction is PostgreSQL's.
                                this.restore.single_transaction =
                                    !*checked && this.family == ClientFamily::Postgres;
                                let value = if *checked {
                                    jobs.to_string()
                                } else {
                                    String::new()
                                };
                                this.jobs
                                    .update(cx, |input, cx| input.set_value(value, window, cx));
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("simple-parallel-hint")
                            .test_support()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(hint)
                            .test_text(hint),
                    ),
                cx,
            )
        });
        vec![
            row(
                "File",
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div().flex_1().min_w_0().child(
                                    TextInput::new(&self.file_field).small().id("restore-file"),
                                ),
                            )
                            .child(
                                Button::new("restore-choose-file")
                                    .small()
                                    .outline()
                                    .label("Choose…")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.choose_input(window, cx)
                                    })),
                            ),
                    )
                    .children(read)
                    .children(self.render_preflight(cx))
                    .children(self.render_source(cx)),
                cx,
            ),
            row(
                "Into",
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .id("simple-into")
                            .test_support()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(SharedString::from(into.clone()))
                            .test_text(into),
                    )
                    .child(
                        div()
                            .id("simple-restore-warning")
                            .test_support()
                            .text_xs()
                            .text_color(theme.warning)
                            .child(SharedString::from(warning.clone()))
                            .test_text(warning),
                    ),
                cx,
            ),
        ]
        .into_iter()
        .chain(roles)
        .chain(speed)
        .chain([row("Client", self.simple_client(cx), cx)])
        .collect()
    }

    /// The simple view's page: the panel, a file's client commands when it
    /// holds some, and "Show command".
    pub(super) fn render_simple(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let rows = match self.mode {
            Mode::Restore => self.simple_restore_rows(cx),
            _ => self.simple_backup_rows(cx),
        };
        let mut parts = vec![
            settings_ui::panel(rows, cx)
                .id("backup-simple")
                .test_support()
                .into_any_element(),
        ];
        if let Some(Ok(Input {
            refusal: Some(found),
            ..
        })) = &self.input
        {
            let text = format!(
                "{} To run it anyway, open Advanced settings… and allow it on its Client commands \
                 step.",
                self.refusal_words(found)
            );
            parts.push(
                div()
                    .id("restore-refusal")
                    .test_support()
                    .p_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.danger)
                    .text_sm()
                    .child(SharedString::from(text.clone()))
                    .test_text(text)
                    .into_any_element(),
            );
        }
        let has_command = matches!(self.tool_state, super::ToolState::Ready { .. })
            && (self.mode == Mode::Backup || self.input.is_some());
        parts.push(
            v_flex()
                .gap_2()
                .child(
                    h_flex().child(
                        Button::new("simple-show-command")
                            .xsmall()
                            .ghost()
                            .icon(if self.show_command {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .label(if self.show_command {
                                "Hide command"
                            } else {
                                "Show command"
                            })
                            .disabled(!has_command)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_command = !this.show_command;
                                cx.notify();
                            })),
                    ),
                )
                .when(self.show_command && has_command, |this| {
                    this.child(
                        self.render_command(
                            self.command(cx).map(|command| self.shown(&command)),
                            cx,
                        ),
                    )
                })
                .into_any_element(),
        );
        parts
    }

    /// Advanced settings… on the left; Cancel and the run on the right.
    pub(super) fn render_simple_footer(&self, cx: &Context<Self>) -> AnyElement {
        let (id, label) = match self.mode {
            Mode::Restore => ("restore-start", "Restore…"),
            _ => ("backup-start", "Back up"),
        };
        h_flex()
            .gap_2()
            .pt_2()
            .child(
                Button::new("backup-advanced")
                    .outline()
                    .label("Advanced settings…")
                    .on_click(cx.listener(|this, _, _, cx| this.show_advanced(cx))),
            )
            .child(div().flex_1())
            .child(
                Button::new("backup-close")
                    .ghost()
                    .label("Cancel")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(
                Button::new(id)
                    .primary()
                    .label(label)
                    .disabled(self.blocked(cx).is_some())
                    .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
            )
            .into_any_element()
    }
}

//! The simple view of a backup and of a restore (K26, D-099): one screen
//! that says what is backed up or restored, where, in which format and
//! with which client, and runs it. "Advanced settings…" is the wizard, on
//! the same state: both views build the command with the same code. Kerno's
//! own design.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
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
use kerno_core::tools::{BackupFormat, BackupOptions, DumpMethod, EngineBackup, RestoreOptions};

use super::{BackupDialog, Input, Loaded, Mode, Purpose, Step, View, is_view, refusal_text};
use crate::settings_ui::{self, OptionCards};

/// The formats the simple view offers, the default first: PostgreSQL's
/// custom archive (restored whole or in part, compressed) or a plain
/// script; MySQL and MariaDB write a script only.
fn simple_formats(engine: Engine) -> &'static [BackupFormat] {
    match engine {
        Engine::Postgres => &[BackupFormat::Custom, BackupFormat::Sql],
        Engine::MySql | Engine::MariaDb => &[BackupFormat::Sql],
    }
}

/// `n` things, in words.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "a, b and c".
fn listed(parts: &[String]) -> String {
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

    /// The wizard's settings the simple view neither shows nor sets that
    /// differ from its defaults, in words: what going back to it resets.
    /// The folder, the file name and a format the simple view offers are
    /// shown there, so they are kept.
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
                note(!self.selection.complete, "the objects ticked");
                note(
                    !simple_formats(self.engine).contains(&self.backup.format),
                    "the format",
                );
                note(self.backup.content != defaults.content, "the content");
                note(
                    self.backup.engine != defaults.engine,
                    "the dump tool's options",
                );
                note(
                    typed(&self.compression) || typed(&self.encoding) || typed(&self.jobs),
                    "the compression, encoding or jobs",
                );
            }
            Mode::Restore => {
                let defaults = RestoreOptions::new(
                    self.engine,
                    self.restore.database.clone(),
                    self.restore.input.clone(),
                    self.restore.format,
                );
                note(self.restore != defaults, "the restore options");
                note(typed(&self.jobs), "the jobs");
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
    /// restore, a format the simple view offers and the client stay.
    fn reset_advanced(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let clear = |input: &Entity<InputState>, window: &mut Window, cx: &mut App| {
            input.update(cx, |input, cx| input.set_value("", window, cx))
        };
        match self.mode {
            Mode::Backup => {
                let mut defaults = BackupOptions::new(self.engine, self.database.clone());
                if simple_formats(self.engine).contains(&self.backup.format) {
                    defaults.format = self.backup.format;
                }
                self.backup = defaults;
                self.selection.complete = true;
                self.selection.schemas.clear();
                self.selection.tables.clear();
                clear(&self.compression, window, cx);
                clear(&self.encoding, window, cx);
            }
            Mode::Restore => {
                self.restore = RestoreOptions::new(
                    self.engine,
                    self.restore.database.clone(),
                    self.restore.input.clone(),
                    self.restore.format,
                );
                self.allow_commands = false;
            }
            Mode::GlobalBackup | Mode::ExecuteScript => {}
        }
        for input in [
            &self.jobs,
            &self.extra,
            &self.auth_user,
            &self.auth_password,
        ] {
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
        let folder = if folder.is_absolute() {
            folder
        } else {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
        };
        let name = self.output_path(cx).ok().and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        });
        let picking = cx.prompt_for_new_path(&folder, name.as_deref());
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = picking.await {
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
    /// (so another format later does not write `name.dump.sql`).
    fn set_file(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        let mut name = name.to_string_lossy().into_owned();
        let chosen = simple_formats(self.engine).iter().find_map(|format| {
            name.strip_suffix(&format!(".{}", format.extension()))
                .filter(|stem| !stem.is_empty())
                .map(|stem| (*format, stem.to_string()))
        });
        if let Some((format, stem)) = chosen {
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
            EngineBackup::Postgres(_) => {
                "pg_dump's custom archive: compressed, and restored by pg_restore whole or only \
                 some of its tables."
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
        let tail = match self.engine {
            Engine::Postgres if self.restore.single_transaction => {
                " It runs in one transaction: if a statement fails, nothing is kept."
            }
            Engine::Postgres => " A failure keeps what ran before it.",
            Engine::MySql | Engine::MariaDb => {
                " MySQL and MariaDB apply it statement by statement: a failure keeps what was \
                 already applied."
            }
        };
        format!(
            "A restore writes into “{}” on top of what it holds and cannot be undone. Kerno \
             shows the command and asks before it runs.{tail}",
            self.database
        )
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
        let file = match self.output_path(cx) {
            Ok(path) => {
                let shown = path.display().to_string();
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
        let formats = simple_formats(self.engine);
        let format = if formats.len() > 1 {
            let entity = cx.entity();
            let selected = formats
                .iter()
                .position(|format| *format == self.backup.format);
            v_flex()
                .gap_1()
                .child(
                    OptionCards::new("simple-format", selected, move |index, _, cx| {
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
            row("Client", self.simple_client(cx), cx),
        ]
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
                    .children(read),
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
            row("Client", self.simple_client(cx), cx),
        ]
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
                refusal_text(found, self.engine, &self.database)
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
                        self.render_command(self.command(cx).map(|command| command.display()), cx),
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

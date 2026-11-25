//! Import from DBeaver (K41, D-114): the preview of what a DBeaver project
//! export or `data-sources.json` brings (folders, connections, scripts),
//! then the import of what is ticked. The file is read and mapped by
//! [`kerno_core::dbeaver`], off the UI thread; nothing is written before
//! Import, and Cancel writes nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task,
    TestSupportExt as _, Window, div, px,
};
use kerno_core::config::ConnectionConfig;
use kerno_core::dbeaver::{self, Credentials, Export};
use kerno_core::environment::{self, EnvironmentDef};
use kerno_core::secrets;
use kerno_core::storage::{SavedConnection, SavedQuery};

use crate::bridge;
use crate::connections::manager::Connections;
use crate::state::AppState;
use crate::theme;

/// What happens to one connection of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Fate {
    /// Imported when ticked.
    Import(bool),
    /// The same server is saved here already, under this id.
    Exists(String),
    /// The same server as the connection at this index of the file.
    Repeat(usize),
    /// Kerno cannot take it.
    Refused(String),
}

/// The import dialog.
pub struct DbeaverImport {
    connections: Entity<Connections>,
    /// The file, kept to read the passwords again only if asked.
    bytes: Vec<u8>,
    export: Export,
    fates: Vec<Fate>,
    queries: Vec<SavedQuery>,
    passwords: bool,
    /// The environment picked from each row's tag (K46).
    picked: Vec<Option<String>>,
    /// Whether DBeaver's types Kerno has no environment for are created.
    create_environments: bool,
    step: Step,
    work: Option<Task<()>>,
}

enum Step {
    Preview,
    Importing,
    Done(String),
    Failed(String),
}

/// The key of a server: a connection to it is imported once.
fn server(config: &ConnectionConfig) -> impl PartialEq + '_ {
    (
        config.engine,
        &config.host,
        config.port,
        &config.database,
        &config.user,
        &config.file,
    )
}

/// Reads the file at `path` and opens the preview, which the task
/// returns; a file that is not an export says why.
pub fn open(
    path: PathBuf,
    connections: Entity<Connections>,
    window: &mut Window,
    cx: &mut App,
) -> Task<Option<Entity<DbeaverImport>>> {
    let state = AppState::get(cx);
    let storage = state.storage.clone();
    let environments = state.settings.environments.clone();
    let source = path.clone();
    let reading = bridge::spawn_blocking(cx, move || {
        let size = std::fs::metadata(&source)
            .map_err(|error| format!("It cannot be read: {error}."))?
            .len();
        if size > dbeaver::MAX_FILE {
            return Err(format!(
                "It is larger than {} MB: it is not a DBeaver export Kerno reads.",
                dbeaver::MAX_FILE / 1024 / 1024
            ));
        }
        let bytes =
            std::fs::read(&source).map_err(|error| format!("It cannot be read: {error}."))?;
        let export = dbeaver::read(&bytes, false, &environments, &theme::TAG_COLORS)?;
        let queries = storage
            .saved_queries()
            .map_err(|error| format!("Kerno's saved queries cannot be read: {error}."))?;
        let stored = storage
            .connections()
            .map_err(|error| format!("Kerno's saved connections cannot be read: {error}."))?;
        Ok::<_, String>((bytes, export, queries, stored))
    });
    window.spawn(cx, async move |cx| {
        let outcome = reading.await;
        cx.update(|window, cx| match outcome {
            Ok((bytes, export, queries, stored)) => {
                tracing::info!(
                    connections = export.connections.len(),
                    scripts = export.scripts.len(),
                    "DBeaver export read"
                );
                let view = cx
                    .new(|cx| DbeaverImport::new(connections, bytes, export, queries, stored, cx));
                let shown = view.clone();
                window.open_dialog(cx, move |dialog, _, _| {
                    let view = shown.clone();
                    dialog
                        .title("Import from DBeaver")
                        .w(px(640.))
                        .on_ok(|_, _, _| false)
                        .content(move |content, _, _| content.child(view.clone()))
                });
                Some(view)
            }
            Err(error) => {
                window.push_notification(
                    Notification::error(SharedString::from(format!(
                        "{} was not imported. {error}",
                        path.display()
                    ))),
                    cx,
                );
                None
            }
        })
        .ok()
        .flatten()
    })
}

/// The fate of each connection of `export`, given those saved here.
fn fates(export: &Export, saved: &[SavedConnection]) -> Vec<Fate> {
    let mut fates: Vec<Fate> = Vec::new();
    for connection in &export.connections {
        let fate = match &connection.config {
            Err(reason) => Fate::Refused(reason.clone()),
            Ok(config) => {
                let here = saved
                    .iter()
                    .find(|saved| server(&saved.config) == server(config));
                let before = export.connections[..fates.len()].iter().position(|other| {
                    other
                        .config
                        .as_ref()
                        .is_ok_and(|other| server(other) == server(config))
                });
                match (here, before) {
                    (Some(saved), _) => Fate::Exists(saved.id.clone()),
                    (None, Some(index)) => Fate::Repeat(index),
                    (None, None) => Fate::Import(true),
                }
            }
        };
        fates.push(fate);
    }
    fates
}

impl DbeaverImport {
    fn new(
        connections: Entity<Connections>,
        bytes: Vec<u8>,
        export: Export,
        queries: Vec<SavedQuery>,
        stored: Vec<SavedConnection>,
        cx: &mut Context<Self>,
    ) -> Self {
        // What is here is read from the store, which an import made a
        // moment before has written: the list in memory reads it back a
        // little later (an import twice in a row made a second copy). The
        // connections opened from a URL are in memory only.
        let mut here = stored;
        let list = connections.read(cx);
        here.extend(
            (list.saved().iter())
                .filter(|saved| list.is_unsaved(&saved.id))
                .cloned(),
        );
        let fates = fates(&export, &here);
        Self {
            connections,
            bytes,
            picked: vec![None; export.connections.len()],
            export,
            fates,
            queries,
            passwords: false,
            create_environments: true,
            step: Step::Preview,
            work: None,
        }
    }

    /// The environment row `index` goes in: the one picked from its tag,
    /// else the one made of its DBeaver type while those are to be created,
    /// else the file's. A picked one to create counts only while ticked.
    fn environment_id(&self, index: usize) -> String {
        let connection = &self.export.connections[index];
        let file = match &connection.config {
            Ok(config) => config.environment.clone(),
            Err(_) => environment::LOCAL.into(),
        };
        let id = (self.picked[index].clone()).or_else(|| connection.new_environment.clone());
        match id {
            Some(id) if self.create_environments || self.new_environment(&id).is_none() => id,
            _ => file,
        }
    }

    /// What row `index` leaves out and what to check, each once (K58): what
    /// the file set that Kerno could not take, and its DBeaver type when that
    /// is not created. The preview and the report say the same.
    pub fn to_check(&self, index: usize) -> Vec<String> {
        let connection = &self.export.connections[index];
        let mut items = connection.not_mapped.clone();
        let file = connection.config.as_ref().map(|c| c.environment.as_str());
        if let Some(def) = (connection.new_environment.as_deref())
            .and_then(|made| self.new_environment(made))
            .filter(|_| !self.create_environments)
            .filter(|_| file == Ok(self.environment_id(index).as_str()))
        {
            items.push(format!(
                "connection type “{}” not created: put in Local",
                def.name
            ));
        }
        let mut seen = BTreeSet::new();
        items.retain(|item| seen.insert(item.clone()));
        items
    }

    fn new_environment(&self, id: &str) -> Option<&EnvironmentDef> {
        self.export.new_environments.iter().find(|def| def.id == id)
    }

    /// The environment `id`: one to create, else one of Kerno's.
    fn environment_def<'a>(&'a self, id: &str, cx: &'a App) -> &'a EnvironmentDef {
        self.new_environment(id).unwrap_or_else(|| {
            environment::environment(&AppState::get(cx).settings.environments, id)
        })
    }

    pub fn set_environment(&mut self, index: usize, id: String, cx: &mut Context<Self>) {
        if let Some(picked) = self.picked.get_mut(index) {
            *picked = Some(id);
        }
        cx.notify();
    }

    pub fn set_create_environments(&mut self, create: bool, cx: &mut Context<Self>) {
        self.create_environments = create;
        cx.notify();
    }

    pub fn set_ticked(&mut self, index: usize, ticked: bool, cx: &mut Context<Self>) {
        if let Some(Fate::Import(state)) = self.fates.get_mut(index) {
            *state = ticked;
        }
        cx.notify();
    }

    pub fn set_passwords(&mut self, passwords: bool, cx: &mut Context<Self>) {
        self.passwords = passwords && self.export.credentials == Credentials::Read;
        cx.notify();
    }

    /// The Kerno connection the scripts of the DBeaver connection `id` go
    /// to, as [`Self::import`] maps them: one ticked (a new connection,
    /// named by its row), one already here, the one a repeat repeats, or
    /// none (a saved query of no connection).
    fn script_target(&self, id: Option<&str>) -> Option<String> {
        let index = (self.export.connections.iter()).position(|c| Some(c.id.as_str()) == id)?;
        match &self.fates[index] {
            Fate::Import(true) => Some(format!("\u{0}new {index}")),
            Fate::Exists(id) => Some(id.clone()),
            Fate::Repeat(first) => self.script_target(Some(&self.export.connections[*first].id)),
            _ => None,
        }
    }

    /// How many saved queries the import would write, and how many it
    /// skips because they are here already (same connection, folder and
    /// name), as [`Self::import`] does (K77).
    fn queries_planned(&self) -> (usize, usize) {
        let mut known: BTreeSet<(Option<String>, String, String)> = (self.queries.iter())
            .map(|q| (q.connection_id.clone(), q.folder.clone(), q.name.clone()))
            .collect();
        let (mut new, mut here) = (0, 0);
        for script in &self.export.scripts {
            let target = self.script_target(script.connection.as_deref());
            if known.insert((target, script.folder.clone(), script.name.clone())) {
                new += 1;
            } else {
                here += 1;
            }
        }
        (new, here)
    }

    /// Writes what is ticked: folders, connections, their passwords when
    /// asked, and the scripts.
    pub fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = AppState::get(cx);
        let (storage, store) = (state.storage.clone(), state.secrets.clone());
        let environments = state.settings.environments.clone();
        let bytes = self.passwords.then(|| self.bytes.clone());
        let fates = self.fates.clone();
        // The environments to create that an imported row goes in, created
        // first; one of that name made since is used, never a second one.
        let chosen: Vec<String> = (0..fates.len())
            .map(|index| self.environment_id(index))
            .collect();
        let wanted: Vec<EnvironmentDef> = (self.export.new_environments.iter())
            .filter(|def| {
                (chosen.iter().zip(&fates))
                    .any(|(id, fate)| *id == def.id && *fate == Fate::Import(true))
            })
            .cloned()
            .collect();
        let mut created: BTreeMap<String, String> = BTreeMap::new();
        if !wanted.is_empty() {
            AppState::update_settings(cx, |settings| {
                for def in wanted {
                    let same = |kept: &&EnvironmentDef| kept.name.eq_ignore_ascii_case(&def.name);
                    let id = match settings.environments.iter().find(same) {
                        Some(kept) => kept.id.clone(),
                        None => {
                            let id = environment::unused_id(&settings.environments, &def.name);
                            settings.environments.push(EnvironmentDef {
                                id: id.clone(),
                                ..def.clone()
                            });
                            id
                        }
                    };
                    created.insert(def.id, id);
                }
            });
            cx.refresh_windows();
        }
        let plans: Vec<(String, Option<ConnectionConfig>, Vec<String>)> = (self.export.connections)
            .iter()
            .zip(chosen)
            .enumerate()
            .map(|(index, (c, id))| {
                let mut config = c.config.as_ref().ok().cloned();
                if let Some(config) = &mut config {
                    config.environment = created.get(&id).cloned().unwrap_or(id);
                }
                (c.id.clone(), config, self.to_check(index))
            })
            .collect();
        let scripts = self.export.scripts.clone();
        let mut known: BTreeSet<(Option<String>, String, String)> = (self.queries.iter())
            .map(|q| (q.connection_id.clone(), q.folder.clone(), q.name.clone()))
            .collect();
        let writing = bridge::spawn_blocking(cx, move || {
            // The passwords, read again only now, and only if asked.
            let mut secrets_by_id = BTreeMap::new();
            if let Some(bytes) = bytes {
                let export = dbeaver::read(&bytes, true, &environments, &theme::TAG_COLORS)
                    .map_err(kerno_core::Error::Config)?;
                for connection in export.connections {
                    secrets_by_id.insert(connection.id, connection.secrets);
                }
            }
            // DBeaver's id → Kerno's.
            let mut ids: BTreeMap<String, String> = BTreeMap::new();
            let mut written = 0;
            let mut notes = Vec::new();
            for (index, (dbeaver_id, config, not_mapped)) in plans.iter().enumerate() {
                match (&fates[index], config) {
                    (Fate::Import(true), Some(config)) => {
                        let mut config = config.clone();
                        let secrets = secrets_by_id.remove(dbeaver_id).unwrap_or_default();
                        let has_secret = secrets.password.is_some()
                            || secrets.ssh_password.is_some()
                            || secrets.ssh_key_passphrase.is_some();
                        if has_secret && !config.engine.is_file() {
                            config.save_password = true;
                        }
                        let id = storage.save_connection(None, &config)?;
                        // As the connection dialog saves a typed password.
                        if has_secret && !config.engine.is_file() {
                            secrets::save_secrets(store.as_ref(), &id, &secrets)?;
                        }
                        ids.insert(dbeaver_id.clone(), id);
                        written += 1;
                        if !not_mapped.is_empty() {
                            notes.push((config.name.clone(), not_mapped.clone()));
                        }
                    }
                    (Fate::Exists(id), _) => {
                        ids.insert(dbeaver_id.clone(), id.clone());
                    }
                    _ => {}
                }
            }
            // A repeat goes where the connection it repeats went.
            for (index, (dbeaver_id, _, _)) in plans.iter().enumerate() {
                if let Fate::Repeat(first) = fates[index]
                    && let Some(id) = ids.get(&plans[first].0).cloned()
                {
                    ids.insert(dbeaver_id.clone(), id);
                }
            }
            drop(secrets_by_id);
            let mut queries = 0;
            for script in scripts {
                let connection_id = script.connection.and_then(|id| ids.get(&id).cloned());
                let key = (
                    connection_id.clone(),
                    script.folder.clone(),
                    script.name.clone(),
                );
                if !known.insert(key) {
                    continue;
                }
                storage.save_query(&SavedQuery {
                    id: 0,
                    connection_id,
                    folder: script.folder,
                    name: script.name,
                    sql: script.sql,
                })?;
                queries += 1;
            }
            Ok::<_, kerno_core::Error>((written, queries, notes))
        });
        let folders = self.export.folders.clone();
        let skipped = self.export.skipped_scripts.clone();
        self.step = Step::Importing;
        self.work = Some(cx.spawn_in(window, async move |this, cx| {
            let outcome = writing.await;
            let _ = this.update_in(cx, |this, window, cx| {
                AppState::update_settings(cx, move |settings| {
                    settings.connection_folders.extend(folders);
                });
                this.connections.update(cx, |list, cx| list.reload(cx));
                this.step = match outcome {
                    Ok((connections, queries, notes)) => {
                        let message = report(connections, queries, &notes, &skipped);
                        window.push_notification(
                            Notification::success(SharedString::from(format!(
                                "Imported from DBeaver: {}.",
                                count(connections, "connection", "connections")
                            ))),
                            cx,
                        );
                        Step::Done(message)
                    }
                    Err(error) => Step::Failed(format!("The import stopped: {error}")),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// The preview and the outcome, in words; for tests.
    #[cfg(test)]
    pub fn summary(&self) -> String {
        let step = match &self.step {
            Step::Preview => "preview".to_string(),
            Step::Importing => "importing".into(),
            Step::Done(message) => format!("done: {message}"),
            Step::Failed(error) => format!("failed: {error}"),
        };
        let rows: Vec<String> = (self.export.connections.iter())
            .zip(&self.fates)
            .enumerate()
            .map(|(index, (connection, fate))| {
                format!(
                    "{} {fate:?} {}",
                    connection.name,
                    self.environment_id(index)
                )
            })
            .collect();
        let new: Vec<&str> = (self.export.new_environments.iter())
            .map(|def| def.name.as_str())
            .collect();
        format!(
            "{step} | {} | folders {:?} | {} scripts | passwords {} | new environments {new:?} \
             created {}",
            rows.join(", "),
            self.export.folders,
            self.export.scripts.len(),
            self.passwords,
            self.create_environments
        )
    }

    fn render_preview(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (muted, warning) = (theme.muted_foreground, theme.warning);
        let (dark, border) = (theme.is_dark(), theme.border);
        let view = cx.entity();
        // Kerno's environments, then those to create, for each tag's menu.
        let choices: Vec<(String, String)> = (AppState::get(cx).settings.environments.iter())
            .map(|def| (def.id.clone(), def.name.clone()))
            .chain(
                (self.export.new_environments.iter())
                    .filter(|_| self.create_environments)
                    .map(|def| (def.id.clone(), format!("{} (new)", def.name))),
            )
            .collect();
        // The environment of a row, as Settings ▸ Environments draws it;
        // a click chooses another.
        let tag = |index: usize| -> AnyElement {
            let current = self.environment_id(index);
            let def = self.environment_def(&current, cx);
            let quiet = def.id == environment::LOCAL;
            let (view, choices) = (view.clone(), choices.clone());
            Button::new(("dbeaver-env", index))
                .ghost()
                .xsmall()
                .flex_none()
                .tooltip("Its environment in Kerno: click to choose another")
                .child(
                    h_flex()
                        .gap_1()
                        .child(crate::environments_view::swatch(def.color(dark), border))
                        .child(
                            div()
                                .text_xs()
                                .when(quiet, |this| this.text_color(muted))
                                .child(SharedString::from(def.name.clone())),
                        ),
                )
                .dropdown_menu(move |mut menu, _, _| {
                    for (id, name) in &choices {
                        let (view, id) = (view.clone(), id.clone());
                        menu = menu.item(
                            PopupMenuItem::new(name.clone())
                                .checked(id == current)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.set_environment(index, id.clone(), cx)
                                    })
                                }),
                        );
                    }
                    menu
                })
                .into_any_element()
        };
        let row = |index: usize, depth: usize| -> AnyElement {
            let connection = &self.export.connections[index];
            let mut detail = vec![connection.engine.clone()];
            if connection.config.as_ref().is_ok_and(|c| c.engine.is_file()) {
                detail.push(connection.database.clone());
            } else {
                let port = match connection.port.as_str() {
                    "" => String::new(),
                    port => format!(":{port}"),
                };
                detail.push(format!("{}{port}", connection.host));
                detail.extend([connection.database.clone(), connection.user.clone()]);
            }
            detail.retain(|part| !part.is_empty());
            let detail = detail.join(" · ");
            let to_check = self.to_check(index);
            let (enabled, note) = match &self.fates[index] {
                Fate::Import(_) if to_check.is_empty() => (true, None),
                Fate::Import(_) => (true, Some(format!("To check: {}.", to_check.join("; ")))),
                Fate::Exists(_) => (false, Some("Already here: skipped".to_string())),
                Fate::Repeat(first) => (
                    false,
                    Some(format!(
                        "Same server as “{}”: imported once",
                        self.export.connections[*first].name
                    )),
                ),
                Fate::Refused(reason) => (false, Some(reason.clone())),
            };
            let ticked = matches!(self.fates[index], Fate::Import(true));
            v_flex()
                .id(("dbeaver-connection", index))
                .test_support()
                .pl(px(16. * depth as f32))
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(
                            Checkbox::new(("dbeaver-tick", index))
                                .checked(ticked)
                                .disabled(!enabled)
                                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                    this.set_ticked(index, *checked, cx)
                                })),
                        )
                        .child(
                            div()
                                .when(!enabled, |this| this.text_color(muted))
                                .child(SharedString::from(connection.name.clone())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(SharedString::from(detail)),
                        )
                        .when(matches!(self.fates[index], Fate::Import(_)), |this| {
                            this.child(tag(index))
                        }),
                )
                .when_some(note, |this, note| {
                    let note = SharedString::from(note);
                    this.child(
                        div()
                            .id(("dbeaver-note", index))
                            .test_support()
                            .pl(px(24.))
                            .text_xs()
                            .text_color(if enabled { muted } else { warning })
                            .child(note.clone())
                            .test_text(note),
                    )
                })
                .into_any_element()
        };
        let in_folder = |folder: Option<&str>| -> Vec<usize> {
            (0..self.export.connections.len())
                .filter(|&index| self.export.connections[index].folder.as_deref() == folder)
                .collect()
        };
        let mut items = Vec::new();
        for (index, folder) in self.export.folders.iter().enumerate() {
            let depth = folder.matches('/').count();
            items.push(
                div()
                    .id(("dbeaver-folder", index))
                    .test_support()
                    .pl(px(16. * depth as f32))
                    .pt_1()
                    .text_xs()
                    .font_semibold()
                    .text_color(muted)
                    .child(SharedString::from(
                        kerno_core::config::last_folder_name(folder).to_string(),
                    ))
                    .into_any_element(),
            );
            items.extend(
                in_folder(Some(folder))
                    .into_iter()
                    .map(|index| row(index, depth + 1)),
            );
        }
        items.extend(in_folder(None).into_iter().map(|index| row(index, 0)));
        let (queries_new, queries_here) = self.queries_planned();
        let mut scripts = format!(
            "{} come with them, as saved queries of their connection.",
            count(self.export.scripts.len(), "script", "scripts")
        );
        // A saved query already here is skipped, as a connection is (K77).
        if queries_here > 0 {
            scripts.push_str(&format!(" Already here, skipped: {queries_here}."));
        }
        if !self.export.skipped_scripts.is_empty() {
            scripts.push_str(&format!(
                " Skipped (over 1 MB): {}.",
                self.export.skipped_scripts.join(", ")
            ));
        }
        let password_note = match self.export.credentials {
            Credentials::Read => None,
            Credentials::Absent => Some("The file holds no saved password."),
            Credentials::Locked => Some(
                "This project is protected by a master password: Kerno cannot read its saved \
                 user names and passwords. The connections come without them.",
            ),
        };
        let chosen = self
            .fates
            .iter()
            .filter(|fate| **fate == Fate::Import(true))
            .count();
        let folders_new = (self.export.folders.iter())
            .filter(|folder| {
                !AppState::get(cx)
                    .settings
                    .connection_folders
                    .contains(*folder)
            })
            .count();
        // Nothing to write at all: the button is off and the dialog says why;
        // saved queries or folders alone are imported (K77).
        let nothing = (chosen == 0 && queries_new == 0 && folders_new == 0).then(|| {
            let fates = || self.fates.iter();
            if fates().any(|fate| *fate == Fate::Import(false)) {
                "Nothing is ticked: tick the connections to import."
            } else if fates().any(|fate| matches!(fate, Fate::Exists(_) | Fate::Repeat(_))) {
                "Nothing to import: every connection Kerno can take from this file is already here."
            } else {
                "Nothing to import: Kerno cannot take any connection of this file."
            }
        });
        let mut body = vec![
            div()
                .id("dbeaver-items")
                .max_h(px(360.))
                .overflow_y_scroll()
                .child(v_flex().gap_1().children(items))
                .into_any_element(),
            v_flex()
                .gap_1()
                .when(!self.export.new_environments.is_empty(), |this| {
                    let names: Vec<&str> = (self.export.new_environments.iter())
                        .map(|def| def.name.as_str())
                        .collect();
                    this.child(
                        div()
                            .id("dbeaver-new-environments")
                            .test_support()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(format!(
                                "DBeaver has {} Kerno has no environment for: {}.",
                                count(names.len(), "type", "types"),
                                names.join(", ")
                            ))),
                    )
                    .child(
                        Checkbox::new("dbeaver-create-environments")
                            .label(
                                "Create these environments, with their colour and rules \
                                 (otherwise their connections go in Local)",
                            )
                            .checked(self.create_environments)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_create_environments(*checked, cx)
                            })),
                    )
                })
                .into_any_element(),
            div()
                .id("dbeaver-scripts")
                .test_support()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(scripts.clone()))
                .test_text(scripts)
                .into_any_element(),
            v_flex()
                .gap_1()
                .child(
                    Checkbox::new("dbeaver-passwords")
                        .label("Also import the saved passwords")
                        .checked(self.passwords)
                        .disabled(self.export.credentials != Credentials::Read)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.set_passwords(*checked, cx)
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(password_note.unwrap_or(
                            "They go to this computer's secret store, as a password typed in \
                             the connection dialog does.",
                        )),
                )
                .into_any_element(),
            h_flex()
                .justify_end()
                .gap_2()
                .when_some(nothing, |this, nothing| {
                    this.child(
                        div()
                            .id("dbeaver-nothing")
                            .test_support()
                            .flex_1()
                            .text_sm()
                            .text_color(muted)
                            .child(nothing)
                            .test_text(nothing),
                    )
                })
                .child(
                    Button::new("dbeaver-cancel")
                        .ghost()
                        .label("Cancel")
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .child(
                    Button::new("dbeaver-import")
                        .primary()
                        .label(SharedString::from(import_label(
                            chosen,
                            queries_new,
                            folders_new,
                        )))
                        .disabled(nothing.is_some())
                        .on_click(cx.listener(|this, _, window, cx| this.import(window, cx))),
                )
                .into_any_element(),
        ];
        // No empty slot when every type has its environment.
        if self.export.new_environments.is_empty() {
            body.remove(1);
        }
        body
    }
}

/// What the Import button writes: its connections, or with none ticked
/// what else it writes ("Import 3 saved queries"), K77.
fn import_label(connections: usize, queries: usize, folders: usize) -> String {
    if connections > 0 || queries + folders == 0 {
        return format!("Import {}", count(connections, "connection", "connections"));
    }
    let parts: Vec<String> = [
        (queries, "saved query", "saved queries"),
        (folders, "folder", "folders"),
    ]
    .into_iter()
    .filter(|(n, _, _)| *n > 0)
    .map(|(n, one, many)| count(n, one, many))
    .collect();
    format!("Import {}", parts.join(" and "))
}

/// "3 connections", "1 script".
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What the import wrote, then, connection by connection, what it left out
/// and what to check (K58).
fn report(
    connections: usize,
    queries: usize,
    notes: &[(String, Vec<String>)],
    skipped: &[String],
) -> String {
    let mut message = format!(
        "Imported {} and {}.",
        count(connections, "connection", "connections"),
        count(queries, "saved query", "saved queries")
    );
    if !notes.is_empty() {
        message.push_str("\nTo check:");
        for (name, items) in notes {
            message.push_str(&format!("\n• {name}: {}.", items.join("; ")));
        }
    }
    if !skipped.is_empty() {
        message.push_str(&format!("\nScripts skipped: {}.", skipped.join(", ")));
    }
    message
}

impl Render for DbeaverImport {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (success, danger) = (theme.success, theme.danger);
        let body = match &self.step {
            Step::Preview => self.render_preview(cx),
            Step::Importing => vec![div().text_sm().child("Importing…").into_any_element()],
            Step::Done(message) | Step::Failed(message) => {
                let failed = matches!(self.step, Step::Failed(_));
                vec![
                    div()
                        .id("dbeaver-done")
                        .test_support()
                        .text_sm()
                        .text_color(if failed { danger } else { success })
                        .child(SharedString::from(message.clone()))
                        .into_any_element(),
                    h_flex()
                        .justify_end()
                        .child(
                            Button::new("dbeaver-close")
                                .primary()
                                .label("Close")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .into_any_element(),
                ]
            }
        };
        v_flex()
            .id("dbeaver-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .children(body)
    }
}

//! The connection dialog: create or edit a saved connection, fill it from
//! a URL, test it, save it.

use gpui_kit::TestSupportExt as _;
use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use kerno_core::Error;
use kerno_core::config::{Engine, Secrets, SshClient, TlsMode};
use kerno_core::environment;
use kerno_core::error::{Explained, explain};
use kerno_core::ssh::SshError;
use kerno_core::storage::SavedConnection;

use super::form::{COMMIT_MODES, ConnectionForm, Field, FieldError, PasswordChoice, SshAuthKind};
use super::manager::{Connections, TestReport};
use crate::engine_logo::engine_logo;
use crate::state::AppState;
use crate::theme;

/// The parts of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    General,
    Tls,
    Ssh,
    Advanced,
}

const SECTIONS: [(Section, &str); 4] = [
    (Section::General, "General"),
    (Section::Tls, "SSL / TLS"),
    (Section::Ssh, "SSH tunnel"),
    (Section::Advanced, "Advanced"),
];

/// What "Test connection" last found.
enum TestState {
    NotRun,
    Running,
    Passed(TestReport),
    Failed(Explained),
    /// The SSH server is not known yet: the user decides.
    UnknownHost {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
    },
}

/// The server's databases, offered by the Database field.
enum DatabaseList {
    /// Not asked for yet, or the fields changed since.
    Unknown,
    /// Asked for while the host, the port or the user is missing.
    Incomplete,
    Loading,
    Listed(Vec<String>),
    Failed(Explained),
}

/// The Database field's placeholder: PostgreSQL always opens a database.
fn database_placeholder(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "the server's default (often postgres)",
        Engine::MySql | Engine::MariaDb => "Default database (optional)",
    }
}

/// The text fields. Everything else lives in [`ConnectionForm`].
struct Inputs {
    engine_filter: Entity<InputState>,
    name: Entity<InputState>,
    group: Entity<InputState>,
    url: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
    options: Entity<TextareaState>,
    default_schema: Entity<InputState>,
    init_sql: Entity<TextareaState>,
    ca_file: Entity<InputState>,
    cert_file: Entity<InputState>,
    key_file: Entity<InputState>,
    ssh_host: Entity<InputState>,
    ssh_port: Entity<InputState>,
    ssh_user: Entity<InputState>,
    ssh_password: Entity<InputState>,
    ssh_key_file: Entity<InputState>,
    ssh_key_passphrase: Entity<InputState>,
    ssh_agent_socket: Entity<InputState>,
    ssh_keepalive: Entity<InputState>,
    ssh_jump: Entity<InputState>,
    command_program: Entity<InputState>,
    command_args: Entity<TextareaState>,
    command_timeout: Entity<InputState>,
    rds_profile: Entity<InputState>,
    rds_region: Entity<InputState>,
    op_reference: Entity<InputState>,
}

/// What "Run now" of the password command last found: the length of its
/// output, never the output.
enum RunState {
    NotRun,
    Running,
    Printed(usize),
    Failed(Explained),
}

/// The dialog's view.
pub struct ConnectionDialog {
    connections: Entity<Connections>,
    /// Id of the connection being edited; `None` for a new one.
    editing: Option<String>,
    /// The choices that are not text: engine, colour, environment, modes.
    form: ConnectionForm,
    inputs: Inputs,
    section: Section,
    errors: Vec<FieldError>,
    test: TestState,
    /// An SSH host key the user agreed to trust while testing.
    trusted_fingerprint: Option<String>,
    running_test: Option<Task<()>>,
    databases: DatabaseList,
    /// Lists the databases once the fields settle; replaced on each change.
    listing: Option<Task<()>>,
    /// Probing the client folder chosen on the Advanced tab.
    choosing_client: Option<Task<()>>,
    /// The `Host` aliases of the user's SSH configuration (OpenSSH).
    ssh_aliases: Vec<String>,
    /// Reading them, off the UI thread.
    loading_aliases: Option<Task<()>>,
    /// "Run now" of the password command (J3).
    run: RunState,
    running_command: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ConnectionDialog {
    /// Opens the dialog for a new connection, or for `existing` with its
    /// secrets.
    pub fn open(
        connections: Entity<Connections>,
        existing: Option<(SavedConnection, Secrets)>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let title = match &existing {
            Some((saved, _)) => format!("Edit “{}”", saved.config.name),
            None => "New connection".to_string(),
        };
        let view = cx.new(|cx| Self::new(connections, existing, None, window, cx));
        Self::show(view, title, window, cx);
    }

    /// Opens the dialog for a new connection of `engine` (the main
    /// toolbar's engine list).
    pub fn open_for_engine(
        connections: Entity<Connections>,
        engine: Engine,
        window: &mut Window,
        cx: &mut App,
    ) {
        let view = cx.new(|cx| {
            let mut dialog = Self::new(connections, None, None, window, cx);
            dialog.pick_engine(engine, window, cx);
            dialog
        });
        Self::show(view, "New connection".into(), window, cx);
    }

    /// Opens the dialog for a new connection in `folder`.
    pub fn open_in(
        connections: Entity<Connections>,
        folder: String,
        window: &mut Window,
        cx: &mut App,
    ) {
        let view = cx.new(|cx| Self::new(connections, None, Some(folder), window, cx));
        Self::show(view, "New connection".into(), window, cx);
    }

    fn show(view: Entity<Self>, title: String, window: &mut Window, cx: &mut App) {
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            dialog
                .title(SharedString::from(title.clone()))
                .w(px(720.))
                // Enter in a field must not close a half-filled form.
                .on_ok(|_, _, _| false)
                .content(move |content, _, _| content.child(view.clone()))
        });
    }

    fn new(
        connections: Entity<Connections>,
        existing: Option<(SavedConnection, Secrets)>,
        folder: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (editing, mut form) = match &existing {
            Some((saved, secrets)) => (
                Some(saved.id.clone()),
                ConnectionForm::from_saved(&saved.config, secrets),
            ),
            None => (None, ConnectionForm::new(Engine::Postgres)),
        };
        if editing.is_none() {
            // A new connection starts as its environment says.
            let environments = &AppState::get(cx).settings.environments;
            form.read_only = environment::environment(environments, &form.environment)
                .rules
                .read_only;
        }
        if let Some(folder) = folder {
            form.group = folder;
        }
        // J2: a new connection goes through the system's OpenSSH when there
        // is one (its default authentication is the agent, not a password).
        let openssh = connections.read(cx).openssh().cloned();
        if editing.is_none() && openssh.is_some() {
            form.set_ssh_client(SshClient::OpenSsh);
        }
        let mut text = |value: &str, placeholder: &'static str| {
            let value = value.to_string();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let engine_filter = text("", "Search engines");
        let name = text(&form.name, "A name for the sidebar");
        let group = text(&form.group, "No folder");
        let url = text("", "postgres://user:password@host:5432/database");
        let host = text(&form.host, "localhost");
        let port = text(&form.port, "");
        let user = text(&form.user, "");
        let database = text(&form.database, database_placeholder(form.engine));
        let ca_file = text(&form.ca_file, "System trust store when empty");
        let cert_file = text(&form.cert_file, "Client certificate (optional)");
        let key_file = text(&form.key_file, "Client key (optional)");
        let ssh_host = text(&form.ssh_host, "bastion.example.com");
        let ssh_port = text(&form.ssh_port, ssh_port_placeholder(form.ssh_client));
        let ssh_user = text(&form.ssh_user, ssh_user_placeholder(form.ssh_client));
        let ssh_jump = text(&form.ssh_jump, "user@jump.example.com:22 (optional)");
        let ssh_key_file = text(&form.ssh_key_file, "~/.ssh/id_ed25519");
        let ssh_agent_socket = text(&form.ssh_agent_socket, "SSH_AUTH_SOCK when empty");
        let ssh_keepalive = text(&form.ssh_keepalive, "30");
        let default_schema = text(&form.default_schema, "public");
        let command_program = text(
            &form.command_program,
            "vault, /usr/local/bin/get-db-password",
        );
        let command_timeout = text(&form.command_timeout, "10");
        let rds_profile = text(&form.rds_profile, "default profile when empty");
        let rds_region = text(&form.rds_region, "us-east-1");
        let op_reference = text(&form.op_reference, "op://vault/item/password");
        let mut secret = |value: &str| {
            let value = value.to_string();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .default_value(value)
            })
        };
        let password = secret(&form.password);
        let ssh_password = secret(&form.ssh_password);
        let ssh_key_passphrase = secret(&form.ssh_key_passphrase);
        let options_text = form.options.clone();
        let options = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 10)
                .placeholder("application_name=kerno")
                .default_value(options_text)
        });
        let args_text = form.command_args.clone();
        let command_args = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder(
                    "One argument per line; ${host} ${port} ${user} ${database} are replaced",
                )
                .default_value(args_text)
        });
        let init_text = form.init_sql.clone();
        let init_sql = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder("SET TIME ZONE 'UTC';")
                .default_value(init_text)
        });

        // Enter in the URL field fills the form from it.
        let fill_on_enter = cx.subscribe_in(&url, window, |this, _, event, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.apply_url(window, cx);
            }
        });
        let refilter = cx.observe(&engine_filter, |_, _, cx| cx.notify());
        // What reaches the server changed: the list read before is another
        // server's or another login's. It is read again only when asked
        // for: nothing is sent to a server while the user types.
        let mut subscriptions = vec![fill_on_enter, refilter];
        for input in [&host, &port, &user, &password] {
            subscriptions.push(cx.subscribe(input, |this, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.forget_databases(cx);
                }
            }));
        }

        Self {
            connections,
            editing,
            form,
            inputs: Inputs {
                engine_filter,
                name,
                group,
                url,
                host,
                port,
                user,
                password,
                database,
                options,
                default_schema,
                init_sql,
                ca_file,
                cert_file,
                key_file,
                ssh_host,
                ssh_port,
                ssh_user,
                ssh_password,
                ssh_key_file,
                ssh_key_passphrase,
                ssh_agent_socket,
                ssh_keepalive,
                ssh_jump,
                command_program,
                command_args,
                command_timeout,
                rds_profile,
                rds_region,
                op_reference,
            },
            section: Section::General,
            errors: Vec::new(),
            test: TestState::NotRun,
            trusted_fingerprint: None,
            running_test: None,
            databases: DatabaseList::Unknown,
            listing: None,
            choosing_client: None,
            run: RunState::NotRun,
            running_command: None,
            ssh_aliases: Vec::new(),
            loading_aliases: openssh.and_then(|env| {
                let file = env.aliases_from?;
                let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
                let reading = crate::bridge::spawn_blocking(cx, move || {
                    kerno_core::openssh::host_aliases(&file, home.as_deref())
                });
                Some(cx.spawn(async move |this, cx| {
                    let aliases = reading.await;
                    let _ = this.update(cx, |this, cx| {
                        this.ssh_aliases = aliases;
                        this.loading_aliases = None;
                        cx.notify();
                    });
                }))
            }),
            _subscriptions: subscriptions,
        }
    }

    /// Switches the SSH client, with the fields that follow it.
    fn set_ssh_client(&mut self, client: SshClient, window: &mut Window, cx: &mut Context<Self>) {
        let mut form = self.read_form(cx);
        form.set_ssh_client(client);
        let (port, user) = (&self.inputs.ssh_port, &self.inputs.ssh_user);
        if port.read(cx).value().as_ref() != form.ssh_port {
            let value = form.ssh_port.clone();
            port.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        port.update(cx, |input, cx| {
            input.set_placeholder(ssh_port_placeholder(client), window, cx)
        });
        user.update(cx, |input, cx| {
            input.set_placeholder(ssh_user_placeholder(client), window, cx)
        });
        self.form = form;
        self.test = TestState::NotRun;
        cx.notify();
    }

    /// Drops the databases listed for other values of the fields.
    fn forget_databases(&mut self, cx: &mut Context<Self>) {
        self.listing = None;
        if !matches!(self.databases, DatabaseList::Unknown) {
            self.databases = DatabaseList::Unknown;
            cx.notify();
        }
    }

    /// Lists the server's databases for the Database field. Only when the
    /// user asks for it: it logs in with what the fields hold, and a host
    /// half typed must never be sent a password.
    fn list_databases(&mut self, cx: &mut Context<Self>) {
        let form = ConnectionForm {
            // Not needed to reach the server.
            name: "-".into(),
            ..self.read_form(cx)
        };
        let filled = [&form.host, &form.port, &form.user]
            .iter()
            .all(|value| !value.trim().is_empty());
        let built = if filled { form.build().ok() } else { None };
        let Some((config, secrets)) = built else {
            self.listing = None;
            self.databases = DatabaseList::Incomplete;
            cx.notify();
            return;
        };
        let trusted = self.trusted_fingerprint.clone();
        self.databases = DatabaseList::Loading;
        cx.notify();
        let listing =
            self.connections
                .read(cx)
                .list_databases(config.clone(), secrets, trusted, cx);
        self.listing = Some(cx.spawn(async move |this, cx| {
            let outcome = listing.await;
            let _ = this.update(cx, |this, cx| {
                this.databases = match outcome {
                    Ok(names) => DatabaseList::Listed(names),
                    Err(error) => DatabaseList::Failed(explain(&error.to_string(), &config)),
                };
                cx.notify();
            });
        }));
    }

    /// The form with what the text fields hold now.
    fn read_form(&self, cx: &App) -> ConnectionForm {
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let i = &self.inputs;
        ConnectionForm {
            name: value(&i.name),
            group: value(&i.group),
            host: value(&i.host),
            port: value(&i.port),
            user: value(&i.user),
            password: value(&i.password),
            database: value(&i.database),
            options: i.options.read(cx).value().to_string(),
            default_schema: value(&i.default_schema),
            init_sql: i.init_sql.read(cx).value().to_string(),
            ca_file: value(&i.ca_file),
            cert_file: value(&i.cert_file),
            key_file: value(&i.key_file),
            ssh_host: value(&i.ssh_host),
            ssh_port: value(&i.ssh_port),
            ssh_user: value(&i.ssh_user),
            ssh_password: value(&i.ssh_password),
            ssh_key_file: value(&i.ssh_key_file),
            ssh_key_passphrase: value(&i.ssh_key_passphrase),
            ssh_agent_socket: value(&i.ssh_agent_socket),
            ssh_keepalive: value(&i.ssh_keepalive),
            ssh_jump: value(&i.ssh_jump),
            command_program: value(&i.command_program),
            command_args: i.command_args.read(cx).value().to_string(),
            command_timeout: value(&i.command_timeout),
            rds_profile: value(&i.rds_profile),
            rds_region: value(&i.rds_region),
            op_reference: value(&i.op_reference),
            ..self.form.clone()
        }
    }

    /// Shows `form` in the fields.
    fn show_form(&mut self, form: ConnectionForm, window: &mut Window, cx: &mut Context<Self>) {
        let mut set = |input: &Entity<InputState>, value: &str| {
            if input.read(cx).value().as_ref() != value {
                let value = value.to_string();
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        };
        let i = &self.inputs;
        set(&i.host, &form.host);
        set(&i.port, &form.port);
        set(&i.user, &form.user);
        set(&i.password, &form.password);
        set(&i.database, &form.database);
        let placeholder = database_placeholder(form.engine);
        i.database.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
        if i.options.read(cx).value().as_ref() != form.options {
            let options = form.options.clone();
            i.options
                .update(cx, |input, cx| input.set_value(options, window, cx));
        }
        self.form = form;
        self.test = TestState::NotRun;
        self.forget_databases(cx);
        cx.notify();
    }

    fn pick_engine(&mut self, engine: Engine, window: &mut Window, cx: &mut Context<Self>) {
        let mut form = self.read_form(cx);
        form.set_engine(engine);
        self.show_form(form, window, cx);
    }

    fn apply_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.inputs.url.read(cx).value().to_string();
        if url.trim().is_empty() {
            return;
        }
        let mut form = self.read_form(cx);
        self.errors.retain(|error| error.field != Field::Url);
        match form.apply_url(&url) {
            Ok(()) => {
                self.show_form(form, window, cx);
                self.inputs
                    .url
                    .update(cx, |input, cx| input.set_value("", window, cx));
            }
            Err(error) => {
                self.errors.push(error);
                cx.notify();
            }
        }
    }

    /// Validates; on failure shows the errors and the first section that has one.
    fn validated(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(kerno_core::ConnectionConfig, Secrets)> {
        match self.read_form(cx).build() {
            Ok(built) => {
                self.errors.clear();
                cx.notify();
                Some(built)
            }
            Err(errors) => {
                if let Some(first) = errors.first() {
                    self.section = section_of(first.field);
                }
                self.errors = errors;
                cx.notify();
                None
            }
        }
    }

    fn run_test(&mut self, cx: &mut Context<Self>) {
        let Some((config, secrets)) = self.validated(cx) else {
            return;
        };
        self.test = TestState::Running;
        let tested = config.clone();
        let testing =
            self.connections
                .read(cx)
                .test(config, secrets, self.trusted_fingerprint.clone(), cx);
        self.running_test = Some(cx.spawn(async move |this, cx| {
            let outcome = testing.await;
            let _ = this.update(cx, |this, cx| {
                this.test = match outcome {
                    Ok(report) => TestState::Passed(report),
                    Err(Error::Ssh(error)) => match *error {
                        SshError::UnknownHostKey {
                            host,
                            port,
                            algorithm,
                            fingerprint,
                        } => TestState::UnknownHost {
                            host,
                            port,
                            algorithm,
                            fingerprint,
                        },
                        other => {
                            TestState::Failed(explain(&Error::from(other).to_string(), &tested))
                        }
                    },
                    Err(error) => TestState::Failed(explain(&error.to_string(), &tested)),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn save(&mut self, connect: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((config, secrets)) = self.validated(cx) else {
            return;
        };
        let id = self.editing.clone();
        let trusted = self.trusted_fingerprint.clone();
        self.connections.update(cx, |connections, cx| {
            connections.save(id, config, secrets, trusted, connect, cx);
        });
        window.close_dialog(cx);
    }

    fn browse(
        &mut self,
        input: Entity<InputState>,
        prompt: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let picking = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Ok(Some(paths))) = picking.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = input.update_in(cx, |input, window, cx| {
                    input.set_value(path.display().to_string(), window, cx);
                });
            }
        })
        .detach();
    }

    // Rendering ---------------------------------------------------------------

    fn error_for(&self, field: Field) -> Option<SharedString> {
        self.errors
            .iter()
            .find(|error| error.field == field)
            .map(|error| SharedString::from(error.message.clone()))
    }

    /// A labelled field with its error, if any, underneath.
    /// The existing folders, to fill the folder field. A new folder is
    /// typed there (`Work/Clients`).
    /// The environments of the settings, each with its colour; picking one
    /// for a new connection also sets its read-only default.
    fn environment_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let environments = AppState::get(cx).settings.environments.clone();
        let dark = cx.theme().is_dark();
        let known = environments
            .iter()
            .any(|def| def.id == self.form.environment);
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .children(environments.into_iter().enumerate().map(|(index, def)| {
                        let selected = def.id == self.form.environment;
                        let dot = def.color(dark).map(|color| {
                            div()
                                .flex_none()
                                .size(px(8.))
                                .rounded_full()
                                .bg(gpui_kit::rgb(color))
                        });
                        theme::chip(("conn-env", index), selected)
                            .small()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .children(dot)
                                    .child(SharedString::from(def.name.clone())),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.form.environment = def.id.clone();
                                if this.editing.is_none() {
                                    this.form.read_only = def.rules.read_only;
                                }
                                cx.notify();
                            }))
                    })),
            )
            .when(!known, |this| {
                this.child(
                    div()
                        .id("conn-env-unknown")
                        .test_support()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(SharedString::from(format!(
                            "Unknown environment “{}”: its rules are local's until you pick one.",
                            self.form.environment
                        ))),
                )
            })
    }

    fn folder_picker(&self, cx: &App) -> impl IntoElement {
        let folders = self.connections.read(cx).folders(cx);
        let input = self.inputs.group.clone();
        Button::new("conn-folder-pick")
            .outline()
            .dropdown_caret(true)
            .tooltip("Pick a folder, or type a new one like Work/Clients")
            .dropdown_menu(move |mut menu: PopupMenu, _, _| {
                for folder in std::iter::once(None).chain(folders.iter().cloned().map(Some)) {
                    let input = input.clone();
                    let label = folder.clone().unwrap_or_else(|| "No folder".into());
                    menu = menu.item(PopupMenuItem::new(SharedString::from(label)).on_click(
                        move |_, window, cx| {
                            let value = folder.clone().unwrap_or_default();
                            input.update(cx, |input, cx| input.set_value(value, window, cx));
                        },
                    ));
                }
                menu
            })
    }

    /// The server's databases, to fill the Database field (still free
    /// text): a button that lists them, then the list to pick from.
    fn database_picker(&self, cx: &Context<Self>) -> AnyElement {
        let DatabaseList::Listed(names) = &self.databases else {
            return Button::new("conn-database-load")
                .outline()
                .label("List…")
                .loading(matches!(self.databases, DatabaseList::Loading))
                .tooltip("Connect with the fields above and list the server's databases")
                .on_click(cx.listener(|this, _, _, cx| this.list_databases(cx)))
                .into_any_element();
        };
        let names = names.clone();
        let input = self.inputs.database.clone();
        Button::new("conn-database-list")
            .outline()
            .dropdown_caret(true)
            .disabled(names.is_empty())
            .tooltip("Pick one of the server's databases")
            .dropdown_menu(move |mut menu: PopupMenu, _, _| {
                for name in &names {
                    let (input, value) = (input.clone(), name.clone());
                    menu = menu.item(
                        PopupMenuItem::new(SharedString::from(name.clone())).on_click(
                            move |_, window, cx| {
                                let value = value.clone();
                                input.update(cx, |input, cx| input.set_value(value, window, cx));
                            },
                        ),
                    );
                }
                menu
            })
            .into_any_element()
    }

    /// What the listing of the databases found, under the field.
    fn render_databases(&self, cx: &App) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        match &self.databases {
            DatabaseList::Unknown | DatabaseList::Listed(_) => div().into_any_element(),
            DatabaseList::Incomplete => div()
                .id("conn-database-incomplete")
                .test_support()
                .text_xs()
                .text_color(muted)
                .child("Fill in the host, the port and the user to list the databases.")
                .into_any_element(),
            DatabaseList::Loading => div()
                .id("conn-database-loading")
                .test_support()
                .text_xs()
                .text_color(muted)
                .child("Listing the server's databases…")
                .into_any_element(),
            DatabaseList::Failed(explained) => {
                let text = format!(
                    "The databases could not be listed. {} {}",
                    explained.headline, explained.next_step
                );
                let text = SharedString::from(text.trim_end().to_string());
                div()
                    .id("conn-database-failed")
                    .test_support()
                    .text_xs()
                    .text_color(muted)
                    .child(text.clone())
                    .test_text(text)
                    .into_any_element()
            }
        }
    }

    fn field(
        &self,
        label: &'static str,
        error: Option<Field>,
        control: impl IntoElement,
        cx: &App,
    ) -> AnyElement {
        let message = error.and_then(|field| self.error_for(field));
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(control)
            .when_some(message, |this, message| {
                this.child(
                    div()
                        .id("conn-field-error")
                        .test_support()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(message),
                )
            })
            .into_any_element()
    }

    fn file_field(
        &self,
        label: &'static str,
        field: Field,
        id: &'static str,
        input: &Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let target = input.clone();
        let control = h_flex()
            .gap_2()
            .child(Input::new(input).id(id).flex_1())
            .child(
                Button::new(SharedString::from(format!("{id}-browse")))
                    .outline()
                    .label("Browse…")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.browse(target.clone(), "Choose", window, cx);
                    })),
            );
        self.field(label, Some(field), control, cx)
    }

    fn render_engines(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let filter = self.inputs.engine_filter.read(cx).value().to_lowercase();
        let engines: Vec<Engine> = Engine::ALL
            .into_iter()
            .filter(|engine| engine.label().to_lowercase().contains(filter.trim()))
            .collect();
        let theme = cx.theme();
        v_flex()
            .gap_2()
            .child(
                Input::new(&self.inputs.engine_filter)
                    .id("engine-filter")
                    .small(),
            )
            .when(engines.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("No engine matches. Kerno speaks PostgreSQL, MySQL and MariaDB."),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .children(engines.into_iter().map(|engine| {
                        let selected = engine == self.form.engine;
                        h_flex()
                            .id(SharedString::from(format!("engine-{}", engine_key(engine))))
                            .w(px(168.))
                            .p_2()
                            .gap_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(if selected {
                                theme.primary
                            } else {
                                theme.border
                            })
                            .when(selected, |this| this.bg(theme.accent))
                            .cursor_pointer()
                            .hover(|style| style.bg(theme.accent))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.pick_engine(engine, window, cx)
                            }))
                            .child(
                                // The engine's own logo, with clear space around it.
                                div()
                                    .flex_none()
                                    .p_1()
                                    .child(engine_logo(engine, px(24.), cx)),
                            )
                            .child(div().text_sm().child(engine.label()))
                    })),
            )
    }

    fn render_general(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let memory_only = !AppState::get(cx).secrets.is_persistent();
        let swatch = |key: Option<&'static str>, cx: &mut Context<Self>| {
            let id = SharedString::from(format!("color-{}", key.unwrap_or("none")));
            theme::swatch(id, key, self.form.color.as_deref() == key, cx).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.form.color = key.map(str::to_string);
                    cx.notify();
                },
            ))
        };
        let mut swatches = vec![swatch(None, cx)];
        for (key, _, _) in theme::TAG_COLORS {
            swatches.push(swatch(Some(key), cx));
        }
        let environments = self.environment_picker(cx).into_any_element();
        let password = self.render_password(!memory_only, cx);

        v_flex()
            .gap_3()
            .child(
                self.field(
                    "Fill from a connection URL",
                    Some(Field::Url),
                    h_flex()
                        .gap_2()
                        .child(Input::new(&self.inputs.url).id("conn-url").flex_1())
                        .child(
                            Button::new("conn-url-apply")
                                .outline()
                                .label("Fill")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.apply_url(window, cx)),
                                ),
                        ),
                    cx,
                ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(div().flex_1().child(self.field(
                        "Name",
                        None,
                        Input::new(&self.inputs.name).id("conn-name"),
                        cx,
                    )))
                    .child(
                        div().w(px(240.)).child(
                            self.field(
                                "Folder",
                                None,
                                h_flex()
                                    .gap_1()
                                    .child(Input::new(&self.inputs.group).id("conn-group").flex_1())
                                    .child(self.folder_picker(cx)),
                                cx,
                            ),
                        ),
                    ),
            )
            .child(
                h_flex()
                    .gap_6()
                    .items_start()
                    .child(self.field("Colour", None, h_flex().gap_1().children(swatches), cx))
                    .child(self.field("Environment", None, environments, cx)),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(div().flex_1().child(self.field(
                        "Host",
                        Some(Field::Host),
                        Input::new(&self.inputs.host).id("conn-host"),
                        cx,
                    )))
                    .child(div().w(px(120.)).child(self.field(
                        "Port",
                        Some(Field::Port),
                        Input::new(&self.inputs.port).id("conn-port"),
                        cx,
                    ))),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(div().flex_1().child(self.field(
                        "User",
                        Some(Field::User),
                        Input::new(&self.inputs.user).id("conn-user"),
                        cx,
                    )))
                    .when(!self.form.password_choice.runs_a_command(), |this| {
                        this.child(
                            div().flex_1().child(
                                self.field(
                                    "Password",
                                    None,
                                    Input::new(&self.inputs.password)
                                        .id("conn-password")
                                        .mask_toggle(),
                                    cx,
                                ),
                            ),
                        )
                    }),
            )
            .child(password)
            .child(
                self.field(
                    "Database",
                    None,
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div().flex_1().child(
                                        Input::new(&self.inputs.database).id("conn-database"),
                                    ),
                                )
                                .child(self.database_picker(cx)),
                        )
                        .child(self.render_databases(cx)),
                    cx,
                ),
            )
            .child(
                Switch::new("conn-read-only")
                    .label("Read-only: block writes here and open sessions read-only on the server")
                    .checked(self.form.read_only)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.form.read_only = *checked;
                        cx.notify();
                    })),
            )
    }

    /// Where the password comes from (J3), as DBeaver's Authentication
    /// list: the choice beside the fields of each, the command line as it
    /// will run with "Run now", and its confirmation when it came from an
    /// imported file. Kept to a few lines: the General tab is tall already.
    fn render_password(&self, persistent: bool, cx: &mut Context<Self>) -> AnyElement {
        let choice = self.form.password_choice;
        let view = cx.entity();
        let picker = Button::new("conn-password-source")
            .outline()
            .label(choice.label(persistent))
            .dropdown_caret(true)
            .tooltip("Where the password comes from")
            .dropdown_menu(move |mut menu: PopupMenu, _, _| {
                for option in PasswordChoice::ALL {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(option.label(persistent))
                            .checked(option == choice)
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.form.set_password_choice(option);
                                    this.run = RunState::NotRun;
                                    cx.notify();
                                })
                            }),
                    );
                }
                menu
            });
        let theme = cx.theme();
        let (muted, warning, success) = (theme.muted_foreground, theme.warning, theme.success);
        let mono = theme.mono_font_family.clone();
        let note = |id: &'static str, text: String, color| {
            div()
                .id(id)
                .test_support()
                .text_xs()
                .text_color(color)
                .child(text.clone())
                .test_text(text)
        };
        let file = match self.form.engine {
            Engine::Postgres => "~/.pgpass",
            Engine::MySql | Engine::MariaDb => "~/.my.cnf",
        };
        let fields = match choice {
            PasswordChoice::Keychain => note(
                "conn-password-hint",
                format!(
                    "Left empty, Kerno reads {file}, as the {} tools do.",
                    self.form.engine.label()
                ),
                muted,
            )
            .into_any_element(),
            PasswordChoice::Ask => note(
                "conn-password-hint",
                "Kerno will ask for the password each time it connects.".into(),
                muted,
            )
            .into_any_element(),
            PasswordChoice::Command => h_flex()
                .gap_2()
                .flex_1()
                .child(
                    Input::new(&self.inputs.command_program)
                        .id("conn-password-program")
                        .flex_1(),
                )
                .child(
                    div().w(px(72.)).child(
                        Input::new(&self.inputs.command_timeout)
                            .id("conn-password-timeout")
                            .suffix("s"),
                    ),
                )
                .into_any_element(),
            PasswordChoice::RdsIam => h_flex()
                .gap_2()
                .flex_1()
                .child(
                    Input::new(&self.inputs.rds_region)
                        .id("conn-rds-region")
                        .flex_1(),
                )
                .child(
                    Input::new(&self.inputs.rds_profile)
                        .id("conn-rds-profile")
                        .flex_1(),
                )
                .into_any_element(),
            PasswordChoice::OnePassword => Input::new(&self.inputs.op_reference)
                .id("conn-op-reference")
                .flex_1()
                .into_any_element(),
        };
        let errors = [
            Field::PasswordSource,
            Field::PasswordProgram,
            Field::PasswordTimeout,
            Field::RdsRegion,
            Field::OpReference,
        ]
        .into_iter()
        .find_map(|field| self.error_for(field));
        let mut block = v_flex().gap_1().child(self.field(
            "Password from",
            None,
            h_flex().gap_2().child(picker).child(fields),
            cx,
        ));
        if choice == PasswordChoice::Command {
            block = block.child(Textarea::new(&self.inputs.command_args));
        }
        if let Some(message) = errors {
            block = block.child(
                div()
                    .id("conn-field-error")
                    .test_support()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(message),
            );
        }
        if !choice.runs_a_command() {
            return block.into_any_element();
        }
        let form = self.read_form(cx);
        let line = format!("Runs: {}", form.command_display().unwrap_or_default());
        let unconfirmed = form.command_unconfirmed;
        let running = matches!(self.run, RunState::Running);
        let printed = match &self.run {
            RunState::Printed(length) => Some(note(
                "conn-password-run-result",
                format!(
                    "printed {length} character{} (not shown)",
                    if *length == 1 { "" } else { "s" }
                ),
                success,
            )),
            _ => None,
        };
        // The whole line, wrapped, never cut: it is what the user confirms.
        block = block
            .child(
                div()
                    .id("conn-password-command-line")
                    .test_support()
                    .w_full()
                    .text_xs()
                    .font_family(mono)
                    .child(line.clone())
                    .test_text(line),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("conn-password-run")
                            .outline()
                            .xsmall()
                            .label("Run now")
                            .loading(running)
                            .disabled(running || unconfirmed)
                            .on_click(cx.listener(|this, _, _, cx| this.run_command(cx))),
                    )
                    .children(printed),
            );
        if let RunState::Failed(explained) = &self.run {
            block = block.child(render_failure("conn-password-run-failed", explained, cx));
        }
        if unconfirmed {
            block = block.child(
                h_flex()
                    .id("conn-password-unconfirmed")
                    .test_support()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(warning)
                            .child("From an imported file: it runs only once you confirm it."),
                    )
                    .child(
                        Button::new("conn-password-confirm")
                            .outline()
                            .xsmall()
                            .label("I trust this command")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.form.command_unconfirmed = false;
                                cx.notify();
                            })),
                    ),
            );
        }
        block.into_any_element()
    }

    /// Runs the password command once and reports how long its output is
    /// (the output itself is dropped at once, off the UI thread).
    fn run_command(&mut self, cx: &mut Context<Self>) {
        let Some((config, _)) = self.validated(cx) else {
            return;
        };
        let env = self.connections.read(cx).password_env().cloned();
        let tested = config.clone();
        self.run = RunState::Running;
        let running = crate::bridge::spawn(cx, async move {
            // The core's own path: an unconfirmed command does not run.
            let resolved = kerno_core::password::resolve(&config, None, None, env.as_ref()).await?;
            Ok::<_, Error>(
                resolved
                    .password
                    .map_or(0, |printed| printed.expose().chars().count()),
            )
        });
        self.running_command = Some(cx.spawn(async move |this, cx| {
            let outcome = running.await;
            let _ = this.update(cx, |this, cx| {
                this.run = match outcome {
                    Ok(length) => RunState::Printed(length),
                    Err(error) => RunState::Failed(explain(&error.to_string(), &tested)),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_tls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = TlsMode::ALL
            .iter()
            .position(|mode| *mode == self.form.tls_mode);
        let explanation = match self.form.tls_mode {
            TlsMode::Disable => "Never use TLS.",
            TlsMode::Prefer => "Use TLS when the server offers it. The certificate is not checked.",
            TlsMode::Require => "Require TLS. The certificate is not checked.",
            TlsMode::VerifyCa => "Require TLS and a certificate signed by a trusted authority.",
            TlsMode::VerifyFull => "As verify-ca, and the certificate must match the host name.",
        };
        v_flex()
            .gap_3()
            .child(
                self.field(
                    "Mode",
                    None,
                    RadioGroup::horizontal("conn-tls-mode")
                        .children(TlsMode::ALL.map(TlsMode::as_str))
                        .selected_index(mode)
                        .on_click(cx.listener(|this, index: &usize, _, cx| {
                            if let Some(mode) = TlsMode::ALL.get(*index) {
                                this.form.tls_mode = *mode;
                                cx.notify();
                            }
                        })),
                    cx,
                ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(explanation),
            )
            .child(self.file_field(
                "CA certificate",
                Field::CaFile,
                "conn-ca-file",
                &self.inputs.ca_file,
                cx,
            ))
            .child(self.file_field(
                "Client certificate",
                Field::CertFile,
                "conn-cert-file",
                &self.inputs.cert_file,
                cx,
            ))
            .child(self.file_field(
                "Client key",
                Field::KeyFile,
                "conn-key-file",
                &self.inputs.key_file,
                cx,
            ))
    }

    fn render_ssh(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let client = self.form.ssh_client;
        let openssh = client == SshClient::OpenSsh;
        let found = self.connections.read(cx).openssh().is_some();
        let choices = SshAuthKind::choices(client);
        let auth = choices.iter().position(|kind| *kind == self.form.ssh_auth);
        let enabled = self.form.ssh_enabled;
        let clients = [SshClient::OpenSsh, SshClient::Builtin];
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_3()
            .child(
                Switch::new("conn-ssh-enabled")
                    .label("Reach the database through an SSH server")
                    .checked(enabled)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.form.ssh_enabled = *checked;
                        cx.notify();
                    })),
            )
            .when(enabled, |this| {
                this.child(
                    self.field(
                        "Client",
                        None,
                        RadioGroup::horizontal("conn-ssh-client")
                            .children(["OpenSSH (your ~/.ssh/config)", "Built-in"])
                            .selected_index(clients.iter().position(|c| *c == client))
                            .on_click(cx.listener(move |this, index: &usize, window, cx| {
                                if let Some(client) = clients.get(*index) {
                                    this.set_ssh_client(*client, window, cx);
                                }
                            })),
                        cx,
                    ),
                )
                .when(openssh && !found, |this| {
                    this.child(
                        div()
                            .id("conn-ssh-openssh-missing")
                            .test_support()
                            .text_xs()
                            .text_color(cx.theme().warning)
                            .child(
                                "OpenSSH was not found on this computer; use the built-in client.",
                            ),
                    )
                })
                .child(
                    h_flex()
                        .gap_3()
                        .items_start()
                        .child(
                            div().flex_1().child(
                                self.field(
                                    if openssh {
                                        "SSH host or alias"
                                    } else {
                                        "SSH host"
                                    },
                                    Some(Field::SshHost),
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            Input::new(&self.inputs.ssh_host)
                                                .id("conn-ssh-host")
                                                .flex_1(),
                                        )
                                        .when(openssh && !self.ssh_aliases.is_empty(), |this| {
                                            this.child(self.alias_picker())
                                        }),
                                    cx,
                                ),
                            ),
                        )
                        .child(div().w(px(120.)).child(self.field(
                            "Port",
                            Some(Field::SshPort),
                            Input::new(&self.inputs.ssh_port).id("conn-ssh-port"),
                            cx,
                        ))),
                )
                .child(self.field(
                    "SSH user",
                    Some(Field::SshUser),
                    Input::new(&self.inputs.ssh_user).id("conn-ssh-user"),
                    cx,
                ))
                .when(openssh, |this| {
                    this.child(self.field(
                        "Jump host",
                        Some(Field::SshJump),
                        Input::new(&self.inputs.ssh_jump).id("conn-ssh-jump"),
                        cx,
                    ))
                })
                .child(
                    self.field(
                        "Authentication",
                        Some(Field::SshAuth),
                        RadioGroup::horizontal("conn-ssh-auth")
                            .children(choices.iter().map(|kind| kind.label_for(client)))
                            .selected_index(auth)
                            .on_click(cx.listener(move |this, index: &usize, _, cx| {
                                if let Some(kind) = choices.get(*index) {
                                    this.form.ssh_auth = *kind;
                                    cx.notify();
                                }
                            })),
                        cx,
                    ),
                )
                .map(|this| match self.form.ssh_auth {
                    SshAuthKind::Agent => this.child(self.field(
                        "Agent socket",
                        Some(Field::SshAgentSocket),
                        Input::new(&self.inputs.ssh_agent_socket).id("conn-ssh-agent-socket"),
                        cx,
                    )),
                    SshAuthKind::Password => this.child(
                        self.field(
                            "SSH password",
                            None,
                            Input::new(&self.inputs.ssh_password)
                                .id("conn-ssh-password")
                                .mask_toggle(),
                            cx,
                        ),
                    ),
                    SshAuthKind::KeyFile => this
                        .child(self.file_field(
                            "Private key",
                            Field::SshKeyFile,
                            "conn-ssh-key-file",
                            &self.inputs.ssh_key_file,
                            cx,
                        ))
                        .child(
                            self.field(
                                "Key passphrase",
                                None,
                                Input::new(&self.inputs.ssh_key_passphrase)
                                    .id("conn-ssh-key-passphrase")
                                    .mask_toggle(),
                                cx,
                            ),
                        ),
                })
                .child(div().w(px(200.)).child(self.field(
                    "Keep-alive (seconds, 0 for none)",
                    Some(Field::SshKeepalive),
                    Input::new(&self.inputs.ssh_keepalive).id("conn-ssh-keepalive"),
                    cx,
                )))
                .when(openssh, |this| {
                    this.child(div().text_xs().text_color(muted).child(
                        "Empty fields come from your SSH configuration. OpenSSH checks host keys \
                         against your known_hosts; a key you trust here is added there.",
                    ))
                })
            })
    }

    /// The aliases of the user's SSH configuration, to fill the host field
    /// (still free text).
    fn alias_picker(&self) -> AnyElement {
        let aliases = self.ssh_aliases.clone();
        let input = self.inputs.ssh_host.clone();
        Button::new("conn-ssh-host-list")
            .outline()
            .dropdown_caret(true)
            .tooltip("Pick a Host of your SSH configuration")
            .dropdown_menu(move |mut menu: PopupMenu, _, _| {
                for alias in &aliases {
                    let (input, value) = (input.clone(), alias.clone());
                    menu = menu.item(
                        PopupMenuItem::new(SharedString::from(alias.clone())).on_click(
                            move |_, window, cx| {
                                let value = value.clone();
                                input.update(cx, |input, cx| input.set_value(value, window, cx));
                            },
                        ),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn render_advanced(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let commit_mode = COMMIT_MODES
            .iter()
            .position(|(mode, _)| *mode == self.form.auto_commit);
        v_flex()
            .gap_3()
            .child(
                self.field(
                    "New query tabs start in",
                    None,
                    RadioGroup::horizontal("conn-commit-mode")
                        .children(COMMIT_MODES.map(|(_, label)| label))
                        .selected_index(commit_mode)
                        .on_click(cx.listener(|this, index: &usize, _, cx| {
                            if let Some((mode, _)) = COMMIT_MODES.get(*index) {
                                this.form.auto_commit = *mode;
                                cx.notify();
                            }
                        })),
                    cx,
                ),
            )
            .when(self.form.engine == Engine::Postgres, |this| {
                this.child(self.field(
                    "Default schema (search_path of every session)",
                    None,
                    Input::new(&self.inputs.default_schema).id("conn-default-schema"),
                    cx,
                ))
            })
            .child(self.field(
                "Bootstrap statements, run in every new session (separate them with ;)",
                Some(Field::InitSql),
                Textarea::new(&self.inputs.init_sql),
                cx,
            ))
            .child(
                Checkbox::new("conn-ignore-init-errors")
                    .label("Ignore errors in bootstrap statements (they are only logged)")
                    .checked(self.form.ignore_init_errors)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.form.ignore_init_errors = *checked;
                        cx.notify();
                    })),
            )
            .child(self.field(
                "Local client (backup, restore and scripts run its tools)",
                None,
                self.render_local_client(cx),
                cx,
            ))
            .child(self.field(
                "Driver options, one key=value per line",
                Some(Field::Options),
                Textarea::new(&self.inputs.options),
                cx,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(match self.form.engine {
                        Engine::Postgres => "application_name and connect_timeout are understood by the driver; any other option is a server setting applied at session start (search_path, statement_timeout, …).",
                        Engine::MySql | Engine::MariaDb => "connect_timeout is understood by the driver; any other option is a session variable set at session start (sql_mode, time_zone, …).",
                    }),
            )
    }

    /// The connection's client folder, or the default; a folder chosen
    /// here must hold the engine's tools, and is added to Settings ▸ Local
    /// clients (a client is only run from a folder listed there).
    fn render_local_client(&self, cx: &mut Context<Self>) -> AnyElement {
        let chosen = self.form.client_dir.clone();
        h_flex()
            .gap_2()
            .child(
                div()
                    .id("conn-client-dir")
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(SharedString::from(chosen.as_ref().map_or_else(
                        || "Default (Settings ▸ Local clients)".to_string(),
                        |dir| dir.display().to_string(),
                    ))),
            )
            .child(
                Button::new("conn-client-choose")
                    .small()
                    .outline()
                    .label("Choose folder…")
                    .on_click(cx.listener(|this, _, window, cx| this.choose_client(window, cx))),
            )
            .child(
                Button::new("conn-client-default")
                    .small()
                    .ghost()
                    .label("Use the default")
                    .disabled(chosen.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.form.client_dir = None;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn choose_client(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picking = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use this client".into()),
        });
        let family = kerno_core::tools::ClientFamily::of(self.form.engine);
        self.choosing_client = Some(cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picking.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            let probing = this.update(cx, |_, cx| {
                let dir = dir.clone();
                crate::bridge::spawn_blocking(cx, move || {
                    kerno_core::tools::probe_client(&dir, family).is_some()
                })
            });
            let Ok(probing) = probing else {
                return;
            };
            let found = probing.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if found {
                    AppState::update_settings(cx, |settings| {
                        if !settings.client_folders.contains(&dir) {
                            settings.client_folders.push(dir.clone());
                        }
                    });
                    this.form.client_dir = Some(dir);
                } else {
                    window.push_notification(
                        gpui_kit::component::notification::Notification::warning(
                            SharedString::from(format!(
                                "{} holds no {} client tool: it was not chosen.",
                                dir.display(),
                                family.label()
                            )),
                        ),
                        cx,
                    );
                }
                cx.notify();
            });
        }));
    }

    fn render_test(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match &self.test {
            TestState::NotRun => div().into_any_element(),
            TestState::Running => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Connecting…")
                .into_any_element(),
            TestState::Passed(report) => v_flex()
                .gap_1()
                .child(
                    div()
                        .id("conn-test-passed")
                        .test_support()
                        .text_sm()
                        .text_color(theme.success)
                        .child(SharedString::from(format!(
                            "Connected to {} · {} ms round trip",
                            short_version(&report.version),
                            report.latency.as_millis().max(1)
                        ))),
                )
                .children(report.password.clone().map(|origin| {
                    div()
                        .id("conn-test-password")
                        .test_support()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(origin.clone()))
                        .test_text(origin)
                }))
                .into_any_element(),
            TestState::Failed(explained) => {
                render_failure("conn-test-failed", explained, cx).into_any_element()
            }
            TestState::UnknownHost {
                host,
                port,
                algorithm,
                fingerprint,
            } => {
                let trusted = fingerprint.clone();
                v_flex()
                    .id("conn-test-unknown-host")
                    .test_support()
                    .gap_2()
                    .text_sm()
                    .child(SharedString::from(format!(
                        "The SSH server {host}:{port} is not known yet. Its {algorithm} key fingerprint is:"
                    )))
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(SharedString::from(fingerprint.clone())),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child("Compare it with the fingerprint your server's administrator gives you before trusting it."),
                    )
                    .child(
                        h_flex().child(
                            Button::new("conn-trust-host")
                                .outline()
                                .label("Trust this key and test again")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.trusted_fingerprint = Some(trusted.clone());
                                    this.run_test(cx);
                                })),
                        ),
                    )
                    .into_any_element()
            }
        }
    }
}

/// The section a field is shown in.
fn section_of(field: Field) -> Section {
    match field {
        Field::Url
        | Field::Host
        | Field::Port
        | Field::User
        | Field::PasswordSource
        | Field::PasswordProgram
        | Field::PasswordTimeout
        | Field::RdsRegion
        | Field::OpReference => Section::General,
        Field::CaFile | Field::CertFile | Field::KeyFile => Section::Tls,
        Field::SshHost
        | Field::SshPort
        | Field::SshUser
        | Field::SshJump
        | Field::SshAuth
        | Field::SshKeyFile
        | Field::SshAgentSocket
        | Field::SshKeepalive => Section::Ssh,
        Field::Options | Field::InitSql => Section::Advanced,
    }
}

fn ssh_port_placeholder(client: SshClient) -> &'static str {
    match client {
        SshClient::OpenSsh => "from SSH config",
        SshClient::Builtin => "22",
    }
}

fn ssh_user_placeholder(client: SshClient) -> &'static str {
    match client {
        SshClient::OpenSsh => "From your SSH configuration when empty",
        SshClient::Builtin => "User on the SSH server",
    }
}

fn engine_key(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "postgres",
        Engine::MySql => "mysql",
        Engine::MariaDb => "mariadb",
    }
}

/// The product and version of a server banner, without its build details
/// (`PostgreSQL 17.4 (Debian …) on aarch64-…, compiled by gcc …`).
fn short_version(banner: &str) -> &str {
    let line = banner.lines().next().unwrap_or_default();
    line.split(" on ").next().unwrap_or(line).trim()
}

impl Render for ConnectionDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = SECTIONS
            .iter()
            .position(|(section, _)| *section == self.section)
            .unwrap_or(0);
        let testing = matches!(self.test, TestState::Running);
        let body = match self.section {
            Section::General => self.render_general(cx).into_any_element(),
            Section::Tls => self.render_tls(cx).into_any_element(),
            Section::Ssh => self.render_ssh(cx).into_any_element(),
            Section::Advanced => self.render_advanced(cx).into_any_element(),
        };
        // A dot on a tab tells where the errors are.
        let has_error = |section: Section| {
            self.errors
                .iter()
                .any(|error| section_of(error.field) == section)
        };
        let danger = cx.theme().danger;
        v_flex()
            .gap_4()
            .pt_2()
            .child(self.render_engines(cx))
            .child(
                TabBar::new("conn-sections")
                    .underline()
                    .selected_index(selected)
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        if let Some((section, _)) = SECTIONS.get(*index) {
                            this.section = *section;
                            cx.notify();
                        }
                    }))
                    .children(SECTIONS.iter().map(|(section, label)| {
                        Tab::new().label(*label).when(has_error(*section), |tab| {
                            tab.suffix(div().size(px(6.)).rounded_full().bg(danger))
                        })
                    })),
            )
            .child(div().min_h(px(392.)).child(body))
            .child(self.render_test(cx))
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        Button::new("conn-test")
                            .outline()
                            .label("Test connection")
                            .loading(testing)
                            .disabled(testing)
                            .on_click(cx.listener(|this, _, _, cx| this.run_test(cx))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("conn-cancel")
                                    .ghost()
                                    .label("Cancel")
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(Button::new("conn-save").outline().label("Save").on_click(
                                cx.listener(|this, _, window, cx| this.save(false, window, cx)),
                            ))
                            .child(
                                Button::new("conn-save-connect")
                                    .primary()
                                    .label("Save and connect")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save(true, window, cx)
                                    })),
                            ),
                    ),
            )
    }
}

/// A connection failure in plain words (CLAUDE.md §3, item 5): what
/// happened (strong) and what to do next, then the raw message of the
/// driver or server, muted and selectable. `id` names the block; the raw
/// text is `{id}-raw`.
pub(crate) fn render_failure(id: &str, explained: &Explained, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .id(SharedString::from(id.to_string()))
        .test_support()
        .gap_1()
        .text_sm()
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.danger)
                .child(SharedString::from(explained.headline.clone()))
                .test_text(explained.headline.clone()),
        )
        .when(!explained.next_step.is_empty(), |this| {
            this.child(
                div()
                    .child(SharedString::from(explained.next_step.clone()))
                    .test_text(explained.next_step.clone()),
            )
        })
        .child(raw_message(&format!("{id}-raw"), &explained.raw, cx))
}

/// A driver's or server's message, muted and selectable, shown as it came.
pub(crate) fn raw_message(id: &str, raw: &str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(
            TextView::markdown(SharedString::from(id.to_string()), plain_markdown(raw))
                .selectable(true)
                .text_xs()
                .text_color(cx.theme().muted_foreground),
        )
}

/// `text` as Markdown that renders as itself: every ASCII punctuation mark
/// escaped, line breaks kept.
fn plain_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        match c {
            '\n' => out.push_str("  \n"),
            c if c.is_ascii_punctuation() => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

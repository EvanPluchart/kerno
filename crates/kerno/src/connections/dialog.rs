//! The connection dialog: create or edit a saved connection, fill it from
//! a URL, test it, save it.

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
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
use kerno_core::config::{Engine, Environment, Secrets, TlsMode};
use kerno_core::ssh::SshError;
use kerno_core::storage::SavedConnection;

use super::form::{COMMIT_MODES, ConnectionForm, Field, FieldError, SshAuthKind};
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
    Failed(String),
    /// The SSH server is not known yet: the user decides.
    UnknownHost {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
    },
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
        if let Some(folder) = folder {
            form.group = folder;
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
        let database = text(&form.database, "Default database (optional)");
        let ca_file = text(&form.ca_file, "System trust store when empty");
        let cert_file = text(&form.cert_file, "Client certificate (optional)");
        let key_file = text(&form.key_file, "Client key (optional)");
        let ssh_host = text(&form.ssh_host, "bastion.example.com");
        let ssh_port = text(&form.ssh_port, "22");
        let ssh_user = text(&form.ssh_user, "User on the SSH server");
        let ssh_key_file = text(&form.ssh_key_file, "~/.ssh/id_ed25519");
        let ssh_agent_socket = text(&form.ssh_agent_socket, "SSH_AUTH_SOCK when empty");
        let ssh_keepalive = text(&form.ssh_keepalive, "30");
        let default_schema = text(&form.default_schema, "public");
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
            },
            section: Section::General,
            errors: Vec::new(),
            test: TestState::NotRun,
            trusted_fingerprint: None,
            running_test: None,
            _subscriptions: vec![fill_on_enter, refilter],
        }
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
        if i.options.read(cx).value().as_ref() != form.options {
            let options = form.options.clone();
            i.options
                .update(cx, |input, cx| input.set_value(options, window, cx));
        }
        self.form = form;
        self.test = TestState::NotRun;
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
                        other => TestState::Failed(Error::from(other).to_string()),
                    },
                    Err(error) => TestState::Failed(error.to_string()),
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
        let environment = Environment::ALL
            .iter()
            .position(|e| *e == self.form.environment);
        let swatch = |key: Option<&'static str>, cx: &mut Context<Self>| {
            let selected = self.form.color.as_deref() == key;
            let color = key.and_then(|key| theme::tag_color(key, cx));
            div()
                .id(SharedString::from(format!(
                    "color-{}",
                    key.unwrap_or("none")
                )))
                .size(px(20.))
                .rounded_full()
                .border_2()
                .border_color(if selected {
                    cx.theme().ring
                } else {
                    cx.theme().border
                })
                .when_some(color, |this, color| this.bg(color))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.form.color = key.map(str::to_string);
                    cx.notify();
                }))
        };
        let mut swatches = vec![swatch(None, cx)];
        for (key, _, _) in theme::TAG_COLORS {
            swatches.push(swatch(Some(key), cx));
        }
        let theme = cx.theme();

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
                    .child(
                        self.field(
                            "Environment",
                            None,
                            RadioGroup::horizontal("conn-environment")
                                .children(Environment::ALL.map(Environment::label))
                                .selected_index(environment)
                                .on_click(cx.listener(|this, index: &usize, _, cx| {
                                    if let Some(environment) = Environment::ALL.get(*index) {
                                        this.form.environment = *environment;
                                        cx.notify();
                                    }
                                })),
                            cx,
                        ),
                    ),
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
                    .child(
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
                    ),
            )
            .child(
                Checkbox::new("conn-save-password")
                    .label(if memory_only {
                        "Remember passwords until Kerno quits (no system secret store in use)"
                    } else {
                        "Save passwords in the system keychain"
                    })
                    .checked(self.form.save_password)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.form.save_password = *checked;
                        cx.notify();
                    })),
            )
            .when(!self.form.save_password, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Kerno will ask for the password each time it connects."),
                )
            })
            .child(self.field(
                "Database",
                None,
                Input::new(&self.inputs.database).id("conn-database"),
                cx,
            ))
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
        let auth = SshAuthKind::ALL
            .iter()
            .position(|kind| *kind == self.form.ssh_auth);
        let enabled = self.form.ssh_enabled;
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
                    h_flex()
                        .gap_3()
                        .items_start()
                        .child(div().flex_1().child(self.field(
                            "SSH host",
                            Some(Field::SshHost),
                            Input::new(&self.inputs.ssh_host).id("conn-ssh-host"),
                            cx,
                        )))
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
                .child(
                    self.field(
                        "Authentication",
                        None,
                        RadioGroup::horizontal("conn-ssh-auth")
                            .children(SshAuthKind::ALL.map(SshAuthKind::label))
                            .selected_index(auth)
                            .on_click(cx.listener(|this, index: &usize, _, cx| {
                                if let Some(kind) = SshAuthKind::ALL.get(*index) {
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
            })
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

    fn render_test(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match &self.test {
            TestState::NotRun => div().into_any_element(),
            TestState::Running => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Connecting…")
                .into_any_element(),
            TestState::Passed(report) => div()
                .id("conn-test-passed")
                .test_support()
                .text_sm()
                .text_color(theme.success)
                .child(SharedString::from(format!(
                    "Connected to {} · {} ms round trip",
                    short_version(&report.version),
                    report.latency.as_millis().max(1)
                )))
                .into_any_element(),
            TestState::Failed(message) => div()
                .id("conn-test-failed")
                .test_support()
                .text_sm()
                .text_color(theme.danger)
                .child(SharedString::from(message.clone()))
                .into_any_element(),
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
        Field::Url | Field::Host | Field::Port | Field::User => Section::General,
        Field::CaFile | Field::CertFile | Field::KeyFile => Section::Tls,
        Field::SshHost
        | Field::SshPort
        | Field::SshUser
        | Field::SshKeyFile
        | Field::SshAgentSocket
        | Field::SshKeepalive => Section::Ssh,
        Field::Options | Field::InitSql => Section::Advanced,
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

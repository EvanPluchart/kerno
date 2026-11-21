//! PostgreSQL's foreign data wrappers (DBeaver's "Foreign data wrappers
//! configurator", `PostgreFDWConfigWizard`, Apache-2.0: its script is
//! `CREATE SERVER … FOREIGN DATA WRAPPER … OPTIONS (…)` then `CREATE USER
//! MAPPING FOR CURRENT_USER SERVER …`): the wrappers, foreign servers and
//! user mappings of a database, and a form that creates a server, the
//! current user's mapping on it and imports a remote schema, shown as SQL
//! and run once confirmed.
//!
//! The user mapping's password is typed for this run: it is never stored,
//! shown masked in the statement (`'***'`, as the history masks password
//! literals), never logged, and masked in a server error. The mappings'
//! options are never read back (`umoptions` holds passwords).
//!
//! DBeaver builds foreign tables one by one from the tables of another
//! connection; Kerno imports a whole remote schema (`IMPORT FOREIGN
//! SCHEMA`), which postgres_fdw and mysql_fdw do themselves.

use std::sync::Arc;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, Window, div, px,
};
use kerno_core::exec::Collected;
use kerno_core::sql::LexRules;
use kerno_core::storage::redact_secrets;
use kerno_core::{Dialect, Engine, Session, exec};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::tabs::Target;

/// What the form holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FdwForm {
    pub server: String,
    pub wrapper: String,
    pub host: String,
    pub port: String,
    /// The remote database (postgres_fdw's `dbname`; mysql_fdw has none).
    pub dbname: String,
    pub user: String,
    /// Typed for this run, never kept.
    pub password: String,
    /// The remote schema (MySQL: database) to import; none when empty.
    pub remote_schema: String,
    /// Where it is imported, created when missing.
    pub local_schema: String,
}

/// The script the form writes: the foreign server, the current user's
/// mapping on it, and the import of the remote schema into a local one;
/// or what is missing. Options are written only when filled in.
pub fn fdw_script(form: &FdwForm, dialect: &dyn Dialect) -> Result<String, String> {
    let ident = |name: &str| dialect.quote_ident(name.trim());
    let options = |pairs: &[(&str, &str)]| {
        let set: Vec<String> = pairs
            .iter()
            .filter(|(_, value)| !value.trim().is_empty())
            .map(|(key, value)| format!("{key} {}", dialect.quote_string(value.trim())))
            .collect();
        if set.is_empty() {
            String::new()
        } else {
            format!(" OPTIONS ({})", set.join(", "))
        }
    };
    if form.server.trim().is_empty() {
        return Err("Name the foreign server.".into());
    }
    if form.wrapper.trim().is_empty() {
        return Err("Choose a foreign data wrapper.".into());
    }
    if !form.port.trim().chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("“{}” is not a port number.", form.port.trim()));
    }
    if form.remote_schema.trim().is_empty() != form.local_schema.trim().is_empty() {
        return Err("Name both the remote schema and the local one, or neither.".into());
    }
    // mysql_fdw names its user `username` and has no database option.
    let mysql = form.wrapper.trim() == "mysql_fdw";
    let server = ident(&form.server);
    let mut script = format!(
        "CREATE SERVER {server} FOREIGN DATA WRAPPER {}{};\n",
        ident(&form.wrapper),
        options(&[
            ("host", &form.host),
            ("port", &form.port),
            ("dbname", if mysql { "" } else { &form.dbname }),
        ])
    );
    script.push_str(&format!(
        "CREATE USER MAPPING FOR CURRENT_USER SERVER {server}{};\n",
        options(&[
            (if mysql { "username" } else { "user" }, &form.user),
            ("password", &form.password),
        ])
    ));
    if !form.remote_schema.trim().is_empty() {
        let local = ident(&form.local_schema);
        script.push_str(&format!(
            "CREATE SCHEMA IF NOT EXISTS {local};\nIMPORT FOREIGN SCHEMA {} FROM SERVER {server} \
             INTO {local};\n",
            ident(&form.remote_schema)
        ));
    }
    Ok(script)
}

/// `script` as it is shown: its passwords masked.
pub fn masked(script: &str) -> String {
    redact_secrets(script, LexRules::POSTGRES)
}

/// What the database holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// Wrappers installed.
    pub wrappers: Vec<String>,
    /// `*_fdw` extensions the server has, and whether each is installed.
    pub extensions: Vec<(String, bool)>,
    /// Servers: name, wrapper, options.
    pub servers: Vec<(String, String, String)>,
    /// User mappings: server, user (never their options).
    pub mappings: Vec<(String, String)>,
}

/// The catalog queries of [`Listing`], in its order.
const LISTING: [&str; 4] = [
    "SELECT fdwname FROM pg_catalog.pg_foreign_data_wrapper ORDER BY 1",
    "SELECT name, installed_version IS NOT NULL FROM pg_catalog.pg_available_extensions \
     WHERE name LIKE '%\\_fdw' ORDER BY 1",
    "SELECT s.srvname, w.fdwname, coalesce(array_to_string(s.srvoptions, ', '), '') \
     FROM pg_catalog.pg_foreign_server s \
     JOIN pg_catalog.pg_foreign_data_wrapper w ON w.oid = s.srvfdw ORDER BY 1",
    "SELECT srvname, usename FROM pg_catalog.pg_user_mappings ORDER BY 1, 2",
];

fn listing(collected: &[Collected]) -> Listing {
    let rows = |index: usize| {
        collected
            .get(index)
            .and_then(|collected| collected.results.first())
            .map(|result| result.rows())
            .unwrap_or_default()
    };
    let text = |row: &[Option<String>], column: usize| {
        row.get(column).cloned().flatten().unwrap_or_default()
    };
    Listing {
        wrappers: rows(0).iter().map(|row| text(row, 0)).collect(),
        extensions: rows(1)
            .iter()
            .map(|row| (text(row, 0), matches!(text(row, 1).as_str(), "t" | "true")))
            .collect(),
        servers: rows(2)
            .iter()
            .map(|row| (text(row, 0), text(row, 1), text(row, 2)))
            .collect(),
        mappings: rows(3)
            .iter()
            .map(|row| (text(row, 0), text(row, 1)))
            .collect(),
    }
}

#[derive(Debug, Clone)]
enum Loaded {
    Loading,
    Failed(String),
    Ready(Listing),
}

pub struct FdwDialog {
    connections: Entity<Connections>,
    connection: String,
    name: String,
    environment: String,
    database: String,
    read_only: bool,
    /// The connection's own session: its dialect.
    meta: Arc<dyn Session>,
    listing: Loaded,
    wrapper: String,
    server: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    dbname: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    remote_schema: Entity<InputState>,
    local_schema: Entity<InputState>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// Opens the foreign data wrappers of the database of `target` (the
/// connection's own when it names none), or says why it cannot.
pub fn open(
    connections: Entity<Connections>,
    target: Target,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<FdwDialog>> {
    let checked = {
        let manager = connections.read(cx);
        let connected = matches!(manager.status(&target.connection), Status::Connected { .. });
        match (
            manager.get(&target.connection),
            manager.meta_session(&target.connection),
        ) {
            (Some(saved), Some(meta)) if connected => {
                if saved.config.engine != Engine::Postgres {
                    Err("Foreign data wrappers are PostgreSQL's.".to_string())
                } else {
                    let database = target
                        .database
                        .clone()
                        .or_else(|| meta.database())
                        .unwrap_or_default();
                    Ok((saved.config.clone(), database, meta))
                }
            }
            _ => Err("Connect first.".into()),
        }
    };
    let (config, database, meta) = match checked {
        Ok(checked) => checked,
        Err(why) => {
            window.push_notification(
                Notification::warning(SharedString::from(format!("Foreign data wrappers: {why}"))),
                cx,
            );
            return None;
        }
    };
    let title = format!("Foreign data wrappers of {database}");
    let view = cx.new(|cx| {
        let input =
            |placeholder: &'static str, window: &mut Window, cx: &mut Context<FdwDialog>| {
                cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
            };
        let server = input("remote_server", window, cx);
        let host = input("host", window, cx);
        let port = input("5432", window, cx);
        let dbname = input("remote database", window, cx);
        let user = input("remote user", window, cx);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("password, for this run only")
        });
        let remote_schema = input("public", window, cx);
        let local_schema = input("local schema", window, cx);
        let subscriptions = [
            &server,
            &host,
            &port,
            &dbname,
            &user,
            &password,
            &remote_schema,
            &local_schema,
        ]
        .into_iter()
        .map(|input| {
            cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            })
        })
        .collect();
        let mut dialog = FdwDialog {
            connections: connections.clone(),
            connection: target.connection.clone(),
            name: config.name.clone(),
            environment: config.environment.clone(),
            database,
            read_only: config.read_only,
            meta,
            listing: Loaded::Loading,
            wrapper: String::new(),
            server,
            host,
            port,
            dbname,
            user,
            password,
            remote_schema,
            local_schema,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        dialog.reload(cx);
        dialog
    });
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        dialog
            .title(SharedString::from(title.clone()))
            .w(px(760.))
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    Some(view)
}

impl FdwDialog {
    /// Runs `sql` on a session of the dialog's database, closed after.
    fn on_database(&self, sql: Vec<String>, cx: &App) -> Task<kerno_core::Result<Vec<Collected>>> {
        let opening = self.connections.read(cx).open_session(
            &self.connection,
            Some(self.database.clone()),
            cx,
        );
        bridge::spawn(cx, async move {
            let session = opening.await?;
            let mut collected = Vec::new();
            let mut failed = None;
            for sql in sql {
                match exec::collect(session.clone(), sql).await {
                    Ok(done) => collected.push(done),
                    Err(error) => {
                        failed = Some(error);
                        break;
                    }
                }
            }
            session.close().await;
            match failed {
                Some(error) => Err(error),
                None => Ok(collected),
            }
        })
    }

    /// Reads the wrappers, servers and mappings again.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.listing = Loaded::Loading;
        let reading = self.on_database(LISTING.map(str::to_string).to_vec(), cx);
        self.tasks.push(cx.spawn(async move |this, cx| {
            let read = reading.await;
            let _ = this.update(cx, |this, cx| {
                this.listing = match read {
                    Ok(collected) => {
                        let listing = listing(&collected);
                        if !listing.wrappers.contains(&this.wrapper) {
                            this.wrapper = listing.wrappers.first().cloned().unwrap_or_default();
                        }
                        Loaded::Ready(listing)
                    }
                    Err(error) => Loaded::Failed(kerno_core::describe(&error).to_string()),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn text(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().to_string()
    }

    /// The form as it stands.
    pub fn form(&self, cx: &App) -> FdwForm {
        FdwForm {
            server: Self::text(&self.server, cx),
            wrapper: self.wrapper.clone(),
            host: Self::text(&self.host, cx),
            port: Self::text(&self.port, cx),
            dbname: Self::text(&self.dbname, cx),
            user: Self::text(&self.user, cx),
            password: Self::text(&self.password, cx),
            remote_schema: Self::text(&self.remote_schema, cx),
            local_schema: Self::text(&self.local_schema, cx),
        }
    }

    /// Whether the form can be filled: the wrappers are read and one is
    /// installed (a server needs one).
    fn has_wrapper(&self) -> bool {
        matches!(&self.listing, Loaded::Ready(listing) if !listing.wrappers.is_empty())
    }

    /// Why Create cannot run, if it cannot.
    fn blocked(&self, cx: &App) -> Option<String> {
        if self.read_only {
            return Some("read-only connection".into());
        }
        if !self.has_wrapper() {
            return Some("Install a wrapper first.".into());
        }
        fdw_script(&self.form(cx), self.meta.dialect()).err()
    }

    /// Shows the script, its password masked, and runs it once confirmed.
    pub fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked(cx).is_some() {
            return;
        }
        let form = self.form(cx);
        let Ok(script) = fdw_script(&form, self.meta.dialect()) else {
            return;
        };
        let view = cx.entity();
        let password = form.password;
        sql_confirm::open(
            Confirmation {
                title: format!("Create the foreign server {}", form.server.trim()),
                explanation: format!(
                    "Creates the server, your user's mapping on it{} in the database “{}”. The \
                     password is used for this statement only: Kerno does not keep it.",
                    if form.remote_schema.trim().is_empty() {
                        String::new()
                    } else {
                        format!(
                            ", and imports “{}” into “{}”",
                            form.remote_schema.trim(),
                            form.local_schema.trim()
                        )
                    },
                    self.database
                ),
                statement: Statement::Fixed(masked(&script)),
                action: "Create",
                destructive: false,
                connection: self.name.clone(),
                environment: self.environment.clone(),
                writes: true,
            },
            window,
            cx,
            // The statement shown is masked: the one run is kept here.
            move |_, window, cx| {
                let (script, password) = (script.clone(), password.clone());
                view.update(cx, |this, cx| this.run(script, password, window, cx));
            },
        );
    }

    /// Runs the confirmed script on a session of its own (one implicit
    /// transaction: all of it or nothing).
    fn run(
        &mut self,
        script: String,
        password: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let running = self.on_database(vec![script], cx);
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let ran = running.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match ran {
                    Ok(_) => {
                        this.password
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        window.push_notification(
                            Notification::success("Created the foreign server."),
                            cx,
                        );
                    }
                    Err(error) => {
                        // Detail and hint too (C14), masked as the message is.
                        let mut text = kerno_core::describe(&error).to_string();
                        if !password.is_empty() {
                            text = text.replace(password.as_str(), "•••");
                        }
                        window.push_notification(crate::error_notification(masked(&text)), cx);
                    }
                }
                this.reload(cx);
            });
        }));
    }

    /// Installs a wrapper's extension, once confirmed.
    fn install(&mut self, extension: String, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(sql) = self
            .meta
            .dialect()
            .create_extension_sql(&extension, None, None, false)
        else {
            return;
        };
        let view = cx.entity();
        sql_confirm::open(
            Confirmation {
                title: format!("Install {extension}"),
                explanation: format!(
                    "Installs the extension {extension} in the database “{}”: its foreign data \
                     wrapper becomes usable there.",
                    self.database
                ),
                statement: Statement::Fixed(sql),
                action: "Install",
                destructive: false,
                connection: self.name.clone(),
                environment: self.environment.clone(),
                writes: true,
            },
            window,
            cx,
            move |sql, _, cx| {
                view.update(cx, |this, cx| {
                    let running = this.on_database(vec![sql], cx);
                    this.tasks.push(cx.spawn(async move |this, cx| {
                        let ran = running.await;
                        let _ = this.update(cx, |this, cx| {
                            if let Err(error) = ran {
                                this.listing =
                                    Loaded::Failed(kerno_core::describe(&error).to_string());
                                cx.notify();
                            } else {
                                this.reload(cx);
                            }
                        });
                    }));
                })
            },
        );
    }

    /// What the database holds, once read; for tests.
    #[cfg(test)]
    pub fn listing(&self) -> Option<Listing> {
        match &self.listing {
            Loaded::Ready(listing) => Some(listing.clone()),
            _ => None,
        }
    }

    /// The statement the confirmation shows now; for tests.
    #[cfg(test)]
    pub fn shown_script(&self, cx: &App) -> Result<String, String> {
        fdw_script(&self.form(cx), self.meta.dialect()).map(|script| masked(&script))
    }

    fn section(title: &'static str, rows: Vec<AnyElement>, empty: &'static str) -> AnyElement {
        v_flex()
            .gap_1()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
            .when(rows.is_empty(), |this| {
                this.child(div().text_xs().child(empty))
            })
            .children(rows)
            .into_any_element()
    }

    fn render_listing(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let listing = match &self.listing {
            Loaded::Loading => {
                return div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Reading the wrappers, servers and mappings…")
                    .into_any_element();
            }
            Loaded::Failed(error) => {
                return div()
                    .id("fdw-error")
                    .test_support()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(SharedString::from(error.clone()))
                    .into_any_element();
            }
            Loaded::Ready(listing) => listing,
        };
        let line = |text: String| {
            div()
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .child(SharedString::from(text.clone()))
                .test_text(text)
                .into_any_element()
        };
        let installable: Vec<AnyElement> = listing
            .extensions
            .iter()
            .filter(|(_, installed)| !installed)
            .enumerate()
            .map(|(index, (name, _))| {
                let name = name.clone();
                h_flex()
                    .gap_2()
                    .child(line(format!("{name} (not installed)")))
                    .child(
                        Button::new(("fdw-install", index))
                            .xsmall()
                            .outline()
                            .label("Install…")
                            .disabled(self.read_only)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.install(name.clone(), window, cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let wrappers: Vec<AnyElement> = listing
            .wrappers
            .iter()
            .map(|name| line(name.clone()))
            .chain(installable)
            .collect();
        h_flex()
            .id("fdw-listing")
            .test_support()
            .gap_6()
            .items_start()
            .child(Self::section(
                "Wrappers",
                wrappers,
                "None: install postgres_fdw to reach another PostgreSQL server.",
            ))
            .child(Self::section(
                "Foreign servers",
                listing
                    .servers
                    .iter()
                    .map(|(name, wrapper, options)| line(format!("{name} · {wrapper} · {options}")))
                    .collect(),
                "None.",
            ))
            .child(Self::section(
                "User mappings",
                listing
                    .mappings
                    .iter()
                    .map(|(server, user)| line(format!("{user} on {server}")))
                    .collect(),
                "None.",
            ))
            .into_any_element()
    }

    fn field(
        label: &'static str,
        id: &'static str,
        input: &Entity<InputState>,
        enabled: bool,
    ) -> AnyElement {
        h_flex()
            .gap_3()
            .child(div().w(px(130.)).flex_none().text_sm().child(label))
            .child(
                div()
                    .flex_1()
                    .child(Input::new(input).small().id(id).disabled(!enabled)),
            )
            .into_any_element()
    }
}

impl Render for FdwDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let wrappers = match &self.listing {
            Loaded::Ready(listing) => listing.wrappers.clone(),
            _ => Vec::new(),
        };
        let blocked = self.blocked(cx);
        let script = fdw_script(&self.form(cx), self.meta.dialect());
        let theme = cx.theme();
        // No wrapper installed: nothing to make a server with, the form is off.
        let enabled = self.has_wrapper();
        let none = matches!(&self.listing, Loaded::Ready(_)) && !enabled;
        v_flex()
            .id("fdw-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .child(self.render_listing(cx))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("New foreign server"),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(div().w(px(130.)).flex_none().text_sm().child("Wrapper"))
                    .when(none, |this| {
                        this.child(
                            div()
                                .id("fdw-no-wrapper")
                                .test_support()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .aria_label("Install a wrapper first (above).")
                                .child("Install a wrapper first (above)."),
                        )
                    })
                    .children(wrappers.into_iter().enumerate().map(|(index, name)| {
                        let chosen = name == self.wrapper;
                        Button::new(("fdw-wrapper", index))
                            .xsmall()
                            .label(SharedString::from(name.clone()))
                            .map(|button| {
                                if chosen {
                                    button.primary()
                                } else {
                                    button.outline()
                                }
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.wrapper = name.clone();
                                cx.notify();
                            }))
                    })),
            )
            .child(Self::field(
                "Server name",
                "fdw-server",
                &self.server,
                enabled,
            ))
            .child(Self::field("Host", "fdw-host", &self.host, enabled))
            .child(Self::field("Port", "fdw-port", &self.port, enabled))
            .when(self.wrapper != "mysql_fdw", |this| {
                this.child(Self::field(
                    "Remote database",
                    "fdw-dbname",
                    &self.dbname,
                    enabled,
                ))
            })
            .child(Self::field("Remote user", "fdw-user", &self.user, enabled))
            .child(Self::field(
                "Password",
                "fdw-password",
                &self.password,
                enabled,
            ))
            .child(Self::field(
                "Remote schema",
                "fdw-remote-schema",
                &self.remote_schema,
                enabled,
            ))
            .child(Self::field(
                "Into schema",
                "fdw-local-schema",
                &self.local_schema,
                enabled,
            ))
            .child(
                div()
                    .id("fdw-script")
                    .test_support()
                    .p_2()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .whitespace_normal()
                    .map(|this| match &script {
                        _ if none => this
                            .text_color(theme.muted_foreground)
                            .child("Install a wrapper first."),
                        Ok(script) => {
                            let shown = masked(script);
                            this.child(
                                div()
                                    .child(SharedString::from(shown.clone()))
                                    .test_text(shown),
                            )
                        }
                        Err(why) => this
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(why.clone())),
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .when_some(blocked.clone().filter(|_| self.read_only), |this, why| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(why)),
                        )
                    })
                    .child(
                        Button::new("fdw-close")
                            .ghost()
                            .label("Close")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("fdw-create")
                            .primary()
                            .label("Create…")
                            .disabled(blocked.is_some())
                            .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> FdwForm {
        FdwForm {
            server: "kerno_loop".into(),
            wrapper: "postgres_fdw".into(),
            host: "localhost".into(),
            port: "5432".into(),
            dbname: "src".into(),
            user: "kerno".into(),
            password: "hunter2".into(),
            remote_schema: "public".into(),
            local_schema: "remote_src".into(),
        }
    }

    #[test]
    fn the_script_creates_the_server_its_mapping_and_imports_the_schema() {
        let pg = kerno_postgres::PgDialect;
        let script = fdw_script(&form(), &pg).unwrap();
        assert_eq!(
            script,
            "CREATE SERVER \"kerno_loop\" FOREIGN DATA WRAPPER \"postgres_fdw\" OPTIONS (host \
             'localhost', port '5432', dbname 'src');\n\
             CREATE USER MAPPING FOR CURRENT_USER SERVER \"kerno_loop\" OPTIONS (user 'kerno', \
             password 'hunter2');\n\
             CREATE SCHEMA IF NOT EXISTS \"remote_src\";\n\
             IMPORT FOREIGN SCHEMA \"public\" FROM SERVER \"kerno_loop\" INTO \"remote_src\";\n"
        );
        // Shown masked.
        let shown = masked(&script);
        assert!(
            !shown.contains("hunter2") && shown.contains("password '***'"),
            "{shown}"
        );

        // Empty options are left out; quotes are escaped.
        let bare = FdwForm {
            host: "o'brien".into(),
            port: String::new(),
            dbname: String::new(),
            user: String::new(),
            password: String::new(),
            remote_schema: String::new(),
            local_schema: String::new(),
            ..form()
        };
        assert_eq!(
            fdw_script(&bare, &pg).unwrap(),
            "CREATE SERVER \"kerno_loop\" FOREIGN DATA WRAPPER \"postgres_fdw\" OPTIONS (host \
             'o''brien');\nCREATE USER MAPPING FOR CURRENT_USER SERVER \"kerno_loop\";\n"
        );
        // mysql_fdw: `username`, no database.
        let mysql = FdwForm {
            wrapper: "mysql_fdw".into(),
            ..form()
        };
        let script = fdw_script(&mysql, &pg).unwrap();
        assert!(
            script.contains("OPTIONS (host 'localhost', port '5432');")
                && script.contains("OPTIONS (username 'kerno', password 'hunter2')"),
            "{script}"
        );
        // What is missing.
        for (broken, why) in [
            (
                FdwForm {
                    server: " ".into(),
                    ..form()
                },
                "Name the foreign server.",
            ),
            (
                FdwForm {
                    port: "54x".into(),
                    ..form()
                },
                "“54x” is not a port number.",
            ),
            (
                FdwForm {
                    local_schema: String::new(),
                    ..form()
                },
                "Name both the remote schema and the local one, or neither.",
            ),
        ] {
            assert_eq!(fdw_script(&broken, &pg).unwrap_err(), why);
        }
    }
}

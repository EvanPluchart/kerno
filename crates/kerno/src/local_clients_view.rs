//! Settings ▸ Local clients (DBeaver's "Local client" preference page,
//! `DBPNativeClientLocation`): every client installed on this computer per
//! engine family, with its source, folder and version; the default of
//! each family; folders the user adds; a rescan. Backup, restore and the
//! other tool wizards run the tools of the connection's client, else of
//! the default, else of the newest found.

use std::path::PathBuf;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    PathPromptOptions, Render, SharedString, Styled as _, Task, TestSupportExt as _, Window, div,
};
use kerno_core::tools::{self, ClientFamily, ClientInstall, ClientRoots, ClientSource};

use crate::bridge;
use crate::state::AppState;

pub struct LocalClientsView {
    /// The clients found; `None` while looking.
    clients: Option<Vec<ClientInstall>>,
    error: Option<String>,
    tasks: Vec<Task<()>>,
}

impl LocalClientsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            clients: None,
            error: None,
            tasks: Vec::new(),
        };
        view.rescan(cx);
        view
    }

    /// Looks for the clients again, off the UI thread.
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        self.clients = None;
        let added = AppState::get(cx).settings.client_folders.clone();
        let detecting = bridge::spawn_blocking(cx, move || {
            tools::detect_clients(&ClientRoots::system(added))
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let clients = detecting.await;
            let _ = this.update(cx, |this, cx| {
                this.clients = Some(clients);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Asks for a folder; one that holds a client is added and listed.
    fn add_folder(&mut self, cx: &mut Context<Self>) {
        let picking = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add this client".into()),
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picking.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            let probing = cx.update(|cx| {
                let dir = dir.clone();
                bridge::spawn_blocking(cx, move || {
                    ClientFamily::ALL
                        .iter()
                        .any(|family| tools::probe_client(&dir, *family).is_some())
                })
            });
            let found = probing.await;
            let _ = this.update(cx, |this, cx| {
                if found {
                    this.error = None;
                    AppState::update_settings(cx, |settings| {
                        if !settings.client_folders.contains(&dir) {
                            settings.client_folders.push(dir.clone());
                        }
                    });
                    this.rescan(cx);
                } else {
                    this.error = Some(format!(
                        "{} holds none of pg_dump, psql, pg_restore, pg_dumpall, mysqldump, \
                         mysql, mariadb-dump or mariadb: it was not added.",
                        dir.display()
                    ));
                    cx.notify();
                }
            });
        }));
    }

    fn remove_folder(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        AppState::update_settings(cx, |settings| {
            settings.client_folders.retain(|added| {
                std::fs::canonicalize(added).unwrap_or_else(|_| added.clone()) != dir
                    && *added != dir
            });
            settings
                .default_clients
                .retain(|_, default| *default != dir);
        });
        self.rescan(cx);
    }

    fn set_default(&mut self, family: ClientFamily, dir: Option<PathBuf>, cx: &mut Context<Self>) {
        AppState::update_settings(cx, |settings| match dir {
            Some(dir) => {
                settings.default_clients.insert(family, dir);
            }
            None => {
                settings.default_clients.remove(&family);
            }
        });
        cx.notify();
    }

    fn render_family(
        &self,
        family: ClientFamily,
        clients: &[ClientInstall],
        first_index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let default = AppState::get(cx)
            .settings
            .default_clients
            .get(&family)
            .cloned();
        let installs: Vec<&ClientInstall> = clients
            .iter()
            .filter(|install| install.family == family)
            .collect();
        let newest = installs.first().map(|install| install.dir.clone());
        let in_use = default
            .clone()
            .filter(|dir| installs.iter().any(|install| install.dir == *dir))
            .or(newest);
        let unknown_default = default
            .clone()
            .filter(|dir| !installs.iter().any(|install| install.dir == *dir));
        let rows = installs.into_iter().enumerate().map(|(offset, install)| {
            let index = first_index + offset;
            let is_default = in_use.as_ref() == Some(&install.dir);
            let dir = install.dir.clone();
            let removed = install.dir.clone();
            h_flex()
                .id(("local-client", index))
                .test_support()
                .gap_2()
                .p_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(if is_default { theme.ring } else { theme.border })
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child({
                            let label = SharedString::from(format!(
                                "{}{}",
                                install.label(),
                                match (is_default, default.is_some()) {
                                    (true, true) => " · default",
                                    (true, false) => " · default (newest)",
                                    _ => "",
                                }
                            ));
                            div()
                                .id(("local-client-label", index))
                                .test_support()
                                .text_sm()
                                .child(label.clone())
                                .test_text(label)
                        })
                        .child(
                            div()
                                .id(("local-client-path", index))
                                .test_support()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(install.dir.display().to_string())),
                        ),
                )
                .child(
                    Button::new(("local-client-default", index))
                        .small()
                        .outline()
                        .label("Make default")
                        .disabled(default.as_ref() == Some(&install.dir))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_default(family, Some(dir.clone()), cx)
                        })),
                )
                .when(install.source == ClientSource::Added, |this| {
                    this.child(
                        Button::new(("local-client-remove", index))
                            .small()
                            .ghost()
                            .label("Remove")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_folder(removed.clone(), cx)
                            })),
                    )
                })
        });
        let empty = clients.iter().all(|install| install.family != family);
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::MEDIUM)
                            .child(family.label()),
                    )
                    .when(default.is_some(), |this| {
                        this.child(
                            Button::new(match family {
                                ClientFamily::Postgres => "local-clients-newest-postgres",
                                ClientFamily::MySql => "local-clients-newest-mysql",
                            })
                            .small()
                            .ghost()
                            .label("Use the newest")
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.set_default(family, None, cx)
                                }),
                            ),
                        )
                    }),
            )
            .children(rows)
            .when(empty, |this| {
                this.child(div().text_sm().text_color(theme.muted_foreground).child(
                    SharedString::from(format!(
                        "None found. It is usually installed {}.",
                        family.install_hint()
                    )),
                ))
            })
            .when_some(unknown_default, |this, dir| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(SharedString::from(format!(
                            "The default, {}, holds no client any more: the newest is used.",
                            dir.display()
                        ))),
                )
            })
            .into_any_element()
    }
}

impl Render for LocalClientsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let danger = theme.danger;
        let body = match self.clients.clone() {
            None => vec![
                div()
                    .id("local-clients-scanning")
                    .test_support()
                    .text_sm()
                    .text_color(muted)
                    .child("Looking for the installed clients…")
                    .into_any_element(),
            ],
            Some(clients) => {
                let postgres = clients
                    .iter()
                    .filter(|install| install.family == ClientFamily::Postgres)
                    .count();
                vec![
                    self.render_family(ClientFamily::Postgres, &clients, 0, cx),
                    self.render_family(ClientFamily::MySql, &clients, postgres, cx),
                ]
            }
        };
        v_flex()
            .id("local-clients")
            .test_support()
            .gap_3()
            .child(div().text_xs().text_color(muted).child(
                "The client tools backup, restore and Execute script run. Found in Homebrew \
                     (every installed version), Postgres.app, the EDB and Linux package folders \
                     and $PATH.",
            ))
            .child(
                div()
                    .id("override-local-clients")
                    .test_support()
                    .text_xs()
                    .text_color(muted)
                    .child(crate::settings_view::LOCAL_CLIENT_OVERRIDE)
                    .test_text(crate::settings_view::LOCAL_CLIENT_OVERRIDE),
            )
            .children(body)
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("local-clients-error")
                        .test_support()
                        .text_sm()
                        .text_color(danger)
                        .child(SharedString::from(error)),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("local-clients-add")
                            .small()
                            .outline()
                            .label("Add folder…")
                            .on_click(cx.listener(|this, _, _, cx| this.add_folder(cx))),
                    )
                    .child(
                        Button::new("local-clients-rescan")
                            .small()
                            .ghost()
                            .label("Rescan")
                            .disabled(self.clients.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.rescan(cx))),
                    ),
            )
    }
}

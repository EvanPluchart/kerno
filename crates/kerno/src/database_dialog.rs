//! The form that creates a database with its options (DBeaver's
//! create-database dialogs, `PostgreDatabaseManager` and
//! `MySQLDatabaseManager`): PostgreSQL's owner, template, encoding, locales
//! and tablespace; MySQL's and MariaDB's character set and collation. The
//! choices are the server's own lists; the statement is shown as it is
//! written, and runs only through the confirmation every write takes.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, TestSupportExt as _,
    Window, div, px,
};
use kerno_core::dialect::DatabaseOptions;
use kerno_core::{Engine, Session, exec};

use crate::bridge;
use crate::tabs::fit_window;

/// Runs once the form is confirmed, with the statement it wrote.
type OnCreate = dyn Fn(String, &mut Window, &mut App);

/// One option of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Owner,
    Template,
    Encoding,
    Collation,
    Ctype,
    Tablespace,
}

impl Field {
    /// The fields an engine takes, in the order they are shown.
    fn of(engine: Engine) -> &'static [Field] {
        match engine {
            Engine::Postgres => &[
                Field::Owner,
                Field::Template,
                Field::Encoding,
                Field::Collation,
                Field::Ctype,
                Field::Tablespace,
            ],
            Engine::MySql | Engine::MariaDb => &[Field::Encoding, Field::Collation],
        }
    }

    fn label(self, engine: Engine) -> &'static str {
        match (self, engine) {
            (Field::Owner, _) => "Owner",
            (Field::Template, _) => "Template",
            (Field::Encoding, Engine::Postgres) => "Encoding",
            (Field::Encoding, _) => "Character set",
            (Field::Collation, Engine::Postgres) => "Collation (LC_COLLATE)",
            (Field::Collation, _) => "Collation",
            (Field::Ctype, _) => "Character type (LC_CTYPE)",
            (Field::Tablespace, _) => "Tablespace",
        }
    }

    /// The name the server's list gives it (see
    /// [`kerno_core::Dialect::database_options_sql`]).
    fn listed_as(self) -> &'static str {
        match self {
            Field::Owner => "owner",
            Field::Template => "template",
            Field::Encoding => "encoding",
            Field::Collation | Field::Ctype => "collation",
            Field::Tablespace => "tablespace",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Field::Owner => "create-database-owner",
            Field::Template => "create-database-template",
            Field::Encoding => "create-database-encoding",
            Field::Collation => "create-database-collation",
            Field::Ctype => "create-database-ctype",
            Field::Tablespace => "create-database-tablespace",
        }
    }

    fn get(self, options: &DatabaseOptions) -> &Option<String> {
        match self {
            Field::Owner => &options.owner,
            Field::Template => &options.template,
            Field::Encoding => &options.encoding,
            Field::Collation => &options.collation,
            Field::Ctype => &options.ctype,
            Field::Tablespace => &options.tablespace,
        }
    }

    fn get_mut(self, options: &mut DatabaseOptions) -> &mut Option<String> {
        match self {
            Field::Owner => &mut options.owner,
            Field::Template => &mut options.template,
            Field::Encoding => &mut options.encoding,
            Field::Collation => &mut options.collation,
            Field::Ctype => &mut options.ctype,
            Field::Tablespace => &mut options.tablespace,
        }
    }
}

/// A value the server lists: `(field, value, encoding it applies to)`.
type Choice = (String, String, Option<String>);

/// Where the server's lists are.
enum Choices {
    Loading,
    Failed(String),
    Ready(Vec<Choice>),
}

pub struct CreateDatabaseForm {
    session: Arc<dyn Session>,
    name: Entity<InputState>,
    choices: Choices,
    options: DatabaseOptions,
    on_create: Rc<OnCreate>,
    _loading: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// Opens the form on the connection's session `session`.
pub fn open(
    session: Arc<dyn Session>,
    window: &mut Window,
    cx: &mut App,
    on_create: impl Fn(String, &mut Window, &mut App) + 'static,
) -> Entity<CreateDatabaseForm> {
    let view = cx.new(|cx| CreateDatabaseForm::new(session, Rc::new(on_create), window, cx));
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        dialog
            .title("Create database")
            .w(px(560.))
            // Enter does not create: the button does, after the preview.
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    let name = view.read(cx).name.clone();
    window.defer(cx, move |window, cx| {
        name.read(cx).focus_handle(cx).focus(window, cx)
    });
    view
}

impl CreateDatabaseForm {
    fn new(
        session: Arc<dyn Session>,
        on_create: Rc<OnCreate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("new_database"));
        let subscriptions = vec![cx.subscribe(&name, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })];
        let listing = session.dialect().database_options_sql().map(|sql| {
            let session = session.clone();
            bridge::spawn(cx, async move { exec::collect(session, sql).await })
        });
        let loading = cx.spawn(async move |this, cx| {
            let choices = match listing {
                None => Choices::Ready(Vec::new()),
                Some(listing) => match listing.await {
                    Ok(collected) => Choices::Ready(
                        collected
                            .results
                            .into_iter()
                            .next()
                            .map(|result| result.rows())
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(|row| {
                                let mut cells = row.into_iter();
                                Some((cells.next()??, cells.next()??, cells.next().flatten()))
                            })
                            .collect(),
                    ),
                    Err(error) => Choices::Failed(kerno_core::describe(&error).to_string()),
                },
            };
            let _ = this.update(cx, |this, cx| {
                this.choices = choices;
                cx.notify();
            });
        });
        Self {
            session,
            name,
            choices: Choices::Loading,
            options: DatabaseOptions::default(),
            on_create,
            _loading: loading,
            _subscriptions: subscriptions,
        }
    }

    fn engine(&self) -> Engine {
        self.session.engine()
    }

    /// What `field` can hold: the server's values, the collations of the
    /// chosen encoding only.
    fn values(&self, field: Field) -> Vec<String> {
        let Choices::Ready(choices) = &self.choices else {
            return Vec::new();
        };
        let encoding = self.options.encoding.as_deref();
        choices
            .iter()
            .filter(|(listed, _, applies_to)| {
                listed == field.listed_as()
                    && (field.listed_as() != "collation"
                        || encoding.is_none()
                        || applies_to.is_none()
                        || applies_to.as_deref().map(str::to_lowercase)
                            == encoding.map(str::to_lowercase))
            })
            .map(|(_, value, _)| value.clone())
            .collect()
    }

    /// Chooses a value for `field` (`None`: the server's default). A new
    /// encoding drops the collations that do not go with it.
    pub fn set(&mut self, field: Field, value: Option<String>, cx: &mut Context<Self>) {
        *field.get_mut(&mut self.options) = value;
        if field == Field::Encoding {
            for locale in [Field::Collation, Field::Ctype] {
                if let Some(current) = locale.get(&self.options).clone()
                    && !self.values(locale).contains(&current)
                {
                    *locale.get_mut(&mut self.options) = None;
                }
            }
        }
        cx.notify();
    }

    /// The statement as it stands, or why there is none.
    pub fn statement(&self, cx: &App) -> Result<String, String> {
        let name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return Err("Type a name.".into());
        }
        self.session
            .dialect()
            .create_database_with_sql(&name, &self.options)
            .map_err(|error| error.to_string())
    }

    /// Whether the server's lists are read; for tests.
    #[cfg(test)]
    pub fn ready(&self) -> bool {
        matches!(self.choices, Choices::Ready(_))
    }

    /// What `field` offers; for tests.
    #[cfg(test)]
    pub fn offered(&self, field: Field) -> Vec<String> {
        self.values(field)
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(sql) = self.statement(cx) else {
            return;
        };
        window.close_dialog(cx);
        // The confirmation opens once this dialog has gone.
        let on_create = self.on_create.clone();
        window.defer(cx, move |window, cx| on_create(sql, window, cx));
    }

    fn render_field(&self, field: Field, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let engine = self.engine();
        let current = field.get(&self.options).clone();
        let values = self.values(field);
        let view = cx.entity();
        let menu = move |mut menu: PopupMenu, window: &mut Window, _: &mut Context<PopupMenu>| {
            for value in std::iter::once(None).chain(values.iter().cloned().map(Some)) {
                let view = view.clone();
                let label = value.clone().unwrap_or_else(|| "Server default".into());
                menu = menu.item(
                    PopupMenuItem::new(SharedString::from(label))
                        .checked(value == current)
                        .on_click(move |_, _, cx| {
                            let value = value.clone();
                            view.update(cx, |this, cx| this.set(field, value, cx))
                        }),
                );
            }
            fit_window(menu, window)
        };
        let shown = field
            .get(&self.options)
            .clone()
            .unwrap_or_else(|| "Server default".into());
        h_flex()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .w(px(180.))
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(field.label(engine)),
            )
            .child(
                Button::new(field.id())
                    .outline()
                    .small()
                    .label(SharedString::from(shown))
                    .dropdown_caret(true)
                    .disabled(!matches!(self.choices, Choices::Ready(_)))
                    .dropdown_menu(menu),
            )
    }
}

impl Render for CreateDatabaseForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let statement = self.statement(cx);
        let status = match &self.choices {
            Choices::Loading => Some("Reading the server's roles, encodings and locales…".into()),
            // Plain words first, then the server's (C14).
            Choices::Failed(error) => Some(format!(
                "The server's lists could not be read: the database can still be made with the \
                 server's defaults.\n{error}"
            )),
            Choices::Ready(_) => None,
        };
        let fields: Vec<_> = Field::of(self.engine())
            .iter()
            .map(|&field| self.render_field(field, cx).into_any_element())
            .collect();
        let theme = cx.theme();
        let postgres = self.engine() == Engine::Postgres;
        v_flex()
            .id("create-database")
            .test_support()
            .gap_3()
            .pt_1()
            .child(div().text_sm().child(
                "A new database on this server. Leave an option on “Server default” to keep \
                 what the server would choose.",
            ))
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .flex_none()
                            .w(px(180.))
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Name"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.name).small().id("create-database-name")),
                    ),
            )
            .children(fields)
            .when(postgres, |this| {
                this.child(div().text_xs().text_color(theme.muted_foreground).child(
                    "Another encoding or locale than the template's needs template0 \
                             as the template.",
                ))
            })
            .children(status.map(|status| {
                div()
                    .id("create-database-status")
                    .test_support()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(status))
            }))
            .child(
                div()
                    .id("create-database-preview")
                    .test_support()
                    .p_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .map(|this| match &statement {
                        Ok(sql) => this.child(SharedString::from(sql.clone())),
                        Err(reason) => this
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(reason.clone())),
                    }),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("create-database-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("create-database-create")
                            .primary()
                            .label("Create…")
                            .disabled(statement.is_err())
                            .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                    ),
            )
    }
}

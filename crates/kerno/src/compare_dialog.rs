//! "Compare schemas…" (K30): the dialog that asks for the two sides of a
//! schema compare, a saved connection with its database and schema each
//! (the database alone on engines without schemas), then opens the
//! compare tab. Its connection list is the copy dialog's (J13): a click
//! picks a connection and connects it when it is not, with its password
//! asked as usual. Two engines of different families are refused here, in
//! plain words, before anything is read.

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input as TextInput, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, TestSupportExt as _, WeakEntity, Window, div, px,
};
use kerno_core::compare;

use crate::compare_tab::Sides;
use crate::connections::manager::{Connections, Status};
use crate::tabs::Target;
use crate::workspace::Workspace;

/// One side being chosen.
struct Picker {
    connection: Option<String>,
    database: Entity<InputState>,
    schema: Entity<InputState>,
}

pub struct CompareDialog {
    connections: Entity<Connections>,
    workspace: WeakEntity<Workspace>,
    /// Source, then target.
    pickers: [Picker; 2],
    _subscriptions: Vec<Subscription>,
}

/// Opens the dialog, its source set to `source` when given (a schema or a
/// database of the tree).
pub fn open(
    connections: Entity<Connections>,
    workspace: WeakEntity<Workspace>,
    source: Option<Target>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<CompareDialog> {
    let view = cx.new(|cx| CompareDialog::new(connections, workspace, source, window, cx));
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        dialog
            .title("Compare schemas")
            .w(px(820.))
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    view
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

impl CompareDialog {
    fn new(
        connections: Entity<Connections>,
        workspace: WeakEntity<Workspace>,
        source: Option<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut picker = |cx: &mut Context<Self>| Picker {
            connection: None,
            database: cx.new(|cx| InputState::new(window, cx).placeholder("database")),
            schema: cx.new(|cx| InputState::new(window, cx).placeholder("schema")),
        };
        let pickers = [picker(cx), picker(cx)];
        let mut subscriptions: Vec<Subscription> = pickers
            .iter()
            .flat_map(|picker| [&picker.database, &picker.schema])
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        // A connection that connects fills what its session knows.
        subscriptions.push(cx.observe_in(&connections, window, |this, _, window, cx| {
            for index in 0..2 {
                this.fill_from_session(index, window, cx);
            }
            cx.notify();
        }));
        let mut dialog = Self {
            connections,
            workspace,
            pickers,
            _subscriptions: subscriptions,
        };
        if let Some(source) = source {
            dialog.choose(0, source.connection.clone(), window, cx);
            let picker = &dialog.pickers[0];
            if let Some(database) = source.database {
                picker
                    .database
                    .update(cx, |input, cx| input.set_value(database, window, cx));
            }
            if let Some(schema) = source.schema {
                picker
                    .schema
                    .update(cx, |input, cx| input.set_value(schema, window, cx));
            }
        }
        dialog
    }

    /// Whether the side's connection has schemas; `None` before it is open.
    fn has_schemas(&self, index: usize, cx: &App) -> Option<bool> {
        let id = self.pickers[index].connection.as_ref()?;
        Some(
            self.connections
                .read(cx)
                .meta_session(id)?
                .dialect()
                .has_schemas(),
        )
    }

    /// Empty fields take what the connection's session says: its database,
    /// its current schema.
    fn fill_from_session(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.pickers[index].connection.clone() else {
            return;
        };
        let Some(meta) = self.connections.read(cx).meta_session(&id) else {
            return;
        };
        let picker = &self.pickers[index];
        if value(&picker.database, cx).is_empty()
            && let Some(database) = meta.database()
        {
            picker
                .database
                .update(cx, |input, cx| input.set_value(database, window, cx));
        }
        if value(&picker.schema, cx).is_empty()
            && let Some(schema) = meta.schema()
        {
            picker
                .schema
                .update(cx, |input, cx| input.set_value(schema, window, cx));
        }
    }

    /// Picks the connection of the side `index` (0: source), and connects
    /// it when it is not.
    pub fn choose(
        &mut self,
        index: usize,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(saved) = self.connections.read(cx).get(&id) else {
            return;
        };
        let database = saved.config.database.clone().unwrap_or_default();
        let picker = &mut self.pickers[index];
        picker.connection = Some(id.clone());
        picker
            .database
            .update(cx, |input, cx| input.set_value(database, window, cx));
        picker
            .schema
            .update(cx, |input, cx| input.set_value("", window, cx));
        if matches!(
            self.connections.read(cx).status(&id),
            Status::Disconnected | Status::Failed { .. }
        ) {
            self.connections
                .update(cx, |connections, cx| connections.connect(&id, cx));
        }
        self.fill_from_session(index, window, cx);
        cx.notify();
    }

    /// Exchanges the source and the target.
    pub fn swap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values: Vec<(Option<String>, String, String)> = self
            .pickers
            .iter()
            .map(|picker| {
                (
                    picker.connection.clone(),
                    value(&picker.database, cx),
                    value(&picker.schema, cx),
                )
            })
            .collect();
        for (picker, (connection, database, schema)) in
            self.pickers.iter_mut().zip(values.into_iter().rev())
        {
            picker.connection = connection;
            picker
                .database
                .update(cx, |input, cx| input.set_value(database, window, cx));
            picker
                .schema
                .update(cx, |input, cx| input.set_value(schema, window, cx));
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn set_fields(
        &mut self,
        index: usize,
        database: &str,
        schema: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let picker = &self.pickers[index];
        picker.database.update(cx, |input, cx| {
            input.set_value(database.to_string(), window, cx)
        });
        picker.schema.update(cx, |input, cx| {
            input.set_value(schema.to_string(), window, cx)
        });
        cx.notify();
    }

    /// The side `index` as chosen, if it is complete.
    fn side(&self, index: usize, cx: &App) -> Option<Target> {
        let picker = &self.pickers[index];
        let connection = picker.connection.clone()?;
        let database = Some(value(&picker.database, cx)).filter(|name| !name.is_empty());
        let schema = Some(value(&picker.schema, cx)).filter(|name| !name.is_empty());
        let schema = match self.has_schemas(index, cx) {
            Some(false) => None,
            _ => Some(schema?),
        };
        database.as_ref()?;
        Some(Target {
            connection,
            database,
            schema,
        })
    }

    /// Why Compare cannot be clicked yet, in plain words.
    pub fn problem(&self, cx: &App) -> Option<String> {
        let manager = self.connections.read(cx);
        let engine = |index: usize| {
            let id = self.pickers[index].connection.as_ref()?;
            Some(manager.get(id)?.config.engine)
        };
        let (source, target) = match (engine(0), engine(1)) {
            (Some(source), Some(target)) => (source, target),
            (Some(_), None) => return Some("Pick the target connection.".into()),
            (None, Some(_)) => return Some("Pick the source connection.".into()),
            (None, None) => {
                return Some("Pick the connection of the source and of the target.".into());
            }
        };
        if let Some(refusal) = compare::refusal(source, target) {
            return Some(refusal);
        }
        for (index, role) in [(0, "source"), (1, "target")] {
            let id = self.pickers[index]
                .connection
                .as_deref()
                .unwrap_or_default();
            match manager.status(id) {
                Status::Connected { .. } => {}
                Status::Failed { .. } => {
                    return Some(format!(
                        "The {role} connection could not connect: pick it again to retry."
                    ));
                }
                _ => return Some(format!("Connecting the {role}…")),
            }
            if self.side(index, cx).is_none() {
                return Some(match self.has_schemas(index, cx) {
                    Some(false) => format!("Name the {role} database."),
                    _ => format!("Name the {role} database and schema."),
                });
            }
        }
        if self.side(0, cx) == self.side(1, cx) {
            return Some("The source and the target are the same schema.".into());
        }
        None
    }

    /// Opens the compare tab and closes the dialog.
    pub fn compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.problem(cx).is_some() {
            return;
        }
        let (Some(source), Some(target)) = (self.side(0, cx), self.side(1, cx)) else {
            return;
        };
        window.close_dialog(cx);
        let sides = Sides { source, target };
        let _ = self.workspace.update(cx, |workspace, cx| {
            workspace.open_compare(sides, window, cx);
        });
    }

    fn render_picker(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let manager = self.connections.read(cx);
        let picker = &self.pickers[index];
        let (heading, hint) = if index == 0 {
            ("Source", "the model")
        } else {
            ("Target", "what the script changes")
        };
        let list = v_flex()
            .id(("compare-connections", index))
            .h(px(180.))
            .overflow_y_scroll()
            .p_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .children(manager.saved().iter().enumerate().map(|(row, saved)| {
                let config = &saved.config;
                let status = match manager.status(&saved.id) {
                    Status::Connected { .. } => "connected",
                    Status::Connecting | Status::WaitingForInput => "connecting…",
                    Status::Failed { .. } => "failed to connect",
                    Status::Disconnected => "not connected",
                };
                let mut details = vec![config.engine.label(), status];
                if config.read_only {
                    details.push("read-only");
                }
                let picked = picker.connection.as_deref() == Some(saved.id.as_str());
                let id = saved.id.clone();
                h_flex()
                    .id(("compare-connection", index * 1000 + row))
                    .test_support()
                    .test_text(config.name.clone())
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .when(picked, |this| this.bg(theme.accent))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(SharedString::from(config.name.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(details.join(" · "))),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose(index, id.clone(), window, cx)
                    }))
            }));
        let field = |label: &'static str, id: (&'static str, usize), input: &Entity<InputState>| {
            h_flex()
                .gap_2()
                .child(
                    div()
                        .w(px(80.))
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(div().flex_1().child(TextInput::new(input).small().id(id)))
        };
        let schemas = self.has_schemas(index, cx) != Some(false);
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(heading))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(hint),
                    ),
            )
            .child(list)
            .child(field(
                "Database",
                ("compare-database", index),
                &picker.database,
            ))
            .when(schemas, |this| {
                this.child(field("Schema", ("compare-schema", index), &picker.schema))
            })
            .into_any_element()
    }
}

impl Render for CompareDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let problem = self.problem(cx);
        let muted = theme.muted_foreground;
        let source = self.render_picker(0, cx);
        let target = self.render_picker(1, cx);
        v_flex()
            .gap_3()
            .child(div().text_sm().text_color(muted).whitespace_normal().child(
                "Compares the structure of two schemas: tables, keys, indexes, views, \
                         routines, sequences, types and triggers. Rows are not read. The script \
                         brings the target to the source; it opens in a query tab and nothing \
                         runs by itself.",
            ))
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(source)
                    .child(
                        div().pt(px(90.)).child(
                            Button::new("compare-dialog-swap")
                                .ghost()
                                .small()
                                .label("⇄")
                                .tooltip("Exchange the source and the target")
                                .on_click(cx.listener(|this, _, window, cx| this.swap(window, cx))),
                        ),
                    )
                    .child(target),
            )
            .when_some(problem.clone(), |this, problem| {
                this.child(
                    div()
                        .id("compare-problem")
                        .test_support()
                        .text_sm()
                        .whitespace_normal()
                        .text_color(muted)
                        .test_text(problem.clone())
                        .child(SharedString::from(problem)),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("compare-dialog-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("compare-dialog-start")
                            .primary()
                            .label("Compare")
                            .disabled(problem.is_some())
                            .on_click(cx.listener(|this, _, window, cx| this.compare(window, cx))),
                    ),
            )
    }
}

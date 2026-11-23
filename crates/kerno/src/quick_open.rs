//! Open Table (Cmd+P, DBeaver's Open Database Object): the tables and views
//! of a connection, found by a few letters of `schema.name`. Enter opens the
//! rows, Cmd+Enter the structure. Also the SQL scripts of a connection
//! (K38), found the same way.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::actions::Confirm;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::{
    App, AppContext as _, Entity, InteractiveElement as _, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
};
use kerno_core::Session;
use kerno_core::browse::Filter;
use kerno_core::schema::{RelationKind, TableRef};
use kerno_core::sql::Catalog;

use crate::bridge;
use crate::sidebar::Script;
use crate::tabs::Target;
use crate::workspace::Workspace;

/// A table or view the dialog lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub table: TableRef,
    pub kind: RelationKind,
}

impl Relation {
    fn qualified(&self) -> String {
        format!("{}.{}", self.table.schema, self.table.name)
    }
}

/// Every table and view of the catalog, schemas of the search path first.
pub fn relations(catalog: &Catalog) -> Vec<Relation> {
    let mut schemas: Vec<_> = catalog.schemas.iter().collect();
    schemas.sort_by_key(|schema| {
        catalog
            .search_path
            .iter()
            .position(|path| *path == schema.name)
            .unwrap_or(usize::MAX)
    });
    schemas
        .into_iter()
        .flat_map(|schema| {
            schema.relations.iter().map(|relation| Relation {
                table: TableRef::new(&schema.name, &relation.name),
                kind: relation.kind,
            })
        })
        .collect()
}

/// The relations matching `query`, best first: the name itself, then names
/// that start with it, then names and `schema.name` that hold it; only when
/// none holds it, those whose letters come in its order (`shcu` finds
/// `shop.customers`).
pub fn ranked<'a>(relations: &'a [Relation], query: &str) -> Vec<&'a Relation> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return relations.iter().collect();
    }
    let in_order = |text: &str| {
        let mut letters = text.chars();
        query.chars().all(|wanted| letters.any(|c| c == wanted))
    };
    let mut scored: Vec<(u8, usize, &Relation)> = relations
        .iter()
        .enumerate()
        .filter_map(|(index, relation)| {
            let name = relation.table.name.to_lowercase();
            let qualified = relation.qualified().to_lowercase();
            let score = if name == query || qualified == query {
                0
            } else if name.starts_with(&query) || qualified.starts_with(&query) {
                1
            } else if qualified.contains(&query) {
                2
            } else if in_order(&qualified) {
                3
            } else {
                return None;
            };
            Some((score, index, relation))
        })
        .collect();
    scored.sort_by_key(|(score, index, _)| (*score, *index));
    // Scattered letters only when no name holds the text itself.
    let contained = scored.iter().any(|(score, _, _)| *score < 3);
    scored
        .into_iter()
        .filter(|(score, _, _)| !contained || *score < 3)
        .map(|(_, _, relation)| relation)
        .collect()
}

/// Opens the dialog on the relations of `connection`, read through its
/// own session.
pub fn open(
    workspace: Entity<Workspace>,
    connection: String,
    session: Arc<dyn Session>,
    window: &mut Window,
    cx: &mut App,
) {
    let state = cx.new(|cx| CommandState::new(window, cx));
    // `None` while the catalog is read.
    let listed: Rc<RefCell<Option<Vec<Relation>>>> = Rc::default();
    let loading = bridge::spawn(cx, async move { session.completion_catalog().await });
    let filled = listed.clone();
    window
        .spawn(cx, async move |cx| {
            let catalog = loading.await;
            let _ = cx.update(|window, cx| {
                *filled.borrow_mut() = Some(match catalog {
                    Ok(catalog) => relations(&catalog),
                    Err(error) => {
                        window.push_notification(
                            crate::error_notification(format!(
                                "The tables could not be listed: {}",
                                kerno_core::describe(&error)
                            )),
                            cx,
                        );
                        Vec::new()
                    }
                });
                window.refresh();
            });
        })
        .detach();

    let target = Target {
        connection,
        database: None,
        schema: None,
    };
    // Opens the relation at `row` of the list for what is typed.
    let choose = Rc::new({
        let (state, listed) = (state.clone(), listed.clone());
        move |row: usize, structure: bool, window: &mut Window, cx: &mut App| {
            let query = state.read(cx).query(cx);
            let chosen = listed
                .borrow()
                .as_deref()
                .and_then(|all| ranked(all, &query).get(row).cloned().cloned());
            let Some(Relation { table, kind }) = chosen else {
                return;
            };
            window.close_dialog(cx);
            let (workspace, target) = (workspace.clone(), target.clone());
            window.defer(cx, move |window, cx| {
                workspace.update(cx, |workspace, cx| {
                    if structure {
                        workspace.open_structure(target, table, kind, window, cx);
                    } else {
                        workspace.open_data(target, table, kind, Filter::default(), window, cx);
                    }
                });
            });
        }
    });
    let focused = Rc::new(std::cell::Cell::new(false));
    window.open_dialog(cx, move |dialog, _, _| {
        let (state, listed, choose, focused) = (
            state.clone(),
            listed.clone(),
            choose.clone(),
            focused.clone(),
        );
        dialog
            .close_button(false)
            .p_0()
            .content(move |content, window, cx| {
                if !focused.replace(true) {
                    let state = state.clone();
                    window.defer(cx, move |window, cx| {
                        gpui_kit::Focusable::focus_handle(state.read(cx), cx).focus(window, cx)
                    });
                }
                let query = state.read(cx).query(cx);
                let muted = cx.theme().muted_foreground;
                let confirm = choose.clone();
                let mut command = Command::new(&state)
                    .bordered(false)
                    .rounded(cx.theme().radius_lg)
                    .filterable(false)
                    .placeholder("Type a table or view name")
                    .max_h(px(420.))
                    .on_confirm(move |index, window, cx| confirm(index.row, false, window, cx));
                let hint = match listed.borrow().as_deref() {
                    None => "Reading the tables…",
                    Some([]) => "This connection has no table or view.",
                    Some(all) => {
                        command = command.items(
                            ranked(all, &query)
                                .into_iter()
                                .map(|relation| CommandItem::new().label(relation.qualified())),
                        );
                        "Enter opens the rows, Cmd+Enter the structure."
                    }
                };
                let structure = choose.clone();
                let state = state.clone();
                content.child(
                    v_flex()
                        .id("quick-open")
                        .test_support()
                        .key_context("QuickOpen")
                        // Cmd+Enter (bound in `actions::bind_keys`): the structure.
                        .capture_action(move |confirm: &Confirm, window, cx| {
                            if !confirm.secondary {
                                return;
                            }
                            cx.stop_propagation();
                            if let Some(index) = state.read(cx).selected_index() {
                                structure(index.row, true, window, cx);
                            }
                        })
                        .child(command)
                        .child(
                            div()
                                .id("quick-open-hint")
                                .test_support()
                                .aria_label(hint)
                                .px_3()
                                .py_2()
                                .text_xs()
                                .text_color(muted)
                                .child(hint),
                        ),
                )
            })
    });
}

/// A group of the scripts picker: its heading, and each script with its label.
type Listed = (&'static str, Vec<(String, Script)>);

/// Every SQL script of the connection `connection` (K38: what its SQL
/// scripts ▸ lists, without the menu's limit), found by a few letters;
/// Enter opens it on the connection, as the menu does.
pub fn open_scripts(
    workspace: Entity<Workspace>,
    connection: String,
    window: &mut Window,
    cx: &mut App,
) {
    let (saved, files) = crate::sidebar::scripts_of(&connection, cx);
    let saved: Vec<(String, Script)> = (saved.into_iter())
        .map(|query| {
            let label = match query.folder.is_empty() {
                true => query.name.clone(),
                false => format!("{} / {}", query.folder, query.name),
            };
            let (id, name) = (query.id, query.name);
            (label, Script::Saved { id, name })
        })
        .collect();
    // Each file with its folder, as the toolbar's recent list shows them.
    let files: Vec<(String, Script)> = (files.iter().zip(crate::sidebar::file_names(&files)))
        .map(|(path, name)| {
            let (file, folder) = (path.file_name(), path.parent());
            let label = format!(
                "{}  {}",
                file.map_or_else(|| name.clone(), |file| file.to_string_lossy().into_owned()),
                folder
                    .map(|folder| folder.display().to_string())
                    .unwrap_or_default()
            );
            let path = path.clone();
            (label, Script::File { path, name })
        })
        .collect();
    let groups: Rc<Vec<Listed>> = Rc::new(
        [("Saved queries", saved), ("Files", files)]
            .into_iter()
            .filter(|(_, scripts)| !scripts.is_empty())
            .collect(),
    );
    let state = cx.new(|cx| CommandState::new(window, cx));
    let focused = Rc::new(std::cell::Cell::new(false));
    window.open_dialog(cx, move |dialog, _, _| {
        let (state, groups, focused) = (state.clone(), groups.clone(), focused.clone());
        let (workspace, connection) = (workspace.clone(), connection.clone());
        dialog
            .close_button(false)
            .p_0()
            .content(move |content, window, cx| {
                if !focused.replace(true) {
                    let state = state.clone();
                    window.defer(cx, move |window, cx| {
                        gpui_kit::Focusable::focus_handle(state.read(cx), cx).focus(window, cx)
                    });
                }
                let chosen = groups.clone();
                let (workspace, connection) = (workspace.clone(), connection.clone());
                let mut command = Command::new(&state)
                    .bordered(false)
                    .rounded(cx.theme().radius_lg)
                    .placeholder("Type a script's name")
                    .max_h(px(420.))
                    .on_confirm(move |index, window, cx| {
                        let Some((_, script)) = (chosen.get(index.section))
                            .and_then(|(_, scripts)| scripts.get(index.row))
                            .cloned()
                        else {
                            return;
                        };
                        window.close_dialog(cx);
                        let (workspace, connection) = (workspace.clone(), connection.clone());
                        window.defer(cx, move |window, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.open_script(&connection, script, window, cx)
                            });
                        });
                    });
                for (label, scripts) in groups.iter() {
                    command = command.group(CommandGroup::new().label(*label).items(
                        (scripts.iter()).map(|(label, _)| CommandItem::new().label(label.clone())),
                    ));
                }
                let hint = match groups.is_empty() {
                    true => {
                        "No SQL scripts yet: open a .sql file on this connection, or save a \
                             query of it, and it is listed here."
                    }
                    false => "Enter opens it on this connection.",
                };
                let muted = cx.theme().muted_foreground;
                content.child(
                    v_flex()
                        .id("connection-scripts")
                        .test_support()
                        .child(command)
                        .child(
                            div()
                                .id("connection-scripts-hint")
                                .test_support()
                                .aria_label(hint)
                                .px_3()
                                .py_2()
                                .text_xs()
                                .text_color(muted)
                                .child(hint),
                        ),
                )
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::sql::{CatalogRelation, CatalogSchema};

    fn relation(name: &str, kind: RelationKind) -> CatalogRelation {
        CatalogRelation {
            name: name.into(),
            kind,
            columns: Vec::new(),
            foreign_keys: Vec::new(),
        }
    }

    #[test]
    fn the_search_path_comes_first_and_matches_rank() {
        let catalog = Catalog {
            schemas: vec![
                CatalogSchema {
                    name: "analytics".into(),
                    relations: vec![relation("orders_daily", RelationKind::View)],
                    routines: Vec::new(),
                },
                CatalogSchema {
                    name: "shop".into(),
                    relations: vec![
                        relation("customers", RelationKind::Table),
                        relation("orders", RelationKind::Table),
                    ],
                    routines: Vec::new(),
                },
            ],
            search_path: vec!["shop".into()],
        };
        let all = relations(&catalog);
        let names: Vec<String> = all.iter().map(Relation::qualified).collect();
        assert_eq!(
            names,
            ["shop.customers", "shop.orders", "analytics.orders_daily"]
        );
        let found = |query: &str| -> Vec<String> {
            ranked(&all, query)
                .into_iter()
                .map(Relation::qualified)
                .collect()
        };
        assert_eq!(found("orders"), ["shop.orders", "analytics.orders_daily"]);
        assert_eq!(found("ORD"), ["shop.orders", "analytics.orders_daily"]);
        assert_eq!(found("shop.c"), ["shop.customers"]);
        assert_eq!(found("shcu"), ["shop.customers"]);
        assert_eq!(found("daily"), ["analytics.orders_daily"]);
        assert_eq!(found("zzz"), Vec::<String>::new());
        assert_eq!(found("").len(), 3);
    }

    #[test]
    fn scattered_letters_match_only_when_no_name_holds_the_text() {
        let catalog = Catalog {
            schemas: vec![CatalogSchema {
                name: "shop".into(),
                relations: vec![
                    relation("orders", RelationKind::Table),
                    relation("product_sales", RelationKind::View),
                    relation("products", RelationKind::Table),
                ],
                routines: Vec::new(),
            }],
            search_path: vec!["shop".into()],
        };
        let all = relations(&catalog);
        let found = |query: &str| -> Vec<String> {
            ranked(&all, query)
                .into_iter()
                .map(Relation::qualified)
                .collect()
        };
        // `shop.product_sales` and `shop.products` hold o, r, d in order.
        assert_eq!(found("ord"), ["shop.orders"]);
        assert_eq!(found("prsa"), ["shop.product_sales"]);
    }
}

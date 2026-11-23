//! Schema-aware completion in the query editor: the UI kit's completion
//! provider over [`kerno_core::sql::complete`].

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::input;
use gpui_kit::component::input::{CompletionProvider, EditorState, Rope, RopeExt as _, TabSize};
use gpui_kit::{App, Context, Entity, Task, Window};
use kerno_core::Session;
use kerno_core::schema::{RelationKind, TableRef};
use kerno_core::sql::{Catalog, CatalogColumn, CatalogRelation, CatalogSchema, complete};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionResponse, CompletionTextEdit, Range, TextEdit,
};

/// What a query tab knows of its database: loaded by the tab, read by the
/// provider at every keystroke.
pub type SharedCatalog = Rc<RefCell<Arc<Catalog>>>;

/// Proposes schemas, tables, columns, joins and keywords at the cursor.
pub struct SqlCompletion {
    pub catalog: SharedCatalog,
    /// The connection's session, for its dialect: how the text is lexed
    /// and names are quoted.
    pub session: Arc<dyn Session>,
    /// The statement the text goes on: empty in the editor, `SELECT * FROM
    /// t WHERE ` in a condition field (K40).
    pub prefix: String,
}

/// What a condition field offers no proposal of: the rest of a statement.
const NOT_A_CONDITION: [&str; 4] = ["GROUP BY", "ORDER BY", "LIMIT", "RETURNING"];

impl SqlCompletion {
    /// The proposals at byte `offset` of `text`.
    pub fn items(&self, text: &Rope, offset: usize) -> Vec<CompletionItem> {
        let at = self.prefix.len();
        let sql = format!("{}{text}", self.prefix);
        let offset = offset + at;
        let dialect = self.session.dialect();
        let catalog = self.catalog.borrow();
        let mut completion = complete(&sql, offset, &catalog, dialect.lex_rules(), &|name| {
            dialect.quote_ident(name)
        });
        if at > 0 {
            completion
                .items
                .retain(|item| !NOT_A_CONDITION.contains(&item.label.as_str()));
        }
        if completion.replace.start < at {
            return Vec::new();
        }
        let word = &sql[completion.replace.clone()];
        // A word typed in full (`join`, `orders`) needs no list: Enter goes
        // to the editor. One more letter brings the list back.
        if completion
            .items
            .first()
            .is_some_and(|best| best.insert.eq_ignore_ascii_case(word))
        {
            return Vec::new();
        }
        // What was typed of the word, for the highlighter.
        let typed = sql[completion.replace.start..offset.max(completion.replace.start)]
            .trim_start_matches(['"', '`']);
        let range = Range::new(
            text.offset_to_position(completion.replace.start - at),
            text.offset_to_position(completion.replace.end - at),
        );
        completion
            .items
            .into_iter()
            .map(|item| CompletionItem {
                label: item.label,
                detail: item.detail,
                filter_text: Some(typed.to_string()),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit::new(range, item.insert))),
                ..Default::default()
            })
            .collect()
    }
}

impl CompletionProvider for SqlCompletion {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        Task::ready(Ok(CompletionResponse::Array(self.items(text, offset))))
    }

    /// The list opens while a word is typed and after a dot, unless the
    /// settings turn that off. Any edit
    /// closes it first, so a space or a parenthesis leaves it closed.
    fn is_completion_trigger(&self, _: usize, new_text: &str, cx: &mut App) -> bool {
        // Off in the settings, the list opens only when asked (Ctrl+Space).
        crate::state::AppState::get(cx).settings.auto_completion
            && new_text
                .chars()
                .last()
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '"' | '`'))
    }
}

/// Paints the part of a label that matches what was typed (the kit's
/// query is everything typed since the list opened, dots included).
pub fn highlight(_: &str, item: &CompletionItem) -> Vec<std::ops::Range<usize>> {
    gpui_kit::component::input::highlight_contains(
        item.filter_text.as_deref().unwrap_or_default(),
        item,
    )
}

/// Completion in a field that holds a condition on `table`, whose columns
/// and their types are `columns` (K40): what the editor proposes after
/// `SELECT * FROM table WHERE`.
pub fn condition_completion(
    session: Arc<dyn Session>,
    table: &TableRef,
    columns: Vec<CatalogColumn>,
) -> Rc<dyn CompletionProvider> {
    let dialect = session.dialect();
    let prefix = format!(
        "SELECT * FROM {}.{} WHERE ",
        dialect.quote_ident(&table.schema),
        dialect.quote_ident(&table.name)
    );
    let catalog = Catalog {
        schemas: vec![CatalogSchema {
            name: table.schema.clone(),
            relations: vec![CatalogRelation {
                name: table.name.clone(),
                kind: RelationKind::Table,
                columns,
                foreign_keys: Vec::new(),
            }],
            routines: Vec::new(),
        }],
        search_path: vec![table.schema.clone()],
    };
    Rc::new(SqlCompletion {
        catalog: Rc::new(RefCell::new(Arc::new(catalog))),
        session,
        prefix,
    })
}

/// A one-line field for a SQL condition (K40): the kit's plain input has no
/// completion, its editor has; Enter applies (`PressEnter`) unless the list
/// is open.
pub fn condition_field(
    placeholder: &'static str,
    value: String,
    window: &mut Window,
    cx: &mut Context<EditorState>,
) -> EditorState {
    let mut field = EditorState::new(window, cx)
        .language("sql")
        .line_number(false)
        .searchable(false)
        .soft_wrap(false)
        .submit_on_enter(true)
        .tab_size(TabSize {
            tab_size: 2,
            hard_tabs: false,
        })
        .placeholder(placeholder)
        .default_value(value);
    field
        .lsp_mut()
        .set_completion_highlighter(Rc::new(highlight));
    field
}

/// Tab on `editor` (J1): accepts the highlighted completion when the list
/// is open and the setting says so; returns whether it did.
pub fn tab_accepts(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) -> bool {
    if !crate::state::AppState::get(cx)
        .settings
        .tab_accepts_completion
        || !editor.read(cx).completion_menu_state().open
    {
        return false;
    }
    let accept = Box::new(input::Enter {
        secondary: false,
        shift: false,
    });
    editor.update(cx, |editor, cx| {
        editor.route_overlay_action(accept, window, cx)
    })
}

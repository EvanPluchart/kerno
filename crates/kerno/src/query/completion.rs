//! Schema-aware completion in the query editor: the UI kit's completion
//! provider over [`kerno_core::sql::complete`].

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::input::{CompletionProvider, Rope, RopeExt as _};
use gpui_kit::{App, Task, Window};
use kerno_core::Session;
use kerno_core::sql::{Catalog, complete};
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
}

impl SqlCompletion {
    /// The proposals at byte `offset` of `text`.
    pub fn items(&self, text: &Rope, offset: usize) -> Vec<CompletionItem> {
        let sql = text.to_string();
        let dialect = self.session.dialect();
        let catalog = self.catalog.borrow();
        let completion = complete(&sql, offset, &catalog, dialect.lex_rules(), &|name| {
            dialect.quote_ident(name)
        });
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
            text.offset_to_position(completion.replace.start),
            text.offset_to_position(completion.replace.end),
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

    /// The list opens while a word is typed and after a dot. Any edit
    /// closes it first, so a space or a parenthesis leaves it closed.
    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        new_text
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

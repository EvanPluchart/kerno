//! Tabs of the central area and what is remembered of them between runs.

use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Window, div,
};
use serde::{Deserialize, Serialize};

/// What is stored of an open tab, so it comes back after a restart with
/// its unsaved text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TabSnapshot {
    /// A query tab.
    Query {
        /// Tab title.
        title: String,
        /// Editor text, saved or not.
        sql: String,
    },
}

/// A tab holding a SQL editor.
pub struct QueryTab {
    title: SharedString,
    editor: Entity<EditorState>,
}

impl QueryTab {
    /// A query tab titled `title` with `sql` in its editor.
    pub fn new(
        title: impl Into<SharedString>,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sql = sql.to_string();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .soft_wrap(false)
                .default_value(sql)
                .placeholder("Write SQL here")
        });
        Self {
            title: title.into(),
            editor,
        }
    }

    /// The tab's title.
    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// The editor, to observe its changes.
    pub fn editor(&self) -> &Entity<EditorState> {
        &self.editor
    }

    /// The editor's text.
    pub fn sql(&self, cx: &App) -> String {
        self.editor.read(cx).text().to_string()
    }

    /// What to store of this tab.
    pub fn snapshot(&self, cx: &App) -> TabSnapshot {
        TabSnapshot::Query {
            title: self.title.to_string(),
            sql: self.sql(cx),
        }
    }
}

impl Focusable for QueryTab {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl Render for QueryTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            Editor::new(&self.editor)
                .h_full()
                .bordered(false)
                .appearance(false),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_are_tagged_by_kind() {
        let snapshot = TabSnapshot::Query {
            title: "Query 1".into(),
            sql: "select 'unsaved'\n".into(),
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"query","title":"Query 1","sql":"select 'unsaved'\n"}"#
        );
        assert_eq!(
            serde_json::from_str::<TabSnapshot>(&json).unwrap(),
            snapshot
        );
        // A kind this version does not know is rejected, and the storage
        // layer then skips that tab instead of failing to start.
        assert!(serde_json::from_str::<TabSnapshot>(r#"{"kind":"from_the_future"}"#).is_err());
    }
}

//! The transaction log of a query tab in manual commit (DBeaver's
//! `core.txn.log`): the statements run since its transaction began, with
//! when, how long, how many rows and how each ended. Kept in memory only,
//! with what the history keeps: passwords taken out of the text, and no
//! error message for a statement that held one.

use std::time::Duration;

use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px,
};
use kerno_core::sql::LexRules;
use kerno_core::storage::{ERROR_WITHHELD, HistoryStatus, now_ms, redact_secrets};

use super::tab::duration_text;
use crate::library::ago;

/// One statement of the open transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxLogEntry {
    /// The statement, passwords taken out.
    pub sql: String,
    pub started_at_ms: i64,
    pub elapsed: Duration,
    /// Rows changed, for the statements that change rows.
    pub rows: Option<u64>,
    pub status: HistoryStatus,
    pub error: Option<String>,
}

impl TxLogEntry {
    /// The entry of a statement as it ran, redacted as the history is.
    pub fn new(
        sql: &str,
        rules: LexRules,
        started_at_ms: i64,
        elapsed: Duration,
        rows: Option<u64>,
        status: HistoryStatus,
        error: Option<String>,
    ) -> Self {
        let redacted = redact_secrets(sql, rules);
        let error = match error {
            Some(_) if redacted != sql => Some(ERROR_WITHHELD.to_string()),
            error => error,
        };
        Self {
            sql: redacted,
            started_at_ms,
            elapsed,
            rows,
            status,
            error,
        }
    }

    /// How it ended, in a word or a count.
    pub fn outcome(&self) -> String {
        match (self.status, self.rows) {
            (HistoryStatus::Ok, Some(rows)) => {
                format!("{rows} row{}", if rows == 1 { "" } else { "s" })
            }
            (HistoryStatus::Ok, None) => "done".into(),
            (HistoryStatus::Error, _) => "failed".into(),
            (HistoryStatus::Cancelled, _) => "cancelled".into(),
        }
    }
}

/// Which transaction a log is of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// The open one.
    Open,
    /// The one the server rolled back when it closed the session (C15).
    Lost,
    /// None is open.
    None,
}

/// Shows `entries`, the log of a transaction.
pub fn open(entries: Vec<TxLogEntry>, shown: Shown, window: &mut Window, cx: &mut App) {
    let open = shown == Shown::Open;
    window.open_dialog(cx, move |dialog, _, cx| {
        let theme = cx.theme();
        let (muted, danger, mono) = (
            theme.muted_foreground,
            theme.danger,
            theme.mono_font_family.clone(),
        );
        let now = now_ms();
        let body = if entries.is_empty() {
            div()
                .id("tx-log-empty")
                .test_support()
                .py_4()
                .text_sm()
                .text_color(muted)
                .child(if open {
                    "No statement has run in this transaction yet: they are listed here as \
                     they run, until Commit or Rollback."
                } else {
                    "No transaction is open. In manual commit mode the first statement opens \
                     one; its statements are listed here until Commit or Rollback."
                })
                .into_any_element()
        } else {
            v_flex()
                .id("tx-log")
                .test_support()
                .max_h(px(420.))
                .overflow_y_scroll()
                .gap_1()
                .children(entries.iter().enumerate().map(|(index, entry)| {
                    v_flex()
                        .id(("tx-log-entry", index))
                        .test_support()
                        .py_1()
                        .border_b_1()
                        .border_color(theme.border)
                        .child(
                            h_flex()
                                .gap_3()
                                .text_xs()
                                .text_color(muted)
                                .child(SharedString::from(ago(now, entry.started_at_ms)))
                                .child(SharedString::from(duration_text(entry.elapsed)))
                                .child(
                                    div()
                                        .when(entry.status != HistoryStatus::Ok, |this| {
                                            this.text_color(danger)
                                        })
                                        .child(SharedString::from(entry.outcome())),
                                ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_family(mono.clone())
                                .child(SharedString::from(entry.sql.clone())),
                        )
                        .children(entry.error.clone().map(|error| {
                            div()
                                .text_xs()
                                .text_color(danger)
                                .child(SharedString::from(error))
                        }))
                }))
                .into_any_element()
        };
        let lost = (shown == Shown::Lost).then(|| {
            div()
                .id("tx-log-lost")
                .test_support()
                .pb_2()
                .text_sm()
                .text_color(danger)
                .child(
                    "The server closed the session: it rolled these statements back, and \
                     nothing of them was committed.",
                )
        });
        dialog
            .title("Transaction log")
            .w(px(640.))
            .children(lost)
            .child(body)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_keeps_what_the_history_keeps() {
        let entry = TxLogEntry::new(
            "ALTER USER app PASSWORD 'hunter2'",
            LexRules::POSTGRES,
            0,
            Duration::from_millis(3),
            None,
            HistoryStatus::Error,
            Some("syntax error near 'hunter2'".into()),
        );
        assert!(!entry.sql.contains("hunter2"), "{}", entry.sql);
        assert_eq!(entry.error.as_deref(), Some(ERROR_WITHHELD));
        assert_eq!(entry.outcome(), "failed");
        let update = TxLogEntry::new(
            "UPDATE t SET a = 1",
            LexRules::POSTGRES,
            0,
            Duration::ZERO,
            Some(2),
            HistoryStatus::Ok,
            None,
        );
        assert_eq!(
            (update.sql.as_str(), update.outcome()),
            ("UPDATE t SET a = 1", "2 rows".into())
        );
    }
}

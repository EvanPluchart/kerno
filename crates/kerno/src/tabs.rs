//! Tabs of the central area and what is remembered of them between runs.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Where a query tab runs its statements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// Id of the saved connection.
    pub connection: String,
    /// Database; the connection's own when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// Schema put first in the search path (PostgreSQL); the default when
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

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
        /// The connection the tab runs on, when one was chosen.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<Target>,
        /// The `.sql` file the tab was opened from or saved to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<PathBuf>,
        /// The text differs from the file's.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unsaved: bool,
        /// Source mode: the schema of the object the text replaces.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    /// The data of a table or view.
    Data {
        /// The connection and database the table is in.
        target: Target,
        /// PostgreSQL schema or MySQL database.
        schema: String,
        table: String,
        /// A view: its rows are read-only.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        view: bool,
    },
    /// The structure of a table or view.
    Structure {
        /// The connection and database the table is in.
        target: Target,
        /// PostgreSQL schema or MySQL database.
        schema: String,
        table: String,
        /// A view.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        view: bool,
        /// A materialized view: its DDL is not a view's.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        materialized: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_are_tagged_by_kind() {
        let snapshot = TabSnapshot::Query {
            title: "Query 1".into(),
            sql: "select 'unsaved'\n".into(),
            target: None,
            file: None,
            unsaved: false,
            source: None,
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
        // A tab remembers where it runs; tabs stored before that still load.
        let bound = TabSnapshot::Query {
            title: "Query 2".into(),
            sql: String::new(),
            target: Some(Target {
                connection: "c1".into(),
                database: Some("kerno".into()),
                schema: None,
            }),
            file: None,
            unsaved: false,
            source: None,
        };
        let json = serde_json::to_string(&bound).unwrap();
        assert!(
            json.ends_with(r#""target":{"connection":"c1","database":"kerno"}}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<TabSnapshot>(&json).unwrap(), bound);
        // A tab in source mode remembers the schema of its object.
        let source = TabSnapshot::Query {
            title: "v".into(),
            sql: String::new(),
            target: None,
            file: None,
            unsaved: false,
            source: Some("shop".into()),
        };
        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"query","title":"v","sql":"","source":"shop"}"#
        );
        assert_eq!(serde_json::from_str::<TabSnapshot>(&json).unwrap(), source);
        // A table's data: where it is, and whether it is a view.
        let data = TabSnapshot::Data {
            target: Target {
                connection: "c1".into(),
                database: None,
                schema: None,
            },
            schema: "shop".into(),
            table: "orders".into(),
            view: false,
        };
        let json = serde_json::to_string(&data).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"data","target":{"connection":"c1"},"schema":"shop","table":"orders"}"#
        );
        assert_eq!(serde_json::from_str::<TabSnapshot>(&json).unwrap(), data);
        // A table's structure, the same way.
        let structure = TabSnapshot::Structure {
            target: Target {
                connection: "c1".into(),
                database: None,
                schema: None,
            },
            schema: "shop".into(),
            table: "product_sales".into(),
            view: true,
            materialized: true,
        };
        let json = serde_json::to_string(&structure).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"structure","target":{"connection":"c1"},"schema":"shop","table":"product_sales","view":true,"materialized":true}"#
        );
        assert_eq!(
            serde_json::from_str::<TabSnapshot>(&json).unwrap(),
            structure
        );
        // A kind this version does not know is rejected, and the storage
        // layer then skips that tab instead of failing to start.
        assert!(serde_json::from_str::<TabSnapshot>(r#"{"kind":"from_the_future"}"#).is_err());
    }
}

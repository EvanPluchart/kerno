//! What a MySQL or MariaDB server can do, by its version (K22).
//!
//! One engine covers MySQL 5.1 to 9.x and MariaDB: the server's version,
//! read at connect, decides which catalog tables, statements and syntax
//! Kerno uses. Every version check of the driver is here, as a named fact,
//! so the code that adapts says what it needs rather than a number.
//!
//! The list follows the version gates of DBeaver's MySQL model
//! (`ext.mysql/.../model/MySQLDataSource.java`: `supportsCheckConstraints`,
//! `supportsSequences`, `supportsColumnStatistics`, `supportsSysSchema`;
//! `MySQLUtils.java`; `plan/MySQLPlanAnalyser.supportsExplainJSON`),
//! completed with the release in which each feature appeared, from the
//! MySQL and MariaDB reference manuals.

/// A server version, `(major, minor, patch)`.
pub(crate) type Version = (u32, u32, u32);

/// The features that came with a release, for one server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Capabilities {
    /// The server's version, for the messages that name it.
    pub(crate) version: Version,
    pub(crate) mariadb: bool,
    /// The `utf8mb4` character set (MySQL 5.5.3).
    pub(crate) utf8mb4: bool,
    /// `@@transaction_isolation` (MySQL 5.7.20); older servers, and MariaDB
    /// before 11.1, have `@@tx_isolation` only.
    pub(crate) transaction_isolation_variable: bool,
    /// `SET SESSION TRANSACTION READ ONLY` (MySQL 5.6.5, MariaDB 10.0).
    pub(crate) read_only_transactions: bool,
    /// `EXPLAIN FORMAT=JSON` (MySQL 5.6.5, MariaDB 10.1.2).
    pub(crate) explain_json: bool,
    /// A plan with what really happened: `EXPLAIN ANALYZE` (MySQL 8.0.18),
    /// `ANALYZE FORMAT=JSON` (MariaDB 10.1.2).
    pub(crate) explain_analyze: bool,
    /// `information_schema.PARAMETERS` (MySQL 5.5.3, MariaDB 5.5).
    pub(crate) parameters_table: bool,
    /// Generated columns and `COLUMNS.GENERATION_EXPRESSION` (MySQL 5.7.6,
    /// MariaDB 10.2.5).
    pub(crate) generated_columns: bool,
    /// The `JSON` type (MySQL 5.7.8; MariaDB 10.2.7, as an alias).
    pub(crate) json: bool,
    /// Check constraints listed in the catalog with their table (MySQL
    /// 8.0.16, MariaDB 10.3.10). Older MySQL servers parse `CHECK` and
    /// ignore it.
    pub(crate) check_constraints: bool,
    /// MariaDB's JSON columns, recognised by the check the server adds
    /// (10.4.3).
    pub(crate) json_checks: bool,
    /// Functional key parts, `STATISTICS.EXPRESSION` (MySQL 8.0.13).
    pub(crate) functional_indexes: bool,
    /// `information_schema.VIEW_TABLE_USAGE` (MySQL 8.0.13; not MariaDB).
    pub(crate) view_table_usage: bool,
    /// A comment on a database, `SCHEMATA.SCHEMA_COMMENT` (MariaDB 10.5).
    pub(crate) database_comments: bool,
    /// `ALTER TABLE … RENAME COLUMN` (MySQL 8.0.3, MariaDB 10.5.2).
    pub(crate) rename_column: bool,
    /// `ALTER TABLE … RENAME INDEX` (MySQL 5.7.1, MariaDB 10.5.2).
    pub(crate) rename_index: bool,
    /// Invisible columns (MySQL 8.0.23, MariaDB 10.3.3).
    pub(crate) invisible_columns: bool,
    /// Roles: `mysql.role_edges` (MySQL 8.0), `mysql.roles_mapping`
    /// (MariaDB 10.0.5).
    pub(crate) roles: bool,
    /// `mysql.user.plugin` (MySQL 5.5.7, MariaDB 5.2).
    pub(crate) auth_plugin_column: bool,
    /// `mysql.user.password_expired` (MySQL 5.6.6).
    pub(crate) password_expiry: bool,
    /// `mysql.user.account_locked` (MySQL 5.7.6).
    pub(crate) account_locking: bool,
    /// `performance_schema.processlist` (MySQL 8.0.22); before it, and on
    /// MariaDB, `information_schema.PROCESSLIST`.
    pub(crate) performance_schema_processlist: bool,
    /// `performance_schema.session_variables` / `global_variables` (MySQL
    /// 5.7.6); before it `information_schema.SESSION_VARIABLES`.
    pub(crate) performance_schema_variables: bool,
    /// `performance_schema.variables_info`, where a setting came from
    /// (MySQL 8.0.2).
    pub(crate) variables_info: bool,
    /// `information_schema.SYSTEM_VARIABLES` (MariaDB 10.1.1).
    pub(crate) system_variables_table: bool,
    /// InnoDB's lock waits in `performance_schema.data_lock_waits` (MySQL
    /// 8.0.1), which replaced `information_schema.INNODB_LOCK_WAITS`.
    pub(crate) performance_schema_locks: bool,
    /// `information_schema.INNODB_LOCK_WAITS`, `INNODB_TRX`, `INNODB_LOCKS`
    /// (MySQL 5.5 to 5.7, MariaDB). MySQL 5.1's built-in InnoDB has none.
    pub(crate) innodb_lock_tables: bool,
}

impl Capabilities {
    /// What a server of this version can do.
    pub(crate) fn of(version: Version, mariadb: bool) -> Self {
        let mysql = |at: Version| !mariadb && version >= at;
        let maria = |at: Version| mariadb && version >= at;
        Self {
            version,
            mariadb,
            utf8mb4: mysql((5, 5, 3)) || maria((5, 5, 0)),
            transaction_isolation_variable: mysql((5, 7, 20)),
            read_only_transactions: mysql((5, 6, 5)) || maria((10, 0, 0)),
            explain_json: mysql((5, 6, 5)) || maria((10, 1, 2)),
            explain_analyze: mysql((8, 0, 18)) || maria((10, 1, 2)),
            parameters_table: mysql((5, 5, 3)) || maria((5, 5, 0)),
            generated_columns: mysql((5, 7, 6)) || maria((10, 2, 5)),
            json: mysql((5, 7, 8)) || maria((10, 2, 7)),
            check_constraints: mysql((8, 0, 16)) || maria((10, 3, 10)),
            json_checks: maria((10, 4, 3)),
            functional_indexes: mysql((8, 0, 13)),
            view_table_usage: mysql((8, 0, 13)),
            database_comments: maria((10, 5, 0)),
            rename_column: mysql((8, 0, 3)) || maria((10, 5, 2)),
            rename_index: mysql((5, 7, 1)) || maria((10, 5, 2)),
            invisible_columns: mysql((8, 0, 23)) || maria((10, 3, 3)),
            roles: mysql((8, 0, 0)) || maria((10, 0, 5)),
            auth_plugin_column: mysql((5, 5, 7)) || maria((5, 2, 0)),
            password_expiry: mysql((5, 6, 6)) || maria((10, 4, 3)),
            account_locking: mysql((5, 7, 6)),
            performance_schema_processlist: mysql((8, 0, 22)),
            performance_schema_variables: mysql((5, 7, 6)),
            variables_info: mysql((8, 0, 2)),
            system_variables_table: maria((10, 1, 1)),
            performance_schema_locks: mysql((8, 0, 1)),
            innodb_lock_tables: (mysql((5, 5, 0)) && !mysql((8, 0, 1))) || mariadb,
        }
    }

    /// The server as people name it: `MySQL 5.6.51`, `MariaDB 10.1.48`.
    pub(crate) fn server(&self) -> String {
        let (major, minor, patch) = self.version;
        let product = if self.mariadb { "MariaDB" } else { "MySQL" };
        // A version string without its patch (Aurora's) is judged as the
        // newest of its release; its number is not shown.
        if patch == u32::MAX {
            return format!("{product} {major}.{minor}");
        }
        format!("{product} {major}.{minor}.{patch}")
    }

    /// `"<what> needs MySQL <first> or later; this server is MySQL <x>."`,
    /// for a feature the server lacks. `mysql` and `mariadb` are the first
    /// releases that have it; `None` when that engine never has.
    pub(crate) fn needs(&self, what: &str, mysql: Option<&str>, mariadb: Option<&str>) -> String {
        let first = match (self.mariadb, mysql, mariadb) {
            (false, Some(first), _) => format!("MySQL {first} or later"),
            (true, _, Some(first)) => format!("MariaDB {first} or later"),
            (false, None, _) => "a MariaDB server".to_string(),
            (true, _, None) => "a MySQL server".to_string(),
        };
        format!("{what} needs {first}; this server is {}.", self.server())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mysql(version: Version) -> Capabilities {
        Capabilities::of(version, false)
    }

    fn mariadb(version: Version) -> Capabilities {
        Capabilities::of(version, true)
    }

    /// The release just before `version`, near enough.
    fn previous((major, minor, patch): Version) -> Version {
        match (minor, patch) {
            (_, 1..) => (major, minor, patch - 1),
            (1.., 0) => (major, minor - 1, 99),
            (0, 0) => (major - 1, 99, 99),
        }
    }

    /// Each fact flips at the release that brought it, not one patch
    /// earlier or later.
    #[test]
    fn each_fact_starts_at_its_release() {
        type Fact = fn(&Capabilities) -> bool;
        let mysql_boundaries: [(&str, Fact, Version); 21] = [
            ("utf8mb4", |c| c.utf8mb4, (5, 5, 3)),
            (
                "transaction_isolation",
                |c| c.transaction_isolation_variable,
                (5, 7, 20),
            ),
            ("read only", |c| c.read_only_transactions, (5, 6, 5)),
            ("explain json", |c| c.explain_json, (5, 6, 5)),
            ("explain analyze", |c| c.explain_analyze, (8, 0, 18)),
            ("parameters", |c| c.parameters_table, (5, 5, 3)),
            ("generated", |c| c.generated_columns, (5, 7, 6)),
            ("json", |c| c.json, (5, 7, 8)),
            ("checks", |c| c.check_constraints, (8, 0, 16)),
            ("functional", |c| c.functional_indexes, (8, 0, 13)),
            ("view usage", |c| c.view_table_usage, (8, 0, 13)),
            ("rename column", |c| c.rename_column, (8, 0, 3)),
            ("rename index", |c| c.rename_index, (5, 7, 1)),
            ("invisible", |c| c.invisible_columns, (8, 0, 23)),
            ("roles", |c| c.roles, (8, 0, 0)),
            ("plugin", |c| c.auth_plugin_column, (5, 5, 7)),
            ("expiry", |c| c.password_expiry, (5, 6, 6)),
            ("locking", |c| c.account_locking, (5, 7, 6)),
            (
                "ps processlist",
                |c| c.performance_schema_processlist,
                (8, 0, 22),
            ),
            (
                "ps variables",
                |c| c.performance_schema_variables,
                (5, 7, 6),
            ),
            ("ps locks", |c| c.performance_schema_locks, (8, 0, 1)),
        ];
        for (name, fact, (major, minor, patch)) in mysql_boundaries {
            assert!(fact(&mysql((major, minor, patch))), "MySQL {name}");
            let before = previous((major, minor, patch));
            assert!(!fact(&mysql(before)), "MySQL {name} before {before:?}");
            assert!(fact(&mysql((9, 5, 0))), "MySQL 9 {name}");
        }
        // MySQL's own lists only.
        for version in [(5, 1, 73), (8, 4, 0), (11, 8, 0)] {
            let c = mariadb(version);
            assert!(!c.functional_indexes && !c.view_table_usage, "{version:?}");
            assert!(!c.performance_schema_processlist && !c.performance_schema_locks);
            assert!(
                !c.transaction_isolation_variable,
                "MariaDB keeps tx_isolation"
            );
        }
        let mariadb_boundaries: [(&str, Fact, Version); 7] = [
            ("explain json", |c| c.explain_json, (10, 1, 2)),
            ("generated", |c| c.generated_columns, (10, 2, 5)),
            ("checks", |c| c.check_constraints, (10, 3, 10)),
            ("json checks", |c| c.json_checks, (10, 4, 3)),
            ("comments", |c| c.database_comments, (10, 5, 0)),
            ("rename column", |c| c.rename_column, (10, 5, 2)),
            ("system variables", |c| c.system_variables_table, (10, 1, 1)),
        ];
        for (name, fact, (major, minor, patch)) in mariadb_boundaries {
            assert!(fact(&mariadb((major, minor, patch))), "MariaDB {name}");
            let before = previous((major, minor, patch));
            assert!(!fact(&mariadb(before)), "MariaDB {name} before {before:?}");
        }
    }

    /// The servers the legacy containers run, and the two current ones.
    #[test]
    fn the_tested_servers() {
        let m51 = mysql((5, 1, 73));
        assert!(!m51.utf8mb4 && !m51.parameters_table && !m51.innodb_lock_tables);
        assert!(!m51.auth_plugin_column && !m51.explain_json && !m51.read_only_transactions);
        let m55 = mysql((5, 5, 62));
        assert!(m55.utf8mb4 && m55.parameters_table && m55.innodb_lock_tables);
        assert!(!m55.explain_json && !m55.read_only_transactions && !m55.password_expiry);
        let m56 = mysql((5, 6, 51));
        assert!(m56.explain_json && m56.read_only_transactions && m56.password_expiry);
        assert!(!m56.explain_analyze && !m56.account_locking && !m56.rename_index);
        let m57 = mysql((5, 7, 44));
        assert!(m57.json && m57.generated_columns && m57.transaction_isolation_variable);
        assert!(m57.rename_index && m57.performance_schema_variables && m57.account_locking);
        assert!(!m57.roles && !m57.rename_column && !m57.variables_info);
        let m84 = mysql((8, 4, 13));
        assert!(m84.explain_analyze && m84.view_table_usage && m84.check_constraints);
        let m11 = mariadb((11, 8, 9));
        assert!(m11.check_constraints && m11.json_checks && m11.roles && m11.innodb_lock_tables);
    }

    #[test]
    fn missing_features_are_said_with_both_versions() {
        assert_eq!(
            mysql((5, 6, 51)).needs("EXPLAIN ANALYZE", Some("8.0.18"), Some("10.1.2")),
            "EXPLAIN ANALYZE needs MySQL 8.0.18 or later; this server is MySQL 5.6.51."
        );
        assert_eq!(
            mariadb((10, 0, 38)).needs("A plan", Some("5.6.5"), Some("10.1.2")),
            "A plan needs MariaDB 10.1.2 or later; this server is MariaDB 10.0.38."
        );
        assert_eq!(mysql((5, 1, 73)).server(), "MySQL 5.1.73");
        // Aurora MySQL 2: `5.7.mysql_aurora.2.11.2`.
        assert_eq!(
            mysql((5, 7, u32::MAX)).needs("EXPLAIN ANALYZE", Some("8.0.18"), None),
            "EXPLAIN ANALYZE needs MySQL 8.0.18 or later; this server is MySQL 5.7."
        );
    }
}

//! Administrative views (plain queries shown in the results grid) and
//! signalling other sessions.
//!
//! The two engines keep this information in different places, and each
//! release moved some of it (see `Capabilities`), so most views have one
//! query per engine and era. All of them are written from the servers'
//! documentation and checked against MySQL 5.1, 5.5, 5.6, 5.7 and 8.4 and
//! MariaDB 11.

use kerno_core::{AdminView, Result};

use crate::capabilities::Capabilities;
use crate::introspect::SYSTEM_DATABASES;
use crate::session::MysqlSession;

/// The query of `view` on a server that can do `caps`; `None` when the
/// server keeps no such list (lock waits on MySQL 5.1).
pub(crate) fn query(view: AdminView, caps: &Capabilities) -> Option<String> {
    let mariadb = caps.mariadb;
    Some(match view {
        // Column 0 is the id cancel_session/terminate_session accept.
        AdminView::Sessions => {
            // MySQL deprecated the information_schema table in 8.0 in
            // favour of the performance_schema one (8.0.22); MariaDB only
            // has the former. Without the PROCESS privilege a user sees
            // its own sessions only.
            let source = if caps.performance_schema_processlist {
                "performance_schema.processlist"
            } else {
                "information_schema.PROCESSLIST"
            };
            format!(
                "\
SELECT p.ID AS id,
       p.USER AS `user`,
       p.HOST AS client,
       p.DB AS `database`,
       p.COMMAND AS command,
       p.TIME AS seconds,
       p.STATE AS state,
       p.ID = CONNECTION_ID() AS this_session,
       p.INFO AS query
FROM {source} p
ORDER BY (p.COMMAND = 'Sleep'), p.TIME DESC, p.ID"
            )
        }
        // Needs SELECT on the mysql database. Roles are accounts too: MySQL
        // keeps who was granted which in mysql.role_edges, MariaDB flags
        // them (is_role) and keeps memberships in mysql.roles_mapping.
        AdminView::Roles if mariadb && caps.roles => "\
SELECT u.User AS `user`,
       u.Host AS host,
       u.is_role = 'Y' AS is_role,
       u.plugin AS authentication,
       u.password_expired = 'Y' AS password_expired,
       u.Super_priv = 'Y' AS super,
       u.Grant_priv = 'Y' AS can_grant,
       u.Create_user_priv = 'Y' AS create_user,
       NULLIF(u.max_user_connections, 0) AS connection_limit,
       NULLIF(u.default_role, '') AS default_role,
       (SELECT GROUP_CONCAT(m.Role ORDER BY m.Role SEPARATOR ', ')
        FROM mysql.roles_mapping m
        WHERE m.User = u.User AND m.Host = u.Host) AS member_of
FROM mysql.user u
ORDER BY u.User, u.Host"
            .to_string(),
        // MySQL before 8.0 has no roles; the columns came one by one.
        AdminView::Roles if !caps.roles => format!(
            "\
SELECT u.User AS `user`,
       u.Host AS host,
       {} AS authentication,
       {} AS locked,
       {} AS password_expired,
       u.Super_priv = 'Y' AS super,
       u.Grant_priv = 'Y' AS can_grant,
       u.Create_user_priv = 'Y' AS create_user,
       NULLIF(u.max_user_connections, 0) AS connection_limit
FROM mysql.user u
ORDER BY u.User, u.Host",
            if caps.auth_plugin_column { "u.plugin" } else { "NULL" },
            if caps.account_locking { "u.account_locked = 'Y'" } else { "NULL" },
            if caps.password_expiry { "u.password_expired = 'Y'" } else { "NULL" },
        ),
        AdminView::Roles => "\
SELECT u.User AS `user`,
       u.Host AS host,
       u.plugin AS authentication,
       u.account_locked = 'Y' AS locked,
       u.password_expired = 'Y' AS password_expired,
       u.Super_priv = 'Y' AS super,
       u.Grant_priv = 'Y' AS can_grant,
       u.Create_user_priv = 'Y' AS create_user,
       NULLIF(u.max_user_connections, 0) AS connection_limit,
       (SELECT GROUP_CONCAT(CONCAT(e.FROM_USER, '@', e.FROM_HOST) ORDER BY e.FROM_USER SEPARATOR ', ')
        FROM mysql.role_edges e
        WHERE e.TO_USER = u.User AND e.TO_HOST = u.Host) AS member_of
FROM mysql.user u
ORDER BY u.User, u.Host"
            .to_string(),
        // Global, database and table privileges. An account without the
        // rights to see others' grants sees its own. An administrator's
        // privileges do not fit MySQL's default GROUP_CONCAT length, hence
        // the hint, which MariaDB (whose default is ample) reads as a comment.
        AdminView::Grants => "\
SELECT /*+ SET_VAR(group_concat_max_len = 1048576) */
       x.GRANTEE AS grantee,
       x.kind,
       x.db AS `database`,
       x.object,
       GROUP_CONCAT(x.PRIVILEGE_TYPE ORDER BY x.PRIVILEGE_TYPE SEPARATOR ', ') AS privileges,
       MAX(x.IS_GRANTABLE = 'YES') AS with_grant_option
FROM (
    SELECT GRANTEE, 'global' AS kind, NULL AS db, NULL AS object, PRIVILEGE_TYPE, IS_GRANTABLE
    FROM information_schema.USER_PRIVILEGES
    UNION ALL
    SELECT GRANTEE, 'database', TABLE_SCHEMA, NULL, PRIVILEGE_TYPE, IS_GRANTABLE
    FROM information_schema.SCHEMA_PRIVILEGES
    UNION ALL
    SELECT GRANTEE, 'table', TABLE_SCHEMA, TABLE_NAME, PRIVILEGE_TYPE, IS_GRANTABLE
    FROM information_schema.TABLE_PRIVILEGES
) x
GROUP BY x.GRANTEE, x.kind, x.db, x.object
ORDER BY 1, 2, 3, 4"
            .to_string(),
        AdminView::DatabaseSizes => "\
SELECT s.SCHEMA_NAME AS `database`,
       s.DEFAULT_CHARACTER_SET_NAME AS encoding,
       s.DEFAULT_COLLATION_NAME AS collation,
       COUNT(t.TABLE_NAME) AS tables,
       CAST(COALESCE(SUM(t.DATA_LENGTH + t.INDEX_LENGTH), 0) AS UNSIGNED) AS size_bytes
FROM information_schema.SCHEMATA s
LEFT JOIN information_schema.TABLES t ON t.TABLE_SCHEMA = s.SCHEMA_NAME
GROUP BY s.SCHEMA_NAME, s.DEFAULT_CHARACTER_SET_NAME, s.DEFAULT_COLLATION_NAME
ORDER BY size_bytes DESC, 1"
            .to_string(),
        // Row counts are the storage engine's estimates.
        AdminView::TableSizes => format!(
            "\
SELECT t.TABLE_SCHEMA AS `database`,
       t.TABLE_NAME AS `table`,
       t.ENGINE AS engine,
       t.TABLE_ROWS AS estimated_rows,
       t.DATA_LENGTH AS table_bytes,
       t.INDEX_LENGTH AS index_bytes,
       t.DATA_LENGTH + t.INDEX_LENGTH AS total_bytes
FROM information_schema.TABLES t
WHERE t.TABLE_TYPE NOT IN ('VIEW', 'SYSTEM VIEW', 'SEQUENCE')
  AND t.TABLE_SCHEMA NOT IN ({SYSTEM_DATABASES})
ORDER BY total_bytes DESC, 1, 2"
        ),
        AdminView::Settings if caps.system_variables_table => "\
SELECT LOWER(v.VARIABLE_NAME) AS name,
       COALESCE(v.SESSION_VALUE, v.GLOBAL_VALUE) AS value,
       v.GLOBAL_VALUE AS global_value,
       v.DEFAULT_VALUE AS default_value,
       v.GLOBAL_VALUE_ORIGIN AS source,
       v.READ_ONLY = 'YES' AS read_only,
       v.VARIABLE_COMMENT AS description
FROM information_schema.SYSTEM_VARIABLES v
ORDER BY 1"
            .to_string(),
        AdminView::Settings if caps.variables_info => "\
SELECT v.VARIABLE_NAME AS name,
       v.VARIABLE_VALUE AS value,
       g.VARIABLE_VALUE AS global_value,
       i.VARIABLE_SOURCE AS source
FROM performance_schema.session_variables v
LEFT JOIN performance_schema.global_variables g ON g.VARIABLE_NAME = v.VARIABLE_NAME
LEFT JOIN performance_schema.variables_info i ON i.VARIABLE_NAME = v.VARIABLE_NAME
ORDER BY 1"
            .to_string(),
        // MySQL 5.7 has the performance_schema tables without the source;
        // older servers, and MariaDB before 10.1, the information_schema
        // ones (whose names are upper case).
        AdminView::Settings => {
            let schema = if caps.performance_schema_variables {
                "performance_schema"
            } else {
                "information_schema"
            };
            format!(
                "\
SELECT LOWER(v.VARIABLE_NAME) AS name,
       v.VARIABLE_VALUE AS value,
       g.VARIABLE_VALUE AS global_value
FROM {schema}.session_variables v
LEFT JOIN {schema}.global_variables g ON g.VARIABLE_NAME = v.VARIABLE_NAME
ORDER BY 1"
            )
        }
        // InnoDB row and table locks. MariaDB still has the
        // information_schema tables MySQL 8.0 replaced by performance_schema
        // ones, whose thread ids are mapped to the processlist ids KILL takes.
        // Metadata locks (a DDL waiting for a transaction) are not listed.
        AdminView::Locks if !caps.performance_schema_locks && !caps.innodb_lock_tables => {
            return None;
        }
        AdminView::Locks if !caps.performance_schema_locks => "\
SELECT r.trx_mysql_thread_id AS waiting_id,
       rp.USER AS waiting_user,
       rp.DB AS `database`,
       TIMESTAMPDIFF(SECOND, r.trx_wait_started, NOW()) AS waited_seconds,
       l.lock_mode,
       l.lock_table AS object,
       r.trx_query AS waiting_query,
       b.trx_mysql_thread_id AS blocking_id,
       bp.USER AS blocking_user,
       bp.COMMAND AS blocking_command,
       b.trx_query AS blocking_query
FROM information_schema.INNODB_LOCK_WAITS w
JOIN information_schema.INNODB_TRX r ON r.trx_id = w.requesting_trx_id
JOIN information_schema.INNODB_TRX b ON b.trx_id = w.blocking_trx_id
LEFT JOIN information_schema.INNODB_LOCKS l ON l.lock_id = w.requested_lock_id
LEFT JOIN information_schema.PROCESSLIST rp ON rp.ID = r.trx_mysql_thread_id
LEFT JOIN information_schema.PROCESSLIST bp ON bp.ID = b.trx_mysql_thread_id
ORDER BY r.trx_wait_started, 1, 8"
            .to_string(),
        AdminView::Locks => "\
SELECT wt.PROCESSLIST_ID AS waiting_id,
       wt.PROCESSLIST_USER AS waiting_user,
       wt.PROCESSLIST_DB AS `database`,
       wt.PROCESSLIST_TIME AS waited_seconds,
       rl.LOCK_MODE AS lock_mode,
       CONCAT(rl.OBJECT_SCHEMA, '.', rl.OBJECT_NAME) AS object,
       wt.PROCESSLIST_INFO AS waiting_query,
       bt.PROCESSLIST_ID AS blocking_id,
       bt.PROCESSLIST_USER AS blocking_user,
       bt.PROCESSLIST_COMMAND AS blocking_command,
       bt.PROCESSLIST_INFO AS blocking_query
FROM performance_schema.data_lock_waits w
JOIN performance_schema.threads wt ON wt.THREAD_ID = w.REQUESTING_THREAD_ID
JOIN performance_schema.threads bt ON bt.THREAD_ID = w.BLOCKING_THREAD_ID
LEFT JOIN performance_schema.data_locks rl
       ON rl.ENGINE = w.ENGINE AND rl.ENGINE_LOCK_ID = w.REQUESTING_ENGINE_LOCK_ID
ORDER BY waited_seconds DESC, 1, 8"
            .to_string(),
    })
}

/// Sends the dialect's `KILL QUERY` or `KILL CONNECTION` statement.
pub(crate) async fn kill(session: &MysqlSession, sql: String) -> Result<()> {
    // The server answers "Unknown thread id" when no such session exists.
    session.query(&sql).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEWS: [AdminView; 7] = [
        AdminView::Sessions,
        AdminView::Roles,
        AdminView::Grants,
        AdminView::DatabaseSizes,
        AdminView::TableSizes,
        AdminView::Settings,
        AdminView::Locks,
    ];

    fn mysql(version: (u32, u32, u32)) -> Capabilities {
        Capabilities::of(version, false)
    }

    #[test]
    fn every_view_has_a_query_on_both_engines() {
        for caps in [mysql((8, 4, 13)), Capabilities::of((11, 0, 0), true)] {
            for view in VIEWS {
                let sql = query(view, &caps).unwrap();
                assert!(sql.starts_with("SELECT "), "{caps:?} {view:?}: {sql}");
            }
        }
    }

    /// The table each list is read from, by release (K22): the first line
    /// that starts with FROM.
    #[test]
    fn each_list_reads_the_table_the_server_has() {
        let source = |view, caps: Capabilities| {
            let sql = query(view, &caps)?;
            sql.lines()
                .find_map(|line| line.strip_prefix("FROM "))
                .map(str::to_string)
        };
        let mariadb = Capabilities::of((11, 8, 9), true);
        for (view, caps, expected) in [
            (
                AdminView::Sessions,
                mysql((8, 4, 13)),
                "performance_schema.processlist p",
            ),
            (
                AdminView::Sessions,
                mysql((8, 0, 21)),
                "information_schema.PROCESSLIST p",
            ),
            (
                AdminView::Sessions,
                mysql((5, 1, 73)),
                "information_schema.PROCESSLIST p",
            ),
            // MariaDB's version numbers are larger and it has no such table.
            (
                AdminView::Sessions,
                mariadb,
                "information_schema.PROCESSLIST p",
            ),
            (
                AdminView::Settings,
                mysql((8, 4, 13)),
                "performance_schema.session_variables v",
            ),
            (
                AdminView::Settings,
                mysql((5, 7, 44)),
                "performance_schema.session_variables v",
            ),
            (
                AdminView::Settings,
                mysql((5, 6, 51)),
                "information_schema.session_variables v",
            ),
            (
                AdminView::Settings,
                mariadb,
                "information_schema.SYSTEM_VARIABLES v",
            ),
            (
                AdminView::Locks,
                mysql((8, 4, 13)),
                "performance_schema.data_lock_waits w",
            ),
            (
                AdminView::Locks,
                mysql((5, 7, 44)),
                "information_schema.INNODB_LOCK_WAITS w",
            ),
            (
                AdminView::Locks,
                mysql((5, 5, 62)),
                "information_schema.INNODB_LOCK_WAITS w",
            ),
            (
                AdminView::Locks,
                mariadb,
                "information_schema.INNODB_LOCK_WAITS w",
            ),
        ] {
            assert_eq!(
                source(view, caps).as_deref(),
                Some(expected),
                "{view:?} {caps:?}"
            );
        }
        // Only performance_schema 8.0 says where a setting came from.
        let settings = |caps| query(AdminView::Settings, &caps).unwrap();
        assert!(settings(mysql((8, 0, 2))).contains("variables_info"));
        assert!(!settings(mysql((8, 0, 1))).contains("variables_info"));
        // MySQL 5.1's built-in InnoDB keeps no lock tables.
        assert_eq!(query(AdminView::Locks, &mysql((5, 1, 73))), None);
    }

    /// MySQL before 8.0 has no roles, and mysql.user grew its columns one
    /// release at a time: each is read only where it exists.
    #[test]
    fn accounts_read_the_columns_each_release_has() {
        let roles = |version| query(AdminView::Roles, &mysql(version)).unwrap();
        assert!(roles((8, 0, 0)).contains("mysql.role_edges"));
        let m57 = roles((5, 7, 44));
        assert!(!m57.contains("role_edges"), "{m57}");
        assert!(
            m57.contains("u.account_locked") && m57.contains("u.plugin"),
            "{m57}"
        );
        let m56 = roles((5, 6, 51));
        assert!(!m56.contains("u.account_locked") && m56.contains("u.password_expired"));
        let m55 = roles((5, 5, 62));
        assert!(
            !m55.contains("u.password_expired") && m55.contains("u.plugin"),
            "{m55}"
        );
        assert!(!roles((5, 1, 73)).contains("u.plugin"));
        let mariadb = query(AdminView::Roles, &Capabilities::of((11, 8, 9), true)).unwrap();
        assert!(mariadb.contains("roles_mapping"));
    }
}

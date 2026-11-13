//! Administrative views (plain queries shown in the results grid) and
//! signalling other sessions.
//!
//! The two engines keep this information in different places, so most
//! views have one query per engine. All of them are written from the
//! servers' documentation and checked against MySQL 8.4 and MariaDB 11.

use kerno_core::{AdminView, Engine, Result};

use crate::introspect::SYSTEM_DATABASES;
use crate::session::MysqlSession;

pub(crate) fn query(view: AdminView, engine: Engine, version: (u32, u32, u32)) -> String {
    let mariadb = engine == Engine::MariaDb;
    match view {
        // Column 0 is the id cancel_session/terminate_session accept.
        AdminView::Sessions => {
            // MySQL deprecated the information_schema table in 8.0 in
            // favour of the performance_schema one (8.0.22); MariaDB only
            // has the former. Without the PROCESS privilege a user sees
            // its own sessions only.
            let source = if !mariadb && version >= (8, 0, 22) {
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
        AdminView::Roles if mariadb => "\
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
        AdminView::Settings if mariadb => "\
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
        AdminView::Settings => "\
SELECT v.VARIABLE_NAME AS name,
       v.VARIABLE_VALUE AS value,
       g.VARIABLE_VALUE AS global_value,
       i.VARIABLE_SOURCE AS source
FROM performance_schema.session_variables v
LEFT JOIN performance_schema.global_variables g ON g.VARIABLE_NAME = v.VARIABLE_NAME
LEFT JOIN performance_schema.variables_info i ON i.VARIABLE_NAME = v.VARIABLE_NAME
ORDER BY 1"
            .to_string(),
        // InnoDB row and table locks. MariaDB still has the
        // information_schema tables MySQL 8.0 replaced by performance_schema
        // ones, whose thread ids are mapped to the processlist ids KILL takes.
        // Metadata locks (a DDL waiting for a transaction) are not listed.
        AdminView::Locks if mariadb => "\
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
    }
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

    #[test]
    fn every_view_has_a_query_on_both_engines() {
        for engine in [Engine::MySql, Engine::MariaDb] {
            for view in VIEWS {
                let sql = query(view, engine, (11, 0, 0));
                assert!(sql.starts_with("SELECT "), "{engine:?} {view:?}: {sql}");
            }
        }
    }

    #[test]
    fn the_session_list_reads_the_table_the_server_has() {
        let source = |engine, version| {
            let sql = query(AdminView::Sessions, engine, version);
            sql.lines()
                .find_map(|line| line.strip_prefix("FROM "))
                .map(str::to_string)
        };
        assert_eq!(
            source(Engine::MySql, (8, 4, 13)).as_deref(),
            Some("performance_schema.processlist p")
        );
        assert_eq!(
            source(Engine::MySql, (8, 0, 21)).as_deref(),
            Some("information_schema.PROCESSLIST p")
        );
        assert_eq!(
            source(Engine::MySql, (5, 7, 44)).as_deref(),
            Some("information_schema.PROCESSLIST p")
        );
        // MariaDB's version numbers are larger and it has no such table.
        assert_eq!(
            source(Engine::MariaDb, (11, 8, 9)).as_deref(),
            Some("information_schema.PROCESSLIST p")
        );
    }
}

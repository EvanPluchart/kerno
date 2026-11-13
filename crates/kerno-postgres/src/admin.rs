//! Administrative views (plain queries shown in the results grid) and
//! signalling other sessions.

use kerno_core::{AdminView, DbError, Error, Result};

use crate::session::PgSession;

/// Schemas that hold the user's objects.
const USER_SCHEMAS: &str = "n.nspname !~ '^pg_' AND n.nspname <> 'information_schema'";

pub(crate) fn query(view: AdminView) -> String {
    match view {
        // Column 0 is the id cancel_session/terminate_session accept.
        AdminView::Sessions => "\
SELECT a.pid,
       a.usename AS \"user\",
       a.datname AS database,
       a.application_name AS application,
       COALESCE(a.client_hostname, host(a.client_addr)) AS client,
       a.state,
       CASE WHEN a.wait_event IS NOT NULL THEN a.wait_event_type || ': ' || a.wait_event END AS waiting_on,
       a.backend_start AS connected_at,
       a.xact_start AS transaction_start,
       a.query_start,
       a.pid = pg_catalog.pg_backend_pid() AS this_session,
       a.query
FROM pg_catalog.pg_stat_activity a
WHERE a.backend_type = 'client backend'
ORDER BY (a.state = 'active') DESC, a.query_start DESC NULLS LAST"
            .to_string(),
        AdminView::Roles => "\
SELECT r.rolname AS role,
       r.rolcanlogin AS can_login,
       r.rolsuper AS superuser,
       r.rolcreatedb AS create_database,
       r.rolcreaterole AS create_role,
       r.rolreplication AS replication,
       r.rolbypassrls AS bypass_rls,
       NULLIF(r.rolconnlimit, -1) AS connection_limit,
       r.rolvaliduntil AS valid_until,
       (SELECT string_agg(g.rolname, ', ' ORDER BY g.rolname)
        FROM pg_catalog.pg_auth_members m
        JOIN pg_catalog.pg_roles g ON g.oid = m.roleid
        WHERE m.member = r.oid) AS member_of,
       pg_catalog.shobj_description(r.oid, 'pg_authid') AS comment
FROM pg_catalog.pg_roles r
WHERE r.rolname !~ '^pg_'
ORDER BY r.rolname"
            .to_string(),
        // Explicit grants only: an object whose ACL is NULL still has its
        // default privileges (everything for the owner).
        AdminView::Grants => format!(
            "\
SELECT COALESCE(grantee.rolname, 'PUBLIC') AS grantee,
       x.kind,
       x.schema,
       x.object,
       string_agg(x.privilege, ', ' ORDER BY x.privilege) AS privileges,
       bool_or(x.grantable) AS with_grant_option,
       grantor.rolname AS granted_by
FROM (
    SELECT 'database' AS kind, NULL::name AS schema, d.datname::text AS object,
           a.grantee, a.grantor, a.privilege_type AS privilege, a.is_grantable AS grantable
    FROM pg_catalog.pg_database d, aclexplode(d.datacl) a
    WHERE NOT d.datistemplate
    UNION ALL
    SELECT 'schema', n.nspname, NULL, a.grantee, a.grantor, a.privilege_type, a.is_grantable
    FROM pg_catalog.pg_namespace n, aclexplode(n.nspacl) a
    WHERE {USER_SCHEMAS}
    UNION ALL
    SELECT CASE c.relkind WHEN 'S' THEN 'sequence' WHEN 'v' THEN 'view'
                          WHEN 'm' THEN 'materialized view' ELSE 'table' END,
           n.nspname, c.relname::text, a.grantee, a.grantor, a.privilege_type, a.is_grantable
    FROM pg_catalog.pg_class c
    JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace, aclexplode(c.relacl) a
    WHERE c.relkind IN ('r', 'p', 'f', 'v', 'm', 'S') AND {USER_SCHEMAS}
    UNION ALL
    SELECT 'routine', n.nspname,
           p.proname || '(' || pg_catalog.pg_get_function_identity_arguments(p.oid) || ')',
           a.grantee, a.grantor, a.privilege_type, a.is_grantable
    FROM pg_catalog.pg_proc p
    JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace, aclexplode(p.proacl) a
    WHERE {USER_SCHEMAS}
) x
LEFT JOIN pg_catalog.pg_roles grantee ON grantee.oid = x.grantee
LEFT JOIN pg_catalog.pg_roles grantor ON grantor.oid = x.grantor
GROUP BY grantee.rolname, x.kind, x.schema, x.object, grantor.rolname
ORDER BY 1, 2, 3, 4"
        ),
        AdminView::DatabaseSizes => "\
SELECT d.datname AS database,
       pg_catalog.pg_get_userbyid(d.datdba) AS owner,
       pg_catalog.pg_encoding_to_char(d.encoding) AS encoding,
       s.bytes AS size_bytes,
       pg_catalog.pg_size_pretty(s.bytes) AS size
FROM pg_catalog.pg_database d
CROSS JOIN LATERAL (
    SELECT CASE WHEN d.datallowconn AND pg_catalog.has_database_privilege(d.oid, 'CONNECT')
                THEN pg_catalog.pg_database_size(d.oid) END AS bytes
) s
WHERE NOT d.datistemplate
ORDER BY s.bytes DESC NULLS LAST, d.datname"
            .to_string(),
        AdminView::TableSizes => format!(
            "\
SELECT n.nspname AS schema,
       c.relname AS \"table\",
       CASE WHEN c.reltuples >= 0 THEN c.reltuples::bigint END AS estimated_rows,
       pg_catalog.pg_table_size(c.oid) AS table_bytes,
       pg_catalog.pg_indexes_size(c.oid) AS index_bytes,
       s.total_bytes,
       pg_catalog.pg_size_pretty(s.total_bytes) AS total
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
CROSS JOIN LATERAL (SELECT pg_catalog.pg_total_relation_size(c.oid) AS total_bytes) s
-- A table dropped while the list is read has no size (NULL): it is gone.
WHERE c.relkind IN ('r', 'm') AND {USER_SCHEMAS} AND s.total_bytes IS NOT NULL
ORDER BY 6 DESC, 1, 2"
        ),
        AdminView::Settings => "\
SELECT s.name, s.setting AS value, s.unit, s.category, s.short_desc AS description,
       s.source, s.boot_val AS default_value, s.pending_restart
FROM pg_catalog.pg_settings s
ORDER BY s.category, s.name"
            .to_string(),
        // pg_blocking_pids() names the sessions in a waiting one's way, for
        // row locks too (the waiter waits on the holder's transaction id).
        // The lock it waits for is its one ungranted row of pg_locks.
        // Without pg_read_all_stats the statements of other users' sessions
        // read "<insufficient privilege>".
        AdminView::Locks => "\
SELECT w.pid AS waiting_id,
       w.usename AS waiting_user,
       w.datname AS database,
       date_trunc('second', clock_timestamp() - l.waitstart) AS waited,
       l.mode AS lock_mode,
       CASE l.locktype
            WHEN 'relation' THEN l.relation::regclass::text
            WHEN 'tuple' THEN l.relation::regclass::text || ' (row)'
            WHEN 'transactionid' THEN 'a row, held by transaction ' || l.transactionid
            ELSE l.locktype
       END AS object,
       w.query AS waiting_query,
       b.pid AS blocking_id,
       b.usename AS blocking_user,
       b.state AS blocking_state,
       b.query AS blocking_query
FROM pg_catalog.pg_stat_activity w
CROSS JOIN LATERAL unnest(pg_catalog.pg_blocking_pids(w.pid)) AS blocker(pid)
LEFT JOIN pg_catalog.pg_stat_activity b ON b.pid = blocker.pid
LEFT JOIN LATERAL (
    SELECT * FROM pg_catalog.pg_locks k
    WHERE k.pid = w.pid AND NOT k.granted
    LIMIT 1
) l ON true
WHERE w.wait_event_type = 'Lock'
ORDER BY l.waitstart NULLS LAST, w.pid, b.pid"
            .to_string(),
    }
}

/// Runs the statement `sql` (one of the dialect's signals) for `id`.
pub(crate) async fn signal(session: &PgSession, sql: String, id: &str) -> Result<()> {
    let rows = session.query(&sql).await?;
    // The functions return false when no such backend exists.
    if rows.first().and_then(|row| row.get(0)) == Some("t") {
        Ok(())
    } else {
        Err(Error::Db(Box::new(DbError::message(format!(
            "session {} no longer exists",
            id.trim()
        )))))
    }
}

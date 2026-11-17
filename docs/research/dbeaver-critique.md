# A general critique of DBeaver, read for Kerno

Source: an AI assistant's summary of the recurring criticisms of DBeaver Community, given by the owner on
2026-11-17. It is a second-hand summary, not issue numbers: `dbeaver-issues.md` stays the evidence with votes.
Where a point here matches an issue there, quote the issue. The points are numbered C1 to C22 so that the
triage and the plan can refer to them.

The "first reading" column is the main session's, from memory of the code plus a few greps. It is **not
verified**: every "has" needs a file and a test as proof before it is written anywhere as a fact.

## Performance and weight

| # | Criticism of DBeaver | First reading for Kerno |
|---|---|---|
| C1 | Eclipse/Java: slow start, high memory, OutOfMemory on large results or schemas, `-Xmx` edited by hand | Has: 264 ms cold start, 340 MB for 1M × 20 fetched in full, paged fetch (`docs/BENCH.md`). Weak spot: idle memory is 149.9 MB for a 150 MB bar, no margin |
| C2 | Metadata on databases with thousands of tables: the tree and completion freeze the UI for seconds | Tree has a bench (200 schemas, 1,200 tables: 22 ms and 103 ms). Completion on a large catalog has no bench: to measure |
| C3 | Long operations block the UI instead of running in the background with a reliable cancel | Rule of §1. To audit: is there any long operation without a cancel or without progress (introspection, DDL apply, export, import, transfer, tools, ERD layout, AI request)? |

## Ergonomics and interface

| # | Criticism of DBeaver | First reading for Kerno |
|---|---|---|
| C4 | Visual overload: menus, tabs, panels and preferences inherited from Eclipse; an option in three places or nowhere | Kerno's bet. Since v1.1 it has many surfaces (toolbar, 19 J items): audit for duplicates and for commands that only live in one hard-to-find place |
| C5 | Preferences are a maze: global, per connection, per database type, with no word on which wins | Kerno has settings, environment rules and connection flags (read-only, local client, password source). To check: does each screen say which level wins? Is there a search box in Settings (grep found none)? |
| C6 | SQL tabs: the link between a script, a connection and the active schema is error-prone; a query runs on the wrong database, dangerous in production | Each tab has its own connection, database and schema selector, and the environment colours the chrome. To check: what a tab shows and does after its connection is closed, renamed, deleted, or after a restart; a script opened from a file; the schema shown versus the session's real `search_path` / `USE` |
| C7 | Dated look: imperfect dark theme, rough rendering on Linux and HiDPI | Themes are tokens, WCAG AA. Linux packages were only start-checked, never looked at: no Linux capture exists |

## SQL editor

| # | Criticism of DBeaver | First reading for Kerno |
|---|---|---|
| C8 | Completion: slow, sometimes irrelevant, weak on CTEs, complex aliases and dialect specifics | `sql/complete.rs` knows aliases and CTE names. To test: columns of a CTE, of a subquery alias, of a `JOIN LATERAL`, nested CTEs, `UPDATE … FROM`, MySQL backticks, quoted mixed-case names |
| C9 | "Statement under the cursor" fails on PL/SQL blocks, procedures and scripts mixing `;` and `$$` | `sql/split.rs` handles MySQL `DELIMITER`; `sql/lex.rs` knows dollar quoting. To test: `DO $$ … $$`, `$tag$`, `CREATE FUNCTION … BEGIN … END`, `BEGIN ATOMIC`, MySQL `CREATE PROCEDURE` without `DELIMITER`, a comment or a string holding `;` |
| C10 | Poor formatter, little configuration | J7 added options (keyword case, indent, comma position, line width). To test on hard input: CTEs, window functions, `CASE`, subqueries, comments kept, dollar-quoted bodies left alone, idempotence |
| C11 | Almost no refactoring: no smart rename, no dependency analysis in SQL | Kerno has none. Candidate: rename an alias or a CTE in the statement (uses J7's occurrences); "used by" for a table or a column (views, functions, foreign keys from the catalog) before a rename or a drop |

## Paid editions

| # | Criticism of DBeaver | First reading for Kerno |
|---|---|---|
| C12 | The line between Community and PRO is blurry and moves; NoSQL, schema compare, the advanced visual query builder, cloud integrations are paid; the product nags | Kerno is MIT OR Apache-2.0 with no paid edition: a message for the README and the site, not code. Schema compare, data compare and the query builder are on `docs/ROADMAP.md`: the triage can argue for moving one up |

## Reliability

| # | Criticism of DBeaver | First reading for Kerno |
|---|---|---|
| C13 | JDBC drivers: automatic download fails behind a proxy; version conflicts | Does not apply: drivers are compiled in. To check behind a proxy: the update check (`update.rs`), and the README should say what happens |
| C14 | Raw Java stack traces instead of explanations | Connection errors follow §3 (plain words first). To audit: every other error surface (query, edit apply, import, export, transfer, tools, SSH, AI, update, storage) for a raw `Debug` or driver string shown alone |
| C15 | Transactions: auto-commit or manual is hard to see; a transaction stays open with its locks unnoticed | Has: per-tab mode, "transaction open" state, environment rule "close idle transactions", a close question naming the loss (`tabs.rs`). To check: quitting the app, losing the connection, and a visible age of the open transaction |
| C16 | Updates that break workspaces or configuration | Has: versioned migrations and a refusal to open data written by a newer version (`storage.rs`). To check: a copy of the database before a migration runs; the wording of the refusal; settings and keymap files from an older version |

## Collaboration and configuration

| # | Criticism of DBeaver | First reading for Kerno |
|---|---|---|
| C17 | Sharing connections with colleagues or in Git is clumsy: JSON mixed with encrypted credentials, confusing projects | Has: J15, one JSON file without any secret. Candidate: a stable, sorted, diff-friendly file and a documented "team file in Git" flow (import that updates instead of duplicating) |
| C18 | Secret managers and the OS keychain are limited in the free edition | Has: keychain, password command, `op read`, AWS RDS IAM, `.pgpass`, `.my.cnf` (J3). RDS IAM and 1Password were never run for real |

## What the critique says to fix first

| # | Priority named | First reading for Kerno |
|---|---|---|
| C19 | Lighter core: memory, everything async and cancellable, lazy metadata | See C1 to C3 |
| C20 | Simpler interface: a "simple" mode, a universal search in preferences, menus cleaned | See C4 and C5. A "simple mode" is probably not needed if the default is already calm: say why or propose it |
| C21 | Production guard-rails: very visible environment (colour, banner), confirmation of `UPDATE`/`DELETE` without `WHERE` on by default, a permanent open-transaction indicator | Has all three in principle (`sql/classify.rs` marks `UPDATE` and `DELETE` without `WHERE` as destructive). To verify by test on each engine and through every way to run SQL (statement, script, multi-run J14, saved task, AI-inserted text) |
| C22 | Readable errors; drivers that work offline and behind a proxy | See C13 and C14 |

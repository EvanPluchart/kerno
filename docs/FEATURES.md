# Features

Every v1 item of CLAUDE.md §4, with its status, the tests that cover it and the screenshot that shows it.
Status: `done` (works end to end against the test servers, tested, captured), `partial` (what is missing is
said), `todo` (milestone given). Test names are functions; the file is given once per group.

Engines: PG = PostgreSQL 17, My = MySQL 8.4, Ma = MariaDB 11.

## A. Connections

Tests: `crates/kerno/src/ui_tests.rs` (UI), `crates/kerno/src/connections/manager_tests.rs` (manager),
`crates/kerno/src/connections/form.rs` (form), `crates/kerno-core/src/config.rs`,
`crates/kerno-{postgres,mysql}/tests/connect.rs` (drivers), `crates/kerno-core/tests/ssh.rs` (tunnel).

| Item | Status | Tests | Screenshot |
| ---- | ------ | ----- | ---------- |
| A1 Engine picker (filterable grid), name, colour tag, environment, group | done | `a_connection_is_filled_from_a_url_tested_saved_and_opened`, form tests | `m3-dialog-general-*.png` |
| A2 Host, port, user, password (save or ask each time), database, URL paste, driver options | done | `a_connection_is_filled_from_a_url_tested_saved_and_opened`, `a_password_that_is_not_saved_is_asked_for`, `a_saved_connection_without_its_password_asks_and_then_keeps_it`, `a_form_with_errors_is_not_saved`, `default_database_and_options` (PG, My, Ma) | `m3-dialog-general-*.png`, `m3-password-prompt-*.png`, `m3-dialog-advanced-light.png` |
| A3 SSL/TLS modes disable/prefer/require/verify-ca/verify-full, CA/cert/key files | done | `tls_modes_without_verification`, `tls_verification`, `client_certificate_authentication` (PG, My, Ma), `tls_through_the_tunnel*` | `m3-dialog-tls-light.png` |
| A4 SSH tunnel: password, key file (+ passphrase), agent; keep-alive; clear errors | done | `a_tunnelled_connection_asks_for_trust_then_for_each_missing_secret`, `an_unknown_ssh_host_is_shown_before_it_is_trusted`, `an_encrypted_ssh_key_asks_for_its_passphrase`, `through_the_ssh_tunnel` (PG, My, Ma), `tests/ssh.rs` | `m3-dialog-ssh-light.png`, `m3-ssh-trust-light.png` |
| A5 Test connection (version, latency) | done | `test_connection_reports_version_and_latency`, `a_connection_is_filled_from_a_url_tested_saved_and_opened` | `m3-dialog-general-light.png` |
| A5 Connect / disconnect | done | `connects_queries_and_disconnects`, `a_failed_connection_says_why` | `m3-sidebar-connected-*.png`, `m3-connection-failed-light.png` |
| A5 Auto-reconnect on a dropped connection | done | `a_lost_connection_is_re_established` | — (no visible state) |
| A5 Read-only flag | partial: the server session is read-only (`read_only_sessions_refuse_writes`, PG, My, Ma); the client-side block arrives with the editor (milestone 5) | `read_only_sessions_refuse_writes` | `m3-dialog-general-*.png` |
| A5 Duplicate, edit, delete | done | `duplicate_import_and_delete`, `deleting_a_connection_asks_first`, `saving_stores_settings_and_secrets_separately` | `m3-sidebar-menu-light.png` |
| A5 Import / export as JSON without secrets | done | `connections_are_exported_without_secrets_and_imported_back`, `connections_export_and_import_without_secrets`, `import_rejects_what_is_not_a_valid_connections_file` | — (file dialog) |

Engine logos (CLAUDE.md §1): engine picker and sidebar connections show the vendors' logos
(`every_engine_has_its_logo_and_the_kit_icons_still_load`; visible in every `m3-*` capture). Navigator root,
tab and status-bar badges follow with those views.

## B. Schema navigator

Tests: `crates/kerno/src/ui_tests.rs` (UI, against the three servers), `crates/kerno/src/navigator/model.rs`
and `actions.rs` (tree and menus), `crates/kerno-core/src/sql/generate.rs` and the drivers' `dialect.rs`
(statements), `crates/kerno-{postgres,mysql}/tests/generate.rs` (statements run on the servers),
`tests/introspect.rs` of both drivers (what the tree shows).

| Item | Status | Tests | Screenshot |
| ---- | ------ | ----- | ---------- |
| B1 Lazy tree, PostgreSQL: databases → schemas → tables, views, materialized views, functions, procedures, sequences, types, triggers, extensions | done | `the_schema_tree_opens_level_by_level`, `the_tree_follows_the_connection`, `databases_then_schemas_then_folders_of_objects` | `m4-navigator-tree-*.png` |
| B1 Lazy tree, MySQL / MariaDB: databases → tables, views, routines, triggers, events | done | `mysql_and_mariadb_list_objects_under_their_databases`, `mysql_databases_hold_the_objects_directly` | `m4-navigator-mariadb-light.png` |
| B1 Columns and indexes under each table | done | `the_schema_tree_opens_level_by_level`, `a_table_loads_its_columns_and_indexes_once` | `m4-navigator-tree-*.png` |
| B2 Filter box | done | `the_filter_narrows_the_tree_and_clearing_it_restores_it`, `the_filter_keeps_what_matches_and_the_way_to_it` | `m4-navigator-filter-light.png` |
| B2 Refresh node | done | `refresh_reloads_the_nearest_node_that_owns_its_children`, `dropping_renaming_and_truncating_show_the_sql_and_ask` (the tree is refreshed after each) | — |
| B2 Row-count and size hints, icons per object kind | done | `databases_then_schemas_then_folders_of_objects`, `counts_and_sizes_are_compact` | `m4-navigator-tree-*.png` |
| B3 New query here, copy name, generate SELECT / INSERT / UPDATE / DELETE, show DDL | done (the tab is bound to the connection in milestone 5) | `statements_and_ddl_open_in_a_tab_and_run_nothing`, `generate.rs` tests | `m4-navigator-menu-light.png` |
| B3 Rename, truncate, drop, with SQL preview and confirmation | done | `dropping_renaming_and_truncating_show_the_sql_and_ask`, `production_asks_twice_and_read_only_refuses` (MySQL, MariaDB), `tests/generate.rs` (PG, My, Ma) | `m4-confirm-drop-*.png`, `m4-confirm-rename-light.png`, `m4-confirm-production-light.png`, `m4-navigator-menu-readonly-light.png` |
| B3 Create schema / database | done | `dropping_renaming_and_truncating_show_the_sql_and_ask` (schema), `tests/generate.rs` (database) | — |
| B3 Create table / view | partial: opens a statement to complete in a query tab; the form editor is milestone 7 | `statements_and_ddl_open_in_a_tab_and_run_nothing` | — |
| B3 Open data, open structure | todo: with the table data view (milestone 6) and the structure view (milestone 7) | | |

## C. SQL editor — todo (milestone 5)

## D. Results grid — todo (milestone 5)

## E. Table data view — todo (milestone 6)

## F. Structure — todo (milestone 7)

## G. Import, export, backup — todo (milestone 8)

## H. Administration — todo (milestone 8)

## I. App

| Item | Status | Tests | Screenshot |
| ---- | ------ | ----- | ---------- |
| I1 Command palette | done | `command_palette_finds_and_runs_commands` | `m2-tabs-light.png` (palette capture due in milestone 10) |
| I1 Settings view (theme, fonts, editor, grid page size, confirmations) | partial: appearance, page size, confirmations; fonts and editor options in milestone 10 | `appearance_and_sidebar_choices_are_applied_and_stored` | due in milestone 10 |
| I1 Keymap override file | done | `overrides_replace_defaults_per_command`, `problems_in_the_keymap_file_are_reported_not_fatal` | — |
| I2 macOS bundle, Linux build, platform dirs, redacted logs | todo (milestone 10); data dirs and log redaction exist (`storage.rs`, `logging.rs` tests) | | |

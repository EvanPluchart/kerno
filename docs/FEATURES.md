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
| A1 Engine picker (filterable grid), name, colour tag, environment, group | done (engine logos replace the monograms next, owner request) | `a_connection_is_filled_from_a_url_tested_saved_and_opened`, form tests | `m3-dialog-general-*.png` |
| A2 Host, port, user, password (save or ask each time), database, URL paste, driver options | done | `a_connection_is_filled_from_a_url_tested_saved_and_opened`, `a_password_that_is_not_saved_is_asked_for`, `a_saved_connection_without_its_password_asks_and_then_keeps_it`, `a_form_with_errors_is_not_saved`, `default_database_and_options` (PG, My, Ma) | `m3-dialog-general-*.png`, `m3-password-prompt-light.png`, `m3-dialog-advanced-light.png` |
| A3 SSL/TLS modes disable/prefer/require/verify-ca/verify-full, CA/cert/key files | done | `tls_modes_without_verification`, `tls_verification`, `client_certificate_authentication` (PG, My, Ma), `tls_through_the_tunnel*` | `m3-dialog-tls-light.png` |
| A4 SSH tunnel: password, key file (+ passphrase), agent; keep-alive; clear errors | done | `a_tunnelled_connection_asks_for_trust_then_for_each_missing_secret`, `an_unknown_ssh_host_is_shown_before_it_is_trusted`, `an_encrypted_ssh_key_asks_for_its_passphrase`, `through_the_ssh_tunnel` (PG, My, Ma), `tests/ssh.rs` | `m3-dialog-ssh-light.png` |
| A5 Test connection (version, latency) | done | `test_connection_reports_version_and_latency`, `a_connection_is_filled_from_a_url_tested_saved_and_opened` | `m3-dialog-test-passed-light.png` |
| A5 Connect / disconnect | done | `connects_queries_and_disconnects`, `a_failed_connection_says_why` | `m3-sidebar-connected-*.png` |
| A5 Auto-reconnect on a dropped connection | done | `a_lost_connection_is_re_established` | — (no visible state) |
| A5 Read-only flag | partial: the server session is read-only (`read_only_sessions_refuse_writes`, PG, My, Ma); the client-side block arrives with the editor (milestone 5) | `read_only_sessions_refuse_writes` | `m3-dialog-general-*.png` |
| A5 Duplicate, edit, delete | done | `duplicate_import_and_delete`, `deleting_a_connection_asks_first`, `saving_stores_settings_and_secrets_separately` | `m3-sidebar-menu-light.png` |
| A5 Import / export as JSON without secrets | done | `connections_are_exported_without_secrets_and_imported_back`, `connections_export_and_import_without_secrets`, `import_rejects_what_is_not_a_valid_connections_file` | — (file dialog) |

## B. Schema navigator — todo (milestone 4)

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

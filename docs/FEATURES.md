# Features

Every v1 item of CLAUDE.md §4, with its status, the tests that cover it and the screenshot that shows it.
Status: `done` (in the committed code and covered by a test), `done, untested` (it exists, no test covers it),
`partial: <what is missing>`, `todo: <plan file and part>` (not built yet). Test names are functions; the file is
given once per group. Screenshot: files of `docs/screenshots/`; `to capture` where the plan has captures not yet
taken (milestones 7 and 8); `—` where there is none. Committed code (HEAD `f982e77`) plus the maintenance dialog of the working tree (G12 app half, not yet committed), 2026-11-13.

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
| A5 Read-only flag | done: the server session is read-only and the client refuses writes | `read_only_sessions_refuse_writes` (PG, My, Ma), `statements_that_destroy_or_write_are_guarded`, `production_asks_twice_and_read_only_refuses`, `a_read_only_connection_cannot_apply`, `a_read_only_connection_cannot_import` | `m3-dialog-general-*.png`, `m4-navigator-menu-readonly-light.png` |
| A5 Duplicate, edit, rename, delete (sidebar menu) | done | `duplicate_import_and_delete`, `deleting_a_connection_asks_first`, `saving_stores_settings_and_secrets_separately`, `the_sidebar_answers_a_right_click_on_each_part`, `folders_are_made_renamed_and_removed_with_what_they_hold` (rename) | `m3-sidebar-menu-light.png` |
| A1 Folders: nested paths kept in `group`, empty folders remembered, create / rename / delete (content moves up after a confirmation), collapse, folder picker in the dialog | done | `folders_are_made_renamed_and_removed_with_what_they_hold`, `a_folder_is_made_renamed_filled_by_drag_and_deleted`, `folder_paths_are_tidied_checked_and_moved`, `the_folder_is_stored_as_a_tidy_path`, `connections_are_exported_without_secrets_and_imported_back` (folders travel with the export) | — |
| A1 Drag a connection onto a folder or to the top level | done | `a_folder_is_made_renamed_filled_by_drag_and_deleted` | — |
| A5 Sidebar menus: empty area, folder, connection (Reconnect, New SQL editor, Move to folder, Copy name / URL without the password / host) | done | `the_sidebar_answers_a_right_click_on_each_part`, `the_empty_area_and_a_folder_offer_what_dbeaver_does`, `a_connection_disables_what_does_not_apply_to_its_status`, `the_copied_connection_url_has_no_password`, `the_connection_url_has_no_password_and_reads_back`, `new_sql_editor_connects_first_then_opens_a_tab_on_the_connection` | `m3-sidebar-menu-light.png` |
| A5 A double click or Enter on a connection connects it and never disconnects; F4 edits it | done | `a_double_click_on_a_connection_connects_and_never_disconnects` | — |
| A5 Import / export as JSON without secrets | done | `connections_are_exported_without_secrets_and_imported_back`, `connections_export_and_import_without_secrets`, `import_rejects_what_is_not_a_valid_connections_file` | — (file dialog) |
| A5 Commit mode per connection (manual by default on production); default schema (PostgreSQL); bootstrap statements run on every session, checked by a strict grammar (D-034) | done | `the_commit_mode_follows_the_environment_unless_chosen`, `a_production_connection_starts_its_tabs_in_manual_commit`, `the_connection_dialog_keeps_the_commit_mode_and_the_bootstrap`, `bootstrap_statements_are_split_and_checked`, `bootstrap_statements_run_in_each_tab_and_a_failing_one_is_named`, `a_refused_bootstrap_statement_is_shown_in_the_advanced_section`; drivers: `connector_runs_the_bootstrap_on_every_session` (PG, My) | — |
| A5 Disconnect closes every session the connection handed out | done | `every_owner_of_a_session_closes_it_when_it_goes_away` (PG, My) | — |
| A5 Read-only toggle, Disconnect all / others, connection colour on tabs and the status bar | todo: another agent is working on them (PARITY-GAPS G13 leftovers) | | |

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
| B3 New query here, copy name, generate SELECT / INSERT / UPDATE / DELETE, show DDL | done | `statements_and_ddl_open_in_a_tab_and_run_nothing`, `generate.rs` tests | `m4-navigator-menu-light.png` |
| B3 Rename, truncate, drop, with SQL preview and confirmation | done | `dropping_renaming_and_truncating_show_the_sql_and_ask`, `production_asks_twice_and_read_only_refuses` (MySQL, MariaDB), `tests/generate.rs` (PG, My, Ma) | `m4-confirm-drop-*.png`, `m4-confirm-rename-light.png`, `m4-confirm-production-light.png`, `m4-navigator-menu-readonly-light.png` |
| B3 Create schema / database | done | `dropping_renaming_and_truncating_show_the_sql_and_ask` (schema), `tests/generate.rs` (database) | — |
| B3 Create table (opens the form editor) | done | `a_table_is_created_from_the_form` | to capture |
| B3 Create view | partial: opens a statement to complete in a query tab | `statements_and_ddl_open_in_a_tab_and_run_nothing` | — |
| B3 Open data, open structure | done | `a_double_click_or_enter_opens_what_the_row_is_for`, `a_table_is_read_in_pages_sorted_and_filtered_by_the_server`, `the_structure_of_a_table_is_shown_in_tabs`, `navigator_keys_run_the_actions_of_the_selected_node` | `m6-data-pending-light.png`, to capture (structure) |
| B3 Edit source, install extension, create enum, Export…, Import CSV… (entries on the nodes they apply to) | done | `editing_a_source_applies_it_once_its_sql_is_confirmed`, `extensions_are_installed_and_dropped_from_the_tree`, `enums_are_created_and_their_values_added_and_renamed`, `a_filtered_table_is_exported_to_csv_and_json_through_the_dialog`, `a_csv_file_is_imported_into_an_existing_table_and_into_a_new_one`, `each_kind_of_node_gets_the_actions_that_make_sense_for_it` | to capture |
| B3 Set as default for the active tab; copy the qualified name; New query on any node | done | `set_as_default_points_the_active_tab_at_the_schema`, `navigator_keys_run_the_actions_of_the_selected_node` | — |
| B3 Empty area below the tree: Refresh, Collapse all, New query here | done | `a_right_click_below_the_schema_tree_opens_its_menu` | — |
| B1 Clicks (§4.B item 1): a click selects; the chevron, Right and Left expand and collapse; a double click or Enter runs the default action (table, view: data, structure or expand by the `table_double_click` setting; routines: source; other objects: DDL; folders: toggle) | done | `a_click_selects_a_row_and_the_chevron_or_the_arrows_open_it`, `a_double_click_or_enter_opens_what_the_row_is_for`, `a_double_click_opens_what_the_node_is_for`, `f4_opens_the_editor_of_what_the_node_is` | — |
| B Keys, as DBeaver: F4 edit, F2 rename, Delete / Backspace drop (with the SQL confirmation), F5 refresh, Cmd+C copy name, Cmd+Shift+C qualified name, Cmd+] new query, Cmd+Shift+A set as default; each a named command in the palette and the keymap file | done | `navigator_keys_run_the_actions_of_the_selected_node`, `command_ids_are_unique_and_shortcuts_valid`, `a_shortcut_is_unique_within_its_context`, `shortcuts_need_a_real_key` | — |
| B1 Under a table: Columns, Constraints, Foreign keys, Indexes, References, Triggers from one structure load; a double click on a key or a reference opens the other table | done | `a_table_lists_its_constraints_keys_references_and_triggers`, `reveal_opens_the_way_to_a_table_one_load_at_a_time` | `m4-navigator-table-folders-light.png` |
| B3 Create function / procedure / sequence / trigger / event: a template per engine in a query tab | done | `create_entries_open_templates_that_run_once_named`, `create_entries_write_a_template_per_engine` | — |
| B3 Rename column, drop column (SQL confirmation); drop with cascade (PostgreSQL) | done | `columns_are_added_renamed_and_dropped_from_the_tree`, `a_drop_with_cascade_also_drops_what_depends_on_the_table`; the column test runs on PostgreSQL and MySQL, not MariaDB | — |
| B3 Tools ▸ on a table, view, materialized view, trigger or database: Vacuum, Analyze, Refresh, Truncate options, Enable / Disable trigger (PG); Analyze, Check, Optimize, Repair (My, Ma) | done (the app half is not yet committed); foreign tables lack Truncate | `the_tools_menu_lists_what_the_engine_offers_for_each_object`, `postgres_tools_analyze_vacuum_refresh_switch_triggers_and_truncate`, `a_postgres_database_is_vacuumed_and_analysed_from_its_node`, `a_vacuum_waiting_for_a_lock_is_cancelled`, `mysql_and_mariadb_tools_show_their_status_rows`, `the_tools_menu_and_command_open_the_dialog_and_read_only_or_production_guard_it`, `options_are_dbeavers_and_what_the_server_refuses_together_is_blocked`; core: `table_tools_and_their_options`, `tools_act_on_tables_views_and_triggers`, `vacuum_and_analyze_run_on_the_connected_database` | to capture |

## C. SQL editor

Tests: `crates/kerno/src/ui_tests.rs` (UI, against the three servers), `crates/kerno-core/src/sql/{split,params,classify,complete,format,lex}.rs`
(statements), `crates/kerno-core/src/plan.rs` and `crates/kerno/src/query/plan_view.rs` (EXPLAIN),
`crates/kerno/src/query/comment.rs`, `crates/kerno/src/library.rs`, `crates/kerno-core/src/storage.rs` (history),
`crates/kerno-{postgres,mysql}/tests/{execute,explain,introspect}.rs` (what the servers do).
Status words: `done` (in the committed code and a test covers it), `done, untested`, `partial: <what is missing>`,
`todo: <plan file and part>`. Screenshots of milestone 5 beyond the two below are not taken yet.

| Item | Status | Tests | Screenshot |
| ---- | ------ | ----- | ---------- |
| C1 Several tabs per connection, each bound to a connection, database and schema | done | `tabs_open_switch_and_close_from_the_keyboard`, `tabs_close_with_their_button_and_open_from_the_empty_state`, `new_sql_editor_connects_first_then_opens_a_tab_on_the_connection`, `set_as_default_points_the_active_tab_at_the_schema`, `a_tab_without_an_open_connection_says_what_to_do` | — |
| C1 Toolbar selectors (connection, database, schema) of a tab | done, untested: no test clicks the dropdowns; the target they set is covered by the row above | | — |
| C1 Tabs and unsaved text restored after a restart | done | `unsaved_text_and_tabs_come_back_after_a_restart`, `the_active_tab_is_active_again_after_a_restart`, `snapshots_are_tagged_by_kind` | — |
| C1 Open and save `.sql` files (name, unsaved mark, a file open once) | done | `sql_files_are_opened_saved_and_marked_when_changed` | — |
| C1 Closing a tab that holds an open transaction, pending edits or an unsaved file asks first | todo: plans/M5.md Part 4 (also plans/CTX.md Part 2) | | — |
| C2 Highlighting | done, untested: the kit's editor with the SQL language | | — |
| C2 Schema-aware completion: keywords, functions, schemas, tables, columns, aliases, joins on foreign keys | done | `completion_proposes_tables_columns_and_joins`, `typing_in_the_editor_opens_completion_and_enter_accepts_it`, `the_catalog_follows_ddl`; core: `after_from_come_relations_of_the_search_path_then_schemas`, `after_an_alias_and_a_dot_come_its_columns`, `join_proposes_joins_along_foreign_keys_first`, `on_proposes_the_foreign_key_condition`, `functions_are_built_in_or_from_the_catalog`, `names_are_quoted_only_when_they_need_it`, `a_large_catalog_and_a_long_statement_stay_fast`; drivers: `completion_catalog_lists_the_user_schemas` (PG), `completion_catalog_lists_the_user_databases` (My, Ma) | — |
| C2 Completion on demand (Ctrl+Space); catalog read through the tab's session in manual commit; MySQL `USE` refreshes the search path | todo: plans/M5.md (left over from Part 2) | | — |
| C2 Bracket matching | done, untested: the kit's editor | | — |
| C2 Comment toggle (Cmd+/) | done | `the_comment_shortcut_toggles_the_lines_of_the_selection`, `lines_are_commented_out_and_back_in`, `a_selection_is_widened_to_whole_lines` | — |
| C2 Find and replace | done | `the_editor_searches_and_replaces` | — |
| C2 Multi-cursor | done, untested: the kit's editor (Cmd+Alt+Up / Down) | | — |
| C2 Format SQL (selection or whole text, one undo step, never changes a token) | done | `format_sql_rewrites_the_selection_or_everything_as_one_undo_step`; core: `formatting_preserves_every_token`, `formatting_is_idempotent`, `representative_statements_come_back_exactly_so`, `output_is_tidy`, `random_token_soup_keeps_the_properties` | — |
| C3 Run the statement at the cursor, or the whole script (one result per statement, stops at the first failure) | done | `statements_run_and_their_rows_fill_the_grid`; core: `statement_at_cursor`, `splits_on_semicolons_and_drops_empty_statements`, `mysql_delimiter_command`, `a_long_script_splits_in_linear_time` | — |
| C3 Run the selection | done, untested | | — |
| C3 Rows arrive a page at a time; fetch more, fetch all | done | `a_large_result_arrives_a_page_at_a_time`; core: `delivers_exactly_the_rows_asked_for_then_waits`, `first_rows_arrive_before_a_large_result_is_done`; drivers: `rows_stream_only_as_far_as_asked` (PG, My, Ma), `fetch_all_delivers_every_row_in_order` | — |
| C3 Cancel a running statement | done | `a_running_statement_is_cancelled_and_the_window_stays_usable`; drivers: `a_running_statement_can_be_cancelled` (PG, My, Ma), `cancelling_a_sleep_ends_it_at_once`, `another_sessions_statement_can_be_cancelled` | — |
| C3 Timing and rows affected | done | `statements_run_and_their_rows_fill_the_grid`; drivers: `commands_report_their_row_counts` | — |
| C3 Server notices and warnings panel; error position underlined with detail and hint | done | `messages_errors_and_history_of_a_run`; drivers: `notices_and_warnings_are_delivered` (PG), `warnings_are_delivered_as_notices` (My, Ma), `server_errors_carry_code_position_detail_and_hint` (PG), `server_errors_carry_code_and_position` (My, Ma) | — |
| C3 Write guards (CLAUDE.md §1): read-only refuses writes, destructive statements confirmed, production twice | done | `statements_that_destroy_or_write_are_guarded`; core: `statements_are_classified`, `the_table_covers_enough`, `executable_comments_are_mysql_only` | — |
| C4 Query parameters (`:name`, `$1`, `?`) asked for before the run | done | `parameters_are_asked_for_before_a_statement_runs`; core: `finds_named_and_numbered_parameters_once_each`, `question_marks_are_separate_parameters_on_mysql_only`, `things_that_look_like_parameters_but_are_not`, `binds_every_occurrence_and_leaves_the_rest`, `values_become_literals` | — |
| C5 EXPLAIN and EXPLAIN ANALYZE as a plan tree (cost, rows, time, slowest step) and the raw output | done | `plans_are_shown_as_a_tree_on_every_engine`; core: `slowest_is_the_largest_exclusive_time`, `exclusive_time_takes_the_children_out`, `the_outline_shows_one_node_per_line`; app: `rows_are_the_nodes_top_down_with_their_depth_and_path`, `measurement_details_are_left_to_the_raw_output`, `numbers_and_times_are_short_and_grouped`; drivers: `plans_are_read_with_and_without_analyze` (PG, My, Ma), `the_scan_of_the_big_table_is_the_slowest_step` | `m5-explain-analyze-light.png` |
| C6 Transaction mode per tab: auto-commit or manual, visible state, Commit and Rollback | done | `manual_commit_keeps_changes_in_a_transaction_until_it_is_ended`; drivers: `transaction_state_follows_the_server` (PG, My, Ma), `abandoning_a_result_inside_a_transaction_keeps_the_transaction` (PG) | — |
| C7 Query history: searchable, per connection, duration and status | done | `history_and_saved_queries_are_searched_and_reopened`, `messages_errors_and_history_of_a_run`; core: `history_is_newest_first_filtered_and_searchable`, `history_never_stores_a_password`, `passwords_in_statements_are_redacted`; app: `times_are_told_in_the_largest_unit_that_fits`, `a_statement_is_shown_on_one_line` | `m5-history-light.png` |
| C7 Saved queries: named, foldered | done | `history_and_saved_queries_are_searched_and_reopened`; core: `saved_queries_are_created_updated_sorted_and_deleted` | `m5-history-light.png` |
| C3 Run on a saved but closed connection connects first; run in a new result tab (`Cmd+\`) keeps earlier results; run from the cursor; next / previous statement; count the rows of a statement; hide, maximise and focus the results panel | done | `running_in_a_tab_of_a_closed_connection_connects_first`, `result_tabs_close_pin_and_rename`, `statements_are_reached_and_run_from_the_cursor`, `the_rows_of_a_statement_are_counted`, `the_results_panel_hides_maximizes_and_takes_the_keyboard` | — |
| C3 Result tabs close, pin and rename from a right-click menu or a middle click | done | `result_tabs_close_pin_and_rename`, `a_result_tab_closes_pins_and_names_as_in_dbeaver` | — |
| C1 Rename a query tab (kept after a restart); closing tabs asks once for what would be lost | done | `a_renamed_query_tab_keeps_its_name_after_a_restart`, `a_file_with_unsaved_text_asks_before_its_tab_closes`, `closing_rolls_back_and_discards_only_once_confirmed` | — |

## D. Results grid

Tests: `crates/kerno/src/ui_tests.rs`, `crates/kerno-core/src/{format,sort,export,result}.rs`, the drivers'
`tests/execute.rs` (`every_type_arrives_in_its_text_form`).

| Item | Status | Tests | Screenshot |
| ---- | ------ | ----- | ---------- |
| D1 Virtualized, streamed, compact storage | partial: rows are laid out only when visible and arrive in pages; smooth scrolling of 1,000,000 rows × 20 columns is measured in milestone 9 (todo: plans/M9.md) | `a_large_result_arrives_a_page_at_a_time`, `cells_round_trip_across_many_rows`, `store_addresses_rows_across_batches`, `a_clone_is_a_snapshot_that_shares_the_rows` | — |
| D1 Client-side sort (numbers as numbers, NULLs last; large results on a background thread) | done | `the_grid_sorts_copies_and_rearranges_what_it_shows`; core: `numbers_compare_by_value`, `nulls_are_greater_than_everything`, `rows_are_sorted_with_ties_in_fetch_order_and_nulls_last`, `two_hundred_thousand_mixed_rows_sort_correctly`, `one_million_mixed_rows_sort_correctly` | — |
| D1 Hide and pin columns, show all | done | `the_grid_sorts_copies_and_rearranges_what_it_shows` | — |
| D1 Column resize and reorder | done, untested | | — |
| D1 Row numbers | done, untested | | — |
| D1 Selection: cells, ranges (shift-click), the whole result | partial: a cell, a range and Cmd+A are tested; selecting a row or a column by its header (the kit's) is not | `the_grid_sorts_copies_and_rearranges_what_it_shows` | — |
| D2 NULL distinct from the empty string | done | `null_is_not_an_empty_string`, `null_is_distinct_from_empty`, `statements_run_and_their_rows_fill_the_grid` | — |
| D2 Numbers right-aligned; colour per kind | partial: the kind that picks alignment and colour is tested; the drawn alignment is not asserted | `style_follows_the_kind_of_the_column` | — |
| D2 Booleans, long text truncated, binary as a hex preview | done | `booleans_are_spelled_out`, `text_is_cut_at_a_number_of_characters`, `a_huge_cell_is_cut_without_reading_all_of_it`, `binary_is_a_hex_preview`, `a_double_click_on_a_cell_shows_its_whole_value` | — |
| D2 Dates and times with zone, UUID, JSON, arrays: shown as the server writes them | done | `every_type_arrives_in_its_text_form` (PG, My, Ma), `numbers_are_shown_as_the_server_printed_them`, `json_numbers_and_strings_are_kept_as_written` | — |
| D2 Value viewer: text, indented JSON, hex dump | done | `a_double_click_on_a_cell_shows_its_whole_value`; core: `json_is_indented_by_two_spaces`, `hex_dump_has_rows_of_sixteen_bytes`, `deeply_nested_json_does_not_overflow` | — |
| D3 Copy as TSV, CSV, JSON, Markdown, SQL INSERT; copy column names | done | `the_grid_sorts_copies_and_rearranges_what_it_shows`; core: `sample_as_csv`, `sample_as_tsv`, `sample_as_json_array`, `sample_as_markdown`, `sample_as_sql_insert`, `delimited_fields_are_quoted_only_when_they_must_be` | — |
| D3 The row menu opens from any cell and offers the copy formats | done | `a_right_click_on_a_cell_opens_the_menu_of_its_row` | `m6-row-menu-dark.png` |
| D3 Export the full result | done | `a_query_result_is_exported_by_running_its_statement_again`, `the_million_row_table_exports_line_by_line` (PG, My, Ma) | — |
| D Cell menu knows its cell: Copy, Copy as, Order by, View value, Pin, Hide, Export…; acts on the selection when the click is inside it; header menu with Order by, Filter on this column, Fit width, Fit all widths | done | `a_result_cell_copies_orders_shows_and_exports_but_never_edits_or_filters`, `the_menu_of_a_result_cell_acts_on_the_selection_or_on_its_cell`, `a_header_copies_orders_filters_and_fits_its_column`, `the_menu_of_a_column_header_orders_filters_and_fits` | — |

## E. Table data view

Tests: `crates/kerno/src/ui_tests.rs`, `crates/kerno-core/src/{browse,pending}.rs`, `crates/kerno-core/src/sql/edits.rs`,
`crates/kerno-core/src/schema.rs`, `crates/kerno-{postgres,mysql}/tests/edits.rs`.

| Item | Status | Tests | Screenshot |
| ---- | ------ | ----- | ---------- |
| E1 Pages of data, server-side sort from a header click, filter bar (column, operator, value) with a raw `WHERE`, row count | done | `a_table_is_read_in_pages_sorted_and_filtered_by_the_server`; core: `conditions_are_written_with_literals`, `patterns_match_the_text_of_any_column`, `conditions_and_the_raw_text_must_all_hold`, `what_cannot_be_a_filter_is_refused`, `a_page_is_sorted_down_to_the_row_identity` | `m6-filter-light.png` |
| E1 Refresh; cancel a slow page | done | `cells_are_edited_reviewed_and_applied_in_one_transaction` (refresh), `a_slow_page_is_cancelled_and_the_tab_stays_usable` (PG) | — |
| E1 Counting a very large table: "about N rows" and a "Count exactly" button | partial: the estimate statement exists in the core (`Dialect::estimated_rows_sql`); the tab still counts exactly (todo: plans/M6.md Part 2) | `the_row_estimate_of_the_big_table_is_close_to_its_count` (PG), `the_row_estimate_of_the_big_table_is_of_its_order` (My, Ma) | — |
| E1 Opened from the tree (double click, Enter, Open data); restored after a restart | done | `a_double_click_or_enter_opens_what_the_row_is_for`, `a_table_opened_from_the_tree_comes_back_after_a_restart` | — |
| E2 Inline edit of cells; add, duplicate, delete rows; Set NULL, Set default; pending changes highlighted and counted | done | `cells_are_edited_reviewed_and_applied_in_one_transaction`; core: `an_edit_back_to_the_fetched_value_is_no_edit`, `rows_are_added_copied_taken_away_and_reverted` | `m6-data-pending-light.png`, `m6-data-pending-dark.png` |
| E2 Review SQL; Apply in one transaction or Discard; a failed apply rolls back and keeps the pending edits | done | `cells_are_edited_reviewed_and_applied_in_one_transaction`; core: `update_sets_cells_of_the_row_addressed_by_its_key`, `delete_addresses_one_row`, `changes_keep_their_order_and_empty_updates_vanish`, `changes_address_rows_by_the_key_they_were_read_with`; drivers: `a_failed_apply_changes_nothing` (PG, My, Ma), `every_type_survives_a_write_from_its_text_form` (PG), `every_type_round_trips_through_edit_statements` (My, Ma) | `m6-review-sql-light.png` |
| E2 A primary or unique key is required, else read-only with the reason | done | `rows_that_cannot_be_addressed_are_read_only_and_say_why` (PG); core: `row_identity_prefers_the_primary_key_in_key_order`, `no_key_means_no_row_identity`, `without_a_key_only_new_rows_can_be_written`, `a_row_without_identity_is_never_updated_or_deleted` | — |
| E2 Enum and boolean cells edited from a list; paste into cells; Delete sets NULL; Cmd+Z takes a change back | todo: plans/M6.md Part 1 | | — |
| E3 Follow a foreign key to the referenced row; list the rows that reference the current one | done | `foreign_keys_lead_to_the_referenced_row_and_back` | `m6-row-menu-dark.png` |
| E2 Keys: Cmd+S reviews the pending changes, add / duplicate / delete rows, copy from the row above or below, set default, Escape reverts the selected cells, Cmd+Z undoes the last change, Cmd+Shift+C copies with column names | done | `data_keys_edit_rows_and_apply`, `the_grid_keys_differ_on_both_systems`, `undo_takes_back_one_step_at_a_time` | — |
| E2 Paste a value or a tab-separated block as pending changes; Delete sets nullable cells to NULL | done (PG only) | `pasting_fills_cells_from_the_clipboard_and_delete_empties_them`, `a_pasted_block_is_split_in_rows_and_cells` | — |
| E1 Filter by this value, Exclude this value, other operators from a cell, Clear filters | done | `filter_by_this_value_and_exclude_it_change_the_rows`, `a_data_cell_adds_paste_filters_edits_and_the_row_in_dbeavers_order` | — |
| E2 A table that no longer exists says so and offers to close the tab | done | `a_data_tab_whose_table_is_gone_shows_no_rows_and_the_tree_forgets_it`, `a_table_dropped_under_its_data_tab_says_so_and_offers_to_close_it` | — |

## F. Structure

Tests: `crates/kerno/src/ui_tests.rs`, `crates/kerno-core/src/table_draft.rs`, `crates/kerno-core/src/dialect.rs`,
`crates/kerno-{postgres,mysql}/tests/{alter,generate,introspect}.rs`. Captures of milestone 7 are not taken yet.

| Item | Status | Tests | Screenshot |
| ---- | ------ | ----- | ---------- |
| F1 Structure tab of a table: columns, indexes, constraints, foreign keys both ways, triggers, DDL | done | `the_structure_of_a_table_is_shown_in_tabs`, `a_view_shows_its_columns_and_ddl`, `the_structure_tab_comes_back_after_a_restart`; drivers: `structure_of_a_table`, `table_ddl_recreates_the_same_structure` | to capture |
| F2 Create a table through a form with a live DDL preview, applied after confirmation | done | `a_table_is_created_from_the_form`, `the_form_refuses_what_is_invalid`; core: `a_new_draft_has_an_auto_increment_primary_key`, `validation_names_every_problem`, `create_table_writes_keys_inline_then_indexes_and_comments`; drivers: `a_table_created_from_a_draft_reads_back_as_the_draft` (PG, My, Ma) | to capture |
| F2 Alter a table through the form (add, drop, rename, modify columns; indexes; foreign keys; comments) with the statements previewed | partial: column order cannot be edited; renaming the table from the form with the structure tab following and the per-engine wording of the confirmation have no test (plans/M7.md Part 1c, item 4) | `a_table_is_altered_from_the_form_with_a_preview`; core: `every_kind_of_change_in_the_order_it_must_run`, `an_unchanged_draft_gives_no_change`, `renaming_a_key_column_keeps_the_keys_and_indexes`, `primary_keys_are_dropped_and_added_as_needed`, `a_foreign_key_with_another_action_is_dropped_and_added`, `alter_table_writes_one_statement_per_action_in_order`, `altering_a_column_writes_only_what_differs`; drivers: `a_table_edited_from_its_structure_becomes_the_draft` (PG, My, Ma) | to capture |
| F2 An edit does not lose what the form does not show: ON UPDATE, INVISIBLE, identity kind, a self-referencing key | done | core: `a_change_of_on_update_alters_the_column`, `an_invisible_column_stays_invisible`, `an_identity_keeps_its_kind`, `a_key_on_its_own_table_follows_a_rename_of_what_it_references`; drivers: `a_comment_only_edit_keeps_on_update_current_timestamp`, `an_invisible_column_stays_invisible_after_an_edit` (My, Ma), `an_identity_generated_always_stays_always` (PG), `a_key_on_its_own_table_follows_a_rename_of_what_it_references` (PG, My, Ma) | — |
| F2 Apply asks twice on production, refuses on a read-only connection, rolls back a failed alter on PostgreSQL | done | `production_asks_twice_before_a_table_is_altered`, `a_read_only_connection_cannot_apply`, `a_failed_alter_changes_nothing_on_postgresql`, `a_table_form_is_not_restored_after_a_restart` | — |
| F3 View, function and procedure source: view and edit, applied with confirmation | done | `editing_a_source_applies_it_once_its_sql_is_confirmed`, `a_materialized_view_whose_new_source_fails_is_kept`; drivers: `edited_sources_replace_views_and_routines` (PG, My, Ma); core: `sources_extensions_and_enums_are_engine_specific` | to capture |
| F4 Create and drop a database and a schema | done | `dropping_renaming_and_truncating_show_the_sql_and_ask` (schema); drivers: `rename_truncate_drop_and_create_do_what_they_say` (database, PG, My, Ma) | — |
| F4 PostgreSQL extensions: install from the available list, drop | done | `extensions_are_installed_and_dropped_from_the_tree`; driver: `an_extension_is_installed_and_dropped` (PG) | to capture |
| F4 PostgreSQL enums: create, add a value, rename a value, drop | done | `enums_are_created_and_their_values_added_and_renamed`; driver: `enums_are_created_extended_and_renamed` (PG) | to capture |

## G. Import, export, backup

Tests: `crates/kerno/src/ui_tests.rs`, `crates/kerno/src/import_dialog.rs`, `crates/kerno-core/src/{export,export_file,import,tools}.rs`,
`crates/kerno-{postgres,mysql}/tests/{transfer,tools}.rs`. Captures of milestone 8 are not taken yet.

| Item | Status | Tests | Screenshot |
| ---- | ------ | ----- | ---------- |
| G1 Export a table or a result to CSV / TSV, JSON array, NDJSON, SQL INSERT, Markdown, streamed to a file with progress | done | `a_filtered_table_is_exported_to_csv_and_json_through_the_dialog`, `a_query_result_is_exported_by_running_its_statement_again`; core: `every_encoding_writes_its_mark_and_its_characters`, `the_temporary_file_sits_next_to_the_target`; drivers: `json_markdown_and_insert_files_hold_every_row` (PG, My, Ma), `the_million_row_table_exports_line_by_line` (PG, My, Ma) | to capture |
| G1 Options: delimiter, header, NULL text, encoding | done | `delimited_files_round_trip_through_an_import` (PG, My, Ma), `options_reach_the_file` (PG), `a_character_split_between_writes_is_encoded_once_whole` | to capture |
| G1 Cancel; a failure removes the partial file | done | `a_slow_export_is_cancelled_and_leaves_no_file`; drivers: `cancel_stops_the_statement_and_removes_the_file`, `failures_remove_the_file_and_say_why` (PG, My, Ma) | to capture |
| G2 Import CSV into an existing table or a new one: preview, guessed delimiter, encoding and types, mapping | done | `a_csv_file_is_imported_into_an_existing_table_and_into_a_new_one`; app: `the_delimiter_and_the_encoding_are_guessed`, `a_table_is_named_after_its_file`; core: `kinds_are_guessed_from_every_value`, `preview_reads_the_header_and_the_first_rows`, `a_new_table_is_created_with_nullable_columns_only`; drivers: `a_mapping_skips_and_reorders_columns` (PG, My, Ma) | to capture |
| G2 Parsing: quotes, line breaks, delimiters, encodings, NULL rule | done | core: `quotes_doubled_quotes_and_line_breaks`, `other_delimiters`, `encodings_and_byte_order_marks`, `null_text_unquoted_is_null_quoted_is_a_value`, `an_unclosed_quote_and_bad_utf8_name_their_line`; app: `the_confirmation_writes_null_where_the_reader_reads_null` | — |
| G2 Batches in one transaction; an error rolls everything back and names the line; read-only refused | done | `a_failed_import_says_which_line_and_keeps_nothing`, `a_read_only_connection_cannot_import`; drivers: `a_bad_record_rolls_everything_back_and_names_its_line`, `a_hundred_thousand_rows_go_into_a_new_table` (PG, My, Ma) | to capture |
| G2 Cancel an import from the dialog; the double confirmation on production; the MySQL "created first" sentence | partial: the core cancel is tested (`cancel_rolls_the_import_back`, PG, My, Ma); the dialog's cancel, the production confirmation and that sentence have no UI test (plans/M8.md, left over from Parts 1 and 2) | `cancel_rolls_the_import_back` | to capture |
| G3 Backup and restore through the installed client tools: find the binaries and their version, command lines without the secret, streamed output, cancel | done | `a_backup_tool_is_found_or_chosen_and_its_output_streams`, `a_database_is_backed_up_and_restored_with_the_real_tools`, `a_running_tool_is_cancelled`, `a_backup_goes_through_the_ssh_tunnel`; core in `tools.rs` | to capture |
| G3 A restore asks (twice on production, refused on read-only); a restored SQL file cannot run commands on this computer unasked, the opt-in checkbox is never pre-checked | done | `a_restore_asks_twice_on_production_and_is_refused_on_read_only`, `the_restore_confirmation_names_what_it_runs_and_what_a_failure_keeps`, `a_file_with_client_commands_is_refused_until_explicitly_allowed`, `a_restore_uses_the_client_sandbox_or_a_check_unless_allowed` | to capture |
| G3 Backup of selected tables only | todo: no tables option in the dialog (plans/M8.md Part 3, DBEAVER-PARITY CM-58) | | |

## H. Administration

The Administration tab (palette "Server Administration", the connection menu and the status bar; restored after a
restart) shows `Session::admin_query` in four sub-tabs. PostgreSQL, MySQL and MariaDB. Tests:
`crates/kerno/src/ui_tests.rs`, `crates/kerno/src/admin/tab.rs`, the drivers' `tests/introspect.rs`. Captures of
milestone 8 are not taken yet.

| Item | Status | Tests | Screenshot |
| ---- | ------ | ----- | ---------- |
| H1 Sessions / processes view: filter, refresh and auto-refresh while shown, Cancel query and Terminate session shown and confirmed, refused on a read-only connection and on the tab's own session | done | `the_sessions_list_cancels_and_terminates_what_the_test_opened`, `the_sessions_refresh_on_their_own_only_while_shown`, `a_read_only_connection_sends_no_signal`, `the_selected_session_stays_selected_through_a_refresh_and_a_sort`, `an_account_without_the_privileges_sees_what_the_server_refused`; drivers: `administrative_views` (PG, My, Ma), `another_sessions_statement_can_be_cancelled` | to capture |
| H2 Users and roles with their grants (read view) | done | `roles_and_their_grants_sizes_and_settings_are_listed`, `grants_show_the_selected_role_only` | to capture |
| H2 Database and table sizes | done | `roles_and_their_grants_sizes_and_settings_are_listed`, `sizes_show_for_people_and_sort_by_bytes` | to capture |
| H2 Server version and settings | done | `roles_and_their_grants_sizes_and_settings_are_listed`, `the_admin_tab_opens_from_the_status_bar_and_comes_back_after_a_restart`; `reports_the_server_and_database` (PG, My, Ma) | to capture |
| H1 Lock manager, hide idle sessions | todo: PARITY-GAPS G16 | | |

## Context menus (CLAUDE.md §3)

Every surface listed in §3 should answer a right-click. Entry lists are built by pure functions and unit-tested;
a real right-click test is named where one exists.

| Surface | Answers a right-click today | Test | Screenshot |
| ------- | --------------------------- | ---- | ---------- |
| Sidebar: empty area and header (New connection, New folder, Import connections, Refresh, Collapse all) | yes | `the_sidebar_answers_a_right_click_on_each_part`; list: `the_empty_area_and_a_folder_offer_what_dbeaver_does` | — |
| Sidebar: folder (New connection here, New subfolder, Rename, Delete) | yes | `the_sidebar_answers_a_right_click_on_each_part`, `a_folder_is_made_renamed_filled_by_drag_and_deleted` | — |
| Sidebar: connection (Connect, Disconnect, Reconnect, New SQL editor, Edit, Duplicate, Rename, Move to folder, Delete, Refresh, Copy) | yes; entries that do not apply are disabled with the reason | `the_sidebar_answers_a_right_click_on_each_part`; list: `a_connection_disables_what_does_not_apply_to_its_status`; `the_copied_connection_url_has_no_password`, `new_sql_editor_connects_first_then_opens_a_tab_on_the_connection` | `m3-sidebar-menu-light.png` |
| Navigator: below the tree (Refresh, Collapse all, New query here) | yes | `a_right_click_below_the_schema_tree_opens_its_menu` | — |
| Navigator: object rows (per kind of node) | yes; the entry lists are tested, no test right-clicks a row (plans/CTX.md acceptance) | `each_kind_of_node_gets_the_actions_that_make_sense_for_it`, `dropping_renaming_and_truncating_show_the_sql_and_ask`, `production_asks_twice_and_read_only_refuses` | `m4-navigator-menu-light.png`, `m4-navigator-menu-readonly-light.png` |
| Results grid and table data: cell (row menu: owner extras, copy formats) | yes | `a_right_click_on_a_cell_opens_the_menu_of_its_row` | `m6-row-menu-dark.png` |
| Results grid and table data: column header (Copy name, Order by, Filter on this column, Pin, Hide, Show all, Fit width, Fit all widths) | yes | `a_header_copies_orders_filters_and_fits_its_column`, `the_menu_of_a_column_header_orders_filters_and_fits` | — |
| Grid: cell-aware entries (Order by, Filter by this value, Exclude, Set NULL, Edit cell, Paste, Export…) | yes; what a cell does not allow is disabled with the reason | `a_result_cell_copies_orders_shows_and_exports_but_never_edits_or_filters`, `a_data_cell_adds_paste_filters_edits_and_the_row_in_dbeavers_order`, `what_a_cell_does_not_allow_is_disabled_with_the_reason`, `the_menu_of_a_result_cell_acts_on_the_selection_or_on_its_cell`, `filter_by_this_value_and_exclude_it_change_the_rows` | — |
| Tabs bar: a tab (Close, Close others, Close to the right, Close all, Rename, Duplicate) and the empty area (New query tab, Reopen closed tab) | yes | `tabs_answer_a_right_click_and_close_or_come_back_from_their_menus`, `a_tab_offers_what_dbeaver_does_and_says_what_does_not_apply`, `the_empty_tab_bar_reopens_only_what_was_closed` | — |
| Result tabs (Close, Close others, Close to the left / right, Close all, Pin, Rename, Copy statement) | yes | `result_tabs_close_pin_and_rename`, `a_result_tab_closes_pins_and_names_as_in_dbeaver` | — |
| SQL editor (Run statement, Run script, Explain, Format, Cut, Copy, Paste, Comment, Save) | yes | `the_editor_answers_a_right_click_with_its_commands`, `the_editor_menu_is_enabled_as_the_toolbar_is` | — |
| Status bar items (connection, database, transaction, read-only, rows, pending changes) | yes; the entry lists are tested, no test right-clicks an item | `each_status_item_has_its_own_actions_or_none`, `a_read_only_connection_explains_itself_in_the_status_bar`, `the_status_bar_menus_open_inside_the_window` | — |
| Drag and drop of connections between folders | yes | `a_folder_is_made_renamed_filled_by_drag_and_deleted` | — |

## I. App

| Item | Status | Tests | Screenshot |
| ---- | ------ | ----- | ---------- |
| I1 Command palette | done | `command_palette_finds_and_runs_commands` | `m2-tabs-light.png` (palette capture due in milestone 10) |
| I1 Settings view (theme, fonts, editor, grid page size, confirmations) | partial: appearance, page size, confirmations; fonts and editor options in milestone 10 | `appearance_and_sidebar_choices_are_applied_and_stored` | due in milestone 10 |
| I1 Keymap override file | done | `overrides_replace_defaults_per_command`, `problems_in_the_keymap_file_are_reported_not_fatal` | — |
| I2 macOS bundle, Linux build, platform dirs, redacted logs | todo (milestone 10); data dirs and log redaction exist (`storage.rs`, `logging.rs` tests) | | |

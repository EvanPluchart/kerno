# DBeaver parity inventory

GOAL criterion 2: what DBeaver Community offers in its navigator, SQL editor, results viewer, connections and
context menus for PostgreSQL and MySQL / MariaDB, and where Kerno stands on each. Built from DBeaver's
source, `plugin.xml` menu contributions included (brief: `docs/plans/PARITY.md`, Part 1). The work orders
for the gaps are in `docs/plans/PARITY-GAPS.md`; the rows moved out of v1 are in `docs/ROADMAP.md`.

## Method

**Source.** DBeaver Community, clone at `~/VM/refs/dbeaver`, commit `cc7e36f` (2026-11-12), Apache-2.0.
Each feature below is described in a line of our own words, with the plugin and the `commandId`, file or
preference it comes from. Plugin names drop the `org.jkiss.dbeaver.` prefix (`ui.navigator` is
`plugins/org.jkiss.dbeaver.ui.navigator`). Keys are quoted as `plugin.xml` writes them: `CTRL` is the Control
key, `M1` and `COMMAND` are Cmd on macOS. Nothing was copied but names of commands and preferences.

**Read.**

- `plugin.xml` and `OSGI-INF/l10n/bundle.properties` (commands, handlers, keys, menu contributions) of
  `ui.navigator`, `core` (the navigator popup groups, the Database menu, transaction menus), `ui.app.standalone`
  (Edit and File menus), `ui.editors.sql`, `ui.editors.data`, `ui.editors.connection`, `data.transfer.ui`,
  `ext.postgresql` and `ext.mysql` (the tree definitions), `ext.postgresql.ui` and `ext.mysql.ui` (tools,
  editors, session and lock managers, SQL generators).
- Java, as far as behaviour and defaults needed it: `NavigatorUtils.createContextMenu` /
  `addStandardMenuItem` (navigator popup), `NavigatorPreferences` and `UINavigatorPreferencesInitializer`
  (double click: object `EDIT`, connection `EXPAND`), `ResultSetViewer.fillContextMenu` and its
  `fillFiltersMenu`, `fillOrderingsMenu`, `fillNavigateMenu`, `fillEditMenu`, `fillLayoutMenu`,
  `fillColumnViewMenu`, `SpreadsheetPresentation` (header menu: pin, hide, fit), `FilterByAttributeType`,
  `ResultSetCopySettings`, the result-set preference initializer (auto-fetch on scroll on, double click edits
  inline, booleans as checkboxes), `ModelPreferences` (fetch size 200), `AutoRefreshControl` (1, 5, 10, 15,
  30, 60 s), `SQLEditorBase.editorContextMenuAboutToShow` (Format submenu), `SQLEditor` (result tab menu),
  the SQL editor preference initializer (connect on execute, replace current result tab, auto-save on
  close), `DBPConnectionType` (Development / Test / Production defaults), `DataSourceTransactionModeContributor`,
  `ConnectionViewSettingsContributor`, `UIConnectionMessages` and `SSHUIMessages` (connection and SSH pages),
  `PostgreToolBaseVacuum`, `PostgreToolTableTruncate`, `PostgreSessionEditor`, `MySQLSessionEditor`,
  `MySQLUserEditorGeneral`.
- Kerno: `crates/kerno/src/actions.rs` (every command), `navigator/actions.rs`, `navigator/view.rs`,
  `query/grid.rs`, `query/tab.rs`, `data/tab.rs`, `sidebar.rs`, `structure/`, `export_dialog.rs`,
  `import_dialog.rs`, `settings.rs`, `workspace.rs` (status bar), the test names of `ui_tests.rs`,
  `connections/manager_tests.rs` and the unit tests of the app modules, `docs/FEATURES.md` and every plan in
  `docs/plans/`. Committed code only (HEAD `cee8bd2`); uncommitted changes in `crates/` were ignored.

**Left out** (brief): Eclipse workbench plumbing (perspectives, views layout, workspace and project
resources, team / Git), PRO and Enterprise features, engines other than PostgreSQL and MySQL / MariaDB, the
internals of the data-transfer wizards, ERD, dashboards, AI, debuggers, task scheduler and mock data. Each
of those families has one roadmap row with its reason.

**Not read**: the entity editor's Java (`ui.editors.entity`, property pages are described from the
navigator model and the engine plugins), the hex / image / JSON / XML value editors, the SQL formatter and
completion engines, the preference pages beyond the defaults named above, the `keyboardOnlyKeyScheme`
bindings other than the panel toggles, and the Eclipse default key scheme itself: F2 rename, F5 refresh,
Delete and Ctrl+C in the navigator are Eclipse's default bindings of `org.eclipse.ui.edit.rename`,
`file.refresh`, `edit.delete` and `edit.copy`, which the `core` plugin contributes to the navigator popup;
they are cited that way.

**Status rules.**

- **done**: Kerno has the command, menu entry or file **and** a test covers it (the test name is given).
  For a context-menu entry, the entry list (a unit test of the function that fills the menu) or a real
  right-click must be tested.
- **done, untested**: it exists, no test covers it. Counted as a gap of kind "test", ordered in
  `PARITY-GAPS.md` Part G18.
- **planned**: a part of an existing plan builds it (`docs/plans/<file>` Part n).
- **gap**: in no plan; ordered in `PARITY-GAPS.md` (G1 to G17).
- **roadmap**: moved to `docs/ROADMAP.md` with its reason (Eclipse-specific, other engines, PRO, CLAUDE.md §4
  last paragraph, data transfer beyond §4.G, or a scope decision that follows from CLAUDE.md).
- CLAUDE.md wins over DBeaver's defaults: where the spec fixes a key or a default (Cmd+Enter, Cmd+Shift+Enter,
  a page size of 1,000, double click opening table data), the row is **done** and DBeaver's default is
  noted. A different key is a gap only when Kerno has no default key at all for a daily action.

**Menu order.** DBeaver builds one navigator popup for every node from contribution groups, in this order:
Edit (F4) and Create ▸; SQL Editor ▸; Connection view ▸; Filter ▸; View Data, Generate SQL ▸, Read data in
SQL console, Execute stored procedure; Compare / Migrate ▸; Export Data, Import Data; Tools ▸; Copy, Paste,
Copy Advanced Info, Delete, Rename; Refresh; Connect, Invalidate / Reconnect, Disconnect; Set as default on
databases and schemas. Each node kind shows the entries that apply to it; the context-menu tables list
them in that order. The result-grid menu follows `ResultSetViewer.fillContextMenu`.

## Connections

| ID | Feature (as DBeaver names it) | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CON-01 | New Database Connection | Wizard with a searchable driver grid, then the connection pages | ui.editors.connection `core.new.connection` (Ctrl+Shift+N) | PG, My | **done**: `new_connection` (Cmd+N), engine picker grid; `a_connection_is_filled_from_a_url_tested_saved_and_opened` |
| CON-02 | New Connection from JDBC URL | Creates a connection from a pasted URL | ui.editors.connection `core.new.connection.from.url` | PG, My | **done**: URL paste fills the form; `a_pasted_url_fills_the_fields_it_carries`, `a_connection_is_filled_from_a_url_tested_saved_and_opened` |
| CON-03 | Main page: host, port, database, user, password | Server address and credentials of the engine page | ext.postgresql.ui / ext.mysql.ui connection pages | PG, My | **done**: `connections/form.rs`; `a_saved_connection_round_trips_through_the_form`, `default_database_and_options` |
| CON-04 | Save password | Keep the password or ask at each connect | ui.editors.connection `dialog_connection_wizard_final_checkbox_save_password` | PG, My | **done**: Keychain or ask; `a_password_that_is_not_saved_is_asked_for`, `a_saved_connection_without_its_password_asks_and_then_keeps_it` |
| CON-05 | Driver properties | Free key/value options passed to the driver | ui.editors.connection `DriverPropertiesDialogPage` | PG, My | **done**: Extra driver options; `default_database_and_options`, `a_secret_typed_as_an_option_is_refused_without_echoing_it` |
| CON-06 | Connection name and connection type | Name plus Development / Test / Production type, each with a colour | ui.editors.connection `ConnectionPageGeneral`; model `DBPConnectionType` | PG, My | **done**: Name, environment (local / staging / production) and colour tag; `a_connection_is_filled_from_a_url_tested_saved_and_opened`, `a_saved_connection_round_trips_through_the_form` |
| CON-07 | Connection type defaults: auto-commit | Production starts in manual commit, Development and Test in auto-commit | model `DBPConnectionType` (PROD autocommit=false) | PG, My | **gap**: G13 |
| CON-08 | Connection type defaults: confirm execution and data changes | Production asks before running SQL and before saving edits; Test asks before saving edits | model `DBPConnectionType` (confirmExecute, confirmDataChange) | PG, My | **done**: Production asks twice before any write, edits always previewed (CLAUDE.md §1); `production_asks_twice_and_read_only_refuses`, `statements_that_destroy_or_write_are_guarded` |
| CON-09 | Edit connection types | User-defined types with their own colours and defaults | ui.editors.connection `PrefPageConnectionTypes` | PG, My | **roadmap**: Kerno has three fixed environments plus free colour tags (CLAUDE.md §4.A1); custom types add little |
| CON-10 | Connection folder | Folder chosen in the General page; nested folders in the navigator | ui.editors.connection `ConnectionPageGeneral`; core `core.new.folder` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 (folders, folder picker) |
| CON-11 | Read-only connection | Blocks writes in the client and opens read-only sessions | ui.editors.connection `dialog_connection_wizard_final_checkbox_connection_readonly` | PG, My | **done**: `read_only` flag; `read_only_sessions_refuse_writes`, `production_asks_twice_and_read_only_refuses` |
| CON-12 | Read-only toggle command | Flips read-only on the selected connection without opening the dialog | core `core.connection.readonly` (Database menu) | PG, My | **gap**: G13 |
| CON-13 | Default auto-commit per connection | Initialization page: start sessions in auto-commit or manual mode | ui.editors.connection `ConnectionPageInitialization` | PG, My | **gap**: G13 |
| CON-14 | Default isolation level | Initialization page: isolation level set at connect | ui.editors.connection `ConnectionPageInitialization` | PG, My | **roadmap**: Rarely changed; a `SET TRANSACTION` in bootstrap queries (G13) covers it |
| CON-15 | Default database | Database selected at connect | ui.editors.connection `ConnectionPageInitialization` | PG, My | **done**: Default database field; `default_database_and_options` |
| CON-16 | Default schema | Schema made current at connect (search path / USE) | ui.editors.connection `ConnectionPageInitialization` | PG | **gap**: G13 |
| CON-17 | Bootstrap queries | Statements run right after each connect, with an "ignore errors" option | ui.editors.connection `EditBootstrapQueriesDialog` | PG, My | **gap**: G13 |
| CON-18 | Keep-alive (database) | Runs a ping statement every N seconds; 0 (off) by default | ui.editors.connection `dialog_connection_wizard_final_label_keepalive` | PG, My | **roadmap**: Off by default in DBeaver; Kerno re-establishes a dropped connection (`a_lost_connection_is_re_established`) |
| CON-19 | Close idle connection after N seconds | Disconnects an idle connection (type default 4 h for Development, 1 h for Production) | ui.editors.connection `dialog_connection_wizard_final_label_close_idle_connections`; model `DBPConnectionType` | PG, My | **roadmap**: Resource saving, not a daily need; needs an idle timer per connection |
| CON-20 | Auto-close idle transactions | Rolls back a transaction left open too long (Production: 10 min) | model `DBPConnectionType` (autoCloseTransactions) | PG, My | **roadmap**: Safety net for manual mode; Kerno already asks before closing a tab with an open transaction (M5 Part 4) |
| CON-21 | SSL / TLS page | Modes, CA, client certificate and key, per engine | ui.editors.connection `SSLConfiguratorTrustStoreUI`; engine SSL configurators | PG, My | **done**: `tls_modes_without_verification`, `tls_verification`, `client_certificate_authentication` |
| CON-22 | SSH tunnel: host, port, user, password / key / agent | Tunnel with password, private key (+ passphrase) or agent socket | net.ssh.ui `SSHTunnelDefaultConfiguratorUI` | PG, My | **done**: `a_tunnelled_connection_asks_for_trust_then_for_each_missing_secret`, `an_encrypted_ssh_key_asks_for_its_passphrase`, `through_the_ssh_tunnel` |
| CON-23 | SSH: save credentials | Keep SSH secrets or ask | net.ssh.ui `model_ssh_configurator_checkbox_save_pass` | PG, My | **done**: Secrets in the keychain or asked; `a_tunnelled_connection_asks_for_trust_then_for_each_missing_secret` |
| CON-24 | SSH: keep-alive interval | Keep-alive packets on the tunnel | net.ssh.ui `model_ssh_configurator_label_keep_alive` | PG, My | **done, untested** (gap of kind test, G18): `SshConfig::keepalive_secs` passed to the SSH client (`kerno-core/src/ssh.rs`); no test checks it (G18) |
| CON-25 | SSH: host key verification | Known-hosts check; option to bypass it | net.ssh.ui `model_ssh_configurator_label_bypass_verification` | PG, My | **done**: Fingerprint shown and trusted on first use, never bypassed; `an_unknown_ssh_host_is_shown_before_it_is_trusted` |
| CON-26 | SSH: jump servers | Chain of SSH hosts before the target | net.ssh.ui `model_ssh_configurator_label_jump_server_edit_hint` | PG, My | **roadmap**: Beyond CLAUDE.md §4.A4 (one tunnel); bastion chains are uncommon |
| CON-27 | SSH: local / remote host and port overrides, connect timeout | Fixed local port, other remote endpoint, tunnel timeout | net.ssh.ui `model_ssh_configurator_label_local_port`, `..._tunnel_timeout` | PG, My | **roadmap**: Automatic local port is enough for a client; rare settings |
| CON-28 | SSH: share tunnel between connections | One tunnel reused by several connections | net.ssh.ui `model_ssh_configurator_label_share_tunnels` | PG, My | **roadmap**: Optimisation only |
| CON-29 | SOCKS proxy and network profiles | Proxy handler and reusable network settings | ui.editors.connection `SocksProxyConfiguratorUI`, `PrefPageNetworkProfiles` | PG, My | **roadmap**: Not in CLAUDE.md §4.A; enterprise networks only |
| CON-30 | Test connection | Connects once and reports server version and timing | ui.editors.connection `ConnectionTestDialog` | PG, My | **done**: `test_connection_reports_version_and_latency` |
| CON-31 | Connect / Disconnect | Opens or closes the connection | core `core.connect`, `core.disconnect` | PG, My | **done**: `connects_queries_and_disconnects`, `a_failed_connection_says_why` |
| CON-32 | Invalidate / Reconnect | Drops and re-opens the connection | core `core.invalidate` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 (Reconnect) |
| CON-33 | Disconnect All | Closes every open connection | core `core.disconnectAll` | PG, My | **gap**: G13 |
| CON-34 | Disconnect Others | Closes every connection but the selected one | core `core.disconnectOther` | PG, My | **gap**: G13 |
| CON-35 | Auto-reconnect | Re-opens a connection lost under a running session | model (connection recovery) | PG, My | **done**: `a_lost_connection_is_re_established` |
| CON-36 | Edit connection | Opens the connection pages for an existing connection | ui.navigator `core.object.open` on a connection (F4) | PG, My | **done**: Sidebar "Edit…"; `a_saved_connection_round_trips_through_the_form` |
| CON-37 | Duplicate (copy / paste) connection | Copies a connection under a new name | core `org.eclipse.ui.edit.copy` / `paste` on connections | PG, My | **done**: `duplicate_import_and_delete` |
| CON-38 | Delete connection | Removes the saved connection after asking | core `org.eclipse.ui.edit.delete` | PG, My | **done**: `deleting_a_connection_asks_first` |
| CON-39 | Rename connection | F2 on the connection | core `org.eclipse.ui.edit.rename` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 (Rename) |
| CON-40 | Import / export connections | Moves connection definitions between installs | core `org.eclipse.ui.file.import` / `export` (project import) | PG, My | **done**: JSON without secrets; `connections_are_exported_without_secrets_and_imported_back` |
| CON-41 | Transaction mode menu | Auto-commit, manual commit, smart commit and isolation level for the active session | core `DataSourceTransactionModeContributor` | PG, My | **done**: Per-tab auto / manual mode; `manual_commit_keeps_changes_in_a_transaction_until_it_is_ended` |
| CON-42 | Smart commit mode | Switches to manual mode when a modifying statement runs, back to auto after commit | ui.editors.connection `action_menu_transaction_smart_auto_commit` | PG, My | **roadmap**: Off by default in DBeaver; manual mode per tab covers the need |
| CON-43 | Commit / Rollback | Ends the transaction of the active session | core `core.commit` (Ctrl+4), `core.rollback` (Ctrl+8) | PG, My | **done**: Toolbar Commit / Rollback; `manual_commit_keeps_changes_in_a_transaction_until_it_is_ended` |
| CON-44 | Commit / Rollback shortcuts | Keys for commit and rollback | core key bindings of `core.commit` / `core.rollback` | PG, My | **planned**: `docs/plans/M10.md` Part 3 (commit, rollback, toggle commit mode commands) |
| CON-45 | Transaction log | Statements run in the current transaction, with time and result | core `core.txn.log` | PG, My | **gap**: G14 |
| CON-46 | Pending transactions | Every connection with an open transaction, across the app | core `core.txn.pending` | PG, My | **roadmap**: Kerno shows the open transaction per tab in the status bar; a cross-app list is rarely needed |
| CON-47 | Connection type colour on editors | Editor area and toolbars tinted with the connection type colour | ui `CSSUtils.markConnectionTypeColor` | PG, My | **gap**: G13 |
| CON-48 | Change user password | Changes the password of the connected user | ui.editors.connection `connection.changeCurrentPassword` | PG, My | **roadmap**: An `ALTER USER` in the editor does it; rare |
| CON-49 | Shell commands on connection events | Runs local commands before / after connect | ui.editors.connection `ConnectionPageShellCommands` | PG, My | **roadmap**: Runs local programs from a connection; outside CLAUDE.md §4.A and a security surface |
| CON-50 | Driver manager, change connection driver | Edits JDBC drivers; migrates a connection to another driver | ui.editors.connection `core.driver.manager`, `core.migrate.connection` | PG, My | **roadmap**: Kerno has native drivers, no JDBC (Eclipse / JDBC plumbing) |
| CON-51 | Connection permissions, lock password, project sync | Team and project features | ui.editors.connection `EditConnectionPermissionsDialog`; core `core.connection.synchronize` | PG, My | **roadmap**: Eclipse project / team features |

## Navigator

| ID | Feature (as DBeaver names it) | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| NAV-01 | Tree: databases, schemas, object folders (PostgreSQL) | Databases, schemas, tables, views, materialized views, functions, procedures, sequences, types, extensions | ext.postgresql plugin.xml `dataSourceProvider` tree | PG | **done**: `the_schema_tree_opens_level_by_level`, `databases_then_schemas_then_folders_of_objects` |
| NAV-02 | Tree: databases and object folders (MySQL) | Databases, tables, views, procedures / functions, triggers, events | ext.mysql plugin.xml tree | My | **done**: `mysql_and_mariadb_list_objects_under_their_databases`, `mysql_databases_hold_the_objects_directly` |
| NAV-03 | Columns and indexes under a table | Columns and Indexes folders of each table | ext.postgresql / ext.mysql tree (`attribute`, `index`) | PG, My | **done**: `a_table_loads_its_columns_and_indexes_once` |
| NAV-04 | Constraints folder under a table | Primary, unique and check constraints with their columns | ext.postgresql / ext.mysql tree (`uniqueKey`, `constraint`) | PG, My | **gap**: G10 |
| NAV-05 | Foreign Keys folder under a table | Outgoing foreign keys with their columns | ext.postgresql / ext.mysql tree (`association`) | PG, My | **gap**: G10 |
| NAV-06 | References folder under a table | Foreign keys of other tables pointing at this one | ext.postgresql / ext.mysql tree (`references`) | PG, My | **gap**: G10 |
| NAV-07 | Triggers folder under a table | Triggers of that table (PostgreSQL and MySQL) | ext.postgresql / ext.mysql tree (`trigger` under table) | PG, My | **gap**: G10 |
| NAV-08 | Partitions and child tables | Partitions and inheritance children under a table | ext.postgresql tree (`partition`, `subTables`); ext.mysql (`partition`) | PG, My | **roadmap**: Partitions are listed as tables of the schema; a nested view is a nice-to-have |
| NAV-09 | Rules, policies, dependencies | PostgreSQL rewrite rules, row-level policies, dependency lists | ext.postgresql tree (`rule`, `policy`, `dependency`) | PG | **roadmap**: Rare in daily use; visible through Show DDL and the catalogs |
| NAV-10 | Aggregates, event triggers, foreign data wrappers, publications, subscriptions, tablespaces | Other PostgreSQL object folders | ext.postgresql tree | PG | **roadmap**: Not in CLAUDE.md §4.B1 tree list; specialised features |
| NAV-11 | Users / Roles folder | Roles (PostgreSQL) or users with grants (MySQL) in the tree | ext.postgresql (`PostgreRole`), ext.mysql (`MySQLUser`, `MySQLGrant`) | PG, My | **planned**: `docs/plans/M8.md` Part 4 (Roles and grants in the administration tab) |
| NAV-12 | Administer folder: Session Manager, Lock Manager | Connection-level editors opened from the tree | ext.postgresql.ui / ext.mysql.ui `editorContribution category="connectionEditor"` | PG, My | **planned**: `docs/plans/M8.md` Part 4 (session manager); the lock manager is a gap: TOOL-14, G16 |
| NAV-13 | System Info folder | PostgreSQL settings; MySQL status, variables, engines, charsets, collations | ext.postgresql (`PostgreSetting`), ext.mysql (`MySQLInformation`) | PG, My | **planned**: `docs/plans/M8.md` Part 4 (Settings); status variables: TOOL-17 |
| NAV-14 | Lazy loading and Refresh (F5) | Children read on expand; Refresh re-reads the node | core `org.eclipse.ui.file.refresh` in the navigator popup | PG, My | **done**: Refresh entry; `refresh_reloads_the_nearest_node_that_owns_its_children` |
| NAV-15 | Filter field | Filters the tree by name | ui.navigator `DatabaseNavigatorTree` filter; `core.navigator.filter.focus` | PG, My | **done**: `the_filter_narrows_the_tree_and_clearing_it_restores_it`, `the_filter_keeps_what_matches_and_the_way_to_it` |
| NAV-16 | Focus navigator filter command | Keyboard command that jumps to the filter field | ui.navigator `core.navigator.filter.focus` | PG, My | **planned**: `docs/plans/M10.md` Part 3 (focus navigator filter) |
| NAV-17 | Filter by object type | Filter applies to connections, containers or tables | ui.navigator `navigator.filter.object.type` | PG, My | **roadmap**: The filter keeps matches and their path; type scoping is rarely used |
| NAV-18 | Show connected only | Hides disconnected connections | ui.navigator `navigator.filter.connected` | PG, My | **roadmap**: Small list of connections in a client; folders (CTX Part 1) organise them |
| NAV-19 | Object filters: hide, show only selected, configure, toggle, clear | Persistent include / exclude masks per connection | core `core.object.filter.add.exclude` / `.include` / `.config` / `.toggle` / `.clear` | PG, My | **gap**: G11 |
| NAV-20 | Collapse All | Folds the whole tree | core `org.eclipse.ui.navigate.collapseAll` (navigator view menu) | PG, My | **planned**: `docs/plans/CTX.md` Part 1 (Collapse all) |
| NAV-21 | Link with editor | Selects in the tree the object of the active editor | ui.navigator `core.navigator.linkeditor` (Ctrl+Shift+,) | PG, My | **gap**: G10 |
| NAV-22 | Statistics hints (row count, size) | Size and row estimates beside objects | ui.navigator pref `NAVIGATOR_SHOW_STATISTICS_INFO` (true) | PG, My | **done**: `counts_and_sizes_are_compact`, `databases_then_schemas_then_folders_of_objects` |
| NAV-23 | Object descriptions in the tree | Comments shown beside names | ui.navigator pref `NAVIGATOR_SHOW_OBJECTS_DESCRIPTION` (false) | PG, My | **roadmap**: Off by default in DBeaver |
| NAV-24 | Show system objects | Lists system schemas and tables | ui.editors.connection `dialog_connection_wizard_final_checkbox_show_system_objects` | PG, My | **roadmap**: System catalogs are reachable from the editor; the tree hides them by design |
| NAV-25 | Double click on an object | Opens the object editor (default `EDIT`, last used page) | ui.navigator `NavigatorHandlerObjectOpen`, pref `NAVIGATOR_OBJECT_DOUBLE_CLICK` | PG, My | **done**: Owner's rule: tables open their data, per setting (CLAUDE.md §4.B1); `a_double_click_or_enter_opens_what_the_row_is_for`, `a_double_click_opens_what_the_node_is_for` |
| NAV-26 | Double click on a connection | Connects if needed and expands it (default `EXPAND`) | ui.navigator pref `NAVIGATOR_CONNECTION_DOUBLE_CLICK` | PG, My | **gap**: G1 (Kerno disconnects a connected connection on double click) |
| NAV-27 | Enter on a node | Same as double click | ui.navigator `NavigatorHandlerObjectOpen` | PG, My | **done**: `a_double_click_or_enter_opens_what_the_row_is_for` |
| NAV-28 | Arrows expand / collapse, click selects | Tree keyboard navigation | ui.navigator `DatabaseNavigatorTree` | PG, My | **done**: `a_click_selects_a_row_and_the_chevron_or_the_arrows_open_it` |
| NAV-29 | F4 Edit object | Opens the object editor; on a connection, the connection dialog | ui.navigator `core.object.open` (F4) | PG, My | **gap**: G1 |
| NAV-30 | F2 Rename | Renames the selected object | core `org.eclipse.ui.edit.rename` | PG, My | **gap**: G1 (menu entry exists, no key) |
| NAV-31 | Delete key | Drops the selected object after the SQL preview | core `org.eclipse.ui.edit.delete` | PG, My | **gap**: G1 (menu entry exists, no key) |
| NAV-32 | Copy (Ctrl+C) | Copies the names of the selected objects | core `org.eclipse.ui.edit.copy` | PG, My | **gap**: G1 (menu entry exists, no key) |
| NAV-33 | Copy Advanced Info (Ctrl+Shift+C) | Copies fully qualified names | core `core.edit.copy.special` ("Copy Advanced Info") | PG, My | **gap**: G1 |
| NAV-34 | Multiple selection | Several objects selected for drop, export, generate | ui.navigator tree (SWT multi-select) | PG, My | **roadmap**: Single selection covers daily use; multi-object actions multiply confirmation flows |
| NAV-35 | Drag an object into the SQL editor | Drops the object name at the cursor | ui.navigator `NavigatorUtils` drag source | PG, My | **roadmap**: Completion inserts names; drag between GPUI views needs a custom drop target in the editor |
| NAV-36 | Set as default (Ctrl+Shift+A) | Makes a schema or database the active one of the connection | ui.navigator `core.navigator.set.default` | PG, My | **gap**: G1 |
| NAV-37 | Open Database Object (Ctrl+Shift+D) | Search dialog over all objects of the connections | core `core.object.goto` | PG, My | **planned**: `docs/plans/M10.md` Part 3 (Quick open Cmd+P; relations only) |
| NAV-38 | Bookmarks | Bookmark a node and jump back to it | core `core.navigator.bookmark.add` / `.navigate` | PG, My | **roadmap**: Workspace / project feature (Eclipse resources) |
| NAV-39 | Browse from here | New navigator view rooted at a node | ui.navigator `core.navigator.open.browser` | PG, My | **roadmap**: Eclipse view plumbing |
| NAV-40 | Projects, active project, resources, scripts folder | Workspace projects and their files | ui.navigator `core.project.*`, `core.resource.*` | PG, My | **roadmap**: Eclipse workspace / project resources (left out by the brief) |
| NAV-41 | Create ▸ (new object of the folder kind) | Creates a child object; Cmd+N in the navigator on macOS | ui.navigator `core.object.create`, `NavigatorHandlerObjectCreateNew` | PG, My | **done**: Create table, view, schema, database, enum; `a_table_is_created_from_the_form`, `dropping_renaming_and_truncating_show_the_sql_and_ask`, `enums_are_created_and_their_values_added_and_renamed` |
| NAV-42 | Create function, procedure, sequence, trigger, event | New-object templates for the other folders | ui.navigator `NavigatorHandlerObjectCreateNew` (per-engine managers) | PG, My | **gap**: G10 |
| NAV-43 | New Index / Constraint from selected columns | Creates an index or key on the columns selected in the tree | ui.navigator `navigator.create.column.index`, `.constraint` | PG, My | **roadmap**: The table form adds indexes and keys (M7 Part 3); needs multi-selection (NAV-34) |
| NAV-44 | Drop with cascade | Delete dialog offers a cascade option and shows the script | ui.navigator `NavigatorObjectsDeleter` (cascade checkbox of the delete confirmation) | PG, My | **gap**: G10 |
| NAV-45 | View Data | Opens the data of a table or view | ui.editors.data `ui.editors.data.forSelection` | PG, My | **done**: Open data; `a_table_opened_from_the_tree_comes_back_after_a_restart`, `a_table_is_read_in_pages_sorted_and_filtered_by_the_server` |
| NAV-46 | Read data in SQL console | Opens a console with a SELECT of the table and runs it | ui.editors.sql `core.sql.editor.forSelection` | PG, My | **gap**: G7 |
| NAV-47 | SQL Editor ▸ Open / Recent / New script / Open console | Script or console bound to the selected node; keys F3 / Ctrl+[, Ctrl+], Ctrl+Alt+Enter | ui.editors.sql `core.sql.editor.open`, `.recent`, `.create`, `.console` | PG, My | **done**: New query here; `statements_and_ddl_open_in_a_tab_and_run_nothing`, `a_query_result_is_exported_by_running_its_statement_again` |
| NAV-48 | Keys for a new SQL editor on the selected node | Ctrl+] / Ctrl+Alt+Enter open an editor bound to the node | ui.editors.sql key bindings of `core.sql.editor.create` / `.console` | PG, My | **gap**: G1 |
| NAV-49 | Generate SQL ▸ SELECT, INSERT, UPDATE, DELETE | Statement templates for a table | ui.editors.sql `sqlGenerator` tableSelect / tableInsert / tableUpdate / tableDelete | PG, My | **done**: `statements_and_ddl_open_in_a_tab_and_run_nothing`, driver `tests/generate.rs` |
| NAV-50 | Generate SQL ▸ DDL | The object's DDL in an editor | ui.editors.sql `sqlGenerator` tableDDL | PG, My | **done**: Show DDL; `statements_and_ddl_open_in_a_tab_and_run_nothing` |
| NAV-51 | Generate SQL ▸ MERGE, JOIN, INNER JOIN | Merge statement; join of two selected tables | ui.editors.sql `sqlGenerator` tableMerge / tableJoin / tableInnerJoin | PG, My | **roadmap**: JOIN needs multi-selection (NAV-34); completion proposes joins on foreign keys already |
| NAV-52 | Generate SQL ▸ INSERT ON CONFLICT, UPDATE FROM, DELETE USING | PostgreSQL-specific templates | ext.postgresql `sqlGenerator` | PG | **roadmap**: PostgreSQL-only variants of templates that exist |
| NAV-53 | Generate SQL ▸ CALL; Execute stored procedure | Call template of a routine with its parameters, optionally run | ui.editors.sql `sqlGenerator` procedureCall, `core.procedure.execute` | PG, My | **gap**: G7 |
| NAV-54 | Export Data / Import Data | Data transfer wizards from the tree | data.transfer.ui `core.export.data`, `core.import.data` | PG, My | **done**: Export…, Import CSV…; `a_filtered_table_is_exported_to_csv_and_json_through_the_dialog`, `a_csv_file_is_imported_into_an_existing_table_and_into_a_new_one` |
| NAV-55 | Rename | Renames an object with a preview | core `org.eclipse.ui.edit.rename` | PG, My | **done**: `dropping_renaming_and_truncating_show_the_sql_and_ask` |
| NAV-56 | Delete (drop) | Drops with SQL preview and confirmation | core `org.eclipse.ui.edit.delete` | PG, My | **done**: `dropping_renaming_and_truncating_show_the_sql_and_ask`, `production_asks_twice_and_read_only_refuses` |
| NAV-57 | Compare / Migrate | Simple schema compare | cmp.simple.ui | PG, My | **roadmap**: Schema compare is outside v1 (CLAUDE.md §4, last paragraph) |
| NAV-58 | View Diagram | ER diagram of a table or schema | ui.editors.erd | PG, My | **roadmap**: ERD is outside v1 (CLAUDE.md §4, last paragraph) |
| NAV-59 | Connection view ▸ (simple / advanced / custom) | Per-connection tree presets and display toggles | core `ConnectionViewSettingsContributor` | PG, My | **roadmap**: One tree layout; system objects (NAV-24) and descriptions (NAV-23) are on the roadmap separately |

## SQL editor

| ID | Feature (as DBeaver names it) | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| SQL-01 | Open SQL console / new script for a connection | Editor bound to a connection, with its own session | ui.editors.sql `core.sql.editor.console`, `.create` | PG, My | **done**: `new_query_tab` (Cmd+T); `tabs_open_switch_and_close_from_the_keyboard` |
| SQL-02 | Separate connection per editor | Each editor opens its own session (default on) | ui.editors.sql `SQLEditorHandlerSeparateConnectionSwitch` | PG, My | **done**: Each query tab has its own session; `manual_commit_keeps_changes_in_a_transaction_until_it_is_ended` |
| SQL-03 | Active connection / schema selectors (Ctrl+9 / Ctrl+0) | Choose the editor's connection and schema | ui.navigator `ui.tools.select.connection`, `.select.schema` | PG, My | **done, untested** (gap of kind test, G18): Toolbar selectors `tab-connection`, `tab-database`, `tab-schema` in `query/tab.rs`; no test clicks them (G18); keys: G15 |
| SQL-04 | Connect on execute | Running in an editor of a disconnected connection connects first (default on) | ui.editors.sql pref `EDITOR_CONNECT_ON_EXECUTE` (true) | PG, My | **gap**: G4 (Kerno says what to do instead: `a_tab_without_an_open_connection_says_what_to_do`) |
| SQL-05 | Execute SQL statement (Ctrl+Enter) | Runs the statement at the cursor or the selection | ui.editors.sql `run.statement` | PG, My | **done**: `run_statement` (Cmd+Enter); `statements_run_and_their_rows_fill_the_grid` |
| SQL-06 | Execute SQL in new tab (`Ctrl+\`) | Runs and keeps the previous results in their tabs | ui.editors.sql `run.statementNew` | PG, My | **gap**: G4 (each run replaces the result tabs) |
| SQL-07 | Execute script (Alt+X) | Runs every statement, one result tab per statement | ui.editors.sql `run.script` | PG, My | **done**: `run_all` (Cmd+Shift+Enter per CLAUDE.md §3); `statements_run_and_their_rows_fill_the_grid` |
| SQL-08 | Execute queries in separate tabs | Script with one result tab per query | ui.editors.sql `run.scriptNew` | PG, My | **done**: Run all always gives one tab per statement; `statements_run_and_their_rows_fill_the_grid` |
| SQL-09 | Execute script from position (Alt+P) | Runs the script from the cursor to the end | ui.editors.sql `run.scriptFromPosition` | PG, My | **gap**: G4 |
| SQL-10 | Execute script natively (Alt+N) | Runs the script through psql / mysql | ui.editors.sql `core.sql.script.run.scriptNative` | PG, My | **roadmap**: The client tools are used for backup / restore (M8 Part 3); running editor scripts through them adds little |
| SQL-11 | Cancel active query | Cancels the running statement | ui.editors.sql `cancel.query` | PG, My | **done**: `cancel_query` (Cmd+.); `a_running_statement_is_cancelled_and_the_window_stays_usable` |
| SQL-12 | Select row count | Runs a count of the query under the cursor | ui.editors.sql `run.count` | PG, My | **gap**: G4 |
| SQL-13 | Select all rows | Runs without the fetch-size limit | ui.editors.sql `run.all.rows` | PG, My | **done**: Fetch all; `a_large_result_arrives_a_page_at_a_time` |
| SQL-14 | Evaluate SQL expression | SELECTs the selected expression | ui.editors.sql `run.expression` | PG, My | **roadmap**: Selecting `select <expr>` and running it does the same |
| SQL-15 | Export from query | Exports the full result of the query under the cursor | ui.editors.sql `export.data` | PG, My | **done**: Result "Export…"; `a_query_result_is_exported_by_running_its_statement_again` |
| SQL-16 | Explain execution plan (Ctrl+Shift+E) | Plan tree of the statement | ui.editors.sql `run.explain` | PG, My | **done**: `explain_statement` (Cmd+E), `explain_analyze`; `plans_are_shown_as_a_tree_on_every_engine` |
| SQL-17 | Load execution plan | Opens a saved plan file | ui.editors.sql `load.plan` | PG, My | **roadmap**: Plan files from elsewhere are rare; raw output can be pasted |
| SQL-18 | Next / previous query (Alt+Down / Alt+Up) | Moves the cursor between statements | ui.editors.sql `query.next`, `query.prev` | PG, My | **gap**: G4 |
| SQL-19 | Result tabs: close, close others / left / right / all, pin, rename, detach | Menu of the result tabs; pinned tabs survive the next run (Ctrl+Shift+P pins) | ui.editors.sql `SQLEditor` result tab menu, `toggle.pinned.tab`, `close.tab` | PG, My | **gap**: G4 |
| SQL-20 | Multiple results per tab | All results in one scrollable tab | ui.editors.sql `multipleResultsPerTab` | PG, My | **roadmap**: Layout preference; one tab per statement is the default in both apps |
| SQL-21 | Toggle / maximize results panel, switch panel | Hides, maximises, or focuses the results area | ui.editors.sql `toggle.result.panel`, `maximize.result.panel`, `switch.panel` | PG, My | **gap**: G4 |
| SQL-22 | Toggle editor layout | Results beside or below the editor | ui.editors.sql `toggleLayout` | PG, My | **roadmap**: Layout preference |
| SQL-23 | Server output panel | Notices and warnings from the server | ui.editors.sql `show.output` | PG, My | **done**: Messages tab; `messages_errors_and_history_of_a_run` |
| SQL-24 | Execution log | Queries run from the editor with time and status | ui.editors.sql `show.log`; core query manager | PG, My | **done**: `show_history` (Cmd+Y); `history_and_saved_queries_are_searched_and_reopened` |
| SQL-25 | Query parameters (`:name`, `?`) | Prompted before run | ui.editors.sql `SQLQueryParameterBindDialog` | PG, My | **done**: `parameters_are_asked_for_before_a_statement_runs` |
| SQL-26 | Script variables and commands (`@set`, `@include`, `@echo`, `@export`, variables panel) | Client-side script commands | ui.editors.sql `sqlCommand` set / unset / echo / include / export, `show.variables` | PG, My | **roadmap**: DBeaver-specific script language; not standard SQL |
| SQL-27 | Outline | Statement outline of the script | ui.editors.sql `show.outline` | PG, My | **roadmap**: Nice-to-have for long scripts |
| SQL-28 | Open / save script files | Load and save `.sql` files | ui.editors.sql `open.file`, `save.file` | PG, My | **done**: `open_sql_file`, `save_file`, `save_file_as`; `sql_files_are_opened_saved_and_marked_when_changed` |
| SQL-29 | Rename SQL script (Ctrl+F2) | Renames the editor / script | ui.editors.sql `rename` | PG, My | **planned**: `docs/plans/CTX.md` Part 2 (tab Rename) |
| SQL-30 | Delete this script, revert file | Script file housekeeping | ui.editors.sql `deleteThisScript`; Eclipse `file.revert` | PG, My | **roadmap**: Scripts as workspace files are Eclipse project resources; Kerno keeps tabs, files and saved queries |
| SQL-31 | Restore editors after restart, auto-save | Editors and unsaved text come back | ui.editors.sql pref `AUTO_SAVE_ON_CLOSE` (true) | PG, My | **done**: `unsaved_text_and_tabs_come_back_after_a_restart`, `the_active_tab_is_active_again_after_a_restart` |
| SQL-32 | Toggle line comment (Ctrl+/) | Comments or uncomments the selected lines | ui.editors.sql `comment.single` | PG, My | **done**: `toggle_comment` (Cmd+/); `the_comment_shortcut_toggles_the_lines_of_the_selection` |
| SQL-33 | Toggle block comment (Ctrl+Shift+/) | Wraps the selection in `/* */` | ui.editors.sql `comment.multi` | PG, My | **gap**: G15 |
| SQL-34 | Format SQL (Ctrl+Shift+F) | Formats the selection or the script | ui.editors.sql `text.content.format` | PG, My | **done**: `format_sql` (Cmd+Shift+I); `format_sql_rewrites_the_selection_or_everything_as_one_undo_step` |
| SQL-35 | Format ▸ Upper case / Lower case | Changes the case of the selection | ui.editors.sql `SQLEditorBase` format menu (Eclipse upperCase / lowerCase) | PG, My | **gap**: G15 |
| SQL-36 | Format ▸ Morph to delimited list, trim spaces | Text transforms of the selection | ui.editors.sql `morph.delimited.list`, `trim.spaces` | PG, My | **roadmap**: Text utilities, not SQL client features |
| SQL-37 | Toggle word wrap | Soft wrap in the editor | ui.editors.sql `word.wrap` | PG, My | **planned**: `docs/plans/M10.md` Part 3 (soft wrap) |
| SQL-38 | Auto-activated completion | Proposals while typing, tables, columns, joins | ui.editors.sql pref `ENABLE_AUTO_ACTIVATION` (true) | PG, My | **done**: `completion_proposes_tables_columns_and_joins`, `typing_in_the_editor_opens_completion_and_enter_accepts_it` |
| SQL-39 | Content assist on demand (Ctrl+Space) | Opens the proposal list | ui.editors.sql `SQLEditorContributor` content assist | PG, My | **planned**: `docs/plans/M5.md` (left over from Part 2: Ctrl+Space) |
| SQL-40 | Templates | Short names expanded to statements | ui.editors.sql `assist.templates` | PG, My | **roadmap**: Saved queries cover reusable SQL |
| SQL-41 | Open declaration (F4 / Ctrl+click) | Opens the object under the cursor | ui.editors.sql `navigate.object` | PG, My | **gap**: G15 |
| SQL-42 | Bracket and quote auto-closing | Closes brackets, quotes and comments | ui.editors.sql prefs `SQLEDITOR_CLOSE_*` (true) | PG, My | **done, untested** (gap of kind test, G18): The kit's editor closes brackets (M5 done list); no test (G18) |
| SQL-43 | Go to / select to matching bracket | Jumps between brackets | ui.editors.sql `gotoMatchingBracket`, `selectToMatchingBracket` | PG, My | **roadmap**: Editor-kit feature; low use |
| SQL-44 | Foldings | Collapse blocks of the script | ui.editors.sql `FoldingsEnabled`, `ExpandAllFoldings` | PG, My | **roadmap**: Depends on the kit's editor |
| SQL-45 | Find / replace | Search in the script | Eclipse `findReplace` in the editor | PG, My | **done**: `find_in_editor`, `replace_in_editor`; `the_editor_searches_and_replaces` |
| SQL-46 | Multi-cursor | Several carets | Eclipse text editor | PG, My | **done, untested** (gap of kind test, G18): The kit's editor (Cmd+Alt+Up / Down, M5 done list); no test (G18) |
| SQL-47 | Error position | Error shown with its position in the script | ui.editors.sql `SQLEditor` error handling | PG, My | **done**: `messages_errors_and_history_of_a_run` |
| SQL-48 | Copy selected query | Copies the statement under the cursor | ui.editors.sql `copy.query` | PG, My | **gap**: G15 |
| SQL-49 | Advanced copy in the editor | Copies with formatting | core `core.edit.copy.special` in the editor | PG, My | **roadmap**: Plain copy is enough for code |
| SQL-50 | Search in web | Searches the selection in a browser | ui.editors.sql `search.web` | PG, My | **roadmap**: Sends text off the machine (CLAUDE.md §1 "Nothing leaves the machine") |
| SQL-51 | Set connection from navigator, auto-sync | The editor follows the navigator selection | ui.editors.sql `sync.connection`, `sync.auto` | PG, My | **roadmap**: Off by default; "New query here" binds a tab to a node |
| SQL-52 | Refresh current schema / all schemas | Reloads the metadata used by completion | ui.editors.sql `refresh.current.schema`, `refresh.all.schemas` | PG, My | **gap**: G15 |
| SQL-53 | Disable SQL syntax parser | Turns off parsing for huge scripts | ui.editors.sql `disableSQLSyntaxParser` | PG, My | **roadmap**: Kerno parses only the statement at the cursor (M9 checks large scripts), so there is nothing to turn off |
| SQL-54 | Saved scripts and query history | Scripts folder and query manager | ui.editors.sql `core.sql.editor.showScripts`; core query manager | PG, My | **done**: Saved queries and history; `history_and_saved_queries_are_searched_and_reopened` |

## Results and data editing

| ID | Feature (as DBeaver names it) | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| RES-01 | Grid presentation | Virtual grid of the result | ui.editors.data `SpreadsheetPresentation` | PG, My | **done**: `statements_run_and_their_rows_fill_the_grid` |
| RES-02 | Record mode (Tab) | One row as a list of column / value pairs, with row navigation | ui.editors.data `core.resultset.toggleMode` (Tab) | PG, My | **gap**: G3 |
| RES-03 | Text presentation, switch presentation | Result as aligned plain text | ui.editors.data `core.resultset.switchPresentation`, `plaintext/` | PG, My | **roadmap**: Copy as Markdown gives a text table; second renderer to maintain |
| RES-04 | Filter field on a query result | WHERE criteria applied to an editor result by re-running it | ui.editors.data `ResultSetFilterPanel` | PG, My | **gap**: G5 (the filter bar exists on table data only) |
| RES-05 | Filter field on table data | WHERE criteria on a table | ui.editors.data `ResultSetFilterPanel` | PG, My | **done**: Filter bar (builder and raw WHERE); `a_table_is_read_in_pages_sorted_and_filtered_by_the_server` |
| RES-06 | Filter history | Previous filters offered in the field | ui.editors.data `ResultSetFilterPanel` history | PG, My | **gap**: G5 |
| RES-07 | Filter autocompletion | Column names proposed in the filter field | ui.editors.data pref `RESULT_SET_FILTER_AUTO_COMPLETE_PROPOSIAL` (true) | PG, My | **roadmap**: The builder lists the columns |
| RES-08 | Order by column header | Sort from the header; server side for tables | ui.editors.data `core.resultset.toggleOrder` (Ctrl+2), header click | PG, My | **done**: Client sort on results, server sort on tables; `the_grid_sorts_copies_and_rearranges_what_it_shows`, `a_table_is_read_in_pages_sorted_and_filtered_by_the_server` |
| RES-09 | Server-side order on a query result | Re-runs the query with ORDER BY when not all rows are fetched | ui.editors.data `ResultSetViewer` ordering; model `OrderingStrategy` (smart, client side, server side) | PG, My | **gap**: G5 |
| RES-10 | Filter by cell value | Filter ▸ = value, <> value, >, <, LIKE, IS NULL, IS NOT NULL, from clipboard or input | ui.editors.data `ResultSetViewer.fillFiltersMenu`, `FilterByAttributeType` | PG, My | **planned**: `docs/plans/CTX.md` Part 3 (Filter by this value, Exclude, Filter ▸) |
| RES-11 | Filter by distinct values (Ctrl+F11) | Checklist of the column's distinct values with counts | ui.editors.data `core.resultset.filterMenu.distinct`, `valuefilter/` | PG, My | **gap**: G5 |
| RES-12 | Remove all filters / reset column filter | Clears the criteria | ui.editors.data `core.resultset.filterClear`, `FilterResetAttributeAction` | PG, My | **planned**: `docs/plans/CTX.md` Part 3 (Clear filters) |
| RES-13 | Save / reset default filter | Remembered filter and order per table | ui.editors.data `core.resultset.filterSave`, `.filterReset` | PG, My | **roadmap**: Saved queries hold a filtered SELECT |
| RES-14 | Customize filters dialog (Alt+Shift+F) | Columns, order and criteria in one dialog | ui.editors.data `core.resultset.filterSettings` | PG, My | **roadmap**: The filter bar and the header menus cover it |
| RES-15 | Navigate foreign key (Alt+Space) and References | Jump to the referenced row; rows referencing this one | ui.editors.data `core.resultset.navigateLink`, `referencesMenu` (Ctrl+Shift+1) | PG, My | **done**: Row menu on table data; `foreign_keys_lead_to_the_referenced_row_and_back` |
| RES-16 | Back / forward in the result viewer | History of followed links | ui.editors.data `fillNavigateMenu` (backward / forward history) | PG, My | **done**: A followed key opens its own data tab; previous tab stays; `foreign_keys_lead_to_the_referenced_row_and_back` |
| RES-17 | Go to row (Ctrl+G), go to column (Ctrl+Shift+G) | Jumps to a row number or a column | ui.editors.data `grid.gotoRow`, `grid.gotoColumn` | PG, My | **gap**: G6 |
| RES-18 | First / previous / next / last row | Moves the current row | ui.editors.data `row.first` / `.previous` / `.next` / `.last` | PG, My | **gap**: G3 (record mode navigation) |
| RES-19 | Fetch next page / fetch all | Reads more rows or all rows | ui.editors.data `core.resultset.fetch.page`, `.fetch.all` | PG, My | **done**: `fetch-more`, `fetch-all`; `a_large_result_arrives_a_page_at_a_time` |
| RES-20 | Fetch keys (Ctrl+Alt+N, Ctrl+Shift+=) | Shortcuts for fetching | ui.editors.data key bindings | PG, My | **planned**: `docs/plans/M10.md` Part 3 (fetch more / all commands) |
| RES-21 | Fetch next segment on scroll | Reaching the end reads the next page (default on) | ui.editors.data pref `RESULT_SET_AUTO_FETCH_NEXT_SEGMENT` (true) | PG, My | **done, untested** (gap of kind test, G18): `Grid::load_more` on reaching the end; no test scrolls to it (G18) |
| RES-22 | Fetch size | Rows per page, default 200, editable in the result status bar | model pref `RESULT_SET_MAX_ROWS` (200); `ResultSetViewer` status bar field | PG, My | **done**: Setting `page_size` (1,000 per CLAUDE.md §2); `settings_round_trip_and_default`, `rows_that_cannot_be_addressed_are_read_only_and_say_why` (pages of 100) |
| RES-23 | Calculate total row count | Counts the rows of the result on request | ui.editors.data `core.resultset.count` | PG, My | **gap**: G5 (table data counts after each page; M6 Part 2 adds "Count exactly"; query results have no count) |
| RES-24 | Refresh (F5) | Re-reads the result | core `org.eclipse.ui.file.refresh` in the result menu | PG, My | **done**: `refresh_data` (Cmd+R) on table data; `a_table_is_read_in_pages_sorted_and_filtered_by_the_server` |
| RES-25 | Auto-refresh | Re-reads every 1, 5, 10, 15, 30 or 60 s | ui `AutoRefreshControl` in `ResultSetViewer` | PG, My | **gap**: G5 |
| RES-26 | Panels: value viewer (F7 toggles panels) | Side panel showing the selected value | ui.editors.data `grid.togglePreview`, `panel/valueviewer` | PG, My | **done**: Value viewer (double click, row menu); `a_double_click_on_a_cell_shows_its_whole_value` |
| RES-27 | Panels: aggregate | Count, sum, average, min, max of the selected cells | ui.editors.data `panel/aggregate` (`column-aggregate`) | PG, My | **gap**: G6 |
| RES-28 | Panels: metadata | Columns of the result with type, table, key, nullability | ui.editors.data `panel/metadata` | PG, My | **gap**: G6 |
| RES-29 | Panels: grouping | Group-by counts on chosen columns | ui.editors.data `panel/grouping` | PG, My | **roadmap**: A GROUP BY in the editor does it |
| RES-30 | Panels: references | Rows referencing the selected row | ui.editors.data `panel/references` | PG, My | **done**: Row menu entries on table data; `foreign_keys_lead_to_the_referenced_row_and_back` |
| RES-31 | Selection counts in the status | Selected rows / columns / cells shown | ui.editors.data `ToggleSelectionStatAction` (off by default) | PG, My | **gap**: G6 |
| RES-32 | View / Format ▸ display format, transformers, binary format | How values are shown (UI, editable, native; hex / text / base64) | ui.editors.data `fillColumnViewMenu` | PG, My | **roadmap**: Kerno shows server text and hex for binary (CLAUDE.md §4.D2); formats are a preference set |
| RES-33 | Row colours by value, colorize data types | Colour rows matching a value; colour by type | ui.editors.data `grid.selectRowColor`, `ColorizeDataTypesToggleAction` | PG, My | **roadmap**: Visual nicety |
| RES-34 | Boolean as checkbox, click toggles | Booleans drawn as checkboxes and toggled by click (default on) | ui.editors.data prefs `RESULT_SET_SHOW_BOOLEAN_AS_CHECKBOX`, `RESULT_SET_CLICK_TOGGLE_BOOLEAN` | PG, My | **planned**: `docs/plans/M6.md` Part 1 (boolean and enum cells edited from a list) |
| RES-35 | Zoom in / out | Grid font size | ui.editors.data `core.resultset.zoomIn` / `zoomOut` | PG, My | **planned**: `docs/plans/M10.md` Part 3 (grid font size) |
| RES-36 | Hide / show columns | Hide columns, show hidden ones | ui.editors.data `grid.hideColumns`, `grid.showColumns` | PG, My | **done**: Header menu; `the_grid_sorts_copies_and_rearranges_what_it_shows` |
| RES-37 | Pin / unpin columns | Keeps columns on the left while scrolling | ui.editors.data `SpreadsheetPresentation` pin action | PG, My | **done**: Header menu; `the_grid_sorts_copies_and_rearranges_what_it_shows` |
| RES-38 | Columns width: fit values / fit screen | Resizes columns to their content or the view | ui.editors.data `grid.columnsFitValue`, `.columnsFitScreen` | PG, My | **planned**: `docs/plans/CTX.md` Part 3 (Fit width, Fit all widths) |
| RES-39 | Hide columns with no data | Hides columns whose loaded values are all NULL | ui.editors.data `grid.columnsHideEmpty` | PG, My | **roadmap**: Rare |
| RES-40 | Move column left / right | Reorders columns by drag or Alt+Shift+arrows | ui.editors.data `grid.moveColumnLeft` / `Right` | PG, My | **done, untested** (gap of kind test, G18): Drag reorder in `query/grid.rs` (M5 done list); no test (G18) |
| RES-41 | Copy (Ctrl+C) | Copies the selected cells | Eclipse `edit.copy` in the grid | PG, My | **done**: Cmd+C, TSV; `the_grid_sorts_copies_and_rearranges_what_it_shows` |
| RES-42 | Advanced Copy (Ctrl+Shift+C) | Copy with options: header, row numbers, quoting, delimiters | core `core.edit.copy.special`; ui.editors.data `ResultSetCopySettings` | PG, My | **roadmap**: Copy with column names and Copy as CSV / TSV cover the usual options; the shortcut is in G2 |
| RES-43 | Copy as ▸ CSV, JSON, Markdown, SQL | Copy in a data-transfer format | ui.editors.data `core.resultset.copyAs`; data.transfer processors | PG, My | **done**: Copy as CSV / JSON / Markdown / SQL INSERT; `the_grid_sorts_copies_and_rearranges_what_it_shows` |
| RES-44 | Copy as ▸ HTML, XML, TXT, source code, DbUnit | Other data-transfer formats | data.transfer `stream.html`, `stream.xml`, `stream.txt`, `stream.source.code`, `stream.dbunit` | PG, My | **roadmap**: Data-transfer formats beyond CLAUDE.md §4.D3 / §4.G |
| RES-45 | Copy column names (Alt+Shift+C) | Copies the selected column names | ui.editors.data `grid.copyColumnNames` | PG, My | **done**: Copy column names; `the_grid_sorts_copies_and_rearranges_what_it_shows` |
| RES-46 | Copy row numbers | Copies the selected row numbers | ui.editors.data `grid.copyRowNames` | PG, My | **gap**: G6 |
| RES-47 | Paste (Ctrl+V) | Pastes the clipboard into the selected cells | Eclipse `edit.paste` in the grid | PG, My | **planned**: `docs/plans/M6.md` Part 1 (paste) |
| RES-48 | Advanced Paste (Ctrl+Shift+V) | Paste with options, including as new rows | core `core.edit.paste.special`; ui.editors.data `ResultSetPasteSettings` | PG, My | **roadmap**: Paste into cells comes with M6 Part 1; inserting new rows from a spreadsheet is less common |
| RES-49 | Inline edit (Enter, double click) | Edits the cell in place (double click default `INLINE_EDITOR`) | ui.editors.data `row.edit.inline`, pref `RESULT_SET_DOUBLE_CLICK` | PG, My | **done**: Double click, Enter or F2 on table data; `cells_are_edited_reviewed_and_applied_in_one_transaction` |
| RES-50 | Edit cell in a value editor (Shift+Enter) | Dialog editor for long text, JSON, binary | ui.editors.data `core.resultset.row.edit` | PG, My | **gap**: G9 |
| RES-51 | Edit results of a query | A single-table SELECT with a key is editable in the SQL editor | ui.editors.data `ResultSetViewer` (single-source model) | PG, My | **gap**: G8 |
| RES-52 | Add row, duplicate row, delete row | Row editing commands | ui.editors.data `row.add`, `row.copy`, `row.delete` | PG, My | **done**: `add_row`, `duplicate_rows`, `delete_rows`; `cells_are_edited_reviewed_and_applied_in_one_transaction` |
| RES-53 | Row editing keys (Alt+Insert, Ctrl+Alt+Insert, Alt+Delete) | Shortcuts to add, duplicate, delete rows | ui.editors.data key bindings | PG, My | **gap**: G2 (commands have no default key) |
| RES-54 | Add / duplicate row before | Inserts above the current row | ui.editors.data `row.add.before`, `row.copy.before` | PG, My | **roadmap**: Row position in an unsorted table is not meaningful to the server |
| RES-55 | Copy from row above / below (Ctrl+D) | Copies the neighbour row's values into the current row | ui.editors.data `row.copy.from.above`, `.from.below` | PG, My | **gap**: G2 |
| RES-56 | Set to NULL, set to default | Cell value commands | ui.editors.data `cell.setNull`, `cell.setDefault` (Ctrl+Backspace) | PG, My | **done**: `set_null`, `set_default`; `cells_are_edited_reviewed_and_applied_in_one_transaction` |
| RES-57 | Set default key (Ctrl+Backspace) | Shortcut for set to default | ui.editors.data key binding of `cell.setDefault` | PG, My | **gap**: G2 |
| RES-58 | Reset cell changes (Esc) | Reverts the selected cell's pending change | ui.editors.data `core.resultset.cell.reset` | PG, My | **gap**: G2 (Kerno has Revert row; M6 Part 1 adds Cmd+Z) |
| RES-59 | Save (Ctrl+S) | Applies pending changes | ui.editors.data `core.resultset.applyChanges` (Ctrl+S) | PG, My | **gap**: G2 (Apply has no key; Cmd+S is Save file) |
| RES-60 | Cancel / reject changes | Discards pending changes | ui.editors.data `core.resultset.rejectChanges` | PG, My | **done**: `discard_changes`; `cells_are_edited_reviewed_and_applied_in_one_transaction` |
| RES-61 | Generate script of pending changes | Shows the SQL that Save would run | ui.editors.data `core.resultset.generateScript` | PG, My | **done**: Review SQL; `cells_are_edited_reviewed_and_applied_in_one_transaction` |
| RES-62 | Show confirmation before save | Always shows the script before saving | ui.editors.data `core.resultset.toggleConfirmSave` | PG, My | **done**: Always on (CLAUDE.md §1); `cells_are_edited_reviewed_and_applied_in_one_transaction` |
| RES-63 | Apply and commit | Saves and commits in manual mode | ui.editors.data `core.resultset.applyAndCommitChanges` | PG, My | **done**: Apply runs in its own transaction; `cells_are_edited_reviewed_and_applied_in_one_transaction` |
| RES-64 | Undo / redo of edits | Edit history in the grid | ui.editors.data `ResultSetUndoRedoManager` | PG, My | **planned**: `docs/plans/M6.md` Part 1 (Cmd+Z) |
| RES-65 | Read-only when no key | Editing needs a unique key | ui.editors.data pref `RS_EDIT_DISABLE_IF_KEY_MISSING` | PG, My | **done**: Read-only with the reason; `rows_that_cannot_be_addressed_are_read_only_and_say_why` |
| RES-66 | Generate SQL from rows | SELECT … WHERE key =, SELECT … IN, INSERT, UPDATE, DELETE by unique key, CREATE from result | ui.editors.sql `sqlGenerator` dataSelect / dataSelectMany / dataInsert / dataUpdate / dataDeleteByUniqueKey / ddlByResultSet | PG, My | **gap**: G7 (Copy as SQL INSERT exists) |
| RES-67 | Export data | Exports the result to a file | ui.editors.data `core.resultset.export` | PG, My | **done**: `a_filtered_table_is_exported_to_csv_and_json_through_the_dialog`, `a_query_result_is_exported_by_running_its_statement_again` |
| RES-68 | Open with | Opens the result in an external application | ui.editors.data `core.resultset.openWith` | PG, My | **roadmap**: Writes a temp file and launches another app; data-transfer feature beyond §4.G |
| RES-69 | Result status line | Rows fetched and duration | ui.editors.data `ResultSetViewer` status bar | PG, My | **done**: Status bar rows and timing; `statements_run_and_their_rows_fill_the_grid` |
| RES-70 | Pending changes highlighted | Changed, added and deleted rows tinted | ui.editors.data spreadsheet colours | PG, My | **done**: `cells_are_edited_reviewed_and_applied_in_one_transaction` |
| RES-71 | Virtual keys and logical structure | User-defined unique keys for keyless tables | ui.editors.data `fillVirtualModelMenu` | PG, My | **roadmap**: Keyless tables stay read-only with the reason (CLAUDE.md §4.E2) |
| RES-72 | Generate UUID | Inserts a new UUID in the cell or editor | ui.editors.data `core.generate.uuid` | PG, My | **roadmap**: `gen_random_uuid()` / `uuid()` as a default covers it |

## Object editors (structure)

| ID | Feature (as DBeaver names it) | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| OBJ-01 | Table editor tabs | Columns, constraints, foreign keys, indexes, references, triggers, DDL | ui.editors.entity / engine `databaseEditor` pages | PG, My | **done**: Structure tab; `the_structure_of_a_table_is_shown_in_tabs` |
| OBJ-02 | View editor | Columns and source of a view | ext.postgresql.ui `postgresql.source.view`; ext.mysql.ui `mysql.source.view` | PG, My | **done**: `a_view_shows_its_columns_and_ddl` |
| OBJ-03 | Properties page | Owner, comment, row estimate, sizes; MySQL engine, collation, auto-increment | ui.editors.entity properties page | PG, My | **gap**: G17 |
| OBJ-04 | Edit and save a table (Ctrl+S with SQL preview) | Changes persisted after a script preview | ui.editors.entity save; ui.navigator pref `NAVIGATOR_SHOW_SQL_PREVIEW` (true) | PG, My | **done**: Table form with live DDL; `a_table_is_altered_from_the_form_with_a_preview`, `the_form_refuses_what_is_invalid` |
| OBJ-05 | Create table | New table editor | ui.navigator `core.object.create` on Tables | PG, My | **done**: `a_table_is_created_from_the_form` |
| OBJ-06 | Columns, indexes, keys created from the editor | Add column / index / foreign key dialogs | engine `*Manager` classes (e.g. `PostgreTableColumnManager`) | PG, My | **done**: `a_table_is_altered_from_the_form_with_a_preview` |
| OBJ-07 | Column order | Move columns (MySQL `AFTER`) | ext.mysql `MySQLTableColumnManager` | My | **roadmap**: Noted as not editable in M7; MySQL-only |
| OBJ-08 | Edit view / function / procedure source and save | Source tab applied as CREATE OR REPLACE | ext.postgresql.ui `PostgreSourceViewEditor`; ext.mysql.ui `MySQLSourceViewEditor` | PG, My | **done**: Edit source; `editing_a_source_applies_it_once_its_sql_is_confirmed`, `a_materialized_view_whose_new_source_fails_is_kept` |
| OBJ-09 | Edit trigger source | Trigger definition edited and saved | engine trigger managers | PG, My | **roadmap**: PostgreSQL trigger bodies are functions (editable); MySQL triggers are re-created from Show DDL |
| OBJ-10 | DDL page | Read-only DDL of the object | ext.postgresql.ui `postgresql.source.ddl`; ext.mysql.ui `mysql.source.ddl` | PG, My | **done**: DDL sub-tab and Show DDL; `the_structure_of_a_table_is_shown_in_tabs` |
| OBJ-11 | Sequence editor | Start, increment, bounds, cycle, current value | ext.postgresql `PostgreSequence` properties | PG | **roadmap**: Show DDL gives the definition; current value is a query away |
| OBJ-12 | Enum type editing | Add and rename values | ext.postgresql `alterTypeAddValue` generator; type editor | PG | **done**: `enums_are_created_and_their_values_added_and_renamed` |
| OBJ-13 | Extensions | Install and drop extensions | ext.postgresql tree `avalextensions` | PG | **done**: `extensions_are_installed_and_dropped_from_the_tree` |
| OBJ-14 | Create database with options | PostgreSQL owner, encoding, template; MySQL charset, collation | ext.postgresql `PostgreDatabaseManager`; ext.mysql `MySQLDatabaseManager` | PG, My | **gap**: G17 |
| OBJ-15 | Create schema | New schema with owner | ext.postgresql `PostgreSchemaManager` | PG | **done**: `dropping_renaming_and_truncating_show_the_sql_and_ask` |
| OBJ-16 | Permissions page | Table and role privileges (PostgreSQL); schema privileges (MySQL) | ext.postgresql.ui `postgresql.table.permissions`, `postgresql.role.permissions`; ext.mysql.ui `editor.schema_privileges` | PG, My | **planned**: `docs/plans/M8.md` Part 4 (grants, read-only per CLAUDE.md §4.H2) |
| OBJ-17 | Role / user editors | Create and edit roles or users, attributes, limits, privileges | ext.postgresql.ui role editor; ext.mysql.ui `MySQLUserEditorGeneral`, `MySQLUserEditorPrivileges` | PG, My | **roadmap**: CLAUDE.md §4.H2 asks for a read view of users and grants; editing them is beyond v1 |
| OBJ-18 | Dependencies page | Objects this one depends on and the reverse | ext.postgresql tree `dependency` | PG | **roadmap**: See NAV-09 |
| OBJ-19 | Statistics page | Table sizes and counters | engine table properties | PG, My | **planned**: `docs/plans/M8.md` Part 4 (Sizes) |
| OBJ-20 | ER diagram page | Diagram tab of a table or schema | ui.editors.erd | PG, My | **roadmap**: ERD is outside v1 (CLAUDE.md §4, last paragraph) |
| OBJ-21 | Data page | Data tab inside the object editor | ui.editors.data `DatabaseDataEditor` | PG, My | **done**: Separate data tab; "Open data" from the structure tab; `a_table_opened_from_the_tree_comes_back_after_a_restart` |

## Tools and administration

| ID | Feature (as DBeaver names it) | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| TOOL-01 | Backup (pg_dump) / Dump database (mysqldump) | Dumps a database with the client tool | ext.postgresql.ui `tools.backup.db`; ext.mysql.ui `tools.exportDatabase` | PG, My | **planned**: `docs/plans/M8.md` Part 3 (dialog; core done) |
| TOOL-02 | Restore | Restores a dump with pg_restore / psql / mysql | ext.postgresql.ui `tools.restore.db`; ext.mysql.ui `tools.importDatabase` | PG, My | **planned**: `docs/plans/M8.md` Part 3 and Part 3b |
| TOOL-03 | Global backup (pg_dumpall) | Dumps roles and all databases | ext.postgresql.ui `tools.backup.all.db` | PG | **roadmap**: Server-wide dumps are a DBA task outside a client's daily use |
| TOOL-04 | Execute script with the local client | Runs a file with psql / mysql | ext.postgresql.ui `tools.execute.script`; ext.mysql.ui `tools.executeScript` | PG, My | **roadmap**: Restore covers SQL files (M8 Part 3) |
| TOOL-05 | Analyze (PostgreSQL) | ANALYZE of tables or a database | ext.postgresql.ui `tools.analyze` (`PostgreToolBaseVacuum`) | PG | **gap**: G12 |
| TOOL-06 | Vacuum (PostgreSQL) | VACUUM with FULL, FREEZE, ANALYZE, DISABLE_PAGE_SKIPPING, SKIP_LOCKED, INDEX_CLEANUP | ext.postgresql.ui `tools.vacuum`; ext.postgresql `PostgreToolBaseVacuum` | PG | **gap**: G12 |
| TOOL-07 | Truncate | Empties tables | ext.postgresql.ui `tools.truncate`; ext.mysql.ui `tools.truncateTable` | PG, My | **done**: Truncate…; `dropping_renaming_and_truncating_show_the_sql_and_ask` |
| TOOL-08 | Truncate options (PostgreSQL) | ONLY, RESTART IDENTITY, CASCADE | ext.postgresql `PostgreToolTableTruncate` | PG | **gap**: G12 |
| TOOL-09 | Refresh materialized view | REFRESH MATERIALIZED VIEW | ext.postgresql.ui `tools.refesh.mview` | PG | **gap**: G12 |
| TOOL-10 | Enable / disable triggers | ALTER TABLE … ENABLE / DISABLE TRIGGER | ext.postgresql.ui `PostgreToolTriggerEnable`, `PostgreToolTriggerDisable` | PG | **gap**: G12 |
| TOOL-11 | Analyze / Check / Optimize / Repair table (MySQL) | Table maintenance statements with their result | ext.mysql.ui `tools.analyzeTable`, `.checkTable`, `.optimizeTable`, `.repairTable` | My | **gap**: G12 |
| TOOL-12 | Session manager | Sessions with refresh, auto-refresh, cancel query, terminate | ext.postgresql.ui `PostgreSessionEditor`; ext.mysql.ui `MySQLSessionEditor`; ui.editors.session | PG, My | **planned**: `docs/plans/M8.md` Part 4 (Sessions) |
| TOOL-13 | Session manager: hide idle / sleeping | Toggle that hides idle sessions | ext.postgresql.ui `ShowIdleAction`; ext.mysql.ui `hideSleeping` | PG, My | **gap**: G16 |
| TOOL-14 | Lock manager | Waiting and blocking locks with the sessions holding them | ext.postgresql.ui `PostgreLockEditor`; ui.editors.locks | PG | **gap**: G16 |
| TOOL-15 | Session details: query text and plan | Selected session's statement and its plan | ui.editors.session `SessionManagerViewer` details | PG, My | **roadmap**: M8 Part 4 shows the query column; the plan of another session is rarely needed |
| TOOL-16 | Server settings | pg_settings / SHOW VARIABLES | ext.postgresql tree `PostgreSetting`; ext.mysql `sessionVariables`, `globalVariables` | PG, My | **planned**: `docs/plans/M8.md` Part 4 (Settings) |
| TOOL-17 | MySQL status, engines, charsets, collations, plugins | System information folders | ext.mysql tree `MySQLInformation` | My | **roadmap**: Available through SHOW statements in the editor |
| TOOL-18 | Refresh publications (subscriptions) | Logical replication tool | ext.postgresql.ui `refreshSubscription` | PG | **roadmap**: Replication objects are outside CLAUDE.md §4 |
| TOOL-19 | Data transfer beyond files | Database to database, XLSX, XML, HTML, Parquet and other formats | data.transfer.ui wizards and processors | PG, My | **roadmap**: Data-transfer wizards beyond CLAUDE.md §4.G (left out by the brief) |
| TOOL-20 | Dashboards | Server activity charts | ui.dashboard | PG, My | **roadmap**: Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-21 | Task scheduler and saved tasks | Scheduled backups, exports, scripts | tasks.ui, tasks.native.ui | PG, My | **roadmap**: Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-22 | Data compare, schema compare | Diff of data or structure | cmp.simple.ui (PRO for full compare) | PG, My | **roadmap**: Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-23 | Mock data generator | Fills tables with generated rows | PRO / datadam | PG, My | **roadmap**: Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-24 | AI assistant | Natural language to SQL | model.ai `sqlCommand` ai | PG, My | **roadmap**: Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-25 | Debugger | PL/pgSQL debugger | ext.postgresql.debug.ui | PG | **roadmap**: Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-26 | Visual query builder | Graphical SELECT builder (PRO) | PRO | PG, My | **roadmap**: Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-27 | SQL terminal presentation | Results shown as a terminal log | ui.editors.sql.terminal | PG, My | **roadmap**: Alternative presentation of results; outside CLAUDE.md §4 |
| TOOL-28 | Git / team | Version control of scripts | team.git.ui | PG, My | **roadmap**: Eclipse team plumbing |

## Context menus

One table per surface, entries in DBeaver's order (contribution groups of the popup, see the method note). "Done" for a menu entry means the entry list or a real right-click is covered by a test.

### Navigator empty area

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-01 | New Connection | Opens the new-connection dialog | core menu `core.databaseNavigator` → `core.new.connection` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 |
| CM-02 | Driver Manager | JDBC drivers | core `core.driver.manager` | PG, My | **roadmap**: No JDBC drivers (CON-50) |
| CM-03 | Active Project ▸ | Project switcher | core `NavigatorActiveProjectContributor` | PG, My | **roadmap**: Eclipse projects (NAV-40) |
| CM-04 | New Folder | Creates a connection folder | core `core.new.folder` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 |
| CM-05 | Import connections | Kerno's own entry, no DBeaver equivalent in this menu | — (CLAUDE.md §3) | PG, My | **planned**: `docs/plans/CTX.md` Part 1 |
| CM-06 | Collapse All | Folds the tree | core `org.eclipse.ui.navigate.collapseAll` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 |
| CM-07 | Link with editor | Selects the active editor's object | ui.navigator `core.navigator.linkeditor` | PG, My | **gap**: G10 |
| CM-08 | Refresh | Reloads the list | core `core.project.refresh` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 |
| CM-09 | Paste | Pastes copied connections | core `org.eclipse.ui.edit.paste` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 (Duplicate covers it) |

### Folder

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-10 | Create ▸ Connection | New connection in this folder | ui.navigator `core.object.create` on a folder | PG, My | **planned**: `docs/plans/CTX.md` Part 1 (New connection here) |
| CM-11 | New Folder | Sub-folder | core `core.new.folder` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 |
| CM-12 | Disconnect | Closes every connection of the folder | core `org.jkiss.dbeaver.folder.disconnect` | PG, My | **gap**: G13 |
| CM-13 | Rename (F2) | Renames the folder | core `org.eclipse.ui.edit.rename` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 |
| CM-14 | Delete | Removes the folder | core `org.eclipse.ui.edit.delete` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 |
| CM-15 | Refresh | Re-reads the folder | core `org.eclipse.ui.file.refresh` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 (Refresh of the list) |

### Connection

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-16 | Edit Connection (F4) | Connection dialog | ui.navigator `core.object.open` | PG, My | **done, untested** (gap of kind test, G18): Sidebar "Edit…" (`sidebar.rs`); no test opens the menu (G18); F4: G1 |
| CM-17 | Create ▸ (database, schema) | New child object | ui.navigator `core.object.create` | PG, My | **roadmap**: Create database / schema exist on the navigator's database and schema nodes (`dropping_renaming_and_truncating_show_the_sql_and_ask`); the sidebar connection menu need not repeat them |
| CM-18 | SQL Editor ▸ (open script, recent, new script, console) | Editor bound to the connection | ui.editors.sql `popup:…?after=sql_tools` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 (New SQL editor) |
| CM-19 | Connection view ▸ | Tree presets | core `ConnectionViewSettingsContributor` | PG, My | **roadmap**: See NAV-59 |
| CM-20 | Filter ▸ | Object filters | core `navigator.filter` menu | PG, My | **gap**: G11 |
| CM-21 | Compare / Migrate ▸ | Schema compare | cmp.simple.ui | PG, My | **roadmap**: Outside v1 (CLAUDE.md §4, last paragraph) |
| CM-22 | Tools ▸ (session manager, lock manager, backup all) | Connection-level tools | ext.postgresql.ui / ext.mysql.ui `connectionEditor` contributions, `tools.backup.all.db` | PG, My | **planned**: `docs/plans/M8.md` Part 4 ("Administration" on the connection menu); lock manager G16 |
| CM-23 | Copy | Copies the connection | core `org.eclipse.ui.edit.copy` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 (Copy ▸ name, URL, host) |
| CM-24 | Paste | Pastes a copied connection | core `org.eclipse.ui.edit.paste` | PG, My | **done, untested** (gap of kind test, G18): Duplicate (`sidebar.rs`); action tested by `duplicate_import_and_delete`, menu not (G18) |
| CM-25 | Delete | Deletes after asking | core `org.eclipse.ui.edit.delete` | PG, My | **done, untested** (gap of kind test, G18): Sidebar "Delete…"; action tested by `deleting_a_connection_asks_first`, menu not (G18) |
| CM-26 | Rename (F2) | Renames the connection | core `org.eclipse.ui.edit.rename` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 |
| CM-27 | Refresh (F5) | Re-reads the connection's tree | core `org.eclipse.ui.file.refresh` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 |
| CM-28 | Connect | Opens the connection | core `core.connect` | PG, My | **done, untested** (gap of kind test, G18): Sidebar "Connect"; action tested by `connects_queries_and_disconnects`, menu not (G18) |
| CM-29 | Invalidate / Reconnect | Re-opens the connection | core `core.invalidate` | PG, My | **planned**: `docs/plans/CTX.md` Part 1 |
| CM-30 | Disconnect | Closes the connection | core `core.disconnect` | PG, My | **done, untested** (gap of kind test, G18): Sidebar "Disconnect"; menu not tested (G18) |
| CM-31 | Security ▸ Change user password | Changes the login's password | ui.editors.connection `connection.changeCurrentPassword` | PG, My | **roadmap**: See CON-48 |
| CM-32 | Move to folder (drag and drop) | Moves a connection between folders | ui.navigator drag and drop | PG, My | **planned**: `docs/plans/CTX.md` Part 1 |

### Database / schema

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-33 | Edit (F4) | Database or schema properties | ui.navigator `core.object.open` | PG, My | **gap**: G1 (F4); properties page G17 |
| CM-34 | Create ▸ (schema, table, view, function, sequence, …) | New child object | ui.navigator `core.object.create` | PG, My | **done**: Create schema / database / table / view; `dropping_renaming_and_truncating_show_the_sql_and_ask`, `a_table_is_created_from_the_form`; other kinds: G10 |
| CM-35 | SQL Editor ▸ | Editor bound to the database or schema | ui.editors.sql `popup:…?after=sql_tools` | PG, My | **done**: New query here on schemas and MySQL databases; `statements_and_ddl_open_in_a_tab_and_run_nothing`; on a PostgreSQL database node: G1 |
| CM-36 | Set as default (Ctrl+Shift+A) | Makes it the active database / schema | ui.navigator `core.navigator.set.default` | PG, My | **gap**: G1 |
| CM-37 | Filter ▸ | Hide / show only | core `navigator.filter` | PG, My | **gap**: G11 |
| CM-38 | Generate SQL ▸ DDL | DDL of the schema / database | ui.editors.sql `sqlGenerator` | PG, My | **done**: Show DDL (schemas, MySQL databases); `statements_and_ddl_open_in_a_tab_and_run_nothing` |
| CM-39 | Tools ▸ Backup, Restore | Dump and restore | ext.postgresql.ui / ext.mysql.ui tools | PG, My | **planned**: `docs/plans/M8.md` Part 3 |
| CM-40 | Tools ▸ Analyze, Vacuum | Database maintenance | ext.postgresql.ui `tools.analyze`, `tools.vacuum` | PG | **gap**: G12 |
| CM-41 | Export Data / Import Data | Data transfer on the container | data.transfer.ui | PG, My | **done**: Import CSV… on schema / MySQL database (new table); `a_csv_file_is_imported_into_an_existing_table_and_into_a_new_one` |
| CM-42 | Copy, Copy Advanced Info | Copies names | core `edit.copy`, `core.edit.copy.special` | PG, My | **done**: Copy name; `each_kind_of_node_gets_the_actions_that_make_sense_for_it`; qualified copy G1 |
| CM-43 | Delete | Drops after preview | core `edit.delete` | PG, My | **done**: Drop…; `dropping_renaming_and_truncating_show_the_sql_and_ask` |
| CM-44 | Rename | Renames | core `edit.rename` | PG | **done**: Rename… (schemas); `dropping_renaming_and_truncating_show_the_sql_and_ask` |
| CM-45 | Refresh | Re-reads | core `file.refresh` | PG, My | **done**: `refresh_reloads_the_nearest_node_that_owns_its_children` |

### Table

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-46 | Edit Table (F4) | Object editor | ui.navigator `core.object.open` | PG, My | **done**: Open structure; `the_structure_of_a_table_is_shown_in_tabs`, `each_kind_of_node_gets_the_actions_that_make_sense_for_it`; F4: G1 |
| CM-47 | View Diagram | ER diagram | ui.editors.erd | PG, My | **roadmap**: Outside v1 (CLAUDE.md §4, last paragraph) |
| CM-48 | View Data | Data editor | ui.editors.data `forSelection` | PG, My | **done**: Open data; `a_table_opened_from_the_tree_comes_back_after_a_restart` |
| CM-49 | Create ▸ (column, index, constraint, foreign key, trigger) | New child objects | ui.navigator `core.object.create` | PG, My | **done**: Through the table form (Edit table…); `a_table_is_altered_from_the_form_with_a_preview`; triggers: G10 |
| CM-50 | Generate SQL ▸ SELECT / INSERT / UPDATE / DELETE / DDL | Templates | ui.editors.sql `sqlGenerator` | PG, My | **done**: `statements_and_ddl_open_in_a_tab_and_run_nothing` |
| CM-51 | Generate SQL ▸ MERGE, INSERT ON CONFLICT, UPDATE FROM, DELETE USING | Further templates | ui.editors.sql / ext.postgresql `sqlGenerator` | PG, My | **roadmap**: See NAV-51, NAV-52 |
| CM-52 | Read data in SQL console | SELECT run in a console | ui.editors.sql `core.sql.editor.forSelection` | PG, My | **gap**: G7 |
| CM-53 | SQL Editor ▸ | Editor bound to the table's schema | ui.editors.sql `popup:…?after=sql_tools` | PG, My | **gap**: G1 (New query here exists on schemas, not tables) |
| CM-54 | Export Data | Export wizard | data.transfer.ui `core.export.data` | PG, My | **done**: Export…; `a_filtered_table_is_exported_to_csv_and_json_through_the_dialog` |
| CM-55 | Import Data | Import wizard | data.transfer.ui `core.import.data` | PG, My | **done**: Import CSV…; `a_csv_file_is_imported_into_an_existing_table_and_into_a_new_one` |
| CM-56 | Tools ▸ Analyze, Vacuum, Truncate (options), Enable / Disable triggers (PG) | Table maintenance | ext.postgresql.ui tools | PG | **gap**: G12 (plain Truncate is done) |
| CM-57 | Tools ▸ Analyze, Check, Optimize, Repair, Truncate (MySQL) | Table maintenance | ext.mysql.ui tools | My | **gap**: G12 (Truncate is done) |
| CM-58 | Tools ▸ Backup (selected tables) | Dump of one table | ext.postgresql.ui `tools.backup.db` | PG, My | **planned**: `docs/plans/M8.md` Part 3 (selected tables option) |
| CM-59 | Compare / Migrate ▸ | Compare | cmp.simple.ui | PG, My | **roadmap**: Outside v1 (CLAUDE.md §4, last paragraph) |
| CM-60 | Filter ▸ | Hide / show only | core `navigator.filter` | PG, My | **gap**: G11 |
| CM-61 | Copy, Copy Advanced Info | Names | core `edit.copy`, `core.edit.copy.special` | PG, My | **done**: Copy name; `each_kind_of_node_gets_the_actions_that_make_sense_for_it`; qualified copy G1 |
| CM-62 | Delete | Drop with preview | core `edit.delete` | PG, My | **done**: Drop…; `dropping_renaming_and_truncating_show_the_sql_and_ask` |
| CM-63 | Rename | Rename with preview | core `edit.rename` | PG, My | **done**: Rename…; `dropping_renaming_and_truncating_show_the_sql_and_ask` |
| CM-64 | Refresh | Re-reads | core `file.refresh` | PG, My | **done**: `refresh_reloads_the_nearest_node_that_owns_its_children` |

### View

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-65 | Edit View (F4) | Source editor | ui.navigator `core.object.open` | PG, My | **done**: Edit source / Open structure; `editing_a_source_applies_it_once_its_sql_is_confirmed`; F4: G1 |
| CM-66 | View Data | Data editor | ui.editors.data `forSelection` | PG, My | **done**: Open data; `each_kind_of_node_gets_the_actions_that_make_sense_for_it` |
| CM-67 | Generate SQL ▸ SELECT, DDL | Templates | ui.editors.sql `sqlGenerator` | PG, My | **done**: `statements_and_ddl_open_in_a_tab_and_run_nothing` |
| CM-68 | Read data in SQL console | SELECT run in a console | ui.editors.sql `forSelection` | PG, My | **gap**: G7 |
| CM-69 | Export Data | Export wizard | data.transfer.ui | PG, My | **done**: Export…; `each_kind_of_node_gets_the_actions_that_make_sense_for_it`, `a_filtered_table_is_exported_to_csv_and_json_through_the_dialog` |
| CM-70 | Tools ▸ Refresh materialized view | REFRESH MATERIALIZED VIEW | ext.postgresql.ui `tools.refesh.mview` | PG | **gap**: G12 |
| CM-71 | Copy, Delete, Rename, Refresh | Standard entries | core `edit.*`, `file.refresh` | PG, My | **done**: `each_kind_of_node_gets_the_actions_that_make_sense_for_it`, `dropping_renaming_and_truncating_show_the_sql_and_ask` |

### Column

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-72 | Edit (F4) | Column properties | ui.navigator `core.object.open` | PG, My | **gap**: G10 (opens the table form on that column) |
| CM-73 | Create ▸ Column | Adds a column | ui.navigator `core.object.create` | PG, My | **gap**: G10 |
| CM-74 | New Index / Constraint from selection | Index or key on selected columns | ui.navigator `navigator.create.column.index`, `.constraint` | PG, My | **roadmap**: See NAV-43 |
| CM-75 | Copy | Column name | core `edit.copy` | PG, My | **done**: Copy name; `each_kind_of_node_gets_the_actions_that_make_sense_for_it` |
| CM-76 | Delete | Drops the column (preview) | core `edit.delete` | PG, My | **gap**: G10 |
| CM-77 | Rename | Renames the column (preview) | core `edit.rename` | PG, My | **gap**: G10 |
| CM-78 | Refresh | Re-reads | core `file.refresh` | PG, My | **done**: `each_kind_of_node_gets_the_actions_that_make_sense_for_it` |

### Routine

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-79 | Edit (F4) | Source editor | ui.navigator `core.object.open` | PG, My | **done**: Edit source (double click opens it); `editing_a_source_applies_it_once_its_sql_is_confirmed`, `a_double_click_opens_what_the_node_is_for`; F4: G1 |
| CM-80 | Execute stored procedure | Console with a call statement | ui.editors.sql `core.procedure.execute` | PG, My | **gap**: G7 |
| CM-81 | Generate SQL ▸ CALL, DDL | Call template; definition | ui.editors.sql `sqlGenerator` procedureCall, tableDDL | PG, My | **gap**: G7 (DDL is done: Show DDL) |
| CM-82 | Copy, Delete, Rename, Refresh | Standard entries | core `edit.*`, `file.refresh` | PG, My | **done**: `each_kind_of_node_gets_the_actions_that_make_sense_for_it` |

### Sequence

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-83 | Edit (F4) | Sequence properties | ui.navigator `core.object.open` | PG, My | **done**: Double click shows DDL; `a_double_click_opens_what_the_node_is_for`; properties editor on the roadmap (OBJ-11) |
| CM-84 | Generate SQL ▸ DDL | Definition | ui.editors.sql `sqlGenerator` | PG, My | **done**: Show DDL; `each_kind_of_node_gets_the_actions_that_make_sense_for_it` |
| CM-85 | Copy, Delete, Rename, Refresh | Standard entries | core `edit.*`, `file.refresh` | PG, My | **done**: `each_kind_of_node_gets_the_actions_that_make_sense_for_it` |

### Type

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-86 | Edit (F4) | Type properties | ui.navigator `core.object.open` | PG | **done**: Show DDL on double click; `a_double_click_opens_what_the_node_is_for` |
| CM-87 | Generate SQL ▸ DDL, ADD VALUE | Definition; enum value template | ext.postgresql `alterTypeAddValue` | PG | **done**: Show DDL, Add value…; `enums_are_created_and_their_values_added_and_renamed` |
| CM-88 | Copy, Delete, Rename, Refresh | Standard entries | core `edit.*`, `file.refresh` | PG | **done**: `each_kind_of_node_gets_the_actions_that_make_sense_for_it`, `enums_are_created_and_their_values_added_and_renamed` |

### Trigger

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-89 | Edit (F4) | Trigger definition | ui.navigator `core.object.open` | PG, My | **done**: Show DDL on double click; `a_double_click_opens_what_the_node_is_for` |
| CM-90 | Tools ▸ Enable / Disable trigger | Toggles the trigger | ext.postgresql.ui `PostgreToolTriggerEnable`, `PostgreToolTriggerDisable` | PG | **gap**: G12 |
| CM-91 | Copy, Delete, Rename, Refresh | Standard entries | core `edit.*`, `file.refresh` | PG, My | **done**: `each_kind_of_node_gets_the_actions_that_make_sense_for_it` |

### SQL editor

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-92 | Execute ▸ statement, in new tab, script, from position, separate tabs, cancel | Run commands | ui.editors.sql `SQLEditor.execute` menu | PG, My | **planned**: `docs/plans/CTX.md` Part 2 (Run statement, Run script); new tab and from position: G4 |
| CM-93 | Execute ▸ row count, all rows, expression, export from query | Other run commands | ui.editors.sql `run.count`, `run.all.rows`, `run.expression`, `export.data` | PG, My | **gap**: G15 (menu entries; row count itself G4; expression on the roadmap SQL-14) |
| CM-94 | Execute ▸ Explain, Load plan | Plans | ui.editors.sql `run.explain`, `load.plan` | PG, My | **planned**: `docs/plans/CTX.md` Part 2 (Explain, Explain analyze); load plan on the roadmap (SQL-17) |
| CM-95 | Execute ▸ Set active connection / schema | Target selectors | ui.navigator `ui.tools.select.connection`, `.schema` | PG, My | **gap**: G15 |
| CM-96 | File ▸ Rename, Revert, Import / Export script | File commands | ui.editors.sql `SQLEditor.file` menu | PG, My | **planned**: `docs/plans/CTX.md` Part 2 (Save, Save as; Rename on the tab) |
| CM-97 | Layout ▸, Panels ▸ | Results layout and side panels | ui.editors.sql `SQLEditor.layout`, `.extraPanels` | PG, My | **gap**: G4 (toggle / maximize results); outline and variables on the roadmap (SQL-26, SQL-27) |
| CM-98 | Format ▸ Format SQL, upper / lower case, toggle line / block comment, word wrap | Text commands | ui.editors.sql `SQLEditorBase.editorContextMenuAboutToShow` | PG, My | **planned**: `docs/plans/CTX.md` Part 2 (Format SQL, Toggle comment); case and block comment: G15 |
| CM-99 | Cut, Copy, Paste, Select all, Undo, Redo | Standard text entries | Eclipse text editor context menu | PG, My | **planned**: `docs/plans/CTX.md` Part 2 |
| CM-100 | Copy Advanced, Copy selected query | Copy variants | ui.editors.sql `popup:…EditorContext?after=group.copy` | PG, My | **gap**: G15 (Copy selected query); advanced copy on the roadmap (SQL-49) |
| CM-101 | Open Declaration (F4) | Opens the object under the cursor | ui.editors.sql `navigate.object` | PG, My | **gap**: G15 |
| CM-102 | Content assist proposals | Opens completion | ui.editors.sql `ACTION_CONTENT_ASSIST_PROPOSAL` | PG, My | **planned**: `docs/plans/M5.md` (Ctrl+Space left over) |

### Result grid cell

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-103 | Copy | Copies the selection | ui.editors.data `fillContextMenu` (`EDIT_COPY`) | PG, My | **done**: Row menu "Copy"; `a_right_click_on_a_cell_opens_the_menu_of_its_row` |
| CM-104 | Advanced Copy ▸ (copy as formats) | Copy in a chosen format | ui.editors.data `ResultSetHandlerCopyAs.fillCopyAsMenu` | PG, My | **done**: Copy as CSV / JSON / Markdown / SQL INSERT entries; `a_right_click_on_a_cell_opens_the_menu_of_its_row`; as a submenu: `docs/plans/CTX.md` Part 3 |
| CM-105 | Paste, Advanced Paste | Paste into cells | ui.editors.data `fillContextMenu` (`EDIT_PASTE`, `CMD_PASTE_SPECIAL`) | PG, My | **planned**: `docs/plans/CTX.md` Part 3 and `docs/plans/M6.md` Part 1; advanced paste on the roadmap (RES-48) |
| CM-106 | Filter ▸ (by value, operators, distinct values, clear, save default) | Filters from the cell | ui.editors.data `fillFiltersMenu` | PG, My | **planned**: `docs/plans/CTX.md` Part 3 (value, exclude, operators, clear); distinct values: G5; on query results: G5 |
| CM-107 | Order ▸ Ascending / Descending / Default | Ordering from the cell | ui.editors.data `fillOrderingsMenu` | PG, My | **planned**: `docs/plans/CTX.md` Part 3 |
| CM-108 | Navigate ▸ (foreign key, references, go to row / column, fetch, refresh on scroll, back / forward) | Navigation entries | ui.editors.data `fillNavigateMenu` | PG, My | **done**: Foreign-key entries on table data; `foreign_keys_lead_to_the_referenced_row_and_back`; go to row / column G6 |
| CM-109 | Edit ▸ Edit cell, inline edit, set NULL, set default, reset | Cell edit entries | ui.editors.data `fillEditMenu` | PG, My | **planned**: `docs/plans/CTX.md` Part 3 (Edit cell, Set NULL, Set default); value editor G9; reset cell G2 |
| CM-110 | Edit ▸ Add row, duplicate row, delete row, copy from above / below | Row edit entries | ui.editors.data `fillContextMenu` edit submenu | PG, My | **done**: Duplicate row, Delete row, Revert row on table data; `a_right_click_on_a_cell_opens_the_menu_of_its_row`; add row / copy from above: G2 |
| CM-111 | Panels ▸ | Value, aggregate, metadata, grouping, references | ui.editors.data `PanelsMenuContributor` | PG, My | **gap**: G6 (aggregate, metadata); value viewer done (View value: `docs/plans/CTX.md` Part 3) |
| CM-112 | View / Format ▸, Hints, Logical structure | Display formats, colours, virtual keys | ui.editors.data `fillColumnViewMenu`, `fillVirtualModelMenu` | PG, My | **roadmap**: See RES-32, RES-33, RES-71 |
| CM-113 | Layout ▸ (record mode, presentation, selection counts, filter panel, zoom) | Viewer layout | ui.editors.data `fillLayoutMenu` | PG, My | **gap**: G3 (record mode); selection counts G6; zoom `docs/plans/M10.md` Part 3 |
| CM-114 | Export data, Open with ▸ | Export the result | ui.editors.data `CMD_EXPORT`, `ResultSetHandlerOpenWith` | PG, My | **planned**: `docs/plans/CTX.md` Part 3 (Export…); Open with roadmap (RES-68) |
| CM-115 | Generate SQL ▸ (from rows) | SELECT / INSERT / UPDATE / DELETE of the selected rows | ui.editors.sql `SQLGeneratorContributor` on result rows | PG, My | **gap**: G7 |
| CM-116 | Refresh | Re-reads the result | core `file.refresh` in `fillContextMenu` | PG, My | **gap**: G6 (menu entry; Refresh exists in the data tab toolbar) |

### Column header

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-117 | Copy column name(s) | Copies names | ui.editors.data `grid.copyColumnNames` | PG, My | **done, untested** (gap of kind test, G18): Header "Copy name" (`Grid::header_menu`); the action is tested (`the_grid_sorts_copies_and_rearranges_what_it_shows`), not the menu (G18) |
| CM-118 | Filter ▸ (on this column) | Criteria on the column | ui.editors.data `fillFiltersMenu` | PG, My | **planned**: `docs/plans/CTX.md` Part 3 (Filter on this column…) |
| CM-119 | Order ▸ | Sorts by the column | ui.editors.data `fillOrderingsMenu` | PG, My | **planned**: `docs/plans/CTX.md` Part 3 |
| CM-120 | Pin / unpin column | Pins | ui.editors.data `SpreadsheetPresentation` pin action | PG, My | **done, untested** (gap of kind test, G18): Header "Pin column"; action tested, menu not (G18) |
| CM-121 | Hide column, show hidden columns | Visibility | ui.editors.data `grid.hideColumns`, `grid.showColumns` | PG, My | **done, untested** (gap of kind test, G18): Header "Hide column", "Show all columns"; actions tested, menu not (G18) |
| CM-122 | Hide empty columns, fit values, fit screen | Widths and empty columns | ui.editors.data `grid.columnsHideEmpty`, `.columnsFitValue`, `.columnsFitScreen` | PG, My | **planned**: `docs/plans/CTX.md` Part 3 (Fit width, Fit all widths); hide empty on the roadmap (RES-39) |

### Row header

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-123 | Copy row number(s) | Copies row numbers | ui.editors.data `grid.copyRowNames` | PG, My | **gap**: G6 |
| CM-124 | Edit ▸ add / duplicate / delete row, Filter ▸, Order ▸, Export | Same entries as a cell, for the row | ui.editors.data `fillContextMenu` (`ROW_HEADER`) | PG, My | **gap**: G6 (row-number column has no menu) |

### Tab

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-125 | Close, Close Others, Close All | Editor tab entries | Eclipse editor tab menu | PG, My | **planned**: `docs/plans/CTX.md` Part 2 |
| CM-126 | Close Tabs to the Left / Right | Closes tabs on one side | Eclipse editor tab menu | PG, My | **planned**: `docs/plans/CTX.md` Part 2 (to the right); to the left: G4 |
| CM-127 | Rename SQL script (Ctrl+F2) | Renames the editor | ui.editors.sql `rename` | PG, My | **planned**: `docs/plans/CTX.md` Part 2 |

### Result tab

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-128 | Close, close query tabs, close others / left / right, close all | Result tab closing | ui.editors.sql `SQLEditor` result tab menu | PG, My | **gap**: G4 |
| CM-129 | Pin / unpin tab, unpin all | Pinned results survive new runs | ui.editors.sql `toggle.pinned.tab` | PG, My | **gap**: G4 |
| CM-130 | Set tab name, detach | Names a result tab; opens it in its own window | ui.editors.sql `action_result_tabs_set_name`, `_detach_tab` | PG, My | **gap**: G4 (set name); detach on the roadmap with SQL-22 |

### Status bar

| ID | Entry | What it does | DBeaver source | PG / MySQL | Kerno |
| -- | ---- | ---- | ---- | ---- | ---- |
| CM-131 | Active connection / schema selector | Switch the editor's target | core / ui.navigator `ui.tools.select.connection`, `.schema` (toolbar) | PG, My | **planned**: `docs/plans/CTX.md` Part 2 (connection and database items) |
| CM-132 | Transaction mode, Commit, Rollback | Transaction controls | core `menu.txn`, `commit.menu`, `rollback.menu` | PG, My | **planned**: `docs/plans/CTX.md` Part 2 (transaction item) |
| CM-133 | Transaction log, pending transactions | Transaction monitor entries | core `menu.txn.log` (`core.txn.log`, `core.txn.pending`) | PG, My | **gap**: G14 (log); pending list on the roadmap (CON-46) |

## Summary

Counts per status and per area (recounted from the rows above). "Gap" rows are ordered in `docs/plans/PARITY-GAPS.md` (G1 to G17), "done, untested" rows in its Part G18. The 67 rows once proposed for the roadmap were accepted on 2026-11-13 and are counted under roadmap.

| Area | done | done, untested | planned | gap | roadmap | total |
| ---- | --: | --: | --: | --: | --: | --: |
| Connections | 22 | 1 | 4 | 9 | 15 | 51 |
| Navigator | 17 | 0 | 6 | 18 | 18 | 59 |
| SQL editor | 20 | 3 | 3 | 12 | 16 | 54 |
| Results and data editing | 26 | 2 | 8 | 21 | 15 | 72 |
| Object editors (structure) | 11 | 0 | 2 | 2 | 6 | 21 |
| Tools and administration | 1 | 0 | 4 | 8 | 15 | 28 |
| Context menus | 39 | 8 | 39 | 36 | 11 | 133 |
| **All** | **136** | **14** | **66** | **106** | **96** | **418** |

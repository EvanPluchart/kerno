# What DBeaver users ask for and complain about, and where Kerno stands

Source: 879 issues of `dbeaver/dbeaver` (the top 500 by +1 and the top 500 by comments, deduplicated), read on
2026-11-14. Kerno docs: `docs/FEATURES.md`, `docs/ROADMAP.md`, `docs/DBEAVER-PARITY.md`, `docs/DECISIONS.md`,
`docs/BENCH.md` and CLAUDE.md §4 of `/Users/e-pluchart/projects/perso/kerno` (HEAD described by FEATURES.md as
`97a05b8`). File references without a path are in `kerno/docs/`. `gpui-base` references are in the pinned kit
source, `~/.cargo/registry/src/*/gpui-base-0.9.1/src/`.

## Method and limits

- **Scope filter.** 211 issues are dropped: other engines (Oracle, SQL Server, ClickHouse, Mongo, SQLite,
  Redshift, BigQuery, new-driver requests...), Windows-only issues, and PRO/EE items. Five PRO-labelled or
  wrongly matched issues whose demand applies to Kerno were kept: #3270 (RDS IAM), #6861 (multiple results),
  #18393 (Parquet), #9107 (SSH over SSM), #21241 (PostgreSQL 17 backup).
- **Grouping.** Titles were matched to themes by keywords, then the top unmatched issues were sorted by hand.
  462 in-scope issues fall into the 49 themes below; 206 in-scope issues stay unclustered. They are a long
  tail of one-off bugs, mostly with 0-8 +1 (largest: #6592 "MockData: remove from CE", 84 reactions but 0 +1;
  #22299 a macOS editor rendering bug; #4738 run a script without opening it, 23 up).
- **Score** = all reactions + comments, as asked. Comments inflate bug threads ("me too"), so the +1 count is
  shown beside it and a second table ranks the actionable themes by +1. `r` counts every reaction type: #6592
  has 68 reactions and 0 +1, so its reactions are not +1 demand.
- **Bias.** The sample is the top of two popularity lists, so it over-represents old, long-running threads and
  under-represents recent requests.
- **Reading.** For the 40 issues with the most weight for the comparison, the body (first ~550 characters) and
  the 3 most +1'd comments were read with `gh issue view --json`: #1361 #7976 #36951 #20072 #5317 #6064 #9414 #2316 #9964 #36006 #19779 #5845 #14749 #7693 #7114
  #6861 #11140 #17767 #7777 #17817 #8219 #37897 #3094 #6903 #38133 #4516 #16973 #3270 #39968 #9413 #2738
  #17240 #23248 #3605 #4222 #290 #15310 #34959 #21242 #222.
- **Kerno status.** `done` = in FEATURES.md with tests; `done (by design)` = the failure cannot occur in
  Kerno's architecture; `partial`; `missing`; `out of scope`. A qualifier in brackets says what is untested or
  unverified (the docs do not say and the code was not conclusive). Recommendations: add / fix / improve / skip.

## Ranked themes (by score = reactions + comments)

In "Demand evidence", `c` is comments. "open since YYYY" marks issues still open.

| # | Theme | Demand evidence | What users want | Kerno status (proof) | Rec. | Why |
|---|---|---|---|---|---|---|
| 1 | Startup failures and crashes after install/update (JVM, SWT, snap glibc) | 32 issues, score 1022, +1 205; top: #19779 (28 up, 79 c), #36006 (46 up, 47 c), #39342 (25 up, 12 c), #25134 (6 up, 36 c, open since 2024); closed not_planned: #36020, #22890 | The app should simply start after an update, with no JVM, SWT or glibc mismatch. | **done (by design)**: No JVM: native binary, cold start 279 ms (BENCH.md:10); Linux tarball built in Docker (FEATURES.md:290) | **improve** | #36006 (glibc 2.33 missing on Ubuntu 20.04) shows a native build can break the same way: pin and document the oldest glibc the tarball runs on. |
| 2 | Linux/macOS distribution and updates (apt/rpm repo, snap, Flatpak, AppImage, auto-update, ARM, keep plugins) | 29 issues, score 1001, +1 446; top: #5317 (129 up, 51 c, open since 2019), #12441 (22 up, 32 c), #1667 (38 up, 15 c), #8508 (20 up, 30 c) | Install from the distro's package manager and get updates automatically, without losing plugins or settings. | **partial (plugin half of #5317 out of scope)**: macOS dmg + Linux tarball (FEATURES.md:290); in-app update check and install (FEATURES.md:291-292); AppImage deferred (ROADMAP.md:45-47); amd64 needs an amd64 host (ROADMAP.md:48-50) | **improve** | #5317 (129 up, open since 2019) is about Eclipse plugins lost on update, which Kerno does not have. The rest is spread over channels (#1667 PPA 38 up, #2299 snap 29, #3060 Flatpak 17, #8057 RPM open since 2020); an AppImage plus a .deb on amd64 covers most of it. |
| 3 | Native client path for backup/restore (pg_dump, mysqldump, psql) | 22 issues, score 859, +1 386; top: #1361 (183 up, 64 c), #7976 (67 up, 50 c), #988 (3 up, 59 c), #18958 (18 up, 32 c) | Backup/restore should find the installed client tools on its own (Homebrew, Postgres.app, /usr/bin) instead of failing with 'Client home is not specified'. | **done**: FEATURES.md:230 (every install per engine family detected, default per family); CLAUDE.md:300-301 | **skip** | Already Kerno's answer to the most-reacted issue in this sample (#1361); say so in the README. |
| 4 | Appearance: follow OS theme, dark theme quality, zoom and fonts | 23 issues, score 542, +1 235; top: #21600 (68 up, 28 c, open since 2023), #17817 (49 up, 7 c, open since 2022), #19982 (3 up, 32 c), #5095 (9 up, 21 c) | Theme that follows the OS, a readable dark theme, and Ctrl/Cmd+wheel zoom in the editor. | **done (editor zoom unverified)**: Follow system: CLAUDE.md:125, crates/kerno/src/theme.rs:30, crates/kerno/src/actions.rs:1728; grid zoom DBEAVER-PARITY.md:295; editor font in settings FEATURES.md:286 | **skip** | #21600 (68 up, open since 2023, labelled third party issue) is done in Kerno; only check that #17817's Ctrl+wheel/Cmd+= also zooms the editor. |
| 5 | Data editing reliability (UUID keys, binding errors, keyless tables, enum/citext/boolean) | 20 issues, score 501, +1 148; top: #9964 (66 up, 40 c), #10048 (36 up, 35 c), #5587 (5 up, 25 c), #21510 (2 up, 25 c); closed not_planned: #21510, #19434 | Editing a cell just works, whatever the key type, and read-only cases say why. | **done (UUID untested)**: Values written as dialect literals (DECISIONS.md:132); enum/boolean editors (FEATURES.md:186); read-only with reason (FEATURES.md:185); no UUID test in crates/kerno/src/ui_tests* | **improve** | #9964 (66 up) and #10048 (36 up) were type-binding regressions: add UI tests editing rows keyed by uuid, citext and enum. |
| 6 | Linux desktop integration: Wayland/fractional scaling, flicker, clipboard, freezes after focus change | 13 issues, score 496, +1 136; top: #22951 (29 up, 41 c), #21242 (12 up, 58 c), #6581 (22 up, 26 c), #8129 (18 up, 30 c); closed not_planned: #34528 | Correct rendering and no freezes on GNOME/KDE Wayland, HiDPI and multi-monitor setups. | **partial (Wayland untested)**: DECISIONS.md:605 ('Not verified: ... Wayland, amd64') | **fix** | #21242 (58 comments) and #37693 are freezes on Ubuntu; GPUI on Wayland is untested in Kerno, so test it before claiming Linux support. |
| 7 | Editor keys: macOS Home/End, Tab to accept completion, word boundaries, shortcuts that stop working | 12 issues, score 460, +1 146; top: #9414 (7 up, 106 c, open since 2020), #2316 (73 up, 38 c), #1155 (17 up, 25 c), #35109 (18 up, 17 c) | Keys should behave like other macOS/IDE editors and never silently change (Home/End jumping to file start, Enter-only completion). | **partial**: Home/End and Cmd+Left bound to line start (gpui-base 0.9.1 input/base/state.rs:250,299); '_' is a word char (rope_ext.rs:373); keymap override (FEATURES.md:287); Tab is missing: the completion popup handles only Enter, Escape, Up, Down (gpui-component 0.9.1 input/popovers/completion_menu.rs:305-315), so Tab falls through to IndentInline (gpui-base state.rs:219) and indents | **fix** | #2316 (73 up, done in DBeaver since) wants Tab to accept a proposal; Kerno's popup accepts on Enter only (the kit's menu ignores Tab): add Tab as an accept key. |
| 8 | Connection resilience: reconnect after network loss, keep-alive, idle transactions | 18 issues, score 444, +1 90; top: #8303 (10 up, 38 c), #6903 (22 up, 24 c, open since 2019), #35705 (7 up, 31 c, open since 2024), #2201 (8 up, 30 c) | Recover in seconds after a VPN/network drop instead of minutes, without restarting the app. | **done (not timed)**: Auto-reconnect (FEATURES.md:35); close idle transactions per environment (ROADMAP.md:129); failures in plain words (FEATURES.md:53) | **improve** | #6903 (open, 22 up) reports about 100 s to reconnect after a VPN drop (reporter on Oracle/SQL Server, commenters on other engines): measure Kerno's reconnect time after a dropped link and put it in BENCH.md. |
| 9 | SSH key formats and auth failures | 13 issues, score 421, +1 97; top: #5845 (38 up, 44 c), #14749 (0 up, 56 c), #38739 (13 up, 42 c), #1677 (24 up, 19 c) | Any key that works with `ssh` should work in the tunnel (OpenSSH-format RSA, ed25519, agent). | **partial**: Key file + passphrase + agent done (FEATURES.md:32); RSA key files refused (ROADMAP.md:51, DECISIONS.md:203) | **fix** | The top SSH complaints are key-format failures (#5845: 38 up, 44 comments; #14749: 56 comments) and #5845's key is `ssh-keygen -t rsa`: refusing RSA files recreates DBeaver's worst SSH pain. |
| 10 | PostgreSQL/MySQL object features: roles, sequences, privileges, enums, key renaming, EXPLAIN, ON CONFLICT | 26 issues, score 391, +1 103; top: #10818 (0 up, 26 c), #1219 (7 up, 14 c), #14107 (0 up, 20 c, open since 2021), #6440 (0 up, 20 c) | Edit roles and sequences, see default privileges, rename keys and indexes, generate ON CONFLICT upserts, and a readable EXPLAIN. | **partial**: Enums create/rename/drop (FEATURES.md:210); EXPLAIN plan tree (FEATURES.md:136); error position (FEATURES.md:133); roles read-only (FEATURES.md:243), role editors deferred (ROADMAP.md:114, OBJ-17); sequence editor deferred (ROADMAP.md:199, OBJ-11); ON CONFLICT templates deferred (ROADMAP.md:152, NAV-52) | **improve** | Mostly old one-off bugs; the open asks are #36108 ON CONFLICT (12 up), #19644 sequences (8 up), #21726 enum refresh. A sequence editor and ON CONFLICT generation are small. |
| 11 | AI assistant (local LLM, Copilot, custom endpoint) | 5 issues, score 390, +1 290; top: #36951 (156 up, 13 c), #20072 (108 up, 35 c), #37960 (13 up, 7 c, open since 2025), #41171 (6 up, 12 c, open since 2026) | Natural language to SQL in Community, with a local model (Ollama) or a user-chosen endpoint so data stays local. | **missing**: Roadmap item 8, conditional on 'nothing leaves the machine' (ROADMAP.md:34-37) | **add** | #36951 (156 up, 2nd most +1'd issue in the sample) and #20072 (108 up) were both closed as completed: AI is now expected in a free client. A local model or user-set endpoint fits the roadmap's own condition. |
| 12 | Active database switching, 'show all databases', search_path | 13 issues, score 388, +1 21; top: #7693 (6 up, 71 c), #11453 (0 up, 55 c), #19032 (0 up, 44 c), #1346 (0 up, 33 c) | Each editor picks its database/schema easily, and every database of a PostgreSQL server is visible. | **done**: Per-tab connection/database/schema selectors (FEATURES.md:114-115); Set as default for the tab (FEATURES.md:87); default database first, others listed (FEATURES.md:77) | **skip** | #7693 (71 comments) is covered. |
| 13 | Navigator organisation: folders, drag, link with editor, sizes | 17 issues, score 382, +1 124; top: #6489 (8 up, 53 c), #18799 (18 up, 32 c), #1323 (15 up, 16 c), #22655 (3 up, 22 c) | Organise connections in folders by drag and drop, link the tree with the editor, see table sizes. | **done**: Folders and drag (FEATURES.md:38-39); link with editor (FEATURES.md:98); size hints (FEATURES.md:79) | **skip** | Covered. |
| 14 | Freezes, UI lag, memory, slow metadata loading | 16 issues, score 370, +1 52; top: #7777 (21 up, 38 c), #36542 (2 up, 36 c), #21214 (1 up, 34 c), #35153 (1 up, 25 c); closed not_planned: #35261, #35153 | A UI that never blocks while typing, scrolling or loading metadata, and does not grow to gigabytes of RAM. | **done**: BENCH.md:10-13 (279 ms start, 148 MB idle RSS, 8.35 ms frames on 1M rows) | **skip** | Kerno's main selling point against #7777 and #290; keep the budgets in BENCH.md. |
| 15 | SQL formatter options (keyword case, format on save, CTEs, comments) | 17 issues, score 353, +1 123; top: #267 (8 up, 54 c), #18131 (7 up, 23 c), #8552 (15 up, 14 c, open since 2020), #1531 (10 up, 18 c) | A configurable formatter: keyword case, indentation style, CTE layout, format on save. | **partial**: Format SQL and case changes done (FEATURES.md:125-126); no formatter preferences listed | **improve** | #267 (54 comments) asks for an individual formatter; #22488, #19379, #38339 ask for case and on-save options. |
| 16 | Result grid: sort, column sizing, navigation | 15 issues, score 348, +1 121; top: #11140 (42 up, 23 c), #18274 (17 up, 13 c), #10162 (0 up, 28 c), #6045 (6 up, 20 c) | Table data sorted by primary key, columns fitted to names, jump to last row, configurable fetch size. | **done**: ORDER BY always ends with the primary key (DECISIONS.md:512); fit width (FEATURES.md:168); go to row/last row (DBEAVER-PARITY.md:277-278); fetch size (DBEAVER-PARITY.md:282) | **skip** | #11140 (42 up) is answered by the paging design. |
| 17 | CSV import/export correctness | 19 issues, score 346, +1 76; top: #130 (15 up, 14 c), #18946 (7 up, 21 c), #7731 (1 up, 24 c), #41263 (0 up, 25 c, open since 2026) | Exports that keep aliases and quoting; imports that handle decimal commas, NULL rules and big files. | **done**: FEATURES.md:219-225 (options, cancel, mapping, NULL rule, one transaction) | **skip** | Check decimal-comma parsing (#35020, open) and disabling FK checks during import (#1752, 11 up). |
| 18 | Autocompletion quality | 17 issues, score 339, +1 60; top: #36725 (5 up, 35 c), #7879 (2 up, 34 c, open since 2020), #5247 (7 up, 22 c), #37135 (2 up, 25 c) | Completion that keeps working after WHERE/JOIN/aliases, ranks well and can expand `*`. | **done**: FEATURES.md:119-120 (tables, columns, aliases, joins on FKs; on demand) | **skip** | Mostly DBeaver regressions; `*` expansion (#11220) not listed in Kerno. |
| 19 | Multi-cursor and multi-line selection | 5 issues, score 277, +1 112; top: #6064 (85 up, 59 c), #39381 (11 up, 34 c), #39695 (4 up, 15 c), #39738 (5 up, 12 c) | VS Code-style multiple cursors and column selection that behave predictably. | **done**: FEATURES.md:124 (Cmd+Alt+Up/Down) | **improve** | #6064 top comment (43 up) complains DBeaver's version is far from VS Code; add Cmd+D (next occurrence) on top of the existing cursors. |
| 20 | Value viewers: JSON, HTML, long text, blobs | 11 issues, score 218, +1 93; top: #36898 (24 up, 18 c), #494 (1 up, 27 c), #2944 (16 up, 8 c), #6989 (0 up, 17 c) | Pretty JSON, rendered HTML, wrapped long text and image/PDF blobs in the value panel. | **partial**: Text, indented JSON, hex dump (FEATURES.md:164) | **improve** | #36898 (24 up) HTML and #40792 text wrap; low cost additions to the existing panel. |
| 21 | Driver download failures | 7 issues, score 205, +1 23; top: #3780 (13 up, 36 c), #34775 (9 up, 33 c), #18721 (0 up, 27 c), #23386 (0 up, 24 c); closed not_planned: #18721, #23386 | No driver to download at first connect. | **done (by design)**: Native drivers, no JDBC (ROADMAP.md:86) | **skip** | Nothing to download. |
| 22 | Transactions and auto-commit defaults | 7 issues, score 189, +1 44; top: #2738 (17 up, 23 c), #1562 (8 up, 33 c), #39979 (12 up, 18 c), #1555 (2 up, 26 c) | Manual commit by default where it matters, and a commit mode indicator that tells the truth. | **done**: Manual by default on production (FEATURES.md:45); per-tab mode, visible state (FEATURES.md:137-139) | **skip** | Covered. |
| 23 | Editor tabs: multiple rows, vertical tabs, overflow | 6 issues, score 179, +1 125; top: #7114 (59 up, 16 c, open since 2019), #23248 (22 up, 24 c, open since 2024), #21485 (20 up, 5 c, open since 2023), #14546 (8 up, 4 c, open since 2021) | With many open queries, wrap tabs on several rows or show them in a vertical list. | **missing**: Tabs bar menu only (FEATURES.md:269); no overflow layout in the docs | **add** | #7114 (59 up, open since 2019) is blocked in DBeaver by Eclipse (a commenter quotes the maintainers saying so); a native UI is free of that limit. |
| 24 | Password from a command / cloud IAM tokens | 4 issues, score 175, +1 66; top: #3270 (30 up, 24 c), #9413 (21 up, 17 c, open since 2020), #21616 (4 up, 30 c), #39968 (11 up, 11 c) | Get the password at connect time from a shell command (aws rds generate-db-auth-token, gcloud, Bitwarden, Vault). | **missing**: CON-49 shell commands deferred as 'a security surface' (ROADMAP.md:137) | **add** | #3270 (57 reactions), #9413 (21 up, open), #39968: one narrow 'password command' field answers all three without running arbitrary connect hooks. |
| 25 | Client time zone breaking PostgreSQL connections | 5 issues, score 175, +1 41; top: #36487 (10 up, 60 c), #18965 (27 up, 16 c), #22947 (0 up, 26 c), #4181 (0 up, 20 c) | Connecting must not fail because the client OS time zone name (Asia/Calcutta, Europe/Kiev) is unknown to the server. | **done (by design, unverified)**: No TimeZone startup parameter set in crates/kerno-postgres/src (grep finds none) | **skip** | #36487 (60 comments) and #18965 come from the client's zone name being sent to the server; verify once with TZ=Asia/Calcutta. |
| 26 | Result grid: quick in-place filter and find | 6 issues, score 166, +1 89; top: #38133 (39 up, 11 c), #3052 (19 up, 12 c), #336 (0 up, 25 c), #18589 (10 up, 13 c) | Type to filter or find in the rows already fetched, without re-running the query (MySQL Workbench / HeidiSQL style). | **partial**: Filter re-runs the SELECT on the server (DBEAVER-PARITY.md:264, FEATURES.md:145); only 'Find in Editor' exists (crates/kerno/src/actions.rs:1192) | **add** | #38133 (39 up) and #3052 (19 up) want client-side find/filter; cheap on the virtualized grid. |
| 27 | TLS/SSL connection setup | 8 issues, score 162, +1 17; top: #12668 (3 up, 23 c), #18749 (0 up, 24 c), #5601 (3 up, 20 c), #11360 (0 up, 23 c) | Connect with client certificates and old/new TLS versions without Java keystore errors. | **done**: FEATURES.md:31 (modes disable..verify-full, CA/cert/key files) | **skip** | Most failures are JVM keystore issues (#5601, #11360) Kerno does not have. |
| 28 | Tabs coloured/grouped by connection, reopen closed tab, tab titles | 8 issues, score 157, +1 100; top: #10235 (31 up, 13 c), #18709 (23 up, 7 c, open since 2023), #21561 (19 up, 9 c, open since 2023), #16827 (8 up, 7 c) | See which connection a tab belongs to, reopen a closed tab, rename and duplicate tabs. | **done**: Tab titles tinted by environment (FEATURES.md:50); Reopen closed tab, Duplicate (FEATURES.md:269); rename (FEATURES.md:144) | **skip** | Covered; #21561 (19 up) still open in DBeaver. |
| 29 | SSH jump hosts, ~/.ssh/config, SSM | 6 issues, score 156, +1 112; top: #9107 (42 up, 14 c), #4516 (23 up, 11 c), #17976 (19 up, 4 c), #10273 (12 up, 8 c, open since 2020) | Reach a database through one or more bastions, reusing the hosts already defined in ~/.ssh/config (ProxyJump). | **missing**: CON-26 deferred, 'bastion chains are uncommon' (ROADMAP.md:130) | **add** | Contradicted by demand: #4516 (23 up), #17976 (19), #11473, #16973; reading ~/.ssh/config with ProxyJump covers most of it. |
| 30 | Vim / Emacs editing modes | 4 issues, score 153, +1 90; top: #8219 (39 up, 17 c), #37897 (32 up, 8 c, open since 2025), #4483 (1 up, 29 c, open since 2018), #3448 (18 up, 9 c) | A vi mode in the SQL editor (DBeaver users install the Vrapper Eclipse plugin and lose it on every update, #5317). | **missing**: No mention in FEATURES.md, DBEAVER-PARITY.md or ROADMAP.md | **add** | 90 up across 3 requests, #37897 open; an opt-in vi mode is a clear differentiator for terminal users. |
| 31 | Lost scripts/connections/settings after update; autosave | 7 issues, score 152, +1 20; top: #4577 (1 up, 33 c), #38351 (1 up, 25 c), #34514 (9 up, 15 c, open since 2024), #18814 (0 up, 22 c); closed not_planned: #38351, #35114 | Updates never lose connections, scripts or settings; no 'save script?' nagging. | **done**: Tabs and unsaved text restored after restart (FEATURES.md:116); update install writes tabs and settings and rolls back on failure (FEATURES.md:292) | **skip** | Keep a storage-migration test per release. |
| 32 | ERD, diagram notations, Mermaid, data dictionary | 9 issues, score 152, +1 106; top: #1017 (35 up, 12 c), #36058 (23 up, 2 c, open since 2024), #4250 (15 up, 9 c, open since 2018), #7257 (6 up, 7 c); closed not_planned: #10767 | ER diagrams with crow's-foot notation, exportable as Mermaid or image, plus a data dictionary. | **missing (planned)**: Roadmap item 1 (ROADMAP.md:9-11) | **add** | #1017 (35 up) and #36058 Mermaid (23 up, open); export to Mermaid text is cheap and fits 'nothing leaves the machine'. |
| 33 | Export formats: XLSX and Parquet | 4 issues, score 127, +1 60; top: #18393 (41 up, 23 c), #625 (6 up, 22 c), #42185 (7 up, 20 c), #23140 (6 up, 1 c, open since 2024) | Export results to Excel and Parquet without a paid edition or an extra plugin. | **missing**: Exports CSV/TSV/JSON/NDJSON/SQL/Markdown only (FEATURES.md:219; ROADMAP.md:98-104) | **add** | #18393 Parquet (41 up) is labelled PRO in DBeaver; #42185 (XLSX target missing in CE 26.2.1, a regression) shows how often export breaks. |
| 34 | Multiple results in one tab / results in a separate window | 4 issues, score 121, +1 57; top: #6861 (37 up, 24 c), #667 (8 up, 12 c), #34959 (7 up, 10 c, open since 2024), #6195 (5 up, 8 c, open since 2019) | Show the results of several statements stacked in one scrollable view (SSMS style), or on a second monitor. | **missing**: SQL-20 deferred as a layout preference (ROADMAP.md:163); detach on the roadmap (DBEAVER-PARITY.md:613) | **add** | #6861 (47 reactions): commenters say its absence keeps them on SSMS; the roadmap's reason undersells it. |
| 35 | Run a script on several connections | 4 issues, score 114, +1 61; top: #3605 (18 up, 28 c), #1804 (28 up, 17 c, open since 2017), #34071 (8 up, 5 c, open since 2024), #1273 (7 up, 3 c) | Run the same script on N connections or databases at once and see per-target results. | **missing**: No mention in FEATURES.md, DBEAVER-PARITY.md or ROADMAP.md | **add** | #3605 (28 comments) and #1804 (28 up, open since 2017); common for multi-tenant schemas. |
| 36 | PostGIS geometry viewer | 4 issues, score 110, +1 23; top: #4222 (12 up, 34 c), #18117 (6 up, 22 c), #5832 (0 up, 23 c), #3358 (5 up, 8 c, open since 2018) | See geometry/geography results on a map. | **missing**: No mention in Kerno docs; MySQL geometry shown as binary (DECISIONS.md:269) | **add** | #4222 (34 comments; pgAdmin 4 has such a viewer, and a commenter said it let them recommend DBeaver over pgAdmin); PostgreSQL-only, in scope. |
| 37 | Copy tables with data to another connection / DB-to-DB transfer | 4 issues, score 105, +1 62; top: #17767 (49 up, 17 c, open since 2022), #19351 (0 up, 17 c), #9757 (4 up, 7 c, open since 2020), #21516 (9 up, 2 c, open since 2023) | Copy one or many tables (structure, constraints, data) from one database to another in one action, like Navicat/HeidiSQL. | **missing**: TOOL-19 deferred (ROADMAP.md:104) | **add** | #17767 (49 up, open) is the top open data-transfer request. |
| 38 | Cancel of running queries | 2 issues, score 105, +1 14; top: #3094 (14 up, 53 c), #2701 (0 up, 38 c) | Cancel returns at once, even when the network is gone. | **done**: FEATURES.md:131 (cancel), FEATURES.md:180 (cancel a slow page) | **skip** | Check cancel while the VPN is down (#3094's case). |
| 39 | Statistics tab stealing focus after a script | 3 issues, score 88, +1 11; top: #7704 (5 up, 35 c), #17763 (3 up, 21 c), #20416 (3 up, 21 c) | After running a script, keep the last result in front instead of a statistics tab. | **done (unverified)**: Kerno opens one result per statement (FEATURES.md:128); no statistics tab is listed | **skip** | Check that a script run focuses the last result. |
| 40 | Highlight / select all occurrences of the selected word | 3 issues, score 87, +1 28; top: #22320 (0 up, 40 c), #222 (19 up, 12 c), #2770 (9 up, 7 c) | Mark every occurrence of the word under the cursor, and select them all (Cmd+D). | **missing**: Not in FEATURES.md or DBEAVER-PARITY.md | **add** | Small, expected in any code editor (#222, 19 up); pairs with multi-cursor. |
| 41 | SQL editor: code folding | 2 issues, score 62, +1 56; top: #17240 (27 up, 5 c, open since 2022), #23137 (29 up, 1 c, open since 2024) | Fold on indentation, on parentheses/CASE blocks and on marker comments, including inside $$ bodies. | **partial (unverified)**: SQL-44 deferred (ROADMAP.md:170), but EditorState::new sets code-editor mode (gpui-base input/base/state.rs:12894-12896) whose default is folding: true (mode.rs:88), fed by tree-sitter fold candidates; Kerno never turns it off | **improve** | Both requests are open (56 up combined). Folding may already work on parentheses/blocks: check in the app, then update ROADMAP.md:170 and add indentation/marker-comment folds if missing. |
| 42 | Schema/data compare | 3 issues, score 51, +1 11; top: #20964 (1 up, 21 c, open since 2023), #229 (4 up, 15 c), #22449 (6 up, 4 c, open since 2024) | Compare structure and data of two tables or databases. | **missing (planned)**: Roadmap items 2-3 (ROADMAP.md:12-18) | **add** | As planned; modest demand in this sample. |
| 43 | HTTP/SOCKS proxy | 2 issues, score 48, +1 13; top: #4445 (13 up, 18 c, open since 2018), #18792 (0 up, 17 c); closed not_planned: #18792 | Connect through a corporate HTTP or SOCKS proxy. | **missing**: CON-29 deferred, 'enterprise networks only' (ROADMAP.md:133) | **skip** | #4445 (13 up, open since 2018): low demand. |
| 44 | PostgreSQL row-level security policies in the navigator | 1 issues, score 46, +1 34; top: #5499 (34 up, 6 c) | See and edit RLS policies of a table. | **partial**: NAV-09 policies deferred, visible through Show DDL (ROADMAP.md:144) | **improve** | #5499 (34 up, 40 reactions); a read-only Policies node under each table is cheap. |
| 45 | Data analysis: charts, notebooks | 2 issues, score 46, +1 36; top: #5583 (20 up, 4 c, open since 2019), #35602 (16 up, 5 c, open since 2024) | Quick charts and notebook-style SQL documents. | **missing**: Dashboards on the roadmap (ROADMAP.md:32) | **skip** | Low demand in this sample (36 up). |
| 46 | Export/share saved tasks | 1 issues, score 42, +1 35; top: #22579 (35 up, 7 c, open since 2024) | Export saved tasks (backup, export jobs) to a file and import them elsewhere. | **partial**: Saved tasks exist (FEATURES.md:232); connections export as JSON (FEATURES.md:44) but tasks are not said to | **add** | #22579 (35 up, open); reuse the JSON export of connections. |
| 47 | PRQL query language | 1 issues, score 32, +1 29; top: #22495 (29 up, 3 c); closed not_planned: #22495 | Write PRQL and run it as SQL. | **missing**: Not in Kerno docs | **skip** | #22495 (29 up) was closed not_planned by DBeaver; niche. |
| 48 | Export DDL of many objects at once | 1 issues, score 30, +1 21; top: #5693 (21 up, 9 c, open since 2019) | Select many tables/views and get one DDL script. | **partial**: Show DDL per object (FEATURES.md:80); schema-only dump through the backup wizard (FEATURES.md:228); multi-selection deferred (ROADMAP.md:149) | **skip** | The pg_dump/mysqldump path covers it; mention it in docs. |
| 49 | Real bind parameters (prepared statements) | 1 issues, score 26, +1 20; top: #15310 (20 up, 6 c, open since 2022) | Send parameters as server binds so plans and behaviour match the application. | **missing**: Parameters become literals (FEATURES.md:135 `values_become_literals`; DECISIONS.md:434) | **improve** | #15310 (20 up, open): same behaviour as DBeaver today; using PG/MySQL binds would be a quiet advantage. |

## Actionable themes ranked by +1 only

Same themes, keeping only add / fix / improve. The +1 count is a better measure of feature demand than
comments. Note: 129 of the 446 +1 in the distribution row come from #5317 (Eclipse plugins lost on update),
which does not apply to Kerno; without it the row has 317.

| Theme | +1 | Open issues | Kerno | Rec. |
|---|---|---|---|---|
| Linux/macOS distribution and updates (apt/rpm repo, snap, Flatpak, AppImage, auto-update, ARM, keep plugins) | 446 | 7 | partial (plugin half of #5317 out of scope) | improve |
| AI assistant (local LLM, Copilot, custom endpoint) | 290 | 2 | missing | add |
| Startup failures and crashes after install/update (JVM, SWT, snap glibc) | 205 | 5 | done (by design) | improve |
| Data editing reliability (UUID keys, binding errors, keyless tables, enum/citext/boolean) | 148 | 2 | done (UUID untested) | improve |
| Editor keys: macOS Home/End, Tab to accept completion, word boundaries, shortcuts that stop working | 146 | 4 | partial | fix |
| Linux desktop integration: Wayland/fractional scaling, flicker, clipboard, freezes after focus change | 136 | 3 | partial (Wayland untested) | fix |
| Editor tabs: multiple rows, vertical tabs, overflow | 125 | 6 | missing | add |
| SQL formatter options (keyword case, format on save, CTEs, comments) | 123 | 9 | partial | improve |
| Multi-cursor and multi-line selection | 112 | 0 | done | improve |
| SSH jump hosts, ~/.ssh/config, SSM | 112 | 2 | missing | add |
| ERD, diagram notations, Mermaid, data dictionary | 106 | 3 | missing (planned) | add |
| PostgreSQL/MySQL object features: roles, sequences, privileges, enums, key renaming, EXPLAIN, ON CONFLICT | 103 | 6 | partial | improve |
| SSH key formats and auth failures | 97 | 0 | partial | fix |
| Value viewers: JSON, HTML, long text, blobs | 93 | 5 | partial | improve |
| Vim / Emacs editing modes | 90 | 2 | missing | add |
| Connection resilience: reconnect after network loss, keep-alive, idle transactions | 90 | 7 | done (not timed) | improve |
| Result grid: quick in-place filter and find | 89 | 0 | partial | add |
| Password from a command / cloud IAM tokens | 66 | 1 | missing | add |
| Copy tables with data to another connection / DB-to-DB transfer | 62 | 3 | missing | add |
| Run a script on several connections | 61 | 2 | missing | add |
| Export formats: XLSX and Parquet | 60 | 1 | missing | add |
| Multiple results in one tab / results in a separate window | 57 | 2 | missing | add |
| SQL editor: code folding | 56 | 2 | partial (unverified) | improve |
| Export/share saved tasks | 35 | 1 | partial | add |
| PostgreSQL row-level security policies in the navigator | 34 | 0 | partial | improve |
| Highlight / select all occurrences of the selected word | 28 | 0 | missing | add |
| PostGIS geometry viewer | 23 | 1 | missing | add |
| Real bind parameters (prepared statements) | 20 | 1 | missing | improve |
| Schema/data compare | 11 | 2 | missing (planned) | add |

## Unmet demand: open for years, or closed as not planned

- **Open the longest with high +1:** #5317 keep plugins on update (129 up, since 2019; N/A to Kerno, which has
  no plugins); #7114 multiple tab rows (59 up, since 2019, labelled third party issue: Eclipse); #1804 composite
  data source / one script on many connections (28 up, since 2017); #6903 slow reconnect after a network loss
  (22 up, since 2019); #5693 export DDL of many objects (21 up, since 2019); #9413 password from a script (21
  up, since 2020); #5583 notebooks (20 up, since 2019); #4445 HTTP proxy (13 up, since 2018).
- **Open and recent, gathering votes ("wait for votes"):** #21600 theme follows the OS (68 up), #17767 copy
  tables with data (49), #17817 Ctrl+wheel zoom (49), #22579 export tasks (35), #37897 vi mode (32), #23137 and
  #17240 code folding (29 + 27), #36058 Mermaid diagrams (23), #23248 vertical tabs (22), #15310 real bind
  parameters (20). 127 of the 462 issues placed in themes are still open.
- **Closed not_planned with real demand:** #22495 PRQL (29 up); #34528 flicker under Hyprland (13 up, 35 c);
  #36020 snap build not running on Ubuntu 24.04 (13 up); #10767 crow's-foot ERD notation (9 up); #38351 and
  #35114 connections or scripts lost after an update; #35153 excessive resource use.

## Where Kerno's own docs disagree with the demand

These are the most useful findings: a deferral reason that the issue data contradicts.

- **SSH jump servers.** ROADMAP.md:130 (CON-26) calls bastion chains "uncommon". #4516 (23 up), #17976 (19
  up), #11473 and #16973 (`~/.ssh/config`, which a commenter wants for multi-level jumps) ask for them; the
  #4516 workaround is a manual `ssh -L` through the jump host.
- **RSA SSH key files.** Kerno refuses them (ROADMAP.md:51, DECISIONS.md:203, RUSTSEC-2023-0071). The two
  largest SSH threads are key-format failures: #5845 (38 up, 44 comments; the key was made with
  `ssh-keygen -t rsa`, and the top comment, 119 up, is the `ssh-keygen -p -m PEM` conversion) and #14749
  (56 comments). Refusing RSA key files would recreate this pain for users who cannot use an agent.
- **Multiple results in one tab.** SQL-20 is deferred as a "layout preference" (ROADMAP.md:163); #6861 has 47
  reactions and commenters call it the feature that keeps them on SSMS.
- **Password from a command.** CON-49 is deferred as "a security surface" (ROADMAP.md:137). #3270 (57
  reactions), #9413 (21 up, open) and #39968 ask for one thing: a field whose command prints the password (AWS
  RDS IAM token, gcloud, Bitwarden, Vault). A single "password command" field, not general connect hooks,
  covers all three.
- **AI assistant.** Roadmap item 8 waits on "nothing leaves the machine" (ROADMAP.md:34-37). The top AI request,
  #36951 (156 up), asks precisely for local models (Ollama) for privacy, so the condition and the demand agree.
- **Code folding.** SQL-44 is deferred because it "depends on the kit's editor" (ROADMAP.md:170). The pinned
  kit's `EditorState::new` sets code-editor mode (`gpui-base` `input/base/state.rs:12894-12896`), whose default
  is `folding: true` (`mode.rs:88`), and Kerno never turns it off: folding may already work, and the roadmap
  line may be stale. #17240 and #23137 (fold on indentation and on marker comments) are open, 56 up together.
- **Tab to accept a completion.** Not in Kerno's docs, and the kit's completion popup reacts only to Enter,
  Escape, Up and Down (`gpui-component` `input/popovers/completion_menu.rs:305-315`), so Tab indents instead.
  #2316 has 73 up and DBeaver has since done it.
- **PostGIS geometry viewer.** In scope (PostgreSQL) and absent from every Kerno doc; #4222 (34 comments) and
  #18117.

## Where Kerno already answers the top complaints (say so)

- The most-reacted in-scope issue, #1361 "Client home is not specified" (183 up, 64 comments), and #7976 (67
  up) are covered by automatic detection of every local client install (FEATURES.md:230).
- The startup-failure cluster (#19779, #36006, #39342, #6353, #15213: JVM, SWT, snap) and the "slow and laggy"
  cluster (#7777, #290 "eats gigabytes of RAM") meet Kerno's measured numbers: 279 ms cold start, 148 MB idle
  RSS, 8.35 ms frames on 1M rows (BENCH.md:10-13).
- Theme follows the OS (#21600, 68 up, open): done (CLAUDE.md:125, `crates/kerno/src/theme.rs:30`).
- Tabs tinted by connection (#10235, 31 up), reopen closed tab (#21561, open): done (FEATURES.md:50, 269).
- Table data sorted by primary key (#11140, 42 up): every page is ordered by the row identity (DECISIONS.md:512).
- Driver download failures (#34775, #18721): native drivers, nothing to download (ROADMAP.md:86).

## What the frustrations sound like

One short verbatim quote; the rest are paraphrases of the issue or of its most +1'd comments (the +1 count
of the comment is given).

- #1361, top comment (112 up), on backup needing a native client: "Why can't i just select a filename and it
  would dump it there?"
- #5845 (54 up): keys made by a plain `ssh-keygen` are in the OpenSSH format, which DBeaver's tunnel rejects;
  the accepted fix (119 up) is to convert the key to PEM by hand.
- #6064 (43 up): DBeaver's multi-cursor is nowhere near VS Code's; selection and arrow keys collapse it.
- #2316 (9 up): accepting a proposal with Enter only is an Eclipse habit nobody else has.
- #9414 (7 up per comment): Home/End start jumping to the start or end of the whole script after a few
  minutes of editing on macOS.
- #6903 (12 and 8 up): reconnecting after a VPN drop takes minutes, while the first connection takes under a
  second; users force-quit instead.
- #21242 (15 up): the table view freezes on Ubuntu multi-monitor setups after switching focus or minimising.
- #7777 (5 up): the UI stays laggy on macOS, worst on a 4K external display.
- #6861 (9 up): stacked results in one view are the feature that keeps SQL Server users on SSMS.
- #38133 (10 up): an in-place filter on fetched rows is the one thing MySQL Workbench has that DBeaver lacks.
- #7114 (5 up): multiple tab rows were asked for years ago with no progress; Eclipse cannot lay tabs out that
  way.
- #5317 (23 up): the Vim plugin (Vrapper) has to be reinstalled after every DBeaver update.
- #20072 (body): the team writes queries in VS Code with Copilot and only pastes them into DBeaver.
- #36951 (body): a local LLM is wanted for people who must keep sensitive data on their machine.
- #9413 (body and comments): passwords live in secret managers or cloud CLIs (AWS, gcloud, Bitwarden) and
  should be fetched at connect time.
- #15310 (13 up): parameters are pasted into the SQL text, so a statement cannot be reproduced the way the
  application runs it with prepared statements.
- #2738 (8 up): auto-commit on by default leads to accidental data changes; commenters want it off.

# What competitors' users ask for, and what Kerno does about it

Triage of 2026-11-14, made for the owner from the most-voted and most-commented GitHub issues of DBeaver
(879 issues) and Beekeeper Studio (827 issues). The evidence, with issue numbers and counts, is in
`docs/research/dbeaver-issues.md` and `docs/research/beekeeper-issues.md`. The owner's rule: to stand out, do
what the competitors' users want. CLAUDE.md §4 J turns the "do" list into requirements; `docs/GOAL-2.md` is the
goal that builds it.

## Already answered by Kerno (say it in the README and on the site)

- Startup failures, JVM crashes, freezes, memory (DBeaver's biggest pain, Beekeeper's Electron slowness): 279 ms
  cold start, 148 MB idle, 8.3 ms frames on 1M rows (`docs/BENCH.md`).
- "Client home is not specified" (DBeaver #1361, the most-reacted issue of the sample): local clients detected,
  every Homebrew version listed.
- Theme follows the OS, connection folders, per-tab database pickers, Run Current, several connections at once,
  auto-reconnect, `RAISE NOTICE` output, renaming tabs, syntax errors with a position.
- Free in Kerno, paid in Beekeeper Ultimate: CSV import, NULL text in export, query formatter, editing query
  results, full database backup.

## Do (v1.1, CLAUDE.md §4 J)

| # | Item | Demand (issues) | Kind |
|---|---|---|---|
| J1 | Tab accepts a completion | DBeaver #2316 (73 up) | fix |
| J2 | SSH through the system OpenSSH: `~/.ssh/config` hosts, ProxyJump/bastions, IdentityAgent (1Password, Secretive), RSA and every key OpenSSH reads | DBeaver #5845, #14749, #4516, #17976; Beekeeper #1240, #1344, #940 | fix + add |
| J3 | Password command, AWS RDS IAM preset, `.pgpass` and `~/.my.cnf` | DBeaver #3270 (57), #9413; Beekeeper #1443, #530 | add |
| J4 | `kerno <url>` from the shell | Beekeeper #361, #2002 | add |
| J5 | Editing reliability: uuid, citext, enum and binary keys, JSON round-trips; SQL expressions as pending values | DBeaver #9964 (66), #10048; Beekeeper #2313, #3029 | improve |
| J6 | Grid: quick filter and find on fetched rows; remember sort, hidden columns and widths per table; Unix timestamp hint | DBeaver #38133 (39); Beekeeper #524, #941 | add |
| J7 | Editor: highlight and select all occurrences, verified folding and zoom, formatter options and format on save | DBeaver #267, #22488, #17240, #17817 | improve |
| J8 | Reorder columns on MySQL/MariaDB | Beekeeper #1341 | add |
| J9 | Benchmarks for the cases competitors fail: 300-column table, 200 schemas × 1,200 tables, 200 MB `.sql` import, reconnect after a network loss | Beekeeper #752, #135, #702, #2325; DBeaver #6903 | measure |
| J10 | Tab overflow menu, several tab rows or vertical tabs | DBeaver #7114 (59), #23248 | add |
| J11 | Several results stacked in one tab | DBeaver #6861 (47) | add |
| J12 | XLSX export | DBeaver #18393 (41) | add |
| J13 | Copy tables with their data to another connection | DBeaver #17767 (49) | add |
| J14 | Run one script on several connections | DBeaver #3605, #1804 | add |
| J15 | Export and import connections and saved tasks (no secrets) | DBeaver #22579 (35) | add |
| J16 | Linux packages: x86_64 and arm64 tarballs, `.deb`, AppImage, documented glibc floor | DBeaver #2299, #36006; Beekeeper #22, #202, #1218 | add |
| J17 | AI assistant, opt-in, local model first | DBeaver #36951 (156), #20072 (108) | add |
| J18 | ER diagram of a schema, SVG/PNG/Mermaid export | DBeaver #36058; Beekeeper #492, #1160 | add |
| J19 | Vim mode, opt-in | DBeaver #37897, #8219; Beekeeper #317 | add |

## Not now (to `docs/ROADMAP.md` with the reason)

- Parquet export: heavy dependency for little demand; XLSX first.
- Wayland and fractional scaling checks: need a real Linux desktop session, not a container.
- Flathub, Snap or an apt repository: publishing is the owner's step; the packages of J16 come first.
- PostGIS geometry viewer, roles and sequences editors, row-level security in the navigator: real but smaller
  demand; next round.
- Schema and data compare, notebooks, PRQL, HTTP proxy, Emacs keys, real bind parameters: low demand in the
  sample.
- Other engines (MongoDB, Oracle, SQL Server, ClickHouse...): the biggest block of demand, out of v1's scope.

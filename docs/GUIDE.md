# Kerno guide

The reference for what the [README](../README.md) only summarises: connections through SSH, where passwords
come from, opening a connection from the shell, moving a setup, the AI assistant and exactly what it sends,
vim mode, installing and signing, building, the checks and the keyboard shortcuts. What works, with the tests
that cover it, is in [FEATURES.md](FEATURES.md).

## Contents

- [Supported servers](#supported-servers)
- [SQLite](#sqlite)
- [SSH](#ssh)
- [Passwords from a command, AWS RDS IAM, 1Password, .pgpass and .my.cnf](#passwords-from-a-command-aws-rds-iam-1password-pgpass-and-mycnf)
- [Open a connection from the shell](#open-a-connection-from-the-shell)
- [Move your setup to another computer](#move-your-setup-to-another-computer)
- [Which databases a PostgreSQL connection shows](#which-databases-a-postgresql-connection-shows)
- [When a connection opens again](#when-a-connection-opens-again)
- [AI assistant and what it sends](#ai-assistant-and-what-it-sends)
- [Vim mode](#vim-mode)
- [Updates](#updates)
- [Settings, environments and what wins](#settings-environments-and-what-wins)
- [Where a running script stands](#where-a-running-script-stands)
- [Rows as you scroll](#rows-as-you-scroll)
- [Foreign keys in the grids](#foreign-keys-in-the-grids)
- [The value panel](#the-value-panel)
- [Split views](#split-views)
- [A connection's SQL scripts](#a-connections-sql-scripts)
- [Backup and restore](#backup-and-restore)
- [PostgreSQL extensions](#postgresql-extensions)
- [Where Kerno keeps its data](#where-kerno-keeps-its-data)
- [Install](#install)
- [Build from source](#build-from-source)
- [Checks and releases](#checks-and-releases)
- [Test databases](#test-databases)
- [Keyboard shortcuts](#keyboard-shortcuts)

## Supported servers

PostgreSQL (tested on 17), MySQL from 5.1 (tested on 5.1.73, 5.5.62, 5.6.51, 5.7.44 and 8.4) and MariaDB
(tested on 11). There is one MySQL engine to pick, whatever the release: Kerno reads the server's version when
it connects ("Connected to MySQL 5.1.73") and uses what that release has (D-094). Where an old release lacks
something Kerno offers, Kerno says so in words that name the release it needs, instead of a syntax error:

| What | MySQL 5.1 | 5.5 | 5.6 | 5.7 | What Kerno does without it |
| --- | --- | --- | --- | --- | --- |
| `utf8mb4` (5.5.3) | no | yes | yes | yes | The connection speaks `utf8`. A character a column's character set cannot hold (an emoji in a `utf8` column, on any release) is refused by an edit, see the next row. A backup of a newer server starts with `SET NAMES utf8mb4` and stops at its first statement on 5.1 (ERROR 1115): the restore says so before it runs |
| Metadata locks (5.5.3) | no | yes | yes | yes | The faster backup cuts a large table into parts read at once only with “Block writes”: `LOCK TABLES … READ` does not hold back a transaction that already wrote |
| Strict mode on by default (5.7) | no | no | no | yes | Outside strict mode the server stores a value too long for its column, out of range, or with a character it cannot hold, cut or changed, with only a warning. Kerno stops the apply at that statement and changes nothing: "the server would store another value than the one written: Data truncated for column 'c' at row 1" |
| TLS that Kerno accepts (TLS 1.2 with an ECDHE key exchange) | no (TLS 1.0) | no (TLS 1.0) | yes (OpenSSL builds) | build-dependent (5.7.44's official build offers DHE only: no) | SSL mode "prefer" connects without TLS; "require" and "verify-*" refuse with "This server only offers an old TLS that Kerno does not accept." and the next step (prefer or disable on a trusted network, an SSH tunnel, or a server update) |
| Passwords with the pre-4.1 hash (`old_passwords`) | can sign in | can sign in | refused by the server (5.6.5) | gone | Refused: "The account … has an old password hash that Kerno does not accept." The administrator sets the password again with `old_passwords = 0` |
| Read-only sessions (5.6.5) | no | no | yes | yes | A read-only connection still opens and Kerno still refuses the statements it knows to write; a notification says the server would accept them, and suggests an account that may only read |
| Routine arguments, `information_schema.PARAMETERS` (5.5.3) | no | yes | yes | yes | Functions and procedures are listed without their arguments |
| Query plans, `EXPLAIN FORMAT=JSON` (5.6.5) | no | no | yes | yes | Explain says "A query plan (EXPLAIN FORMAT=JSON) needs MySQL 5.6.5 or later; this server is MySQL 5.5.62." and runs nothing; `EXPLAIN …` typed in the editor shows the server's own table |
| `EXPLAIN ANALYZE` (8.0.18) | no | no | no | no | "EXPLAIN ANALYZE needs MySQL 8.0.18 or later; this server is …" |
| Lock waits (`INNODB_LOCK_WAITS`, 5.5) | no | yes | yes | yes | The list of locks says "not supported by this database: the locks of this server" |
| Accounts: authentication plugin (5.5.7), password expiry (5.6.6), locked accounts (5.7.6), roles (8.0) | none | plugin | plugin, expiry | plugin, expiry, locked | The columns the release lacks are empty; no roles before 8.0 |
| Settings with their source (8.0.2) | no | no | no | no | Name, session and global values (from `information_schema` before 5.7.6) |
| Rename an index (5.7.1) | no | no | no | yes | Refused in words: drop it and create it again under the new name |
| Rename a column alone, `RENAME COLUMN` (8.0.3) | no | no | no | no | Refused in words; a rename together with another change of the column (type, default…) is one `CHANGE COLUMN` and works |
| Generated columns (5.7.6), `JSON` (5.7.8), check constraints (8.0.16) | no | no | no | yes, yes, no | Not listed; imported JSON goes into `longtext` |
| Views over views in what a drop breaks (`VIEW_TABLE_USAGE`, 8.0.13) | no | no | no | no | The views are found by a search of the view definitions for the name, as on MariaDB |
| Transactions on MyISAM tables (the default engine before 5.5.5) | — | — | — | — | Edits are applied in a transaction as everywhere; a MyISAM table has none, so when one statement fails the ones before it stay: the failure says so |
| Backups with a `mysqldump` 8.4 or later | no | yes | yes | yes | `mysqldump` 8.4 and later read no column of a 5.1 table (and MariaDB's `mariadb-dump` 11.8 none of 5.1 to 5.6): the backup says so before it runs and does not start; choose a MySQL 5.7 client, 5.7.11 or later, with “Local client…” (it names one found). With SSL mode “prefer”, the tools go without TLS on 5.1 and 5.5, as the connection does |

MariaDB releases before 11 share some of these gaps (database comments 10.5, `RENAME COLUMN` 10.5.2, check
constraints with their table 10.3.10, JSON columns 10.4.3, `information_schema.SYSTEM_VARIABLES` 10.1.1): Kerno
uses the same facts for them, but only MariaDB 11 is tested. MySQL 5.0 and older are not supported.

## SQLite

A SQLite database is a file, and Kerno opens it itself: there is no server, host, port, user or password. In
the new-connection dialog, pick SQLite, then either **Choose…** an existing file or **New file…** to make an
empty database (a path that is there already is opened as it is, never emptied). Test connection opens the file
and names SQLite's version. A file that is missing, is not a SQLite database, is locked by another program, or
that you may only read is said in plain words first, SQLite's own message below; tick Read-only to open a file
you may not write, or to make sure nothing writes to it.

Under the connection the tree shows `main` (the file), then its tables, views and triggers, each table's columns
and indexes. Queries, the grid, table data editing, the structure tabs, the table form, exports and CSV import
work as on the other engines, with these differences:

- SQLite types each value, not each column: the grid shows each cell as it is stored (a number as a number, a
  blob in hex), the header the column's declared type. A DATE, BOOLEAN or JSON column gets its rendering only
  when its values are stored that way; a date kept as a Unix time shows as a number.
- The table form renames the table, renames, adds and drops columns, and creates and drops indexes. Changing a
  column's type, NULL or default, the keys or a column's place needs the table rebuilt, which Kerno does not do
  yet: it says so instead of trying.
- Explain shows SQLite's query plan; there is no EXPLAIN ANALYZE.
- While a result is still being read in one tab, the file is held for reading: a write from another tab waits
  five seconds, then fails with "database is locked". Read the result to its end or close it first.
- Not there yet: backup and restore, the Tools ▸ menu (ANALYZE, VACUUM, integrity check), the administration
  tab, copying tables to or from another connection, opening a file from the shell (`kerno shop.db`),
  attached databases, and editing the rows of a query result (a table's data tab edits them).

## SSH

A connection reaches its database through an SSH server with one of two clients, chosen on the SSH tab.
**OpenSSH**, the default when `ssh` is installed, runs your own `ssh`, so everything in `~/.ssh/config` applies:
`Host` aliases (the host field lists them), `ProxyJump` and the Jump host field (`ssh -J`, several hosts
separated by commas), IdentityAgent (1Password, Secretive), RSA and every other key type, and OpenSSH's
known_hosts; port, user and key may be left empty for the configuration to give them. It forwards a private
Unix socket, never a TCP port. A key's passphrase is asked for and kept in the secret store, never on the
command line; a server key you choose to trust is added to your known_hosts. `KERNO_SSH_CONFIG=<file>` makes
Kerno pass another configuration file to `ssh` (`-F`). The **built-in** client needs no `ssh` and is the one for
a password login; it takes Ed25519 and ECDSA key files and the keys of an SSH agent, not RSA key files.

On Windows, OpenSSH is the `ssh.exe` Windows ships, `C:\Windows\System32\OpenSSH\ssh.exe` (the optional
feature "OpenSSH Client"), or the one Win32-OpenSSH's installer puts in `C:\Program Files\OpenSSH`; never one
found on `PATH` (the SSH tab says which runs), and your file is `%USERPROFILE%\.ssh\config`. What differs:
Windows' OpenSSH forwards no private socket, so each connection to the database is its own `ssh.exe` (its
standard input and output are the connection), and each session, and each cancelled query, logs in to the SSH
server again. PostgreSQL connections listen nowhere on your computer; MySQL and MariaDB connections and the
client tools (`pg_dump.exe`, `mysqldump.exe`, …) dial an address, so Kerno opens a one-use loopback port for
each, which another account of this computer could take in the moment before the connection does (the
database's own login is still needed). A key's passphrase and a host key you trust are answered by Kerno over
a named pipe whose two ends prove they can read a token in the tunnel's private folder (in
`%LOCALAPPDATA%\Kerno\data\ssh`, removed when the connection closes), and only while that `ssh.exe` logs in;
the passphrase is kept in memory while the connection is open. A wrong passphrase is said as wrong. An
`ssh.exe` older than 8.4 cannot ask Kerno anything: load the key in the agent (`ssh-add`), connect once with
`ssh` in a terminal to record the server's key, or use the built-in client. Windows' OpenSSH cannot start a
program whose path has a letter beyond A to Z (`C:\Users\Élodie\…`): Kerno names itself to it by a path
relative to the tunnel's folder, which works for an install in `%LOCALAPPDATA%\Programs\Kerno`; installed in
a folder whose own name has such a letter, the failure says so first, with the same three ways out. Every
`ssh.exe` of a connection, its jump hosts' included, stops when the connection closes or Kerno quits.
ControlMaster and Pageant are not used. Kerno opens no file of another computer named in a connection or a saved task (a key, a certificate, a
SQLite database, a backup's folder at `\\server\share\…`): Windows would log in there with your account. Use a
path such as `C:\Users\you\…` or `~\…`; for a database on a share, map the share to a drive letter first. When
you import connections, each saved task shows the files it would read or write before you save it.

## Where passwords are kept

**Settings ▸ Passwords ▸ Keep saved passwords in** chooses where the passwords, SSH passwords, key passphrases
and AI keys you save are kept:

- **Kerno's file** (the default on macOS): `secrets.kerno` in Kerno's data folder, encrypted (AES-256-GCM)
  with a random key kept in `secrets.key` beside it, both readable by your account only. Nothing asks for your
  password. It keeps them out of plain sight: a copy or a backup of the file, a file search, a shared screen
  or another user of the computer shows nothing. It does not keep them from a program running under your
  account, which can read both files: the level of DBeaver's saved passwords, `~/.pgpass` or an SSH key
  without a passphrase.
- **macOS Keychain**, **Secret Service** (the default on Linux: GNOME Keyring, KWallet) or **Windows Credential
  Manager** (the default on Windows): the system's store, guarded by your login. On macOS, Kerno's builds are not signed with an Apple Developer ID, so the Keychain asks
  for your login password again, once per saved password, after every update. On Linux without a Secret
  Service running, passwords are kept until Kerno quits (the status bar says so).

The change applies from the next connect, without a restart. A password saved in the other place moves the
first time it is used: with Kerno's file, a password Kerno saved in the keychain before is read there once
(macOS may ask one last time), then only from the file; the keychain is never written or emptied from then on,
so its old copy stays there until you delete it in Keychain Access (items named "Kerno"). Back on the keychain,
what the file holds moves to the keychain and leaves the file. A connection set to **Ask each time** saves
nothing and reads neither. If the file cannot be read (damaged, or its key file missing or replaced), Kerno
says so when it needs a password, sets it aside as `secrets.kerno.damaged` (with its key) and asks for the
password again; nothing is overwritten. The new file then looks in the keychain once more, so a password the
keychain still holds from before comes back from there, even an older one. `KERNO_SECRET_STORE=memory`, `file` or `keychain` uses that store alone
for a run, whatever the setting says, and moves nothing.

## Passwords from a command, AWS RDS IAM, 1Password, .pgpass and .my.cnf

The connection dialog's **Password from** list says where the database password comes from. The first choice,
named after where saved passwords are kept (**Kerno's file** or **macOS Keychain**, see above), saves it
there; left empty, Kerno reads `~/.pgpass` (or `PGPASSFILE`) for
PostgreSQL and the `password` of the `[client]` group of `~/.my.cnf` for MySQL and MariaDB, as their own tools
do (`!include` lines of `.my.cnf` are not followed). A `.pgpass` that other users can read, or a `.my.cnf`
anyone can change, is ignored, and the error says so. On Windows the file is libpq's
`%APPDATA%\postgresql\pgpass.conf` (or `PGPASSFILE`), and no MySQL option file is read: MySQL has no per-user
one there but the encrypted `.mylogin.cnf`. **Ask each time** asks at every connect. **Command**
runs a program of yours and uses what it prints (one trailing newline removed): no shell, one argument per
line, `${host}`, `${port}`, `${user}` and `${database}` replaced inside the arguments, at most 120 s.
**AWS RDS IAM** runs `aws rds generate-db-auth-token` with the connection's host, port and user, your region
and optional profile, and needs TLS (the SSL mode moves to `require`). **1Password** runs `op read
op://vault/item/field`. A command's output is used for that connect only, never stored or logged, and runs
again for new sessions after ten minutes (an RDS token lasts fifteen); "Run now" checks it and shows only the
length of what it printed. Programs are found on your `PATH` and in `/opt/homebrew/bin`, `/usr/local/bin`,
`/usr/bin` and `/bin`. A test or a connection says where its password came from ("Password from ~/.pgpass").
An exported connections file carries the command but no password; on import a command does not run until you
have read it in the connection's settings and clicked "I trust this command".

## Open a connection from the shell

`kerno postgres://ada@db.example.com:5432/shop` opens that connection (`postgresql://`, `mysql://` and
`mariadb://` too; `sslmode` and other parameters as in the connection dialog). A saved connection to the same
engine, host, port, user and database is used when there is one; otherwise the connection opens **unsaved**:
marked in the sidebar, kept for this run only, its password in memory, never saved, until "Save
connection…" in its menu stores it. When Kerno is already running, the URL opens in its window, which comes to
the front. `kerno --version` and `kerno --help` print and exit.

Leave the password out of the URL: other programs running as your user can read a command line (`ps`). Without
one, Kerno asks for it when the server wants it.

In the connection dialog, **pasting** a URL in "Fill from a connection URL" fills the fields below at once (a URL
of another engine switches the engine); a URL typed there fills on Fill or Enter, and Fill with the field empty
takes the URL in the clipboard. The URL's fields replace the
form's, a URL without a port meaning the engine's standard port; the fields it does not name (the name, the
folder, a user or database it leaves out) keep what you typed. The field's example follows the chosen engine.

## Move your setup to another computer

**Export Connections…** (File menu, the sidebar's menu, the palette) writes a JSON file with what you tick:
the connections (with their ids), the folders' colours and icons, your environments and the built-ins you
edited, and the saved tasks. The environments your connections use always go with them. The file holds no
password, passphrase or key, no local client folder and no folder picture. **Import Connections…** first lists
what the file adds, replaces (same id; same name for a task) or leaves as it is, then asks for each connection
written where its password comes from: **Enter now** (kept in the secret store), **Ask when connecting**, or,
when the secret store already holds secrets under that connection's id, **Use the saved secrets**
(the database password and the SSH password or key passphrase, all or none), which names where each will be
sent and never shows them. A saved password is linked automatically only to a connection this copy of Kerno
already has, pointing at the same server (SSH tunnel included); otherwise you choose, and a connection left
unlinked cannot read any of the old secrets. Files written by earlier versions still import.

**Import from DBeaver…** (File menu, the sidebar's menu, the palette) takes a DBeaver project export (in DBeaver,
File ▸ Export ▸ Project: a `.dbp` file) or a `data-sources.json`. It lists the folders and their connections,
ticked, or greyed with the reason; untick what you do not want, then Import. The scripts become saved queries of
their connection. **Also import the saved passwords** (off by default) puts them in this computer's secret store.
Importing the same file again adds nothing twice: a saved query already here (same connection, folder and name)
is skipped, and the preview says how many. With no connection ticked, the scripts and folders not here yet are
still imported, the button saying so ("Import 1 saved query"); when nothing at all would be written, the button
is off and the dialog says why (so does a connections file whose items are all here already). An SSH tunnel's
jump servers come as its jump hosts (through the system's OpenSSH). Under each connection the preview says what
was left out and what to check ("To check: certificate pasted in DBeaver, not in a file: save it to a file and
choose it on the SSL tab"), and the report after the import lists them again, connection by connection. A
PostgreSQL connection shows the databases DBeaver showed (see the next section).

## Which databases a PostgreSQL connection shows

In the schema tree, a database the account may not open (it has no CONNECT privilege there, or the database
takes no connection, as `template0`) is dimmed. Opening it shows one line, "This account may not connect here."
or "This database takes no connection.": Kerno knows it from the server's catalog and tries no connection. Its
menu keeps what runs from the connection's own database (Create database…, Drop…, Copy name, Filter ▸); its
Refresh lists the databases again, so a privilege granted since shows. If a session is refused anyway (a rule
of the server's `pg_hba.conf`, a privilege revoked meanwhile), the row says "You may not connect to database
“x”." first, then the server's words.

On a server with many databases, choose what the tree shows in the connection's settings: **Databases shown**,
beside the Database field, PostgreSQL only.

- **All** (the default): every database, templates included.
- **Those this account can open**: the databases with the CONNECT privilege, no template.
- **Only the default database**: the one in the Database field, or where the server puts the session when it is
  empty.

The database the connection opens on is always shown. The tree follows as soon as the settings are saved; a
line after the databases says how many are hidden ("12 databases hidden"), and a click on it (or Enter) opens
the settings. The main toolbar's database picker, the status bar's and a query tab's offer the databases the
tree shows, never one the account may not open; the Database list of the settings offers every database the
account can open. A global backup (pg_dumpall) leaves out the databases the account may not open, and says
so under its list of databases and as the first line of its log ("Left out, this account may not open them:
billing, crm"); a saved task reads them again at each run. MySQL and MariaDB list only what the account may
see, and have no such choice. To hide databases by name, use Filter ▸ on a database of the tree. An import
from DBeaver keeps DBeaver's "Show all databases" (off in DBeaver: the default database alone), and an
exported connections file carries the choice.

## When a connection opens again

Reconnect, the Read-only switch (its toolbar button, the connection's menu, the palette), saving an open
connection's settings and a connection that comes back after the server went away all close the connection's
sessions and open new ones. The schema tree is then read again on the new sessions and put back where you were:
while the connection opens its line says "Connecting…" (or "Connection lost: reconnecting…"), then "Loading
databases…"; then the rows that were open come back open, each with its own "Loading…" line until it is read, the
row you had selected is selected again and the tree scrolls back as far as its rows allow. A row that no longer
exists on the server is simply not there; a click on a row meanwhile keeps your choice. A table open in a tab
reads its rows again, and a query tab opens a new session at its next statement. An open transaction is never
carried over: the first three ask about it before, and a lost connection says that the server rolled it back.
After Disconnect then Connect, the tree starts afresh.

A refresh keeps your place the same way, as DBeaver does: Refresh on a connection, a folder of connections or
the empty area of the sidebar, Refresh the schema tree (Cmd+Alt+Y), Refresh (F5) on a database, a schema, a
folder or a table, "Try again" under a connection whose databases could not be listed, and the refresh Kerno
does itself after a change (a table created or altered, an import, a restore or a script that ended well). What
it reads again comes back with the rows that were open under it open again (each read again), the selected row
selected again if it still exists, and the scroll kept; the rest of the tree is not touched. Refresh on a
connection selects that connection's row.

A read of the tree whose session the server closed (an administrator, a timeout) opens a new session and reads
again once, without a word. If that fails too, its row says so in plain words first ("The connection to
“name” was lost. Reconnect it (its menu in the sidebar), then try again."), the driver's words below.

## AI assistant and what it sends

**What it is for.** Turned on, the assistant does three things, only when you ask: it writes SQL from a
sentence (inserted in the editor, never run by itself), explains a query or its plan, and explains an error
with a proposed fix. You find it in the editor's right-click menu (**AI: Write SQL…**, **Explain query**,
**Explain plan**, **Explain error**), the command palette and Query ▸ AI Assistant, and the **Explain with
AI…** and **Explain error with AI…** buttons under a plan or an error. It sends the endpoint you choose the
names of your schemas, tables and columns with their types; rows only when you tick **Include sample rows**
for that request.

The assistant is off until **Settings → AI ▸ Enable the AI assistant** is ticked. While it is off, no
request is ever made, not even Detect or Test: its entries say "turn on in Settings → AI" and do nothing.
Import and Add by hand work while it is off: they read only files and what you type.

### The conversation panel

The assistant answers in a panel at the right of the window (**AI: Show or Hide the Assistant**, ⇧⌘J,
Ctrl+Shift+J on Linux; also Query ▸ AI Assistant, the palette and the **AI** entry of the status bar). Its
width is yours: drag its edge, between 320 and 720 points; it is remembered. Each query tab has its own
conversation, kept while the tab is open and never written to disk; switching tabs switches the
conversation, and **New conversation** clears it.

- Type a question or describe a query, then **Enter** (Shift+Enter makes a new line; Escape goes back to
  the editor). Follow-ups ("add the date filter", "no, by customer") go with the earlier messages, so the
  model refines its own answer. A long conversation sends its latest messages only (20 at most, 40,000
  characters): the thread marks where "Earlier messages are no longer sent".
- **The editor's query** (the selection, else the statement at the cursor) goes with a message as a chip;
  click its × to leave it out of that message.
- **AI: Write SQL…**, **Explain query**, **Explain plan**, **Explain error** and the **Explain … with AI**
  buttons open the panel with that request ready: you send it.
- Answers appear as they are written when the server streams them; **Stop** closes the request and drops
  what came. Under each SQL block: **Insert** (in place of the selection, or on its own line at the cursor;
  a statement before it without its `;` gets one, so that each runs on its own), **Replace** (the
  selection, else the statement at the cursor) and **Copy**. Nothing runs until you run it.
- The panel is never wider than the window allows: with a small window or a wide sidebar it narrows, and
  takes your width again when there is room.

### From a right-click

**AI ▸** in the menu of a table, a view or a materialized view of the schema tree, and the **AI** button of a
table's data tab (in its footer) and structure tab (in its bar), ask about that table: **Ask about this
table…** (you type the question), **Explain this table** (what it holds and how it relates to the tables it
references and that reference it), **Write a query on this table…** (as Write SQL, the table named) and
**Generate test data…** (`INSERT` statements for the number of rows you set, 10 by default, 100 at most,
that respect the types, NOT NULL, the keys and the foreign keys). On a column of the tree, the column selected
in a structure tab's Columns, or a result grid's column headers (several when selected), **Explain these
columns**. In a result grid's menu, **Explain this result** (its statement, column names and types, row
count) and **Ask about the selection…**. Each is also an "AI: …" command of the palette and of Query ▸ AI
Assistant, on the active tab's table or result, else on the table selected in the tree.

Each opens the panel with what it asks shown as a chip (its × removes it) and the keyboard in the message
field: nothing goes until you press Enter, and **What is sent…** shows the whole message. A question about a
table goes to a query tab titled for it ("Ask · customers") on the table's connection and schema; the next
one on the same connection goes to the same tab while you have not typed in its editor, and the conversation
goes on with the new table as its subject. A question about a query's result goes to that query tab's
conversation. The table and the tables linked to it by foreign keys are described first, with
their keys and indexes. A result's values never go unless you tick **Include its first 20 rows** or, for a
selection, **Include the selected values**, which says how many cells go; the tick is off at each message.
Generated test data is offered with **Open in a new tab** under its SQL: a query tab on the same connection,
where nothing runs until you run it. On a read-only connection or in production, the entry says that it only
writes text. While the assistant is off, every entry says "turn on in Settings → AI".

### Endpoints

Settings → AI keeps several named endpoints, any OpenAI-compatible server: one is active, and requests go to
it alone. **Use** on a row (or the "endpoint · model" picker at the top of the AI panel) switches, in one
click; each endpoint keeps its own model and its own key. Three ways to add one:

- **Detect on this computer** asks `127.0.0.1` on the usual ports, only when you click: Ollama (11434), LM
  Studio (1234), llama.cpp (8080), vLLM (8000), LiteLLM (4000) and Jan (1337). It lists what answered with its
  models ("Ollama · 4 models"); **Add** saves it. Nothing leaves the computer.
- **Import from omp, pi or the environment** reads `~/.omp/agent/models.yml` (or `models.yaml`),
  `~/.pi/agent/models.json` and the variables `OPENAI_BASE_URL` / `OPENAI_API_KEY`, and lists the
  OpenAI-compatible providers it found (`api: openai-completions`) with their address, models and whether they
  have a key; the others are listed as skipped with the reason. **Import ticked** saves the ones you tick: the
  address, the models with the names the file gives them ("Qwen · gpu1 · low"), and the key where saved
  passwords are kept (never in the settings or the logs). A key written as an environment variable's name is read from
  that variable when you import; if it is not set, or the key comes from a command (`!…`, which Kerno never
  runs), the endpoint is imported without a key and the page says so: type it in **API key of …**. Kerno
  started from the Dock or the Finder sees the variables macOS gives it, not those of your shell.
- **Add by hand**: a name, an address and a key. The address may be written with or without `/v1` and with a
  trailing slash (`llm.example.lan`, `https://llm.example.lan/v1/`); the form says what Kerno will call
  (`https://llm.example.lan/v1/models` and `…/chat/completions`).

Plain `http://` is accepted only for this computer (`localhost`, `127.0.0.1`, `::1`); any other host needs
`https://`, and Kerno names where it is ("Requests will leave this computer and go to llm.example.lan, on your
network…", "…on the internet" for a public host) and sends nothing there until you click **Allow requests
to …** once for that endpoint. Requests never go through a proxy and never follow a redirect. Kerno trusts the
certificates the system trusts (the macOS keychains, the Linux CA bundle), so a company's internal authority
that the computer trusts is trusted here too, and never less verification than that.

**Test** on a row asks `GET <address>/models` once and says how long it took and how many models it lists,
or why not, in plain words first and the server's own message under it:

- the host was not found ("Is this computer on the network that serves it, or on its VPN?");
- the connection was refused on that port, or no answer came in time;
- the server presented a certificate the computer does not trust: the issuer it names is shown as unverified, and Kerno says to get the authority's certificate from the administrator, not from the server, or to send nothing if this was not expected;
- the key was refused (401, 403): check the key saved for it;
- nothing at that address (404): check the address; if only the list of models is missing, type the model.

### The model

Under the endpoints, the active endpoint's model: type its id, or pick it from the list (filtered by what you
type, by name or id). **List models** fills the list from the endpoint; an import fills it from the file.

### What a request holds

- your message: your words, with the instruction of the action that opened it and its subject (the
  statement, the plan on screen, or the statement and the server's error message, which may quote a value:
  the preview shows it before you send), and the editor's query unless you removed its chip;
- the earlier messages of the conversation and the model's answers to them, within the limit above (an
  answer to a message sent with sample rows may quote them);
- the engine, its version and the current database; the names of the schemas; the tables the statement or
  the sentence names, in the order it names them (else the tables of the tab's schema): their names, column
  names and types and foreign keys, 200 tables at most with their columns, then names only;
- for the first 30 of those tables, read when the request opens ("reading the tables' keys and indexes…"
  meanwhile): which columns refuse NULL (`not null`), the primary key, the foreign keys and the indexes,
  one line each under the table's name (100 columns, 12 keys and 12 indexes a table, the rest counted). Never
  a default, a comment, a check or a count of rows;
- rows only when you tick **Include sample rows (first 5 rows of the tables above)** for that message (up to
  10 tables); the tick is off again at every new message.

**What is sent…** in the panel shows the exact bytes before you send them; Settings → AI ▸ **What was sent**
lists every request of the session (time, endpoint, body), in memory only. The key goes as `Authorization:
Bearer …` and nowhere else (previews and the log show `Bearer ***`). The answer is shown as text: nothing in it
runs or opens anything. Models that think aloud (Qwen, DeepSeek, GLM variants) have their reasoning left out,
whether it comes in its own field or between `<think>` and `</think>`: the panel, Copy and Insert get the
answer only. **Insert** and **Replace** put a block's SQL in the editor (selected, one undo step) and you run
it yourself, through the usual confirmations, read-only and production rules. An answer may take up to 5 minutes (a reasoning model on a server of your network thinks
before it answers); **Stop** closes the request at once.

### Ask in the editor, fix an error in one click

**AI: Ask Here…** (⌘⇧H, Ctrl+Shift+H on Linux; also in the editor's right-click menu, the palette and Query ▸
AI Assistant) opens a small field under the selection, else under the statement at the cursor. Type what you
want ("only the orders of this year") and press Enter: the request holds the schema context above, that SQL
as "the SQL to change" and your sentence; never rows. On an empty line nothing of the editor is sent: the
assistant writes from your sentence alone, and its SQL goes on that line.

The answer comes back as a diff: the lines it would remove are tinted in your text, and a card under them
shows the proposal line by line, `−` before a removed line, `+` before an added one. Nothing of your text
changes until **Accept** (⌘↩ in the field, Ctrl+Enter on Linux), which puts the proposal in place as one undo
step and selects it; nothing runs. **Reject** (Esc) leaves the text, the cursor, the selection and the undo
history as they were. While the card is shown, ⌘↩ in the editor itself runs the editor's own text, never the
proposal. Type a further sentence in the same field ("no, by month") and Enter: it goes with the exchange so
far and the diff is replaced. **Open in the panel** carries the exchange on in the tab's conversation. An
answer without SQL is shown as its text, with nothing to accept.

**Fix with AI**, under a failed statement's error and beside the error's mark in the editor (or **AI: Fix
Error** in the palette), sends that statement and what the error view shows (message, code, detail, hint, the
character the server pointed at) at once; the corrected statement comes back as the same diff on that
statement. It never runs it. **Explain error with AI…** is still there, in the panel.

While a request waits, the place it is about is framed and the card says "Asking …" with **Stop** (or ⌘.).
An edit that changes or moves that place, or leaving the tab, closes the request and its answer is never
shown; the card says so. **What is sent…** in the card shows the exact body; every request joins **What was
sent** in Settings → AI.

## Vim mode

Off by default. Turn it on with **Settings → Editor ▸ Vim mode** or the command **Toggle Vim Mode** (palette,
Edit menu). While it is off nothing of it runs: the editor behaves exactly as without it.

On, the editor of every query tab (scripts, and the sources of views and routines opened there) has vim's
Normal, Insert, Visual (`v`) and Visual line (`V`) modes. The status bar shows the mode; in Normal mode the
character under the cursor is shaded as a block (the editor's thin caret stays). What is supported, and
nothing more:

- **Motions**, with a count: `h j k l`, `w b e`, `0 ^ $`, `gg` and `G` (with a count, that line), `f{char}`
  and `t{char}` on the line, `%` (the matching bracket; brackets in strings and comments are text), Enter,
  Backspace and Space.
- **Operators**: `d`, `c` and `y` with a motion, with `iw` or `aw`, or on a Visual selection; `dd`, `cc`, `yy`;
  `x`; `p` and `P`; `u` and `Ctrl+R` (undo, redo); `.` repeats the last change, typed text included.
- **Insert**: `i a I A o O` (`o` and `O` keep the line's indentation), `Esc` back to Normal mode.
- **Search**: `/text` then Enter, `n` and `N` (plain text, case-sensitive, wraps around).
- **Commands**: `:w` saves the file, `:q` closes the tab, `:wq` (or `:x`) saves then closes, `:q!` closes
  without keeping unsaved text. `:` and `/` open a one-line field under the editor; Escape closes it.
- **One register, the system clipboard**: `y`, `d` and `c` write it, `p` reads it, so they work with Cmd+C
  and Cmd+V. Lines yanked by vim paste as lines while the clipboard still holds them; text copied elsewhere
  pastes as characters.

In every mode the arrows, Home/End, Page Up/Down, the mouse and every Cmd shortcut (Ctrl on Linux) keep
working: run (Cmd+Enter), save, the palette, find, zoom, completion. In Insert mode every key but Escape is
the editor's, so Tab and Enter still accept a completion. `:q` asks before closing what would be lost, as
Cmd+W does; `:q!` lets go of unsaved text, but an open transaction or pending changes still ask. On Linux,
Ctrl+R is Refresh Rows outside Normal mode.

Not supported: counts on `i a o`, `F T ; ,`, text objects other than `iw` and `aw`, named registers, marks,
macros, `r ~ J > <`, Visual block (`Ctrl+V`) and other `:` commands. The editor records each edit on its own,
so `ciw` followed by typed text is undone in two steps (the deletion, then the typing).

## Updates

Kerno asks GitHub on its own: at launch, then whenever 6 hours have passed by the clock (a computer that slept
checks within two minutes of waking, so a Kerno left open learns of a new version without a restart), it sends a
`HEAD` request to `https://github.com/EvanPluchart/kerno/releases/latest` through the system's `curl`
(`/usr/bin/curl`; on Windows the `curl.exe` of `System32`, never one found on `PATH`), with
the redirect not followed, a `Kerno/<version>` user agent, no cookie, no identifier and nothing else, and
reads the version in the redirect. It offers only a strictly newer `X.Y.Z` and says nothing when offline. When
a newer version is found (and checks are on), it also reads that release's notes file from GitHub to show what
it brings in the update dialog; nothing about you is sent. Behind a proxy, `curl` uses the `HTTPS_PROXY`
variable when Kerno was started from a shell that sets it; started from the Dock or a desktop launcher it has
no proxy, and a network that allows nothing else simply finds no update (database drivers are built in:
nothing is downloaded to connect). Turn it off in Settings ▸ Updates, or for a run with
`KERNO_NO_UPDATE_CHECK=1`; Help ▸ Check for Updates… asks only when you click it. An update is installed only
after its SHA-256 matches the release's `SHA256SUMS`; the [architecture
notes](ARCHITECTURE.md#the-update-check) give the details.

When a check at launch (or 6 hours later) finds a new version, a window says so once per version and run (a
newer one found later in the same run is offered too): the new version, the one you have, and what Update does
on your copy (an installed `Kerno.app` or archive replaces itself and starts again; on Windows the installer
runs for your account; any other copy opens the release page). **What's new** opens the release page in your
browser. **Update** installs as the status bar item does; **Later** closes it, and the next launch asks again;
**Skip this version** stops the window for that version (a newer one asks again). No key acts in it while you
type: Enter and Space do nothing, Escape is Later, and Tab goes to Later first and to Update last; it waits
while another dialog is open. The status bar item stays in every case, a skipped version included; Settings ▸
Updates shows the skipped version with **Ask again**.

## Settings, environments and what wins

Settings (Cmd+,) opens with the keyboard in its search field: type a word ("dark", "font size", "autocommit",
"confirm", "vim", "pg_dump") and only the settings whose label, description or usual synonyms start with it
stay, from every section, under their section's name; each works where it is found. Cmd+F gives the search the
keyboard again, Escape empties it (and closes Settings when it is empty); a word that matches nothing says so
and offers to clear the search. Environments, Local clients and AI are views of their own: the search lists
them with an **Open** button. The command **Find a Setting…** in the palette opens Settings the same way.

A muted line under a setting says what can override it, in the same shape everywhere ("… wins: …"), worked out
from your environments as they stand:

- **Confirmations ▸ Ask before running a statement … without a WHERE** covers statements run from a query tab
  (a statement, a selection, a script, EXPLAIN ANALYZE, a run on several connections). A drop or a truncate
  from the tree, and the changes applied from a grid or a table form, always show their SQL first. An environment whose rules confirm every statement or ask twice
  before writes asks whatever this says; with the setting off, these statements run without asking in the others
  (with the built-ins: Production still asks, Local and Staging run the statement).
- **Ask before closing a tab …**: an open transaction always asks.
- **Grid ▸ Show the results of a script one under the other**: a result's "Open in its own tab" wins for its
  query tab until the next run.
- **Updates ▸ Check for updates**: `KERNO_NO_UPDATE_CHECK` wins.
- **Local clients**: a connection's own client (its Advanced tab, or "Local client…" in a wizard) wins over the
  default chosen there.
- **Environments**: a connection's own "New query tabs start in" (Advanced tab) wins over the auto-commit rule
  unless it says "From the environment"; the read-only rule only sets a new connection's Read-only switch, and
  the switch wins; "Confirm every statement" and "Ask a second time" win over Settings ▸ Confirmations.

The connection dialog says under the Environment field what the chosen environment does ("Production: asks
before every statement run from an editor, twice before any write, starts query tabs in manual commit, rolls
back a transaction left idle for 10 min."), and **Edit environments…** opens them.

Every entry of a context menu and every toolbar button is also a command of the palette (Cmd+K), except the
ones that only make sense where they are: one entry per connection, database, schema, folder or foreign key,
the value or column under the pointer, the text field's own Cut, Copy and Paste.

## Where a running script stands

While statements run (Run script, a selection of several, Run statement), the margin left of the line numbers
marks each statement the run has reached, the moment it starts and ends: a play sign in a halo that breathes for
the one the server is working on, a tick for one that went well, a cross for one that failed, a square for one
you cancelled (⌘.), a barred circle for a write refused on a read-only connection. A statement not reached yet
has no mark, so after a failure or a cancel the last mark shows where the run stopped. The status bar says
"running statement 3 of 12 · 4.20 s", the time of the statement now running, and once the run ended what the
marks say ("statement 3 of 12 failed", "12 of 12 statements done"). The marks stay after the run, until
you run again from the editor or change the text (a change during the run ends them too; the status bar keeps
counting). Nothing moves your cursor, your selection or the view while the script runs.

## Rows as you scroll

A query result arrives 1,000 rows at a time (Settings ▸ Grid ▸ "Rows fetched at a time"). Its footer says "1,000
rows fetched, more on the server" while the server holds more. Scrolling to the last rows, with the mouse or
the keyboard (Page Down, the arrows), fetches the next page by itself, once each time the end is reached: the
footer says "fetching more…" meanwhile, and Stop (⌘.) cancels it. "Fetch more" (Ctrl+Alt+N) and "Fetch all"
(Ctrl++) stay in the footer. Settings ▸ Grid ▸ "Fetch the next rows when scrolling reaches the end of a
result" turns the automatic fetch off, for a slow link. It also waits while the find shows matching rows only
and while the rows are sorted in Kerno (a result the server cannot read again in another order), where new rows
would land out of place.

A table or a view opened from the tree is read the same way, ordered and filtered by the server: its next rows
are read and added under the others when the scroll reaches the last ones, and its footer says "Rows 1–3,000 of
12,345" ("of about" when the count is the server's estimate). At the bottom right, "Fetch 1,000 more"
(Ctrl+Alt+N), outlined while the table has more, and "Fetch all" (Ctrl++), which reads load after load with the
count growing until Cancel stops it. What is on screen stays as each load comes: the selection, the scroll, the
record view and the changes waiting to be applied. "Fetch size: 1,000" changes how many rows each load reads in
this tab, from a list or typed ("Other size…", 100 to 10,000); Settings keeps the size of the next table opened.
Refresh reads again as many rows as are loaded and keeps the selection; a new order or filter starts again from
one load. A load that fails keeps everything and says why; "Try again" reads it again. While a row is being
added, the scroll waits; "Fetch more" puts the new rows above the added one. A table without a key, or a view,
is read again from its first row at each load (the server keeps no order between two reads of it), so no row
shows twice and none is missed.

A sort, a filter, a refresh or an Apply goes back to the first row and leaves the columns scrolled where they
were. The WHERE field proposes the table's columns as you type, with their type, then what a condition is made
of (AND, OR, IN, IS NULL, LIKE, ILIKE on PostgreSQL, BETWEEN…): Tab or Enter takes one, quoted when the name
needs it, Escape closes the list; with the list closed, Enter applies the filter. Ctrl+Space (or ⌘I) opens the
list when it is closed. The field "Filter the result on the server" above a query's result does the same with
the result's columns, the names the statement gave them included. The order, widths, hidden and
pinned columns and the sort of a table are kept for the next time it is opened, after a restart too.

## Foreign keys in the grids

A column that belongs to a foreign key has a small link mark after its name in the header; its tooltip says
where it leads ("→ shop.customers (id)"; for a key of several columns, "(a, b) → shop.pairs (x, y)"). The cell
under the pointer, and the selected cell, show an arrow at their right edge when they hold a value: a click on it
opens the referenced table in a tab of its own, filtered on that value, exactly as the cell's menu entry "Open
shop.customers where id = 42" does. ⌘-click on the value (Ctrl-click on Linux) and ⌘↓ (Ctrl+Down on Linux) on
the selected cell do the same. NULL has no arrow. A value you changed and have not
applied is the one followed. A double click on the value still edits it. The record view (Tab) has the same
arrow beside the field of a foreign key.

A query's result has the marks and the arrows on every column that shows a table's column, whatever the
statement: a join, a few columns, an alias, a result that cannot be edited, a read-only connection (following
only reads), on every engine. A key of several columns is followed only when all its columns are in the result;
otherwise its columns keep the mark, whose tooltip names the column missing, and have no arrow. A column in two
keys names both in its tooltip, and its arrow opens a small menu of the two (⌘-click and ⌘↓ open the first). A
binary key (`bytea`, `BINARY(16)`) is followed with the engine's binary literal, shortened in the tooltip. An
expression has no mark; neither has a view's column on PostgreSQL, MySQL and MariaDB (a view has no foreign
key), while SQLite names the table a view reads.

## The value panel

⌃⌥V or F7 (or "Value" in the footer of a result or a table, or "Toggle Value Panel" in the palette and the Data menu)
shows the selected cell whole at the right of the grid, in every query tab and table tab, and follows the
selection: JSON indented and highlighted, long text wrapped, binary as a hex dump, NULL told from an empty
string, the column's name and type above. Drag its left edge to change its width; Kerno keeps it, and whether
the panel is shown, for the next start. Where the cell takes a value, edit it there: Set or Cmd+Enter makes the
text a pending change (reviewed and applied with the grid's other changes), Escape drops it and gives the
keyboard back to the grid; JSON that does not parse is refused with where it breaks. Text you have not set
keeps the panel on its cell while you move around the grid. With the panel shown, a double click or Shift+Enter
on a cell puts the keyboard in the panel; with it hidden, Shift+Enter opens the value in a dialog as before, as
does "View value" in a cell's menu.

When the selected cell holds a foreign key (an order's `customer_id`, say), the panel shows under the value the
row it points to, headed "customers, by customer_id": the columns a person reads first (a name, an email, a
title, a code…), "Show all N columns" for the rest. "Choose columns…" picks the ones shown for that table, for
every key that points to it, and Kerno remembers them; "Kerno's choice" goes back to the default. "Open the
row" opens it in a tab of its own, as the cell's menu does. The row is read once the selection rests on the
cell for a moment, on a session of the panel's own, so it shows committed rows: a key your own open
transaction added says "No row with this key". A key you edited and not applied shows the row of the new
value, and says so. Nothing is shown for NULL; a key that points nowhere says "No row with this key"; a read
the server does not answer in 10 s is stopped, with Try again. Results of a query show this too when they are
one table's and can be edited.

## Split views

To see two tabs side by side, drag a tab by its title onto the left, right, top or bottom edge of the tab area:
the half it will take is shown while you drag ("Split to the right"), and dropping it there makes a new group of
tabs. Dropped in the middle of another group, or on its row of tabs, the tab moves into that group (on a row of
tabs, before the tab under the pointer; dragged within its own row, it changes place). Without the mouse: "Split
right" and "Split down" in a tab's menu, the palette and the Window menu (⌘⌥⇧L and ⌘⌥⇧I), "Move to the other
group" (⌘⌥⇧M; with one group it splits). Drag the line between two groups to share the space differently.

Each group has its own row of tabs and shows its own tab. The group you clicked last, or are typing in, is the
focused one: its tab is underlined in the accent colour, the others' in grey. ⌘T opens a tab there, and ⌘W,
⌘Enter, ⌘., Commit, Roll back, the main toolbar, the status bar and the window's environment colour act on its
tab; ⌘⌥⇧N and ⌘⌥⇧P go to the next and the previous group. Close others, to the left, to the right and all close
tabs of the group only; "All open tabs" (the arrow at the end of each row) lists the tabs of every group. A group
whose last tab closes goes, and its neighbour takes its space. Kerno shows four groups at most (a 2 × 2 grid):
past that, Split says so and a dragged tab can only join a group. The groups, their sizes and which tab is where
come back at the next start.

## A connection's SQL scripts

Right-click a connection ▸ **SQL scripts** lists what you work with on it: the queries you saved from a tab of that
connection ("Save Query…"), grouped by their folders, the last ten saved, then the last ten `.sql` files you opened
or saved in a tab of that connection, or whose tab you pointed at it, the latest first. Two files with the same
name show their folder. A click opens it in a SQL editor on that connection, connecting it first when it is closed
(with its password prompt, if any); a script already open is shown instead of being opened twice, and a file that
has gone since is said and leaves the list. "Open SQL file…" opens another file on that connection, "Saved
queries…" the saved queries view on that connection's (those you saved for any connection are there too), and with
more than ten saved queries "All scripts…" lists them all to type a name. From the keyboard: select the connection
in the sidebar, then "SQL Scripts of Selected Connection…" or "Open SQL File on Selected Connection…" in the
command palette (⌘K).

A file used with two connections is in both lists. A copy of a connection starts with none; deleting a
connection deletes its saved queries and forgets its files (the files themselves stay on disk). Exported
connections never carry the paths of these files.

## Backup and restore

Kerno backs up and restores with the engine's own tools, run on this computer: `pg_dump`, `pg_restore` and
`psql` for PostgreSQL, `mysqldump` and `mysql` for MySQL and MariaDB. The client used is the newest one found
that is not older than the server (Settings ▸ Local clients lists them; a connection can name its own). On
Windows they are looked for in the installers' folders of Program Files (`PostgreSQL\<n>\bin`,
`MySQL\<product>\bin`, `MariaDB <X.Y>\bin`) and on `PATH`.

**Back up…** on a database (the tree, Tools ▸, the main toolbar, the palette) opens one screen: what is backed
up (the whole database, with its schemas and tables counted), the file it writes, the format and the client.
The file is proposed in the folder of the last backup (else your home folder) as `<database>-<date and
time>`; **Choose…** opens the save panel. PostgreSQL offers two formats: the **compressed archive** (pg_dump's
custom format, the default, which pg_restore restores whole or only some of its tables) or a **plain SQL
script**. MySQL and MariaDB write a SQL script, read in one consistent snapshot, with the procedures, functions
and triggers. **Show command** shows the exact command (the password never: it goes to the tool through its
environment). **Back up** (or Enter) runs it; the progress shows the tool's output, the time, Cancel, and Show
in Finder at the end.

Beside PostgreSQL's compressed archive, **Faster: back up on N connections at once** (N is your computer's cores,
8 at most; off by default) has pg_dump read several tables at the same time (`--format=directory --jobs=N`). The
backup is then a folder, one file per table, chosen and shown like a file: **Restore…** takes that folder (pick
it in the open panel or type its path) and can restore it on several connections too. It helps a database of
many tables; one large table is still read by one connection. A failed or cancelled backup removes the folder
it made, never a folder that was already there: a folder of that name stops the backup before it starts.

MySQL and MariaDB get the same **Faster: back up on N connections at once** beside their SQL script: Kerno runs
N `mysqldump` at once, one per table, into a folder of SQL files, and cuts a large table whose key is one
whole-number column into parts read side by side, where every part can be read at the same moment: a large
InnoDB table (MySQL 5.5.3 and later, every MariaDB), and with **Block writes** (below) a large table of any engine
on any version, MySQL 5.1 included. Any other table is read by one connection, and so is every table with
"No locks". The procedures, functions, triggers, events and views come last, in their own files, and a small
`kerno-backup.json` names every file: **Restore…** takes that folder as one backup, and its **Faster: restore
on N connections at once** first creates every table of the backup, one after the other on one connection
(two tables linked by a foreign key cannot be dropped and created side by side), then loads several tables'
rows at a time (the parts of a large InnoDB table side by side). A failed or cancelled restore stops the others
and says which table, the server's error, and, as the database holds them then, which tables are loaded,
partly loaded, empty or missing; running it again starts each table over. Kerno reads what the mysql client
prints during such a restore, so the extra arguments that change it (`--table`, `--xml`, `--html`,
`--vertical`, `--silent`, `--verbose` and their letters) are refused there, under the field.

With "Faster", **Block writes during the backup, so that every table is read at the same moment** (off by
default) says how the backup is consistent. Off, each table is read at one moment, all its parts included (the
parts of a large InnoDB table start together while Kerno holds that table's read lock for a moment; a table whose
lock does not come within 10 s is read whole; before MySQL 5.5.3 that lock does not hold back a transaction that
already wrote, so no table is cut there), but two tables are not read at the same instant. On, Kerno holds
`FLUSH TABLES WITH READ LOCK` on the server from before it lists the tables until the last is read: every write
on that server waits until the backup ends, so Kerno shows the statement and asks first (twice on production),
naming any query that has been running for 5 s or more, since the lock waits for it. A lock that does not come
within 10 s stops the backup before it starts; one lost during the run (the server closed Kerno's session)
fails it. It needs the RELOAD privilege; an account without it is told so and cannot tick it. Without the PROCESS
privilege Kerno sees only its own account's queries, and the confirmation says so. The extra arguments may not
set what the folder backup sets for each file (`--where`, `--tab`, `--no-data`, `--ignore-table` and the like),
nor ask for a replication position (`--source-data`, `--master-data`, `--dump-replica`…: use the plain backup to
seed a replica) or `--flush-logs`; each refusal names the argument and what the folder sets instead.
`--set-gtid-purged=OFF` and `--skip-add-locks` are kept; `--skip-lock-tables` is dropped (the folder sets each
file's lock) and `--skip-triggers` turns Triggers off. Neither
backup takes `--log-error` or `--skip-verbose`: Kerno reads what mysqldump says.

A folder restored into a database that already holds some of the backup's tables is said before the run, and in
the confirmation, as the backup was made: how many of the backup's tables are there, and that they are dropped
and replaced with the backup's (empty, for a backup of the definitions only), the database's other tables stay,
and a restore that stops leaves a table it has not finished empty or missing until it runs again; without DROP
TABLE statements the restore stops at the first of them; a backup of the rows only adds its rows to the tables
there (a table that already holds some stops on a duplicate key, or ends with rows twice when it has no key).
The wizard reads the database's tables again after each run, so a second restore from the same wizard says
what the first one left. For a plain SQL dump, the wizard says that a dump with DROP TABLE statements replaces
its tables in a database that is not empty, without naming them (that would mean reading the whole file first).

A backup restored into another database than the one it was taken from can hold statements written for that
first database: a trigger created as `CREATE TRIGGER shop.audit_bi …` from a tab on another database keeps its
database's name on MariaDB, and a routine, an event, a trigger or a view may name `shop.orders` in its body.
Before Start, the restore says how many of each kind name it and what they would do: restored into
“shop_copy” as they are, the first kind would be created in “shop”, not in “shop_copy”, and Kerno leaves those
statements out unless you tick **Load them as written (they act on “shop”)**; the tables, their rows and
everything else are loaded, and the end of the run names what was left out and how to get it (edit
`routines.sql` and run it in “shop_copy”, or restore into a database named “shop”). A statement that only names
“shop” in its body is loaded either way and said once before the run and at its end: it acts on “shop” each
time it runs; an enabled event whose body names “shop” is left out too, as it would run against “shop” on
its schedule with no one running it. The statements mysqldump writes itself to change “shop”'s character set
around a routine are left out as well, and the routine is created with “shop_copy”'s own. On a server that
compares names without case, “SHOP” and “shop” are the same database and nothing is left out. The
confirmation says the same, the choice included. Kerno reads `routines.sql` and `views.sql` for
this, strings and comments left aside, names in double quotes read as names where the file's `sql_mode` makes
them so; a statement it cannot leave out alone (one that shares its lines with another, a view, a line that
selects a database with `use`) leaves its whole file out, and says so. A plain SQL dump is read only for its first lines:
the restore says which database it was taken from, or the database it selects itself (`USE`), when that is
not the one chosen. A routines or views file that fails once every table is in says that the tables and their
rows are in place, and what a rerun would do, rather than "run it again"; the backup's views a database
already held are said to be left as they were when the restore stopped before them.

MySQL and MariaDB tools talk `utf8mb4` with a server that has it (5.5.3 on): a MySQL 5.7 client no longer
writes a 4-byte character (an emoji) as `?`, in the plain backup as in the folder. Such a backup starts with
`SET NAMES utf8mb4`, which a server before 5.5.3 refuses (ERROR 1115): restoring it into MySQL 5.1, the wizard
says so before it runs.

An old server needs an old client. MySQL's `mysqldump` 8.4 and later, and MariaDB's `mariadb-dump` 11.8, read
no column of a MySQL 5.1 table (the backup would hold no table), and `mariadb-dump` cannot read MySQL 5.5 and
5.6 either: for those pairs the backup says so before it runs, names a MySQL 5.7 client found on the computer
(5.7.11 or later), and does not start until you choose one with **Local client…**.

**Restore…** opens the same kind of screen: the file (Choose… or a typed path; its format is read from the file
and says which tool restores it), the database it goes into, what a restore does to it, the client.
**Restore…** shows the command and asks first, twice on a production connection; a read-only connection has
no restore. Enter never starts a restore. A SQL file that holds client commands (`\!`, `\connect`, `system`…)
is refused; it can be allowed in the advanced settings, for a file you trust. A client that stops reading the
file on its own before its end (psql's `\q`, MariaDB's `quit`) and ends without an error fails the restore or
the script in plain words ("psql stopped reading before the end of … the rest of the script was not run"),
when more than a few dozen kilobytes were left; a stop closer to the end is not seen.

A backup made on another server gives its objects to that server's roles. Restored into a server without them
(a production backup into your own PostgreSQL), pg_restore stops at the first one; Kerno then says which role
the backup names and that this server has none, above the tool's output. **Restore without owners and
privileges…** runs the same restore again with pg_restore's `--no-owner --no-privileges`: everything restored
belongs to the user you connect as, without the backup's grants. It asks first, as any restore. You can tick
**Leave out the backup's owners and privileges** before the first run too (PostgreSQL archives only; it is off
by default, which keeps the owners). A plain SQL script sets its owners in its own text, so psql cannot leave
them out: create the role on the server, or back up again as a compressed archive.

Before a PostgreSQL archive is restored, Kerno reads its list and asks the server what it lacks. An extension
the backup needs and the server does not offer (its files are installed on the machine PostgreSQL runs on, not
by a restore) is said above the settings, with **Leave it out of the restore** ticked: the restore then runs
with pg_restore's `--use-list`, and the command says which entries it leaves out. Roles that own the backup's
objects and that the server lacks tick **Leave out the backup's owners and privileges**, with the reason. When
the server has everything, nothing is shown. A failed `CREATE EXTENSION` is said after the run too, and an
archive gets one button that restores again without what the server lacks.

**Advanced settings…** opens the full wizard on what the screen holds (the file, the format, the client): the
objects to back up (schemas and tables), every option of the tool, the file name pattern with its variables,
extra arguments, another user, **Save as task…**. **Simple view** goes back; when an option the simple screen
does not show was changed, it says which and asks before putting them back to their defaults. Tools ▸ Back up…
on a schema or a table, a new task and a saved task open in the wizard; the global backup (`pg_dumpall`) and
Execute script are wizards only.

## PostgreSQL extensions

Under each database of a PostgreSQL connection, the "Extensions" folder lists the extensions installed in it,
with their version and, once opened, their number. Everything else is in the database's extensions manager: a
double click or Enter on the folder, "Manage extensions…" in its menu, "Show in the extensions manager" on an
installed extension, Tools ▸ Extensions… on the database, or "Manage Extensions…" in the palette (for the
database selected in the tree, else the active tab's).

The manager lists every extension the server has files for: its name, what it is for (the description its own
files give), its version, and whether it is installed, has an update, or is available. Search over names and
descriptions; filter All, Installed, Available or To update, each with its count. Select one to see the schema
it is in or would go to, whether it can be moved later, the versions the server has, what it requires (and
whether that is installed), and who may install it: a trusted extension any user who may create objects in the
database, the others a superuser (servers before PostgreSQL 13 have no trusted extensions). An extension the
server does not list is not on the server's machine yet: install its package there first (for example from
your distribution's PostgreSQL packages), then it shows up.

Install… takes the schema and the version picked above it (the server's defaults otherwise); what it requires
and is not installed is named in the confirmation and installed with it (`CASCADE`). Update goes to the default
version, "Update to…" to another one the server has a path to; "Move to schema…" is there when the extension
allows it; Drop… lists what depends on it, and "Drop with cascade…" drops that too. Each shows its SQL first,
follows the connection's rules (nothing on a read-only connection, a second click on production), can be
cancelled while it runs, and a refusal says why in plain words before the server's message. Enter acts on the
selected row (Install… or Update), the arrows move in the list, ⌘F goes to the search, and each row has its
menu with Copy name. MySQL and MariaDB have no extensions.

## Compare two schemas

Server ▸ Tools ▸ Compare Schemas… (or the palette, or "Compare with…" and Tools ▸ "Compare schemas…" on a
schema or a database of the tree) asks for two sides: the **source**, the model, and the **target**, what the
script will change. Each is a saved connection (a click connects it), its database and its schema; on MySQL and
MariaDB the database is the schema. ⇄ exchanges them. Both must be of the same kind: two PostgreSQL schemas, or
two MySQL or MariaDB databases; another pairing is refused before anything is read.

The compare tab reads both at once (each side's progress shows, Cancel stops it) and lists tables, views,
materialized views, functions, procedures, sequences, types and triggers, grouped by kind, each marked `+` only
in the source, `−` only in the target, `≠` different or `=` the same. The filters count them; "Same" is hidden
until picked. Select an object to read what differs in plain words ("column `price`: numeric(10,2) →
numeric(12,2)", "index `orders_customer_idx` is missing in the target") and both definitions side by side.
Names are compared as the server compares them; a renamed object shows as one only in the source and one only
in the target. Rows are never read.

Each object has a checkbox: ticked ones go into the script (All and None act on what is shown). "Open script"
opens the script that brings the target to the source in a new query tab on the target: nothing has run. Read
it, then run it there; the connection's rules apply (a read-only target refuses it, production asks twice).
What only the target has is dropped only when "Also drop what only the target has" is ticked; otherwise its
`DROP` statements are comments. What cannot be changed in place (a type, a sequence) is a comment that says
why, with the source's definition. Swap and Refresh compare again; a row's menu opens the object in the source
or the target.

## Compare the rows of two tables

Right-click a table in the tree (or the tab of its data) and choose "Compare data with…", or use Server ▸ Tools
▸ Compare Data… and the palette, which start from the data tab on screen. The dialog asks for two tables: the
**source**, the model, and the **target**, what the script will change, each a saved connection, its database,
its schema (not on MySQL and MariaDB) and the table, typed or picked from Tables ▾. Both start on the table you
asked from: change the target's schema, database or connection. ⇄ exchanges them. Both must be of the same
kind: two PostgreSQL tables, or MySQL and MariaDB tables; another pairing is refused, and so is SQLite for now.

Once both tables are read, the dialog shows the **key** the rows are matched by (the target's primary key, or a
unique key of the target on columns that never hold NULL; pick another when there are several) and the
**columns** both tables have, each ticked: untick one to leave it out. A column only one table has is said and
not compared; so is a column whose type is of another kind on each side. The key must be unique in the target,
so that each statement of the script changes one row at most: a target without a key cannot be compared, and a
key only the source holds unique is not offered (the dialog says so). Nothing is written: both tables are only
read.

Two keys the target's server holds equal though they are written differently (`a` and `A` under a
case-insensitive collation, `a` and `a ` where trailing spaces are ignored, `citext`, a uuid against text) are
one row: it shows as "key spelled otherwise", its key as `source → target`, and its `UPDATE` writes the source's
spelling. A row whose twin the compare could not keep is said, and the script writes a comment for it instead
of a statement.

The tab reads both tables at once, each in the order of its key, and compares their rows as they come, so a
table of millions of rows takes little memory; each side's row counter and the time show, and Cancel stops
both reads on their servers. Then it counts the rows only in the source (`+`), only in the target (`−`),
different (`≠`) and the same, and shows the differences one row per key: a changed cell reads `source →
target`. The filters show one kind; "Any column ▾" shows the rows where one column differs, with each column's
count. Click a row to read all its values on both sides beside the list; its menu copies the key, either row,
or the row as TSV, CSV, JSON, Markdown or an `INSERT`, and ⌘C copies it. Values are compared as the server
types them: `1.50` and `1.5` are the same number, NULL is not an empty text, JSON is compared without its
spaces, and keys are read in the order of their exact text, then matched as the target's server matches them
(above). Each table is read in one statement: a row someone changes during the compare is compared as it was
when its read began.
At most 10,000 differences are kept to show and to write; beyond, the counts go on and the tab says so.

"Open script" opens, in a new query tab on the target, the statements that make it like the source: an `INSERT`
for each row only in the source, an `UPDATE` of the changed columns for each different row, and a `DELETE` for
each row only in the target when "Also delete what only the target has" is ticked. Nothing has run: read the
script, then run it there; the connection's rules apply (a read-only target refuses it, production asks twice).
Switch that tab to manual commit to apply it all or nothing. Columns the server computes are compared but never
written. Each value is written as its server prints it; types beyond the common ones (bit strings, spatial
types, ranges) were not tried, so read their lines; a row holding text that is not UTF-8 gets a comment, no
statement. New rows come before the rows that point at them through the
table's own foreign keys, and deleted rows after them; if a unique value moves from a deleted row to a new one,
run the deletes first. When the script holds only the first 10,000 differences, compare again once it has run.

## Generate test data for a table

Right-click a table in the tree (or the tab of its data) and choose "Fill with test data…", or use Server ▸
Tools ▸ Generate Test Data… and the palette, which start from the data tab on screen. It works on PostgreSQL,
MySQL, MariaDB and SQLite. The rows are made on this computer, nothing is sent anywhere: this is not the AI
assistant's "Generate test data…" (under AI ▸), which writes what a model imagines.

The dialog reads the table once and shows one line per column with the values it will get, guessed from its
type, its keys and its name, and that kind's settings: a **sequence** (start, step) for a key of whole numbers,
starting after the largest value the table holds; a **random number** or **decimal** in a range the type can
hold; **random words** to a length the column holds; an **email address**, a **first**, **last** or **full
name**, a **phone number**, a **city**, a **country** (a two-letter code where the column is short), a **web
address** or a **user name** for a text column named so; dates and times in a range of days (with the zone
where the type keeps one); UUIDs; a small JSON object; one of an enum's or a `SET`'s labels; random bytes or
bits; arrays of up to three elements. A **foreign key** picks keys that exist in the referenced table (a key of
several columns as a whole, among its first 10,000 keys). Columns the server makes (generated, identity,
auto-increment) are left to it; a column with a default can be left to its default; a nullable column gets a
**share of NULL** (10 % by default outside the keys). On SQLite, which keeps dates as text, a text column whose
default is a date or a time (`CURRENT_TIMESTAMP`, `datetime('now')`, …) is left to that default, and one named
like a date (`created_at`, `due_date`, `date`, `timestamp`) or a time of day (`time`, `start_time`,
`time_of_day`) gets dates or times written as SQLite writes them. Pick another kind with the button of its
line; the settings change with it.

Every value comes from the **seed**: the same settings give the same rows, except values that must be unique,
which continue after the rows already in the table. So the same seed on an empty table gives the same rows
each time, and on a table that already holds them it inserts again without repeating a key. New seed gives
other rows. The dialog keeps each table's last settings, seed included, until Kerno quits. Keys stay unique: a
sequence never repeats, a unique text ends with a number beyond the rows there, and other values are drawn
again until they differ (compared without case, as a case-insensitive server would). What cannot
work is said before anything runs: a range the type cannot hold, more rows than a unique column has values for,
a referenced table without rows, a MySQL `TIMESTAMP` outside 1970 to 2038. Rules Kerno does not check (a
`CHECK`, an exclusion constraint, a partial unique index) are listed under the columns: a row that breaks one
stops the insert, and nothing is kept. The preview shows the first ten rows as they will be written, and
follows each change.

Ask for 1 to 1,000,000 rows. **Open script** writes the `INSERT` statements (500 rows each) into a new query tab
on the table's connection, up to 10,000 rows, and runs nothing. **Insert…** shows the first statement, asks as
the connection's environment says (production asks twice), then inserts every row in one transaction, with
progress and Cancel: all of them, or none. A row the server refuses is named with the server's message, and
nothing is kept. A read-only connection offers Open script only. Once the rows are in, an open data tab of the
table shows them.

## Export a schema as a document

Right-click a schema (a database on MySQL, MariaDB and SQLite) and choose "Export schema…", or a table or a view
to start with it alone; in an ER diagram, "Export document…" starts with its tables; Server ▸ Tools ▸ Export
Schema… and the palette start from the active tab. The dialog lists the tables and views, all ticked: search,
All and None act on what is shown. "Also routines, sequences, types and triggers" adds those with their DDL.
Only the structure is read: no row, nothing changed, so a read-only or production connection exports too.

Pick what the document is for:

- **HTML**: one page to read, send or print, for a team or a client: the diagram, a table of contents, then
  every table with its columns, keys, constraints and indexes, and the views. In Kerno's light or dark style or
  plain black on white; it opens in any browser, asks the network for nothing, and prints a page per table (the
  dark style prints on white).
- **PDF**: the same document ready to print or send, on A4 or US Letter (the Paper choice under the styles): a
  contents page whose entries lead to their pages, the diagram drawn to fit a page (zoom into it: it is drawn,
  not an image), a page per table whose rows continue on the next pages with their header, a bookmark per
  table, the title and "page n of N" at the foot of each page. Its text can be searched and copied. The dark
  style keeps its dark pages; pick Kerno light or Plain to print.
- **Markdown**: a data dictionary for a wiki or a pull request, and a compact form of the schema to give an AI
  assistant.
- **DBML**: the tables and their references, to draw on dbdiagram.io or publish with dbdocs.
- **SQL**: the statements that create what is ticked, in an order that runs (tables, then their keys, then the
  views): to keep the schema in git or rebuild it elsewhere.
- **Mermaid**: the diagram, for Markdown files that render Mermaid.

The title starts as `database.schema`; change it as you like. Choose… picks the file (its name starts as
`<schema>-schema.<ext>`); Export writes it, Cancel stops while it reads, then Show in Finder or Open. A document
never names the server's host, the user or a database file's path.

## Where Kerno keeps its data

Everything Kerno remembers (saved connections without their passwords, folders, environments, settings, open
tabs, where the window was and how large, history, saved queries and tasks) is in one SQLite file, `kerno.db`,
in its data folder: `~/Library/Application Support/Kerno` on macOS, `~/.local/share/kerno` on Linux,
`%APPDATA%\Kerno\data` on Windows (and `%APPDATA%\Kerno\config` for `keymap.json`). Beside it:
`logs/` (a file a day, the last seven), `known_hosts` (the SSH host keys you accepted in Kerno),
`folder-icons/`, and `backups/`. `keymap.json` is in the same folder on macOS and in `~/.config/kerno` on Linux. Saved passwords
are in `secrets.kerno` (encrypted, its key in `secrets.key`) or in the system keychain, as Settings ▸ Passwords
says ([above](#where-passwords-are-kept)); `secrets.lock` is an empty file held while one is written.
`KERNO_DATA_DIR=<folder>` puts all of it in another folder for a
run.

Before a new version of Kerno changes the layout of `kerno.db`, it copies the file into `backups/` as
`kerno-<date>-<time>-schema<N>.sqlite3` (UTC, `N` the layout it had) and keeps the three newest; if the copy
cannot be written, Kerno does not change the file and says why. To go back to an older Kerno, quit Kerno,
move `kerno.db`, `kerno.db-wal` and `kerno.db-shm` aside together (SQLite would apply a `-wal` left behind to
the file you put in), and copy the backup in as `kerno.db`.

When Kerno cannot start (data written by a newer Kerno, a damaged file, a folder it cannot write), it opens a
window that says what happened and what to do, with the error below it, instead of quitting. **Open data
folder** shows the folder; **Start fresh (move data aside)**, offered when the data itself is the problem,
renames `kerno.db` and its `-wal` and `-shm` files to `kerno-moved-<date>-<time>-schema<N>.sqlite3` in the same
folder, deletes nothing, and starts Kerno on a new, empty file; the keychain is not touched. To use the moved
data again, quit Kerno and rename those files back to `kerno.db`, `kerno.db-wal` and `kerno.db-shm`. A saved
connection that this Kerno cannot read (written by a newer one) is hidden and named in a notification at
start, never deleted.

## Install

The packages are on the [releases page](https://github.com/EvanPluchart/kerno/releases/latest); the README has
the short version. On macOS, open the `.dmg` and drag Kerno onto the Applications folder beside it, as its window
shows. To build them yourself, `scripts/bundle-macos.sh` builds `dist/Kerno.app` and a `.dmg`,
`scripts/bundle-linux.sh` builds the Linux packages (below), and `scripts/bundle-windows.sh` (Git Bash with Inno
Setup 6, on Windows) the Windows zip and installer.

On macOS the bundle is signed ad hoc unless `KERNO_SIGN_IDENTITY` names a code-signing identity of yours
(`KERNO_SIGN_IDENTITY="Apple Development: …" scripts/bundle-macos.sh`). This matters only when Settings ▸
Passwords keeps passwords in the macOS Keychain (Kerno's own file, the default, asks nothing): Kerno then reads
each saved password from the keychain once per run, and as an ad-hoc signature changes with every build and
every update, the keychain asks for your login password again after each one, once per saved password; "Always
allow" stops it for that build. A stable identity avoids it.

### Windows

Each release carries `Kerno-<version>-windows-x86_64-setup.exe`, the installer, and
`kerno-<version>-windows-x86_64.zip`, the same files to unpack anywhere. The installer is for your account
only and never asks for an administrator: it puts Kerno in `%LOCALAPPDATA%\Programs\Kerno`, adds it to the
Start menu (a desktop shortcut if you tick it), and lists it in Settings ▸ Apps ▸ Installed apps with its
version, where **Uninstall** removes it. It shows both licences (MIT and Apache 2.0, either at your choice).

The installer is not signed (there is no certificate yet). Windows may say "Windows protected your PC" when you
open it after a download: click **More info**, check that the file is the one from Kerno's release page (its
SHA-256 is in the release's `SHA256SUMS`: `Get-FileHash Kerno-<version>-windows-x86_64-setup.exe` in
PowerShell), then **Run anyway**.

Updates from the app work on both: Kerno downloads the new installer and `SHA256SUMS` from the release (into
`%LOCALAPPDATA%\Kerno\data\updates`, inside your profile), runs the installer only when its SHA-256 matches,
quits (after asking about open transactions and pending changes, as quitting does), and the installer replaces
Kerno and starts it again; your tabs, unsaved text included, come back. A copy unpacked from the zip is updated
the same way: the new version is installed for your account as above, and the unpacked folder stays where it
is, yours to delete. A Kerno started as administrator ("Run as administrator") never runs an installer it
downloaded: Update opens the release page instead. Kerno takes `curl.exe` and the other programs Windows ships
only from `C:\Windows\System32`: on a Windows installed elsewhere (rare), it checks for no update and Update
opens the release page. If the installer cannot finish (another Kerno still open from the same folder, a folder
it cannot write), it changes nothing, starts the Kerno you had again, and that Kerno says what happened and
where the installer's log is (`%APPDATA%\Kerno\data\update-setup.log`).

Quit Kerno before you install a version over it by hand. Uninstalling removes the program, its shortcuts and its
entry, never your data: connections, settings, history, saved queries and Kerno's password file stay in
`%APPDATA%\Kerno` (delete that folder to remove them too), and passwords kept in Windows' Credential Manager
stay there.

### Linux

Each release carries, for x86_64 and arm64 (aarch64):

| Package | Install |
|---|---|
| `kerno_<version>_amd64.deb`, `kerno_<version>_arm64.deb` | `sudo apt install ./kerno_<version>_amd64.deb` (Debian, Ubuntu and their derivatives; apt installs the dependencies) |
| `kerno-<version>-linux-x86_64.tar.gz`, `…-aarch64.tar.gz` | `tar -xzf kerno-<version>-linux-x86_64.tar.gz -C ~/.local --strip-components=1`, then run `~/.local/bin/kerno`. Kerno updates itself in place from this one |
| `Kerno-<version>-x86_64.AppImage`, `…-aarch64.AppImage` | `chmod +x Kerno-<version>-x86_64.AppImage` and run it. It mounts itself through FUSE (the `fusermount3` or `fusermount` program, from the `fuse3` or `fuse` package); without FUSE, run it with `--appimage-extract-and-run` |

The packages run on any distribution with **glibc 2.31 or newer** (Ubuntu 20.04, Debian 11 and later): they are built on Ubuntu 20.04, and each is started there and on Debian 13 before a release
(`scripts/check-linux-packages.sh`). The tarball and the AppImage use the system's libraries, as the `.deb`
does through its dependencies; install them if they are missing (Debian and Ubuntu names):
`sudo apt install libxcb1 libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libvulkan1`, and a Vulkan driver
(`mesa-vulkan-drivers` or your GPU vendor's). The fonts are built in.

Backup and restore call `pg_dump`, `pg_restore`, `psql`, `mysqldump` and `mysql` when they are installed;
nothing else needs them.

## Build from source

The repository pins its Rust toolchain in `rust-toolchain.toml`; rustup installs it on first use.

macOS: install the Xcode command line tools, then

```sh
nice -n 10 cargo build --release -p kerno
nice -n 10 cargo run --release -p kerno
```

Metal shaders are compiled at runtime, so the Metal toolchain is not required (see D-002 in
[docs/DECISIONS.md](DECISIONS.md)).

Linux (Debian or Ubuntu): install GPUI's native dependencies as root, then build the same way. The script also
adds the PostgreSQL project's apt repository (its key checked by fingerprint) for the PostgreSQL 17 client the
tests' server needs: a distribution's older `pg_dump` refuses a newer server.

```sh
sudo scripts/install-linux-deps.sh
nice -n 10 cargo build --release -p kerno
```

`scripts/bundle-linux.sh` builds the Linux packages inside Docker, from macOS or Linux, on an arm64 host (Apple
Silicon): the image of `packaging/linux/Dockerfile` (Ubuntu 20.04) builds arm64 natively and cross-compiles
x86_64. `KERNO_JOBS` sets cargo's jobs (3 by default). The image `kerno-linux-build` and the volume
`kerno-linux-cache` (cargo registry and build output) stay for the next run; `scripts/bundle-linux.sh --clean`
removes both. `.cargo/config.toml` caps cargo at 3 jobs.

## Checks and releases

There is no hosted CI: the checks run locally. `scripts/check.sh` runs `cargo fmt --check`, clippy with
`-D warnings`, `cargo deny check` and the workspace tests (starting the `kerno-*` containers if they are not
running, and stopping them again if it started them), one summary line per step, stopping at the first
failure. `scripts/check.sh --no-tests` skips the tests. It refuses to start when the machine is busy.

A release is built locally with `scripts/release.sh [version]`: it checks the tree is clean and the version
matches `Cargo.toml`, builds the macOS `.dmg` (on macOS) and the Linux packages, starts each Linux package in
clean containers (`scripts/check-linux-packages.sh`), and writes `dist/SHA256SUMS`.
`scripts/release.sh --linux-only` builds and checks the Linux packages alone, on a tree that may be dirty.
It then prints the `gh release create` command; the owner runs it. The script creates no tag, pushes and
uploads nothing; its only downloads are the Docker base images and pinned build tools of the Linux build.

## Test databases

The integration tests run against containers named `kerno-*`: PostgreSQL 17 (host port 55432), MySQL 8.4
(53306), MariaDB 11 (53307), an SSH server (52222) and a jump host in front of it (52223), all bound to
127.0.0.1 with test-only credentials.

```sh
scripts/test-env.sh up        # generate test certificates and keys, start, wait until seeded
nice -n 10 cargo test --workspace -- --test-threads=3
scripts/test-env.sh down      # stop and remove the containers
```

Tests skip with a message when a container is down; `KERNO_REQUIRE_CONTAINERS=1` makes that a failure.
Details in [CONTRIBUTING.md](../CONTRIBUTING.md).

`scripts/test-env.sh up --legacy` adds the old MySQL servers of the compose file's `legacy` profile:
`kerno-mysql51` (53351), `kerno-mysql55` (53355), `kerno-mysql56` (53356) and `kerno-mysql57` (53357), same
user and password, seeded with `tests/fixtures/mysql-legacy`. The 5.x images are amd64 only and run under
emulation on Apple Silicon; 5.1 has no image of its own and is built by `tests/fixtures/mysql51/Dockerfile` from
Oracle's archived binaries, checked against Oracle's MD5. `scripts/check.sh` starts them; their tests are in
`crates/kerno-mysql/tests/legacy.rs`.

## Keyboard shortcuts

`Cmd` on macOS is `Ctrl` on Linux and Windows, which share every key this guide gives "on Linux" (Ctrl+Y is the
text fields' Redo on both); `Ctrl` written alone is the Control key everywhere. Every action is also
reachable from the command palette: type words of a command's name, its section or its keywords, in any order and
without accents ("theme light", "sql new"); Escape closes it.

On macOS the Edit menu starts with Undo, Redo, Cut, Copy, Paste and Select All, with their usual keys. They act on
whatever has the keyboard: the SQL editor and every text field, a grid for Copy, Paste and Select All, and for
Copy what Cmd+C copies elsewhere (the name of the object selected in the schema tree, the cell selected in a
structure or administration list); an entry is greyed where nothing focused takes it.

| Shortcut | Action |
| -------- | ------ |
| Cmd+K | Command palette |
| Cmd+, | Settings |
| Cmd+F | Find a setting (in Settings) |
| Cmd+N | New connection |
| Cmd+P | Open table (quick open) |
| Cmd+B | Toggle sidebar |
| Cmd+Alt+F | Filter the schema tree |
| Cmd+T | New query tab (on the connection selected in the sidebar, connected first if it is not) |
| Cmd+W / Cmd+Shift+W | Close tab / close all tabs of the group |
| Ctrl+Tab / Ctrl+Shift+Tab, also Cmd+Alt+Right / Left (Super on Linux) and Ctrl+PageDown / PageUp | Next / previous tab of the group |
| Cmd+Alt+Shift+L / Cmd+Alt+Shift+I | Split right / split down (the tab goes to a new group) |
| Cmd+Alt+Shift+M | Move the tab to the next group (with one group: split right) |
| Cmd+Alt+Shift+N / Cmd+Alt+Shift+P | Focus the next / previous group |
| Cmd+O | Open SQL file |
| Cmd+S / Cmd+Shift+S | Save / save as |
| Cmd+Enter | Run statement |
| Cmd+Shift+Enter | Run all statements |
| Cmd+\ | Run statement in a new result tab |
| Cmd+Alt+Enter or Alt+P | Run the script from the cursor (editor) |
| Cmd+Alt+] / Cmd+Alt+[ (or Alt+Down / Alt+Up) | Go to the next / previous statement (editor) |
| Cmd+. | Cancel the running statement |
| Ctrl+Alt+N / Ctrl++ | Fetch more rows / fetch all rows, of a result or a table |
| Ctrl+Shift+P | Pin or unpin the result tab |
| Ctrl+Shift+\ (written Ctrl+\|) | Close the result tab |
| Cmd+Alt+R or Alt+Shift+T | Show or hide the results panel |
| Ctrl+Shift+T | Maximize the results panel |
| Cmd+Alt+T or Alt+T | Move the focus between results and editor |
| Ctrl+4 / Ctrl+8 | Commit / roll back the transaction |
| Ctrl+Alt+A | Switch between auto-commit and manual commit |
| Cmd+E / Cmd+Shift+E | Explain / explain analyze |
| Cmd+Y (macOS; Ctrl+Alt+E on Linux, where Ctrl+Y is the text fields' Redo) | Query history |
| Cmd+/ | Toggle line comment |
| Cmd+Shift+/ | Toggle block comment |
| Cmd+Shift+I | Format SQL |
| Cmd+Shift+X / Cmd+Shift+Y | Upper case / lower case |
| Ctrl+Alt+Shift+W | Toggle soft wrap |
| Ctrl+Space or Cmd+I | Show completions (editor, WHERE fields) |
| F4 / Shift+F4 | Open the rows / the structure of the table at the cursor (editor) |
| F4 | Edit the selected object, connection or folder (sidebar) |
| F2 / F5 | Rename / refresh the selected object |
| Backspace or Delete | Drop the selected object / delete the selected connection (sidebar) |
| Enter | Open the selected connection (sidebar) |
| Cmd+C / Cmd+Shift+C | Copy object name / qualified name (navigator) |
| Cmd+] or Cmd+Alt+Enter | New query on the selected object (navigator) |
| Cmd+Shift+A | Set the selected schema as the default of this tab (navigator) |
| Cmd+Shift+, | Link the schema tree with the editor |
| Cmd+R | Refresh rows (data view) |
| Alt+Insert or Cmd+Shift+N | Add row (grid) |
| Ctrl+Alt+Insert or Cmd+D | Duplicate the selected rows (grid) |
| Alt+Delete or Cmd+Backspace | Delete the selected rows (grid) |
| Ctrl+Backspace | Set the selected cells to their default (grid) |
| Ctrl+D / Ctrl+Alt+D | Copy the values of the row above / below (grid) |
| Escape | Revert the selected cells (grid) |
| Cmd+Z | Undo the last pending change (grid) |
| Cmd+Shift+C | Copy the selection with column names (grid) |
| Shift+Enter | View or edit the value of the selected cell (in the value panel when it is shown) |
| ⌃⌥V or F7 | Show or hide the value panel beside the grids (on a Mac keyboard F7 needs Fn) |
| Tab | Toggle record view (grids) |
| Ctrl+Alt+Left / Right | Previous / next row (Add Shift for the first / last row) |
| Ctrl+G / Ctrl+Shift+G | Go to row / go to column |
| Cmd+Down | Open the rows the selected cell's foreign key points to (grid) |
| Ctrl+F11 | Filter by distinct values of the column |
| Cmd+= / Cmd+- / Cmd+0 (or Alt+0 / Alt+9) | Grid text larger / smaller / usual |
| Cmd+Shift+D | Switch between the light and the dark theme (the theme shown; Settings ▸ Appearance ▸ Follow system goes back to the system's) |
| Cmd+Alt+W | Close the other tabs of the group |
| Cmd+Shift+T (macOS; Super+Shift+T on Linux) | Reopen the tab closed last |
| Ctrl+Alt+Shift+K / Ctrl+Alt+Shift+R | Commit / roll back the transaction (beside Ctrl+4 / Ctrl+8, easier on an AZERTY keyboard) |
| Ctrl+Shift+O | Show the messages of the last run |
| Cmd+Alt+Y | Refresh the schema tree |
| Cmd+Alt+N (macOS; Super+Alt+N on Linux) | Set the selected cells to NULL |
| Cmd+Alt+Z (macOS; Super+Alt+Z on Linux) | Revert the selected rows |
| Cmd+Alt+Shift+U (macOS; Super+Alt+Shift+U on Linux) | View the ER diagram |
| Ctrl+Alt+Shift+B | Server administration |
| Cmd+Q | Quit |

These are the defaults in `crates/kerno/src/actions.rs` (the source of each, DBeaver, DataGrip or a macOS
convention, is named beside it there); Settings ▸ Shortcuts lists every command with the shortcuts in
effect.

### Changing shortcuts

**Settings ▸ Shortcuts** (also "Keyboard Shortcuts…" in the Kerno menu and the palette) lists every command
by group with its shortcuts; the ones you changed say "Changed". The filter takes a word of a command's name
or group, or a shortcut, typed as the tooltips write it (⌘K) or in words (`cmd k`, `ctrl shift d`).

- **Change** a shortcut: click it, then press the new one with its modifiers. **Add** records one more,
  **×** removes one, **Reset to default** gives the command its defaults back, **Reset all…** (asked first)
  every command.
- What can be recorded: a letter, a digit or a sign needs Cmd or Ctrl (Option or Shift alone would type a
  character); Alt is enough with an arrow, Enter, Tab or another named key, and a command of the schema tree or
  a grid may take a named key alone; F1 to F24 work alone. A key the text fields use (Cmd+C, Cmd+Z,
  Alt+Left…) stays theirs for a command that works everywhere. Escape cancels the recording.
- A shortcut another command already answers where this one would is never taken silently: the page names
  that command and asks, **Replace** (it loses the shortcut) or **Cancel**. Commands of parts of the window
  that never have the keyboard together may share one (F4 in the schema tree and in the editor).
- A change works at once: the keys, the tooltips, the palette and the menu bar's shortcuts.

Changes are stored in `keymap.json`, in Kerno's config directory (the platform's config directory for
"Kerno"), which stays yours to edit: the page changes only the entries of the commands you change and keeps
the rest of the file. "Open Keymap File" (Kerno menu, palette, the button at the bottom of Settings) creates
it with every command id and its default shortcuts under `commands`. Each entry of `bindings` replaces the
shortcuts of one command: one shortcut, a list of shortcuts, or `null` for none. `secondary` means Cmd on
macOS and Ctrl on Linux; a sequence of keystrokes is separated by spaces. A change typed in the file applies
when Kerno starts or when Settings opens; problems in it are reported then. A file Kerno cannot
read is said on the page, which then changes nothing until it is fixed.

```json
{
  "bindings": {
    "new_query_tab": "secondary-alt-shift-t",
    "close_tab": ["secondary-w", "ctrl-f4"],
    "toggle_sidebar": null
  }
}
```

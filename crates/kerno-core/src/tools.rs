//! Backup and restore through the engines' own client tools (`pg_dump`,
//! `pg_restore`, `psql`, `mysqldump`, `mysql` and their MariaDB names): find
//! them, build their command lines from a connection, run them with their
//! output streamed line by line.
//!
//! The password never reaches a command line, where any local user can read
//! it in the process list: it goes to the child through its environment
//! (`PGPASSWORD`, `MYSQL_PWD`), and [`ToolCommand::display`] and `Debug` only
//! name the variable.
//!
//! # A restored SQL file is a script
//!
//! `psql --file` and `mysql < file` run the *client commands* a file holds
//! (`\!`, `\copy … program`, `\o |…`, `\i` in psql; `system`, `source`, `tee`,
//! `pager` in mysql) on this computer, so a dump from an untrusted source
//! could start programs or read and write files. Unless
//! [`RestoreOptions::allow_client_commands`] is set, [`restore_command`]
//! protects each tool this way:
//!
//! - `psql`, every version: scanned. psql has no option that turns its
//!   meta-commands off (`\restrict`, which pg_dump 17.6 / 18 writes, is a
//!   command of the file itself, so a hostile file leaves it out), so
//!   [`run`] reads the file with [`check_script`] before starting psql and
//!   refuses it when it holds one. Its rule depends on the version (see
//!   the psql table below).
//! - MySQL's `mysql` from 8.0.43, 8.4.6 and 9.4 (when `--commands` appeared),
//!   given the version found by [`version`]: `--commands=OFF`.
//! - MariaDB's `mariadb` from 10.6.18, 10.11.8, 11.0.6, 11.1.5, 11.2.4,
//!   11.4.2 and every series after 11.4: `--sandbox --binary-mode`. The
//!   sandbox leaves `connect` on (checked with 11.8.9: `\r db host` still
//!   reconnects, with the password the client holds); binary mode turns off
//!   every command but `\C` and `delimiter`, and mariadb-dump's
//!   `/*M!999999\- … */` first line and `DELIMITER ;;` still work under it.
//! - Every other `mysql` / `mariadb`, and an unknown version: scanned, the
//!   scan being the only protection.
//! - `pg_restore`: nothing to do, a custom-format archive holds no client
//!   script.
//!
//! psql reads a command name up to whitespace or a backslash. What the scan
//! refuses (the full list is `PSQL_REFUSED`):
//!
//! | Written | psql 8.4 and later (checked: 14.24, 18.6) | psql before 8.4 | Scan, psql known to be 8.4+ | Scan, older or unknown psql |
//! |---|---|---|---|---|
//! | `\! cmd`, `\o file`, `\i file`, `\set x …`… (a refused command, then whitespace, a backslash or the end of the line) | runs it | runs it | refused | refused |
//! | `\!cmd`, `\o\|cmd`, `\ifile`, `\w/f`, `\s/f`, `\g\|cmd`, `\cdb`, `\ir/f`, `\efname` (argument glued on) | "invalid command", nothing runs | runs the one-letter command with that argument | allowed | refused (first letter one of `! c e g i o s w`) |
//! | `\s+`, `\w*` in a regular expression (`\d+`, `\S`, `\W` start with no refused letter) | invalid command | `\s` and `\w` run | allowed | refused |
//! | COPY data escapes `\N \t \n \r \\`… | not read as commands inside COPY data | same | allowed | allowed |
//!
//! So the strict rule costs a false alarm on any dump with a `\s` or `\w`
//! regular expression (the fixture database has one, in a domain check): give
//! the version ([`RestoreOptions::tool_version`]) to get the lenient one.
//!
//! The mysql / mariadb client commands (from `help` in MySQL 26.10 and
//! MariaDB 11.8) and what stops each one:
//!
//! | Command | What it does | Scan | `--commands=OFF` | `--sandbox` | `--binary-mode` |
//! |---|---|---|---|---|---|
//! | `system` `\!` | runs a shell command | refused | off | off | off |
//! | `source` `\.` | reads and runs a file | refused | off | off | off |
//! | `tee` `\T` | appends the output to a file | refused | off | off | off |
//! | `pager` `\P` | pipes the output to a program | refused | off | off with an argument | off |
//! | `edit` `\e` | runs `$EDITOR` | refused | off | on | off |
//! | `connect` `\r` | reconnects, to the database and host it names, with the password | refused (every `\r`) | off | **on** | off |
//! | `ssl_session_data_print` | writes the TLS session to a file | refused | off | (MySQL only) | off |
//! | `use` `\u` | another database of the same server | allowed | passed to the server | on | off |
//! | `charset` `\C` | client character set | allowed (see [`check_script`]) | on | on | on |
//! | `delimiter` `\d` | statement delimiter (`DELIMITER ;;`) | allowed | on | on | on |
//! | `clear` `\c`, `go` `\g`, `ego` `\G`, `print` `\p`, `quit` / `exit` `\q`, `status` `\s`, `help` `\h` `\?`, `rehash` `\#`, `prompt` `\R`, `notee` `\t`, `nopager` `\n`, `warnings` `\W`, `nowarning` `\w`, `resetconnection` `\x`, `query_attributes`, `costs` `\Q`, `sandbox` `\-` | the session and the display only | allowed | off or on | on | off |
//!
//! The scan cannot tell the `\r` of `connect` from a carriage return inside
//! a string (`'a\r\nb'`, how mysqldump writes one): it refuses both. Where a
//! string ends depends on the server's `sql_mode` (`NO_BACKSLASH_ESCAPES`,
//! `ANSI_QUOTES`), which the file itself can set in ways a scan cannot
//! follow (a variable, a bit mask, `@saved_sql_mode`), so following quotes
//! would leave a way through. A dump with carriage returns in its data thus
//! needs the opt-in on a client without the options above.
//!
//! A version wrongly taken for a recent one fails closed: the client refuses
//! the unknown option and runs nothing. MySQL restores also get
//! `--local-infile=0`, so `LOAD DATA LOCAL INFILE` cannot send a file of this
//! computer to the server.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

use crate::config::{ConnectParams, Engine, Secret, TlsConfig, TlsMode};
use crate::error::{Error, Result};
use crate::schema::TableRef;
use crate::ssh::Tunnel;

mod archive;
pub mod mysql_folder;
mod roles;

pub use archive::{
    list_database, list_extensions, list_without, missing_owners, refused_extensions,
};
pub use roles::{RefusedRole, refused_roles};

/// A client tool Kerno runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tool {
    /// `pg_dump`.
    PgDump,
    /// `pg_dumpall`, for the global backup (roles, tablespaces, every
    /// database).
    PgDumpall,
    /// `pg_restore`, for custom, directory and tar archives.
    PgRestore,
    /// `psql`, for plain SQL dumps and scripts.
    Psql,
    /// `mysqldump`, or MariaDB's `mariadb-dump`.
    MysqlDump,
    /// `mysql`, or MariaDB's `mariadb`.
    Mysql,
}

impl Tool {
    /// The binary names the tool goes by, in the order they are looked for.
    pub fn names(self) -> &'static [&'static str] {
        match self {
            Self::PgDump => &["pg_dump"],
            Self::PgDumpall => &["pg_dumpall"],
            Self::PgRestore => &["pg_restore"],
            Self::Psql => &["psql"],
            Self::MysqlDump => &["mysqldump", "mariadb-dump"],
            Self::Mysql => &["mysql", "mariadb"],
        }
    }

    /// The name messages use: the first binary name.
    pub fn name(self) -> &'static str {
        self.names()[0]
    }

    /// The engines whose client ships the tool.
    pub fn family(self) -> ClientFamily {
        match self {
            Self::PgDump | Self::PgDumpall | Self::PgRestore | Self::Psql => ClientFamily::Postgres,
            Self::MysqlDump | Self::Mysql => ClientFamily::MySql,
        }
    }
}

/// The client tools of one kind of engine, installed together in one
/// folder (DBeaver's "local client", `DBPNativeClientLocation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientFamily {
    /// `pg_dump`, `pg_dumpall`, `pg_restore`, `psql`.
    Postgres,
    /// `mysqldump`, `mysql` and MariaDB's `mariadb-dump`, `mariadb`.
    MySql,
}

impl ClientFamily {
    /// Both families, in the order the settings list them.
    pub const ALL: [ClientFamily; 2] = [Self::Postgres, Self::MySql];

    /// The family whose tools reach `engine`; `None` for SQLite, whose
    /// files Kerno opens itself.
    pub fn of(engine: Engine) -> Option<Self> {
        match engine {
            Engine::Postgres => Some(Self::Postgres),
            Engine::MySql | Engine::MariaDb => Some(Self::MySql),
            Engine::Sqlite => None,
        }
    }

    /// [`ClientFamily::of`], with the refusal in words for an engine
    /// without client tools.
    pub fn required(engine: Engine) -> Result<Self> {
        Self::of(engine).ok_or_else(|| Error::Unsupported(NO_CLIENT_TOOLS.into()))
    }

    /// Its tools; the first one present in a folder identifies the install
    /// and gives its version.
    pub fn tools(self) -> &'static [Tool] {
        match self {
            Self::Postgres => &[Tool::PgDump, Tool::Psql, Tool::PgRestore, Tool::PgDumpall],
            Self::MySql => &[Tool::MysqlDump, Tool::Mysql],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::MySql => "MySQL and MariaDB",
        }
    }

    /// How the client is usually installed, for a message that says none
    /// was found.
    pub fn install_hint(self) -> &'static str {
        match self {
            Self::Postgres if cfg!(windows) => {
                "with PostgreSQL's installer for Windows (EDB's), which puts them in Program \
                 Files"
            }
            Self::MySql if cfg!(windows) => {
                "with the MySQL Server or MariaDB installer for Windows, which puts them in \
                 Program Files"
            }
            Self::Postgres => {
                "with “brew install libpq” on macOS, or the postgresql-client package of the \
                 Linux distribution"
            }
            Self::MySql => {
                "with “brew install mysql-client” (or “mariadb”) on macOS, or the \
                 default-mysql-client or mariadb-client package of the Linux distribution"
            }
        }
    }
}

/// Where an install was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientSource {
    /// A folder the user added (or chose for a connection).
    Added,
    /// A Homebrew formula (`Cellar/<formula>/<version>/bin`).
    Homebrew,
    /// A version of Postgres.app.
    PostgresApp,
    /// An EnterpriseDB installer (`/Library/PostgreSQL/<n>/bin`).
    Edb,
    /// A distribution package (`/usr/lib/postgresql/<n>/bin`).
    Linux,
    /// Windows: the MySQL or MariaDB installer's folder in `%ProgramFiles%`.
    ProgramFiles,
    /// A folder of `$PATH` or of [`FALLBACK_DIRS`].
    Path,
}

impl ClientSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Homebrew => "Homebrew",
            Self::PostgresApp => "Postgres.app",
            Self::Edb => "EDB installer",
            Self::Linux => "system package",
            Self::ProgramFiles => "Program Files",
            Self::Path => "PATH",
        }
    }
}

/// One installed client: a folder holding the tools of a family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInstall {
    pub family: ClientFamily,
    /// The folder, links resolved: the real folder of its first tool.
    pub dir: PathBuf,
    pub source: ClientSource,
    /// The version of its first tool; `None` when it does not run.
    pub version: Option<ToolVersion>,
    /// Each tool present, with its binary in `dir`.
    pub tools: Vec<(Tool, PathBuf)>,
}

impl ClientInstall {
    /// The binary of `tool`, when this install has it.
    pub fn path(&self, tool: Tool) -> Option<&Path> {
        self.tools
            .iter()
            .find(|(present, _)| *present == tool)
            .map(|(_, path)| path.as_path())
    }

    /// `PostgreSQL 18.6 · Homebrew` or `MariaDB 11.8.3 · PATH`.
    pub fn label(&self) -> String {
        let product = match (&self.version, self.family) {
            (Some(version), _) if version.mariadb => "MariaDB",
            (_, ClientFamily::Postgres) => "PostgreSQL",
            (_, ClientFamily::MySql) => "MySQL",
        };
        let version = self.version.as_ref().map_or("version unknown".into(), |v| {
            let (major, minor, patch) = v.number;
            if self.family == ClientFamily::Postgres {
                format!("{major}.{minor}")
            } else {
                format!("{major}.{minor}.{patch}")
            }
        });
        format!("{product} {version} · {}", self.source.label())
    }
}

/// Where [`detect_clients`] looks, injectable for tests.
#[derive(Debug, Clone, Default)]
pub struct ClientRoots {
    /// `$PATH`.
    pub path_var: Option<std::ffi::OsString>,
    /// Homebrew prefixes (`/opt/homebrew`, `/usr/local`): their `Cellar`.
    pub homebrew: Vec<PathBuf>,
    /// `/Applications/Postgres.app/Contents/Versions`.
    pub postgres_app: Vec<PathBuf>,
    /// `/Library/PostgreSQL`.
    pub edb: Vec<PathBuf>,
    /// `/usr/lib/postgresql`.
    pub linux: Vec<PathBuf>,
    /// Windows' `%ProgramFiles%`: the MySQL installer's `MySQL\<product>\bin` and MariaDB's
    /// `MariaDB <X.Y>\bin` (PostgreSQL's installer, EDB's, is in [`ClientRoots::edb`]).
    pub program_files: Vec<PathBuf>,
    /// Folders looked at after `$PATH` ([`FALLBACK_DIRS`]).
    pub fallbacks: Vec<PathBuf>,
    /// Folders the user added or chose, looked at first.
    pub added: Vec<PathBuf>,
}

impl ClientRoots {
    /// The usual places on this computer, plus `added`.
    #[cfg(not(windows))]
    pub fn system(added: Vec<PathBuf>) -> Self {
        let paths = |list: &[&str]| list.iter().map(PathBuf::from).collect();
        Self {
            path_var: std::env::var_os("PATH"),
            homebrew: paths(&["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"]),
            postgres_app: paths(&["/Applications/Postgres.app/Contents/Versions"]),
            edb: paths(&["/Library/PostgreSQL"]),
            linux: paths(&["/usr/lib/postgresql"]),
            program_files: Vec::new(),
            fallbacks: paths(FALLBACK_DIRS),
            added,
        }
    }

    /// The usual places on this computer, plus `added`: on Windows the installers' folders in
    /// `%ProgramFiles%` (`PostgreSQL\<n>\bin`, `MySQL\<product>\bin`, `MariaDB <X.Y>\bin`) and
    /// `PATH`, versions read from the folders, never assumed.
    #[cfg(windows)]
    pub fn system(added: Vec<PathBuf>) -> Self {
        let program_files: Vec<PathBuf> = std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .into_iter()
            .collect();
        Self {
            path_var: std::env::var_os("PATH"),
            edb: program_files
                .iter()
                .map(|dir| dir.join("PostgreSQL"))
                .collect(),
            program_files,
            added,
            ..Self::default()
        }
    }
}

/// The Homebrew formulas that hold client tools, by name or prefix.
const HOMEBREW_FORMULAS: [&str; 5] = ["libpq", "postgresql", "mysql-client", "mysql", "mariadb"];

/// Every client installed (DBeaver's `findLocalClients` of
/// `PostgreDataSourceProvider` and `MySQLDataSourceProvider`, which look in
/// the PostgreSQL and Postgres.app folders and in `$PATH`), widened to each
/// Homebrew Cellar version: one entry per real folder (links resolved, a
/// folder reached twice kept once, where it is first found), each with the
/// version of its first tool. Sorted by family, newest version first, then
/// by folder. Blocking: it reads the file system and runs `--version`.
pub fn detect_clients(roots: &ClientRoots) -> Vec<ClientInstall> {
    let mut candidates: Vec<(PathBuf, ClientSource)> = Vec::new();
    let mut push_all = |dirs: Vec<PathBuf>, source| {
        candidates.extend(dirs.into_iter().map(|dir| (dir, source)));
    };
    push_all(roots.added.clone(), ClientSource::Added);
    for prefix in &roots.homebrew {
        let formulas = subdirs(&prefix.join("Cellar"))
            .into_iter()
            .filter(|formula| {
                formula.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    HOMEBREW_FORMULAS.iter().any(|known| {
                        name == *known
                            || name
                                .strip_prefix(known)
                                .is_some_and(|rest| rest.starts_with('@'))
                    })
                })
            });
        let versions = formulas.flat_map(|formula| subdirs(&formula));
        push_all(
            versions.map(|version| version.join("bin")).collect(),
            ClientSource::Homebrew,
        );
    }
    for (roots, source) in [
        (&roots.postgres_app, ClientSource::PostgresApp),
        (&roots.edb, ClientSource::Edb),
        (&roots.linux, ClientSource::Linux),
    ] {
        for root in roots {
            push_all(
                subdirs(root)
                    .into_iter()
                    .map(|dir| dir.join("bin"))
                    .collect(),
                source,
            );
        }
    }
    for root in &roots.program_files {
        let mariadb = subdirs(root).into_iter().filter(|dir| {
            dir.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("MariaDB"))
        });
        push_all(
            subdirs(&root.join("MySQL"))
                .into_iter()
                .chain(mariadb)
                .map(|dir| dir.join("bin"))
                .collect(),
            ClientSource::ProgramFiles,
        );
    }
    let path = roots
        .path_var
        .as_deref()
        .into_iter()
        .flat_map(std::env::split_paths)
        .chain(roots.fallbacks.iter().cloned())
        // A relative entry (`.`) would run a tool of the current folder.
        .filter(|dir| dir.is_absolute())
        .collect();
    push_all(path, ClientSource::Path);

    let mut seen = std::collections::HashSet::new();
    let mut found = Vec::new();
    for (dir, source) in candidates {
        for family in ClientFamily::ALL {
            let Some(key) = family.tools().iter().find_map(|tool| tool_in(&dir, *tool)) else {
                continue;
            };
            // The real folder of the first tool: `/opt/homebrew/bin/pg_dump`
            // is a link into `Cellar/postgresql@14/…/bin`.
            let real = real_path(&key);
            let Some(real_dir) = real.parent().map(Path::to_path_buf) else {
                continue;
            };
            if !seen.insert((family, real_dir.clone())) {
                continue;
            }
            let tools: Vec<(Tool, PathBuf)> = family
                .tools()
                .iter()
                .filter_map(|tool| tool_in(&real_dir, *tool).map(|path| (*tool, path)))
                .collect();
            let Some((_, first)) = tools.first() else {
                continue;
            };
            found.push(ClientInstall {
                family,
                version: version(first),
                dir: real_dir,
                source,
                tools,
            });
        }
    }
    found.sort_by(|a, b| {
        let number = |install: &ClientInstall| install.version.as_ref().map(|v| v.number);
        a.family
            .cmp(&b.family)
            .then_with(|| number(b).cmp(&number(a)))
            .then_with(|| a.dir.cmp(&b.dir))
    });
    found
}

/// The folders inside `dir`, sorted; none when it cannot be read.
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The binary of `tool` in `dir`, by the first of its names present.
fn tool_in(dir: &Path, tool: Tool) -> Option<PathBuf> {
    tool.names()
        .iter()
        .map(|name| binary(dir, name))
        .find(|path| is_executable(path))
}

/// The file of the program `name` in `dir`: `pg_dump`, `pg_dump.exe` on Windows.
fn binary(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

/// `path` with its links resolved, itself when that fails. On Windows without the `\\?\` that
/// `canonicalize` puts before a drive letter: the folder is shown to the user, and compared with
/// folders typed or saved without it.
pub fn real_path(path: &Path) -> PathBuf {
    plain_path(&std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// `path` without the `\\?\` before a drive letter (Windows; itself elsewhere). A tool given
/// `\\?\C:\out\k48` for a folder writes `\\?\C:\out\k48/toc.dat`, which Windows refuses:
/// that form takes no `/` (pg_dump's directory format, K68).
pub fn plain_path(path: &Path) -> PathBuf {
    path.to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
        .filter(|rest| cfg!(windows) && !rest.starts_with(r"UNC\"))
        .map_or_else(|| path.to_path_buf(), PathBuf::from)
}

/// Probes one folder chosen by the user: the install it holds for
/// `family`, or `None` when it holds none of the family's tools. Blocking.
pub fn probe_client(dir: &Path, family: ClientFamily) -> Option<ClientInstall> {
    let roots = ClientRoots {
        added: vec![dir.to_path_buf()],
        ..ClientRoots::default()
    };
    detect_clients(&roots)
        .into_iter()
        .find(|install| install.family == family)
}

/// Why an engine has no client tools in Kerno.
pub const NO_CLIENT_TOOLS: &str = "SQLite has no client tools in Kerno: Kerno opens the file \
     itself, and backing up or restoring a database file is still to come";

/// Why no client can be used for a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientMissing {
    /// `None`: the engine has no client tools at all (SQLite).
    pub family: Option<ClientFamily>,
}

impl ClientMissing {
    /// What happened and what to do, in words.
    pub fn text(&self) -> String {
        let Some(family) = self.family else {
            return format!("{NO_CLIENT_TOOLS}.");
        };
        if cfg!(windows) {
            return format!(
                "No {} client was found: Kerno looked in Program Files (the PostgreSQL, MySQL \
                 and MariaDB installers' folders), PATH and the folders added in Settings ▸ \
                 Local clients. It is usually installed {}. Then add its folder in Settings ▸ \
                 Local clients.",
                family.label(),
                family.install_hint()
            );
        }
        format!(
            "No {} client was found: Kerno looked in Homebrew, Postgres.app, the EDB and Linux \
             package folders, $PATH, {} and the folders added in Settings ▸ Local clients. It is \
             usually installed {}. Then add its folder in Settings ▸ Local clients.",
            family.label(),
            FALLBACK_DIRS.join(", "),
            family.install_hint()
        )
    }
}

/// What a connection asks of [`client_for`].
#[derive(Debug, Clone, Copy)]
pub struct ClientWanted<'a> {
    pub engine: Engine,
    /// The connection's own choice (Advanced tab, or the wizard's "Local
    /// client…").
    pub connection_dir: Option<&'a Path>,
    /// The default of the family (Settings ▸ Local clients).
    pub default_dir: Option<&'a Path>,
}

/// The client [`client_for`] chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientPick<'a> {
    pub install: &'a ClientInstall,
    /// A folder asked for (by the connection, or as the default) that is
    /// neither a detected install nor a folder the user added: never run,
    /// the next choice was taken instead.
    pub ignored: Option<PathBuf>,
}

/// The client a connection uses: its own folder, else the family's
/// default, else the newest install found (of the server's own flavour
/// first: MariaDB's client for a MariaDB server).
///
/// A folder is used only when it is one of `detected` ([`detect_clients`],
/// whose [`ClientRoots::added`] are the folders the user added in Settings
/// ▸ Local clients or with the wizard's Browse…): a folder stored with a
/// connection or in saved settings could otherwise run programs someone
/// else chose. One that is not is reported in [`ClientPick::ignored`].
pub fn client_for<'a>(
    detected: &'a [ClientInstall],
    wanted: &ClientWanted,
) -> std::result::Result<ClientPick<'a>, ClientMissing> {
    let Some(family) = ClientFamily::of(wanted.engine) else {
        return Err(ClientMissing { family: None });
    };
    let in_family = || {
        detected
            .iter()
            .filter(move |install| install.family == family)
    };
    let mut ignored = None;
    for dir in [wanted.connection_dir, wanted.default_dir]
        .into_iter()
        .flatten()
    {
        let real = real_path(dir);
        match in_family().find(|install| install.dir == real || install.dir == dir) {
            Some(install) => return Ok(ClientPick { install, ignored }),
            None => {
                ignored.get_or_insert_with(|| dir.to_path_buf());
            }
        }
    }
    let mariadb = wanted.engine == Engine::MariaDb;
    in_family()
        .max_by(|a, b| {
            let rank = |install: &ClientInstall| {
                (
                    install
                        .version
                        .as_ref()
                        .is_some_and(|version| version.mariadb == mariadb),
                    install.version.as_ref().map(|version| version.number),
                )
            };
            // Ties keep the first (detection order): `max_by` keeps the last.
            rank(a).cmp(&rank(b)).then(std::cmp::Ordering::Greater)
        })
        .map(|install| ClientPick { install, ignored })
        .ok_or(ClientMissing {
            family: Some(family),
        })
}

/// Whether a client suits a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compatibility {
    Ok,
    /// PostgreSQL: `pg_dump` and `pg_dumpall` refuse a server of a newer
    /// major version than theirs.
    ClientOlder {
        client_major: u32,
        server_major: u32,
    },
    /// MySQL / MariaDB: another major version, or the other product's
    /// client; it usually works, so it is only a warning.
    OtherVersion {
        client_major: u32,
        server_major: u32,
    },
    /// The client's version could not be read.
    Unknown,
}

impl Compatibility {
    /// Whether the tool would refuse to run.
    pub fn blocks(self) -> bool {
        matches!(self, Self::ClientOlder { .. })
    }

    /// The warning to show before running, if any.
    pub fn warning(self, engine: Engine) -> Option<String> {
        match self {
            Self::Ok => None,
            Self::ClientOlder {
                client_major,
                server_major,
            } => Some(format!(
                "This client is PostgreSQL {client_major}, older than the server \
                 (PostgreSQL {server_major}): pg_dump refuses a server newer than itself. \
                 Choose a client {server_major} or newer with “Local client…”."
            )),
            Self::OtherVersion {
                client_major,
                server_major,
            } => Some(format!(
                "This client is version {client_major} and the server {} {server_major}: it \
                 usually works; if the tool fails, choose a client of the server's version with \
                 “Local client…”.",
                engine.label()
            )),
            Self::Unknown => Some(
                "The client's version could not be read: it may not run. Choose another with \
                 “Local client…”."
                    .into(),
            ),
        }
    }
}

/// Whether `client` suits a server of `engine` at `server_version`.
pub fn compatibility(
    client: Option<&ToolVersion>,
    engine: Engine,
    server_version: (u32, u32, u32),
) -> Compatibility {
    let Some(client) = client else {
        return Compatibility::Unknown;
    };
    let (client_major, server_major) = (client.number.0, server_version.0);
    match engine {
        Engine::Postgres if client_major < server_major => Compatibility::ClientOlder {
            client_major,
            server_major,
        },
        Engine::Postgres => Compatibility::Ok,
        Engine::MySql | Engine::MariaDb => {
            let same_product = client.mariadb == (engine == Engine::MariaDb);
            if same_product && client_major == server_major {
                Compatibility::Ok
            } else {
                Compatibility::OtherVersion {
                    client_major,
                    server_major,
                }
            }
        }
        // No client tools: no version to compare.
        Engine::Sqlite => Compatibility::Unknown,
    }
}

/// Where the tools usually are when `$PATH` does not have them (a GUI app on
/// macOS gets a short `$PATH`). None on Windows, whose installers' folders are
/// [`ClientRoots::system`]'s.
#[cfg(not(windows))]
pub const FALLBACK_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/opt/homebrew/opt/libpq/bin",
    "/opt/homebrew/opt/mysql-client/bin",
    "/opt/homebrew/opt/mariadb/bin",
    "/Applications/Postgres.app/Contents/Versions/latest/bin",
    "/usr/bin",
];

/// Where the tools usually are when `$PATH` does not have them (a GUI app on
/// macOS gets a short `$PATH`). None on Windows, whose installers' folders are
/// [`ClientRoots::system`]'s.
#[cfg(windows)]
pub const FALLBACK_DIRS: &[&str] = &[];

/// The installed binary of `tool`: the first match in `$PATH`, then in the
/// usual install directories.
pub fn find(tool: Tool) -> Option<PathBuf> {
    let fallbacks: Vec<PathBuf> = FALLBACK_DIRS.iter().map(PathBuf::from).collect();
    find_in(tool, std::env::var_os("PATH").as_deref(), &fallbacks)
}

/// [`find`] with the search path given: the directories of `path_var`, then
/// `fallbacks`. In each directory every name of the tool is tried before the
/// next directory.
pub fn find_in(tool: Tool, path_var: Option<&OsStr>, fallbacks: &[PathBuf]) -> Option<PathBuf> {
    find_all_in(tool, path_var, fallbacks).into_iter().next()
}

/// Every installed binary of `tool`, in the order [`find`] looks: several
/// versions can be installed side by side (`/opt/homebrew/bin/pg_dump` 14
/// and `/opt/homebrew/opt/libpq/bin/pg_dump` 18), and the first one may be
/// too old for the server. Blocking (it reads the file system).
pub fn find_all(tool: Tool) -> Vec<PathBuf> {
    let fallbacks: Vec<PathBuf> = FALLBACK_DIRS.iter().map(PathBuf::from).collect();
    find_all_in(tool, std::env::var_os("PATH").as_deref(), &fallbacks)
}

/// [`find_all`] with the search path given, as [`find_in`]. A binary
/// reached twice (a directory listed twice, a link to another match) is
/// kept once, where it is first found.
pub fn find_all_in(tool: Tool, path_var: Option<&OsStr>, fallbacks: &[PathBuf]) -> Vec<PathBuf> {
    let from_path = path_var.into_iter().flat_map(std::env::split_paths);
    let mut seen = std::collections::HashSet::new();
    from_path
        .chain(fallbacks.iter().cloned())
        // A relative entry (`.`) would run a tool of the current folder.
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| tool.names().iter().map(move |name| binary(&dir, name)))
        .filter(|candidate| is_executable(candidate))
        .filter(|candidate| {
            seen.insert(std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.clone()))
        })
        .collect()
}

/// Whether `path` is a file this system would run: on Unix, one with an
/// execute bit. Windows has no such bit (what runs is decided by the file's
/// extension): any file.
#[cfg(unix)]
pub(crate) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Whether `path` is a file this system would run: on Unix, one with an
/// execute bit. Windows has no such bit (what runs is decided by the file's
/// extension): any file.
#[cfg(not(unix))]
pub(crate) fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// A tool's version, as `--version` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolVersion {
    /// The first line of the output, to show the user.
    pub text: String,
    /// Major, minor and patch (0 when absent).
    pub number: (u32, u32, u32),
    /// Whether the tool is MariaDB's.
    pub mariadb: bool,
}

/// The version of the tool at `path`, by running `path --version`
/// (blocking: call it off the UI thread). `None` when it does not run or
/// prints no version.
pub fn version(path: &Path) -> Option<ToolVersion> {
    let mut command = std::process::Command::new(path);
    command.arg("--version").stdin(Stdio::null());
    no_console(&mut command);
    let output = command.output().ok()?;
    parse_version(&String::from_utf8_lossy(&output.stdout))
}

/// Reads `psql (PostgreSQL) 18.6`, `mysql  Ver 8.0.36 for Linux …`,
/// `mysql  Ver 15.1 Distrib 10.6.18-MariaDB, …` (where 15.1 is the client
/// protocol, not the version) and `mariadb from 11.8.9-MariaDB, client 15.2 …`.
/// MySQL's tools start with their own path, which on Windows holds spaces and
/// a version of its own (`C:\Program Files\MySQL\MySQL Server 8.4\bin\mysql.exe
/// Ver 8.4.13 …`): the word after `Ver` is the version.
fn parse_version(output: &str) -> Option<ToolVersion> {
    let text = output.lines().next()?.trim();
    let words: Vec<&str> = text.split_whitespace().collect();
    let after = |markers: &[&str]| {
        words
            .iter()
            .position(|word| markers.contains(word))
            .and_then(|at| words.get(at + 1))
    };
    let word = after(&["Distrib", "from"])
        .or_else(|| after(&["Ver"]))
        .or_else(|| {
            words
                .iter()
                .skip(1)
                .find(|word| word.starts_with(|c: char| c.is_ascii_digit()))
        })?;
    let digits = word
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .map_or(*word, |end| &word[..end]);
    let mut parts = digits.split('.').map(|part| part.parse::<u32>().ok());
    let major = parts.next().flatten()?;
    let mut next = || parts.next().flatten().unwrap_or(0);
    Some(ToolVersion {
        text: text.to_string(),
        number: (major, next(), next()),
        mariadb: text.contains("MariaDB"),
    })
}

/// The options that turn the client commands of a MySQL / MariaDB client
/// off, when its version has them (see the module documentation).
fn sandbox_option(version: &ToolVersion) -> Option<&'static [&'static str]> {
    let (major, minor, patch) = version.number;
    let since = if version.mariadb {
        match (major, minor) {
            (10, 6) => 18,
            (10, 11) => 8,
            (11, 0) => 6,
            (11, 1) => 5,
            (11, 2) => 4,
            (11, 4) => 2,
            (11, 5..) | (12.., _) => 0,
            _ => return None,
        }
    } else {
        match (major, minor) {
            (8, 0) => 43,
            (8, 4) => 6,
            (9, 4..) | (10.., _) => 0,
            _ => return None,
        }
    };
    // MariaDB's sandbox leaves `connect` (`\r db host`) on, which would
    // send the password to a host the file names: binary mode turns every
    // command off but `\C` and `delimiter`.
    (patch >= since).then_some(if version.mariadb {
        &["--sandbox", "--binary-mode"]
    } else {
        &["--commands=OFF"]
    })
}

/// A route through an SSH tunnel. The tunnel's local port admits a single
/// connection for a few seconds ([`Tunnel::forward_once`]), so it is opened
/// by [`run`] right before the tool starts, and the argument at `port_arg`
/// gets its port.
#[derive(Debug, Clone)]
pub struct TunnelHop {
    /// The tunnel.
    pub tunnel: Arc<Tunnel>,
    /// The database host as the SSH server resolves it.
    pub host: String,
    /// The database port.
    pub port: u16,
    /// Index in [`ToolCommand::args`] of the `--port=` argument.
    pub port_arg: usize,
}

/// A tool invocation, ready to [`run`]. Its `Debug` output and
/// [`ToolCommand::display`] never contain the password.
#[derive(Debug, Clone)]
pub struct ToolCommand {
    /// The binary.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// Environment variables set for the child. Nothing secret here.
    pub env: Vec<(String, String)>,
    /// The password and the variable that carries it to the child.
    pub password: Option<(&'static str, Secret)>,
    /// A file fed to the child's standard input.
    pub stdin: Option<PathBuf>,
    /// Parts of files and texts fed to the child's standard input after
    /// `stdin`, in this order: the definitions or the rows of a folder
    /// backup's files (K74).
    pub stdin_parts: Vec<StdinPart>,
    /// How many bytes of `stdin` [`run`] has fed to the child so far: the
    /// progress of a script, against the file's size.
    pub sent: Arc<AtomicU64>,
    /// The SSH tunnel to open before starting the child.
    pub tunnel: Option<TunnelHop>,
    /// A script the child will read, which [`run`] checks with
    /// [`check_script`] before starting it.
    pub script_check: Option<ScriptCheck>,
    /// A task's files the child reads or writes (its output, its input):
    /// on Windows [`run`] refuses one that is not a file of this computer
    /// before anything opens it (K69, D-138).
    pub files: Vec<PathBuf>,
}

/// A part of what [`run`] feeds a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinPart {
    /// The bytes `range` of the file at `path`, opened when the feed
    /// reaches it; `counted` in [`ToolCommand::sent`] or not (a part fed
    /// twice is counted once).
    File {
        path: PathBuf,
        range: std::ops::Range<u64>,
        counted: bool,
    },
    /// A text of Kerno's own, never counted.
    Text(String),
}

/// A script to check for client commands, see [`check_script`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptCheck {
    /// [`Tool::Psql`], or [`Tool::Mysql`] for MySQL and MariaDB: whose
    /// client commands to look for.
    pub tool: Tool,
    /// The script.
    pub path: PathBuf,
    /// The version of the tool that will read it, from [`version`]; `None`
    /// when unknown, which applies the strictest rule.
    pub version: Option<ToolVersion>,
}

impl ToolCommand {
    /// The command as shown to the user, shell-quoted: the environment it
    /// sets, the password as `•••`, and `< file` for the standard input.
    pub fn display(&self) -> String {
        let env = self
            .env
            .iter()
            .map(|(name, value)| format!("{name}={}", quote(value)));
        let password = self.password.iter().map(|(name, _)| format!("{name}=•••"));
        let args = self
            .args
            .iter()
            .enumerate()
            .map(|(index, arg)| match &self.tunnel {
                Some(hop) if hop.port_arg == index => "--port=<SSH tunnel>".to_string(),
                _ => quote(arg),
            });
        let stdin = self
            .stdin
            .iter()
            .map(|path| format!("< {}", quote(&path.to_string_lossy())));
        env.chain(password)
            .chain([quote(&self.program.to_string_lossy())])
            .chain(args)
            .chain(stdin)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Single-quotes `text` for a POSIX shell when it holds anything but plain
/// characters.
fn quote(text: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-./=:,@%+".contains(c);
    if !text.is_empty() && text.chars().all(plain) {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// The tool that writes a backup of `engine`; `None` without client tools.
pub fn backup_tool(engine: Engine) -> Option<Tool> {
    match engine {
        Engine::Postgres => Some(Tool::PgDump),
        Engine::MySql | Engine::MariaDb => Some(Tool::MysqlDump),
        Engine::Sqlite => None,
    }
}

/// The tool that restores a backup of `engine` written in `format`; `None`
/// without client tools.
pub fn restore_tool(engine: Engine, format: BackupFormat) -> Option<Tool> {
    match (engine, format) {
        (Engine::Postgres, BackupFormat::Sql) => Some(Tool::Psql),
        (Engine::Postgres, _) => Some(Tool::PgRestore),
        (Engine::MySql | Engine::MariaDb, _) => Some(Tool::Mysql),
        (Engine::Sqlite, _) => None,
    }
}

/// The tool that runs a SQL script on `engine`; `None` without client tools.
pub fn script_tool(engine: Engine) -> Option<Tool> {
    restore_tool(engine, BackupFormat::Sql)
}

/// How a backup is written (DBeaver's `PostgreBackupRestoreSettings.ExportFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupFormat {
    /// A plain SQL script (every engine), run by `psql` / `mysql`.
    #[default]
    Sql,
    /// PostgreSQL's compressed custom archive (`pg_dump -Fc`).
    Custom,
    /// PostgreSQL's directory archive (`-Fd`), one file per table; the
    /// only one `pg_dump --jobs` writes.
    Directory,
    /// PostgreSQL's tar archive (`-Ft`).
    Tar,
}

impl BackupFormat {
    /// PostgreSQL's formats, in DBeaver's order.
    pub const POSTGRES: [BackupFormat; 4] = [Self::Sql, Self::Custom, Self::Directory, Self::Tar];

    /// `pg_dump --format`'s letter.
    fn letter(self) -> &'static str {
        match self {
            Self::Sql => "p",
            Self::Custom => "c",
            Self::Directory => "d",
            Self::Tar => "t",
        }
    }

    /// The extension of the file written; none for a directory.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Sql => "sql",
            Self::Custom => "dump",
            Self::Directory => "",
            Self::Tar => "tar",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Sql => "Plain SQL",
            Self::Custom => "Custom",
            Self::Directory => "Directory",
            Self::Tar => "Tar",
        }
    }
}

/// What a backup contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupContent {
    /// Definitions and data.
    #[default]
    All,
    /// Definitions only.
    SchemaOnly,
    /// Data only.
    DataOnly,
}

/// `pg_dump`'s own options (DBeaver's `PostgreDatabaseBackupSettings`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PgBackup {
    /// `--compress=0..9`; the format's default when `None`.
    pub compression: Option<u8>,
    /// `--encoding`; the server's when `None`.
    pub encoding: Option<String>,
    /// `--inserts`: INSERT statements instead of COPY.
    pub inserts: bool,
    /// `--no-privileges`.
    pub no_privileges: bool,
    /// `--no-owner`.
    pub no_owner: bool,
    /// `--clean`: DROP statements before each CREATE.
    pub clean: bool,
    /// `--if-exists` (with `clean`).
    pub if_exists: bool,
    /// `--create`: CREATE DATABASE and a reconnect to it.
    pub create: bool,
    /// `--jobs`: parallel dump, directory format only.
    pub jobs: Option<u32>,
}

/// How `mysqldump` keeps the dump consistent (DBeaver's `DumpMethod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DumpMethod {
    /// `--single-transaction`: a consistent snapshot of InnoDB tables, no
    /// lock.
    #[default]
    SingleTransaction,
    /// `--lock-all-tables`: a global read lock for the whole dump.
    LockAllTables,
    /// `--skip-lock-tables`: no lock, no snapshot.
    NoLocks,
}

impl DumpMethod {
    pub const ALL: [DumpMethod; 3] = [Self::SingleTransaction, Self::LockAllTables, Self::NoLocks];

    fn option(self) -> &'static str {
        match self {
            Self::SingleTransaction => "--single-transaction",
            Self::LockAllTables => "--lock-all-tables",
            Self::NoLocks => "--skip-lock-tables",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SingleTransaction => "Single transaction",
            Self::LockAllTables => "Lock all tables",
            Self::NoLocks => "No locks",
        }
    }
}

/// `mysqldump`'s own options (DBeaver's `MySQLExportSettings`), at the
/// tool's defaults except `routines` and `hex_blob`, on as Kerno always
/// had them (a binary column dumped raw can spell a client command that
/// the restore check then refuses).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MySqlBackup {
    pub method: DumpMethod,
    /// `--routines`: procedures and functions.
    pub routines: bool,
    /// `--events`.
    pub events: bool,
    /// `--skip-triggers` when off.
    pub triggers: bool,
    /// `--skip-extended-insert` when off: one INSERT per row.
    pub extended_inserts: bool,
    /// `--skip-add-drop-table` when off.
    pub add_drop_table: bool,
    /// `--skip-disable-keys` when off.
    pub disable_keys: bool,
    /// `--hex-blob`.
    pub hex_blob: bool,
    /// `--skip-comments` when off.
    pub comments: bool,
    /// The folder backup's mysqldump processes at once, with
    /// [`BackupFormat::Directory`] ([`mysql_folder`], K63).
    pub jobs: Option<u32>,
}

impl Default for MySqlBackup {
    fn default() -> Self {
        Self {
            method: DumpMethod::default(),
            routines: true,
            events: false,
            triggers: true,
            extended_inserts: true,
            add_drop_table: true,
            disable_keys: true,
            hex_blob: true,
            comments: true,
            jobs: None,
        }
    }
}

/// The options of the engine's dump tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "engine", rename_all = "lowercase")]
pub enum EngineBackup {
    Postgres(PgBackup),
    #[serde(rename = "mysql")]
    MySql(MySqlBackup),
}

/// The default name of a backup file, without its extension (DBeaver's
/// `dump-${database}-${timestamp}`).
pub const DEFAULT_FILE_PATTERN: &str = "${database}-${timestamp}";

/// A backup of one database: the settings of the backup wizard, which a
/// saved task keeps. The file is `output_folder` / `file_pattern` with its
/// variables filled ([`file_name`]) and the format's extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupOptions {
    /// The database to back up.
    pub database: String,
    /// The format (MySQL and MariaDB: SQL only).
    #[serde(default)]
    pub format: BackupFormat,
    /// Definitions, data, or both.
    #[serde(default)]
    pub content: BackupContent,
    /// PostgreSQL: only these schemas (`--schema`). Empty with `tables`
    /// empty: the whole database.
    #[serde(default)]
    pub schemas: Vec<String>,
    /// Only these tables (`--table`, or after the database on MySQL, where
    /// they must be in `database`). pg_dump ignores `--schema` once a
    /// `--table` is given: both at once are refused.
    #[serde(default)]
    pub tables: Vec<TableRef>,
    /// The dump tool's own options.
    pub engine: EngineBackup,
    /// The folder the file is written to.
    #[serde(default)]
    pub output_folder: PathBuf,
    /// The file name, with variables ([`file_name`]).
    #[serde(default = "default_pattern")]
    pub file_pattern: String,
    /// More arguments, split as a shell would ([`extra_args`]).
    #[serde(default)]
    pub extra_args: String,
    /// Another user for this tool (DBeaver's "override credentials"); its
    /// password is asked at each run and never kept here.
    #[serde(default)]
    pub auth_user: Option<String>,
    /// MySQL / MariaDB: `--default-character-set=utf8mb4`, from
    /// [`utf8mb4`] (the server and the client have it); otherwise a client
    /// that defaults to `utf8` (mysqldump 5.7) writes a 4-byte character as
    /// `?`. Set at each run, never kept.
    #[serde(skip)]
    pub utf8mb4: bool,
}

fn default_pattern() -> String {
    DEFAULT_FILE_PATTERN.into()
}

impl BackupOptions {
    /// A whole-database backup at the defaults of `engine`'s tool (DBeaver
    /// starts PostgreSQL backups in the custom format).
    pub fn new(engine: Engine, database: impl Into<String>) -> Self {
        let (format, tool) = match engine {
            Engine::Postgres => (
                BackupFormat::Custom,
                EngineBackup::Postgres(PgBackup::default()),
            ),
            Engine::MySql | Engine::MariaDb => (
                BackupFormat::Sql,
                EngineBackup::MySql(MySqlBackup::default()),
            ),
            // No dump tool: `backup_command` refuses SQLite whatever the
            // options say, and no wizard opens for it. A plain SQL file is
            // what a SQLite backup will be once it comes (`.dump`).
            Engine::Sqlite => (
                BackupFormat::Sql,
                EngineBackup::MySql(MySqlBackup::default()),
            ),
        };
        Self {
            database: database.into(),
            format,
            content: BackupContent::All,
            schemas: Vec::new(),
            tables: Vec::new(),
            engine: tool,
            output_folder: PathBuf::new(),
            file_pattern: default_pattern(),
            extra_args: String::new(),
            auth_user: None,
            utf8mb4: false,
        }
    }
}

/// `pg_dumpall`'s options (DBeaver's `PostgreBackupAllSettings`): the
/// global backup of a whole server. It only reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalBackupOptions {
    /// `--encoding`; the server's when `None`.
    pub encoding: Option<String>,
    /// `--schema-only`.
    pub schema_only: bool,
    /// `--globals-only`: roles and tablespaces, no database.
    pub globals_only: bool,
    /// `--roles-only`.
    pub roles_only: bool,
    /// `--tablespaces-only`.
    pub tablespaces_only: bool,
    /// `--no-privileges`.
    pub no_privileges: bool,
    /// `--no-owner`.
    pub no_owner: bool,
    /// Off: `--no-role-passwords` (DBeaver leaves the passwords out by
    /// default).
    pub role_passwords: bool,
    /// `--exclude-database`, each name matched literally.
    pub exclude_databases: Vec<String>,
    pub output_folder: PathBuf,
    pub file_pattern: String,
    pub extra_args: String,
    pub auth_user: Option<String>,
}

impl Default for GlobalBackupOptions {
    fn default() -> Self {
        Self {
            encoding: None,
            schema_only: false,
            globals_only: false,
            roles_only: false,
            tablespaces_only: false,
            no_privileges: false,
            no_owner: false,
            role_passwords: false,
            exclude_databases: Vec::new(),
            output_folder: PathBuf::new(),
            file_pattern: "${host}-${timestamp}".into(),
            extra_args: String::new(),
            auth_user: None,
        }
    }
}

/// What to restore and where (DBeaver's `PostgreDatabaseRestoreSettings`,
/// and MySQL's script import). The settings of the restore wizard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreOptions {
    /// The database restored into.
    pub database: String,
    /// The backup file (a folder for the directory format).
    pub input: PathBuf,
    /// Its format, as [`guess_format`] reads it.
    #[serde(default)]
    pub format: BackupFormat,
    /// pg_restore `--clean`: drop each object before creating it.
    #[serde(default)]
    pub clean: bool,
    /// pg_restore `--if-exists` (with `clean`).
    #[serde(default)]
    pub if_exists: bool,
    /// pg_restore `--create`: create the database named in the archive
    /// (dropped first with `clean`) and restore into it, not into
    /// `database`, which is only where pg_restore connects first.
    #[serde(default)]
    pub create: bool,
    /// pg_restore `--no-owner`.
    #[serde(default)]
    pub no_owner: bool,
    /// pg_restore `--no-privileges`.
    #[serde(default)]
    pub no_privileges: bool,
    /// pg_restore `--jobs` (custom and directory archives; not in one
    /// transaction).
    #[serde(default)]
    pub jobs: Option<u32>,
    /// PostgreSQL: all or nothing (`--single-transaction`).
    #[serde(default)]
    pub single_transaction: bool,
    /// MySQL / MariaDB: `SET SESSION FOREIGN_KEY_CHECKS=0` first.
    #[serde(default)]
    pub disable_foreign_keys: bool,
    #[serde(default)]
    pub extra_args: String,
    #[serde(default)]
    pub auth_user: Option<String>,
    /// Run the client commands a SQL file holds (see the module
    /// documentation): only for a file the user trusts, asked at each run.
    #[serde(skip)]
    pub allow_client_commands: bool,
    /// The restore tool's version, from [`version`]. `None` (unknown) means
    /// the file is scanned instead of relying on the tool's own sandbox.
    #[serde(skip)]
    pub tool_version: Option<ToolVersion>,
    /// pg_restore `--use-list`: the archive's list with entries left out
    /// (K39, [`list_without`]), written by the caller before the run.
    #[serde(skip)]
    pub use_list: Option<PathBuf>,
    /// MySQL / MariaDB: the client reads the file as `utf8mb4` (see
    /// [`BackupOptions::utf8mb4`]).
    #[serde(skip)]
    pub utf8mb4: bool,
    /// A MySQL / MariaDB folder backup restored into another database than
    /// its own: run as written the statements that would create, drop or
    /// alter something in the database the backup was taken from (K78),
    /// which are left out otherwise. Asked at each run, never kept.
    #[serde(skip)]
    pub load_as_written: bool,
    /// The target server compares database and table names without case
    /// (`lower_case_table_names` not 0), with case, or not known (`None`):
    /// how a folder restore tells the backup's database from the target and
    /// reads qualifiers (K78).
    #[serde(skip)]
    pub names_fold: Option<bool>,
}

impl RestoreOptions {
    /// A restore of `input` into `database` at Kerno's defaults: PostgreSQL
    /// restores in one transaction.
    pub fn new(
        engine: Engine,
        database: impl Into<String>,
        input: impl Into<PathBuf>,
        format: BackupFormat,
    ) -> Self {
        Self {
            database: database.into(),
            input: input.into(),
            format,
            clean: false,
            if_exists: false,
            create: false,
            no_owner: false,
            no_privileges: false,
            jobs: None,
            single_transaction: engine == Engine::Postgres,
            disable_foreign_keys: false,
            extra_args: String::new(),
            auth_user: None,
            allow_client_commands: false,
            tool_version: None,
            use_list: None,
            utf8mb4: false,
            load_as_written: false,
            names_fold: None,
        }
    }
}

/// How much `mysql` says while running a script (DBeaver's
/// `MySQLScriptExecuteSettings.LogLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptLogLevel {
    #[default]
    Normal,
    /// `--verbose`. (DBeaver's third level, `--debug-info`, makes a
    /// client built without debugging exit with an error: not offered.)
    Verbose,
}

impl ScriptLogLevel {
    pub const ALL: [ScriptLogLevel; 2] = [Self::Normal, Self::Verbose];

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Verbose => "Verbose",
        }
    }
}

/// A SQL file run by the engine's client (DBeaver's "Execute script":
/// `PostgreScriptExecuteSettings`, `MySQLScriptExecuteSettings`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptOptions {
    /// The database the script runs in.
    pub database: String,
    /// The script.
    pub input: PathBuf,
    /// MySQL / MariaDB only.
    #[serde(default)]
    pub log_level: ScriptLogLevel,
    /// MySQL / MariaDB: `--no-beep`.
    #[serde(default = "yes")]
    pub no_beep: bool,
    /// MySQL / MariaDB: foreign-key checks off for the session.
    #[serde(default)]
    pub disable_foreign_keys: bool,
    #[serde(default)]
    pub extra_args: String,
    #[serde(default)]
    pub auth_user: Option<String>,
    #[serde(skip)]
    pub allow_client_commands: bool,
    #[serde(skip)]
    pub tool_version: Option<ToolVersion>,
}

fn yes() -> bool {
    true
}

impl ScriptOptions {
    pub fn new(engine: Engine, database: impl Into<String>, input: impl Into<PathBuf>) -> Self {
        Self {
            database: database.into(),
            input: input.into(),
            log_level: ScriptLogLevel::Normal,
            no_beep: engine != Engine::Postgres,
            disable_foreign_keys: false,
            extra_args: String::new(),
            auth_user: None,
            allow_client_commands: false,
            tool_version: None,
        }
    }
}

/// The values a file name pattern's variables take (DBeaver's
/// `NativeToolUtils.ALL_VARIABLES`).
#[derive(Debug, Clone, Default)]
pub struct NameVars {
    pub host: String,
    pub database: String,
    /// The first schema backed up, or the database (DBeaver's rule).
    pub schema: Option<String>,
    /// The first table backed up.
    pub table: Option<String>,
    /// The connection's environment (DBeaver's `${connection.type}`).
    pub environment: String,
    pub now: Stamp,
}

/// A local date and time, to the minute.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stamp {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

impl Stamp {
    /// Now, in the computer's time zone, read through SQLite's
    /// `localtime` (the standard library has no time zones); UTC if that
    /// fails.
    pub fn now() -> Self {
        let read = || -> rusqlite::Result<String> {
            rusqlite::Connection::open_in_memory()?.query_row(
                "SELECT strftime('%Y %m %d %H %M', 'now', 'localtime')",
                [],
                |row| row.get(0),
            )
        };
        let text = read().unwrap_or_default();
        let mut parts = text.split(' ').map(|part| part.parse::<u32>().unwrap_or(0));
        let mut next = || parts.next().unwrap_or(0);
        Self {
            year: i32::try_from(next()).unwrap_or(1970),
            month: next(),
            day: next(),
            hour: next(),
            minute: next(),
        }
    }
}

/// `pattern` with its variables filled, as DBeaver's `resolveVars`:
/// `${host}`, `${database}`, `${schema}`, `${table}` (`null` when none),
/// `${date}` (yyyyMMdd), `${timestamp}` (yyyyMMddHHmm), `${year}`,
/// `${month}`, `${day}`, `${hour}`, `${minute}`, `${connection.type}`. An
/// unknown variable stays as written. A value cannot leave the folder:
/// `/`, `\` and a leading `.` become `_`. Refused when the name itself
/// holds a `/` or is empty.
pub fn file_name(pattern: &str, vars: &NameVars) -> Result<String> {
    let safe = |value: &str| {
        let mut value = value.replace(['/', '\\', '\0'], "_");
        if value.starts_with('.') {
            value.replace_range(..1, "_");
        }
        value
    };
    let now = &vars.now;
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let name = &after[..end];
        let value = match name {
            "host" => Some(vars.host.clone()),
            "database" => Some(vars.database.clone()),
            "schema" => Some(vars.schema.clone().unwrap_or_else(|| vars.database.clone())),
            "table" => Some(vars.table.clone().unwrap_or_else(|| "null".into())),
            "connection.type" => Some(vars.environment.clone()),
            "date" => Some(format!("{:04}{:02}{:02}", now.year, now.month, now.day)),
            "timestamp" => Some(format!(
                "{:04}{:02}{:02}{:02}{:02}",
                now.year, now.month, now.day, now.hour, now.minute
            )),
            "year" => Some(format!("{:04}", now.year)),
            "month" => Some(format!("{:02}", now.month)),
            "day" => Some(format!("{:02}", now.day)),
            "hour" => Some(format!("{:02}", now.hour)),
            "minute" => Some(format!("{:02}", now.minute)),
            _ => None,
        };
        match value {
            Some(value) => out.push_str(&safe(&value)),
            None => out.push_str(&rest[start..start + 3 + end]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    if out.trim().is_empty() || out.contains('/') || out == "." || out == ".." {
        return Err(Error::Config(format!(
            "“{pattern}” does not make a file name: it is empty or holds a “/”"
        )));
    }
    Ok(out)
}

/// The file a backup writes: the folder, the pattern filled, and the
/// format's extension (none for a directory archive).
pub fn output_path(
    folder: &Path,
    pattern: &str,
    format: BackupFormat,
    vars: &NameVars,
) -> Result<PathBuf> {
    // Windows: `~\` expanded; the caller checks the file before it looks
    // at it, `run` before the tool writes it (K69, D-138).
    let folder = &crate::local_path::expand(folder);
    let name = file_name(pattern, vars)?;
    let extension = format.extension();
    let name = if extension.is_empty() || name.ends_with(&format!(".{extension}")) {
        name
    } else {
        format!("{name}.{extension}")
    };
    Ok(folder.join(name))
}

/// The extra arguments typed for a tool, split as a POSIX shell splits
/// words (spaces separate; `'…'` keeps everything; `"…"` and `\` escape),
/// and checked (DBeaver's `addExtraCommandArgs`, which only splits on
/// spaces). Refused, with the reason:
///
/// - arguments that are not options (`name`): a positional database name
///   or connection string would replace the database Kerno passes, and
///   with it the server the password is sent to; write `--name=value`;
/// - options that choose the server, the port, the user, the database or
///   the TLS settings, or that carry or ask for a password: they belong to
///   the connection, and the password, given through the environment,
///   would go to whatever server they name;
/// - options that undo the restore protections or run something else:
///   `--set`/`--variable`, `--command`, `--file`, `--output`, `--log-file`
///   (psql); `--commands`, `--local-infile`, `--binary-mode`, `--sandbox`,
///   `--init-command`, `--execute`, `--pager`, `--tee`, `--result-file`,
///   `--system-command`, option files, plugins and authentication (mysql).
///
/// A long option is compared after MySQL's `--skip-`, `--enable-`,
/// `--disable-`, `--loose-` and `--maximum-` prefixes are removed and `_`
/// read as `-`; an abbreviation (`--ho` for `--host`) is refused like the
/// option it abbreviates. A short-option cluster is refused when it holds
/// any of the refused letters.
pub fn extra_args(text: &str, family: ClientFamily) -> Result<Vec<String>> {
    let words = split_words(text)?;
    for word in &words {
        if let Some(reason) = refused_arg(word, family) {
            return Err(Error::Config(format!(
                "“{word}” is not allowed here: {reason}"
            )));
        }
    }
    Ok(words)
}

/// Shell-style word splitting; an unclosed quote is refused.
fn split_words(text: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if let Some(done) = word.take() {
                    words.push(done);
                }
            }
            '\'' => {
                let current = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err(Error::Config("a ' quote is not closed".into())),
                    }
                }
            }
            '"' => {
                let current = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => current.push(c),
                            Some(c) => {
                                current.push('\\');
                                current.push(c);
                            }
                            None => return Err(Error::Config("a \" quote is not closed".into())),
                        },
                        Some(c) => current.push(c),
                        None => return Err(Error::Config("a \" quote is not closed".into())),
                    }
                }
            }
            '\\' => {
                let current = word.get_or_insert_with(String::new);
                if let Some(c) = chars.next() {
                    current.push(c);
                }
            }
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    Ok(words)
}

/// Long options refused, whole names (an abbreviation of one is refused
/// too), and name prefixes refused as families (`ssl…`).
const PG_REFUSED_OPTIONS: &[&str] = &[
    "host", "port", "username", "dbname", "database", "password", "file", "command", "set",
    "variable", "output", "log-file", "service",
];
const MYSQL_REFUSED_OPTIONS: &[&str] = &[
    "host",
    "port",
    "user",
    "password",
    "password1",
    "password2",
    "password3",
    "socket",
    "protocol",
    "pipe",
    "shared-memory-base-name",
    "bind-address",
    "dns-srv-name",
    "login-path",
    "plugin-dir",
    "default-auth",
    "cleartext-plugin",
    "server-public-key-path",
    "get-server-public-key",
    "commands",
    "local-infile",
    "load-data-local-dir",
    "binary-mode",
    "sandbox",
    "init-command",
    "init-command-add",
    "execute",
    "pager",
    "tee",
    "result-file",
    "system-command",
    "database",
];
const MYSQL_REFUSED_FAMILIES: &[&str] = &[
    "ssl",
    "tls",
    "defaults",
    "authentication",
    "plugin",
    "fido",
    "oci",
    "webauthn",
];
const PG_REFUSED_FAMILIES: &[&str] = &["ssl"];
/// Short options refused: PostgreSQL's `-h -p -U -d -l -W -f -c -v -o -L`
/// and MySQL's `-h -P -u -p -S -D -e -r`.
const PG_REFUSED_LETTERS: &str = "hpUdlWfcvoL";
const MYSQL_REFUSED_LETTERS: &str = "hPupSDer";

/// MySQL's and MariaDB's options that make a tool print something and stop
/// without running anything, exit 0: a run would look done with nothing
/// loaded or written (K74, N3). By long name, then by letter (`-V -? -I`).
const MYSQL_NOTHING_RUN: &[&str] = &["print-defaults", "version", "help"];
const MYSQL_NOTHING_RUN_LETTERS: &str = "V?I";

fn refused_arg(word: &str, family: ClientFamily) -> Option<&'static str> {
    let connection = "the server, the user, the database, the TLS settings and the password are \
                      the connection's own";
    let nothing = "it makes the tool print something and stop without running anything, and the run \
                   would look done";
    if family == ClientFamily::MySql {
        let stops = match word.strip_prefix("--") {
            Some(long) => {
                let name = long
                    .split('=')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .replace('_', "-");
                let name = name.strip_prefix("loose-").unwrap_or(&name).to_string();
                !name.is_empty() && MYSQL_NOTHING_RUN.iter().any(|stop| stop.starts_with(&name))
            }
            None => word.strip_prefix('-').is_some_and(|cluster| {
                cluster
                    .chars()
                    .any(|letter| MYSQL_NOTHING_RUN_LETTERS.contains(letter))
            }),
        };
        if stops {
            return Some(nothing);
        }
    }
    let (refused, families, short) = match family {
        ClientFamily::Postgres => (PG_REFUSED_OPTIONS, PG_REFUSED_FAMILIES, PG_REFUSED_LETTERS),
        ClientFamily::MySql => (
            MYSQL_REFUSED_OPTIONS,
            MYSQL_REFUSED_FAMILIES,
            MYSQL_REFUSED_LETTERS,
        ),
    };
    if let Some(long) = word.strip_prefix("--") {
        let name = long
            .split('=')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut name = name.replace('_', "-");
        if family == ClientFamily::MySql {
            // MySQL reads them stacked (`--loose-skip-sandbox`).
            const PREFIXES: [&str; 5] = ["skip-", "enable-", "disable-", "loose-", "maximum-"];
            while let Some(rest) = PREFIXES.iter().find_map(|prefix| name.strip_prefix(prefix)) {
                name = rest.to_string();
            }
        }
        if name.is_empty() {
            return Some("“--” would make the next words positional");
        }
        let abbreviates = refused
            .iter()
            .any(|option| option.starts_with(name.as_str()));
        let in_family = families.iter().any(|prefix| name.starts_with(prefix));
        return (abbreviates || in_family).then_some(connection);
    }
    if let Some(cluster) = word.strip_prefix('-') {
        return (cluster.is_empty() || cluster.chars().any(|c| short.contains(c)))
            .then_some(connection);
    }
    Some(
        "write options as --name=value: a word alone would be read as the database or a connection string",
    )
}

/// Refuses an option the engine (`engine`) or the format does not have.
fn refuse(condition: bool, what: &str) -> Result<()> {
    if condition {
        Err(Error::Unsupported(what.into()))
    } else {
        Ok(())
    }
}

/// The user for the tool: the override, else the connection's.
fn with_user(params: &ConnectParams, auth_user: &Option<String>) -> ConnectParams {
    let mut params = params.clone();
    if let Some(user) = auth_user
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        params.user = user.to_string();
    }
    params
}

/// A PostgreSQL identifier quoted for a `--schema` / `--table` pattern,
/// which then matches literally.
fn pg_pattern(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The command that backs up `options.database` to `output` with the dump
/// tool at `tool_path` (`pg_dump`, or `mysqldump` / `mariadb-dump`).
/// Options the engine or the format does not have are refused (ported
/// from DBeaver's `PostgreDatabaseBackupHandler.fillProcessParameters` and
/// `MySQLDatabaseExportHandler.fillProcessParameters`).
pub fn backup_command(
    tool_path: &Path,
    params: &ConnectParams,
    options: &BackupOptions,
    output: &Path,
) -> Result<ToolCommand> {
    // Windows: `~\` expanded; `run` checks the file (K69, D-138).
    let output = &crate::local_path::expand(output);
    let extra = extra_args(&options.extra_args, ClientFamily::required(params.engine)?)?;
    let mut command = connection_command(tool_path, &with_user(params, &options.auth_user))?;
    command.files.push(output.to_path_buf());
    let args = &mut command.args;
    match (params.engine, &options.engine) {
        (Engine::Postgres, EngineBackup::Postgres(pg)) => {
            refuse(
                !options.schemas.is_empty() && !options.tables.is_empty(),
                "pg_dump ignores the schemas once tables are named: name the tables only",
            )?;
            refuse(
                pg.jobs.is_some() && options.format != BackupFormat::Directory,
                "parallel jobs need the directory format",
            )?;
            refuse(
                pg.compression.is_some() && options.format == BackupFormat::Tar,
                "the tar format is not compressed",
            )?;
            refuse(
                pg.compression.is_some_and(|level| level > 9),
                "compression goes from 0 to 9",
            )?;
            refuse(
                pg.if_exists && !pg.clean,
                "“if exists” goes with the DROP statements",
            )?;
            refuse(
                pg.clean && options.content == BackupContent::DataOnly,
                "DROP statements need the definitions",
            )?;
            args.push("--verbose".into());
            args.push(format!("--dbname={}", pg_dbname(&options.database)));
            args.push(format!("--file={}", plain_path(output).display()));
            args.push(format!("--format={}", options.format.letter()));
            if let Some(level) = pg.compression {
                args.push(format!("--compress={level}"));
            }
            if let Some(encoding) = pg.encoding.as_deref().filter(|e| !e.is_empty()) {
                args.push(format!("--encoding={encoding}"));
            }
            if let Some(jobs) = pg.jobs {
                args.push(format!("--jobs={jobs}"));
            }
            match options.content {
                BackupContent::All => {}
                BackupContent::SchemaOnly => args.push("--schema-only".into()),
                BackupContent::DataOnly => args.push("--data-only".into()),
            }
            for (on, flag) in [
                (pg.inserts, "--inserts"),
                (pg.no_privileges, "--no-privileges"),
                (pg.no_owner, "--no-owner"),
                (pg.clean, "--clean"),
                (pg.if_exists, "--if-exists"),
                (pg.create, "--create"),
            ] {
                if on {
                    args.push(flag.into());
                }
            }
            if !options.schemas.is_empty() || !options.tables.is_empty() {
                // A pattern that matches nothing fails instead of dumping
                // nothing.
                args.push("--strict-names".into());
            }
            for schema in &options.schemas {
                args.push(format!("--schema={}", pg_pattern(schema)));
            }
            for table in &options.tables {
                args.push(format!(
                    "--table={}.{}",
                    pg_pattern(&table.schema),
                    pg_pattern(&table.name)
                ));
            }
            args.extend(extra);
        }
        (Engine::MySql | Engine::MariaDb, EngineBackup::MySql(my)) => {
            refuse(
                options.format != BackupFormat::Sql,
                "MySQL and MariaDB backups are SQL scripts; the other formats are PostgreSQL's",
            )?;
            refuse(
                my.jobs.is_some(),
                "several mysqldump at once write a folder, one file per table",
            )?;
            if let Some(word) = extra.iter().find(|word| silences(word)) {
                return Err(Error::Unsupported(format!(
                    "“{word}” would hide the lines Kerno reads from mysqldump (its progress, and \
                     a table it could not read): leave it out"
                )));
            }
            refuse(
                !options.schemas.is_empty(),
                "a MySQL backup is of one database: name its tables",
            )?;
            args.push("--verbose".into());
            args.push(my.method.option().into());
            // mysqldump 8.0 asks MySQL's column statistics by default,
            // which MariaDB does not have (DBeaver adds the same option).
            if params.engine == Engine::MariaDb && !is_mariadb_client(tool_path) {
                args.push("--column-statistics=0".into());
            }
            if options.utf8mb4 {
                args.push(UTF8MB4.into());
            }
            match options.content {
                BackupContent::All => {}
                BackupContent::SchemaOnly => args.push("--no-data".into()),
                BackupContent::DataOnly => args.push("--no-create-info".into()),
            }
            let has_definitions = options.content != BackupContent::DataOnly;
            for (on, flag) in [
                (my.routines && has_definitions, "--routines"),
                (my.events && has_definitions, "--events"),
                (!my.triggers || !has_definitions, "--skip-triggers"),
                (!my.extended_inserts, "--skip-extended-insert"),
                (!my.add_drop_table, "--skip-add-drop-table"),
                (!my.disable_keys, "--skip-disable-keys"),
                (my.hex_blob, "--hex-blob"),
                (!my.comments, "--skip-comments"),
            ] {
                if on {
                    args.push(flag.into());
                }
            }
            args.push(format!("--result-file={}", output.display()));
            args.extend(extra);
            // The database and the tables are positional: a leading '-'
            // would be read as an option.
            args.push(positional(&options.database)?);
            for table in &options.tables {
                if table.schema != options.database {
                    return Err(Error::Config(format!(
                        "table {}.{} is not in database {}",
                        table.schema, table.name, options.database
                    )));
                }
                args.push(positional(&table.name)?);
            }
        }
        (engine, _) => {
            return Err(Error::Unsupported(format!(
                "these are another engine's backup options, not {}'s",
                engine.label()
            )));
        }
    }
    Ok(command)
}

/// The command that backs up a whole PostgreSQL server with `pg_dumpall`
/// to `output` (ported from DBeaver's
/// `PostgreDatabaseBackupAllHandler.fillProcessParameters`).
pub fn global_backup_command(
    tool_path: &Path,
    params: &ConnectParams,
    options: &GlobalBackupOptions,
    output: &Path,
) -> Result<ToolCommand> {
    // Windows: `~\` expanded; `run` checks the file (K69, D-138).
    let output = &crate::local_path::expand(output);
    refuse(
        params.engine != Engine::Postgres,
        "the global backup is PostgreSQL's pg_dumpall",
    )?;
    let only = [
        options.globals_only,
        options.roles_only,
        options.tablespaces_only,
    ];
    refuse(
        only.iter().filter(|on| **on).count() > 1,
        "globals only, roles only and tablespaces only exclude each other",
    )?;
    refuse(
        only.contains(&true) && !options.exclude_databases.is_empty(),
        "excluded databases go with a backup of the databases, not of the globals only",
    )?;
    let extra = extra_args(&options.extra_args, ClientFamily::Postgres)?;
    let mut command = connection_command(tool_path, &with_user(params, &options.auth_user))?;
    command.files.push(output.to_path_buf());
    let args = &mut command.args;
    args.push("--verbose".into());
    if let Some(encoding) = options.encoding.as_deref().filter(|e| !e.is_empty()) {
        args.push(format!("--encoding={encoding}"));
    }
    for (on, flag) in [
        (options.schema_only, "--schema-only"),
        (options.globals_only, "--globals-only"),
        (options.roles_only, "--roles-only"),
        (options.tablespaces_only, "--tablespaces-only"),
        (options.no_privileges, "--no-privileges"),
        (options.no_owner, "--no-owner"),
        (!options.role_passwords, "--no-role-passwords"),
    ] {
        if on {
            args.push(flag.into());
        }
    }
    args.push(format!("--file={}", plain_path(output).display()));
    for database in &options.exclude_databases {
        args.push(format!("--exclude-database={}", pg_pattern(database)));
    }
    args.extend(extra);
    Ok(command)
}

/// The command that restores `options.input` into `options.database` with
/// the tool at `tool_path` (see [`restore_tool`]). Refused on a read-only
/// connection, and for options the format's tool does not have (ported
/// from DBeaver's `PostgreDatabaseRestoreHandler`, whose options apply to
/// archives only). A SQL file is kept from running client commands as the
/// module documentation says: by the tool's option, or by a check that
/// [`run`] makes before starting the tool.
pub fn restore_command(
    tool_path: &Path,
    params: &ConnectParams,
    options: &RestoreOptions,
) -> Result<ToolCommand> {
    // Windows: `~\` expanded; `run` checks the file (K69, D-138).
    let options = &RestoreOptions {
        input: crate::local_path::expand(&options.input),
        ..options.clone()
    };
    let mut command = restore_command_for(tool_path, params, options)?;
    command.files.push(options.input.clone());
    Ok(command)
}

fn restore_command_for(
    tool_path: &Path,
    params: &ConnectParams,
    options: &RestoreOptions,
) -> Result<ToolCommand> {
    if params.read_only {
        return Err(Error::ReadOnly(
            "restoring a backup writes to the database".into(),
        ));
    }
    let family = ClientFamily::required(params.engine)?;
    let extra = extra_args(&options.extra_args, family)?;
    let archive_only = options.clean
        || options.if_exists
        || options.create
        || options.no_owner
        || options.no_privileges
        || options.jobs.is_some()
        || options.use_list.is_some();
    let params = with_user(params, &options.auth_user);
    match (params.engine, options.format) {
        (Engine::Postgres, BackupFormat::Sql) => {
            refuse(
                archive_only,
                "a SQL script runs as it is: clean, create, owner, privileges and jobs are \
                 pg_restore's options, for archives",
            )?;
            refuse(
                options.disable_foreign_keys,
                "foreign-key checks off is MySQL's",
            )?;
            let mut command = sql_script_command(
                tool_path,
                &params,
                &SqlScript {
                    database: &options.database,
                    input: &options.input,
                    allow_client_commands: options.allow_client_commands,
                    tool_version: options.tool_version.as_ref(),
                },
            )?;
            if options.single_transaction {
                let at = command
                    .args
                    .iter()
                    .position(|arg| arg.starts_with("--file="))
                    .unwrap_or(command.args.len());
                command.args.insert(at, "--single-transaction".into());
            }
            command.args.extend(extra);
            Ok(command)
        }
        (Engine::Postgres, format) => {
            refuse(
                options.if_exists && !options.clean,
                "“if exists” goes with clean",
            )?;
            refuse(
                options.single_transaction && options.jobs.is_some(),
                "parallel jobs cannot run in one transaction",
            )?;
            refuse(
                options.single_transaction && options.create,
                "pg_restore cannot create the database in one transaction",
            )?;
            refuse(
                options.jobs.is_some() && format == BackupFormat::Tar,
                "parallel jobs read custom and directory archives only",
            )?;
            refuse(
                options.disable_foreign_keys,
                "foreign-key checks off is MySQL's",
            )?;
            let mut command = connection_command(tool_path, &params)?;
            let args = &mut command.args;
            args.push("--verbose".into());
            args.push(format!("--dbname={}", pg_dbname(&options.database)));
            args.push(format!("--format={}", format.letter()));
            if let Some(jobs) = options.jobs {
                args.push(format!("--jobs={jobs}"));
            }
            if let Some(list) = &options.use_list {
                args.push(format!("--use-list={}", list.display()));
            }
            for (on, flag) in [
                (options.clean, "--clean"),
                (options.if_exists, "--if-exists"),
                (options.create, "--create"),
                (options.no_owner, "--no-owner"),
                (options.no_privileges, "--no-privileges"),
                (options.single_transaction, "--single-transaction"),
            ] {
                if on {
                    args.push(flag.into());
                }
            }
            args.extend(extra);
            args.push(plain_path(&options.input).display().to_string());
            Ok(command)
        }
        (Engine::MySql | Engine::MariaDb, BackupFormat::Sql) => {
            refuse(
                archive_only || options.single_transaction,
                "these are pg_restore's options; mysql runs the file statement by statement",
            )?;
            let mut command = sql_script_command(
                tool_path,
                &params,
                &SqlScript {
                    database: &options.database,
                    input: &options.input,
                    allow_client_commands: options.allow_client_commands,
                    tool_version: options.tool_version.as_ref(),
                },
            )?;
            if options.disable_foreign_keys {
                command.args.push(FOREIGN_KEYS_OFF.into());
            }
            if options.utf8mb4 {
                command.args.push(UTF8MB4.into());
            }
            command.args.extend(extra);
            Ok(command)
        }
        (Engine::MySql | Engine::MariaDb, _) => Err(Error::Unsupported(
            "MySQL backups are SQL scripts; the archive formats are PostgreSQL's".into(),
        )),
        (Engine::Sqlite, _) => Err(Error::Unsupported(NO_CLIENT_TOOLS.into())),
    }
}

/// Whether the user's argument `word` turns mysqldump's `--verbose` off
/// (`--skip-verbose`, `--verbose=0`…) or sends what it writes on its
/// standard error to a file (`--log-error`): Kerno reads those lines (a
/// table it could not read, a range's snapshot taken).
fn silences(word: &str) -> bool {
    let Some(long) = word.strip_prefix("--") else {
        return false;
    };
    let normal = long.to_ascii_lowercase().replace('_', "-");
    let (mut name, value) = match normal.split_once('=') {
        Some((name, value)) => (name.to_string(), Some(value.to_string())),
        None => (normal.clone(), None),
    };
    let mut off = false;
    loop {
        if let Some(rest) = name
            .strip_prefix("skip-")
            .or_else(|| name.strip_prefix("disable-"))
        {
            off = true;
            name = rest.to_string();
        } else if let Some(rest) = name
            .strip_prefix("loose-")
            .or_else(|| name.strip_prefix("enable-"))
            .or_else(|| name.strip_prefix("maximum-"))
        {
            name = rest.to_string();
        } else {
            break;
        }
    }
    let off_value = value
        .as_deref()
        .is_some_and(|value| matches!(value, "0" | "false" | "off"));
    let log_error = name.len() >= 3 && "log-error".starts_with(&name);
    let verbose = name.len() >= 3 && "verbose".starts_with(&name);
    log_error || (verbose && (off || off_value))
}

/// The character set a MySQL / MariaDB tool talks in, when [`utf8mb4`].
const UTF8MB4: &str = "--default-character-set=utf8mb4";

/// Whether a MySQL / MariaDB tool of version `client` should talk to a
/// server of version `server` in `utf8mb4`: both have it (MySQL 5.5.3,
/// MariaDB 5.5). A server without it (5.1 to 5.5.2) holds no 4-byte
/// character, and the tool keeps its default there.
pub fn utf8mb4(client: Option<&ToolVersion>, server: (u32, u32, u32)) -> bool {
    server >= (5, 5, 3)
        && client.is_some_and(|client| {
            client.number >= if client.mariadb { (5, 5, 0) } else { (5, 5, 3) }
        })
}

/// `mysql`'s option that turns foreign-key checks off for the session
/// (DBeaver's `MySQLScriptExecuteHandler`).
const FOREIGN_KEYS_OFF: &str = "--init-command=SET SESSION FOREIGN_KEY_CHECKS=0";

/// The command that runs the SQL script `options.input` in
/// `options.database` with `psql` or `mysql` (DBeaver's "Execute script",
/// `PostgreScriptExecuteHandler` and `MySQLScriptExecuteHandler`): the
/// restore of a SQL file without a transaction, with the same protections
/// (refused on a read-only connection, client commands kept off).
pub fn script_command(
    tool_path: &Path,
    params: &ConnectParams,
    options: &ScriptOptions,
) -> Result<ToolCommand> {
    // Windows: `~\` expanded; `run` checks the file (K69, D-138).
    let options = &ScriptOptions {
        input: crate::local_path::expand(&options.input),
        ..options.clone()
    };
    let mut command = script_command_for(tool_path, params, options)?;
    command.files.push(options.input.clone());
    Ok(command)
}

fn script_command_for(
    tool_path: &Path,
    params: &ConnectParams,
    options: &ScriptOptions,
) -> Result<ToolCommand> {
    if params.read_only {
        return Err(Error::ReadOnly("a script can write to the database".into()));
    }
    let family = ClientFamily::required(params.engine)?;
    let extra = extra_args(&options.extra_args, family)?;
    let mysql_only = options.log_level != ScriptLogLevel::Normal
        || options.no_beep
        || options.disable_foreign_keys;
    refuse(
        params.engine == Engine::Postgres && mysql_only,
        "the log level, the beep and the foreign-key checks are mysql's options",
    )?;
    let params = with_user(params, &options.auth_user);
    let mut command = sql_script_command(
        tool_path,
        &params,
        &SqlScript {
            database: &options.database,
            input: &options.input,
            allow_client_commands: options.allow_client_commands,
            tool_version: options.tool_version.as_ref(),
        },
    )?;
    let args = &mut command.args;
    match params.engine {
        Engine::Postgres => {
            // psql writes the statement that failed (DBeaver's
            // `--echo-errors`, from PostgreSQL 9.5).
            let at = args
                .iter()
                .position(|arg| arg.starts_with("--file="))
                .unwrap_or(args.len());
            args.insert(at, "--echo-errors".into());
        }
        Engine::MySql | Engine::MariaDb => {
            for (on, flag) in [
                (options.log_level == ScriptLogLevel::Verbose, "--verbose"),
                (options.no_beep, "--no-beep"),
                (options.disable_foreign_keys, FOREIGN_KEYS_OFF),
            ] {
                if on {
                    args.push(flag.into());
                }
            }
        }
        Engine::Sqlite => return Err(Error::Unsupported(NO_CLIENT_TOOLS.into())),
    }
    args.extend(extra);
    Ok(command)
}

/// A SQL file for [`sql_script_command`].
struct SqlScript<'a> {
    database: &'a str,
    input: &'a Path,
    allow_client_commands: bool,
    tool_version: Option<&'a ToolVersion>,
}

/// `psql` or `mysql` reading a SQL file, its client commands kept off by
/// the client's sandbox or by the check [`run`] makes first.
fn sql_script_command(
    tool_path: &Path,
    params: &ConnectParams,
    script: &SqlScript,
) -> Result<ToolCommand> {
    let mut command = connection_command(tool_path, params)?;
    let check = |tool| {
        (!script.allow_client_commands).then(|| ScriptCheck {
            tool,
            path: script.input.to_path_buf(),
            version: script.tool_version.cloned(),
        })
    };
    let args = &mut command.args;
    match params.engine {
        Engine::Postgres => {
            // ~/.psqlrc could turn ON_ERROR_STOP off or run its own commands.
            args.push("--no-psqlrc".into());
            args.push(format!("--dbname={}", pg_dbname(script.database)));
            // Without ON_ERROR_STOP psql goes on after a failed statement
            // and exits 0.
            args.push("--set=ON_ERROR_STOP=1".into());
            // Read from the standard input, which `run` feeds and counts:
            // `--file=<path>` would leave no way to show the progress.
            args.push("--file=-".into());
            command.stdin = Some(script.input.to_path_buf());
            command.script_check = check(Tool::Psql);
        }
        Engine::MySql | Engine::MariaDb => {
            args.push(format!("--database={}", script.database));
            // LOAD DATA LOCAL INFILE would send a file of this computer.
            args.push("--local-infile=0".into());
            let sandbox = script.tool_version.and_then(sandbox_option);
            match sandbox.filter(|_| !script.allow_client_commands) {
                Some(sandbox) => args.extend(sandbox.iter().map(|option| option.to_string())),
                None => command.script_check = check(Tool::Mysql),
            }
            command.stdin = Some(script.input.to_path_buf());
        }
        Engine::Sqlite => return Err(Error::Unsupported(NO_CLIENT_TOOLS.into())),
    }
    Ok(command)
}

/// The format of a backup to restore: a folder holding `toc.dat` is a
/// directory archive (a folder holding Kerno's [`mysql_folder::MANIFEST`]
/// for MySQL and MariaDB), a file starting with `PGDMP` a custom archive, one
/// with `ustar` at byte 257 a tar archive (pg_dump's tar has `toc.dat`
/// first), anything else a SQL script. Blocking.
pub fn guess_format(path: &Path, engine: Engine) -> std::result::Result<BackupFormat, String> {
    let format = if path.is_dir() {
        match engine {
            Engine::Postgres if path.join("toc.dat").is_file() => BackupFormat::Directory,
            // A MySQL / MariaDB backup of one file per table (K63).
            Engine::MySql | Engine::MariaDb if path.join(mysql_folder::MANIFEST).is_file() => {
                BackupFormat::Directory
            }
            Engine::MySql | Engine::MariaDb if path.join("toc.dat").is_file() => {
                return Err(format!(
                    "{} is a PostgreSQL archive: MySQL and MariaDB restore SQL files.",
                    path.display()
                ));
            }
            Engine::MySql | Engine::MariaDb => {
                return Err(format!(
                    "{} is a folder without {}: not a backup Kerno made of a MySQL or MariaDB \
                     database.",
                    path.display(),
                    mysql_folder::MANIFEST
                ));
            }
            _ => {
                return Err(format!(
                    "{} is a folder without toc.dat: not a pg_dump directory archive.",
                    path.display()
                ));
            }
        }
    } else {
        let mut start = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(262).read_to_end(&mut start))
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        if start.starts_with(b"PGDMP") {
            BackupFormat::Custom
        } else if start.get(257..262) == Some(b"ustar") {
            BackupFormat::Tar
        } else {
            BackupFormat::Sql
        }
    };
    if matches!(format, BackupFormat::Custom | BackupFormat::Tar) && engine != Engine::Postgres {
        return Err(format!(
            "{} is a PostgreSQL archive: MySQL and MariaDB restore SQL files.",
            path.display()
        ));
    }
    Ok(format)
}

/// The program with the arguments and environment that say which server to
/// reach, as whom, and over which TLS.
fn connection_command(tool_path: &Path, params: &ConnectParams) -> Result<ToolCommand> {
    // The TLS files go to the tool: `~\` expanded, none of another machine
    // (K69, D-138).
    let params = &crate::local_path::resolve_params(params)?;
    let mut args = Vec::new();
    let mut env = Vec::new();
    let tunnelled = params.tunnel.is_some();
    let password_var;
    match params.engine {
        Engine::Postgres => {
            password_var = "PGPASSWORD";
            // Never wait on a password prompt.
            args.push("--no-password".into());
            // Through a tunnel the name stays the server's (TLS checks it)
            // and the address is the tunnel's local end.
            args.push(format!("--host={}", params.host));
            if tunnelled {
                env.push(("PGHOSTADDR".into(), "127.0.0.1".into()));
            }
            args.push(format!("--port={}", params.port));
            args.push(format!("--username={}", params.user));
            pg_tls_env(&params.tls, &mut env);
        }
        Engine::MySql | Engine::MariaDb => {
            password_var = "MYSQL_PWD";
            // Option files (~/.my.cnf) could name another host or user.
            args.push("--no-defaults".into());
            // "localhost" would mean the Unix socket.
            args.push("--protocol=TCP".into());
            let host = if tunnelled {
                "127.0.0.1"
            } else {
                params.host.as_str()
            };
            args.push(format!("--host={host}"));
            args.push(format!("--port={}", params.port));
            args.push(format!("--user={}", params.user));
            mysql_tls_args(is_mariadb_client(tool_path), &params.tls, &mut args);
        }
        Engine::Sqlite => return Err(Error::Unsupported(NO_CLIENT_TOOLS.into())),
    }
    let port_arg = args
        .iter()
        .position(|arg| arg.starts_with("--port="))
        .unwrap_or_default();
    Ok(ToolCommand {
        program: tool_path.to_path_buf(),
        args,
        env,
        password: params
            .password
            .clone()
            .filter(|password| !password.is_empty())
            .map(|password| (password_var, password)),
        stdin: None,
        stdin_parts: Vec::new(),
        sent: Arc::default(),
        tunnel: params.tunnel.clone().map(|tunnel| TunnelHop {
            tunnel,
            host: params.host.clone(),
            port: params.port,
            port_arg,
        }),
        script_check: None,
        files: Vec::new(),
    })
}

fn pg_tls_env(tls: &TlsConfig, env: &mut Vec<(String, String)>) {
    env.push(("PGSSLMODE".into(), tls.mode.as_str().into()));
    for (name, file) in [
        ("PGSSLROOTCERT", &tls.ca_file),
        ("PGSSLCERT", &tls.cert_file),
        ("PGSSLKEY", &tls.key_file),
    ] {
        if let Some(file) = file {
            env.push((name.into(), file.display().to_string()));
        }
    }
}

/// Whether the binary is MariaDB's client rather than MySQL's: the options
/// follow the tool, not the server (MySQL's `mysqldump` can back up a
/// MariaDB server). MariaDB installs `mysqldump` / `mysql` as links to
/// `mariadb-dump` / `mariadb`, so the name is read after following links.
// ponytail: a MariaDB older than 10.5 ships a real `mysqldump` and is taken for
// MySQL's; run `--version` if those clients matter.
fn is_mariadb_client(tool_path: &Path) -> bool {
    let real = std::fs::canonicalize(tool_path).unwrap_or_else(|_| tool_path.to_path_buf());
    real.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("mariadb"))
}

/// MySQL's `--ssl-mode`; MariaDB's client has no such option and uses
/// `--ssl` / `--ssl-verify-server-cert` (which also checks the host name, so
/// verify-ca is stricter there).
fn mysql_tls_args(mariadb: bool, tls: &TlsConfig, args: &mut Vec<String>) {
    if !mariadb {
        let mode = match tls.mode {
            TlsMode::Disable => "DISABLED",
            TlsMode::Prefer => "PREFERRED",
            TlsMode::Require => "REQUIRED",
            TlsMode::VerifyCa => "VERIFY_CA",
            TlsMode::VerifyFull => "VERIFY_IDENTITY",
        };
        args.push(format!("--ssl-mode={mode}"));
    } else {
        match tls.mode {
            TlsMode::Disable => args.push("--skip-ssl".into()),
            TlsMode::Prefer => {}
            TlsMode::Require => {
                args.extend(["--ssl".into(), "--skip-ssl-verify-server-cert".into()]);
            }
            TlsMode::VerifyCa | TlsMode::VerifyFull => {
                args.extend(["--ssl".into(), "--ssl-verify-server-cert".into()]);
            }
        }
    }
    for (option, file) in [
        ("--ssl-ca", &tls.ca_file),
        ("--ssl-cert", &tls.cert_file),
        ("--ssl-key", &tls.key_file),
    ] {
        if let Some(file) = file {
            args.push(format!("{option}={}", file.display()));
        }
    }
}

/// A database name for libpq's `dbname`. libpq reads a value with `=` or a
/// URI prefix as a whole connection string, which could name another host
/// (and send it the password): such a name is passed quoted, as a
/// connection string that only sets `dbname`.
fn pg_dbname(name: &str) -> String {
    if name.contains('=') || name.starts_with("postgres://") || name.starts_with("postgresql://") {
        format!(
            "dbname='{}'",
            name.replace('\\', r"\\").replace('\'', r"\'")
        )
    } else {
        name.to_string()
    }
}

fn positional(name: &str) -> Result<String> {
    if name.starts_with('-') {
        Err(Error::Config(format!(
            "\"{name}\" starts with '-' and would be read as an option by the tool"
        )))
    } else {
        Ok(name.to_string())
    }
}

/// Whether an environment variable is read by libpq or the MySQL / MariaDB
/// clients.
fn is_database_variable(name: &OsStr) -> bool {
    let mut name = name.to_string_lossy().into_owned();
    // Windows' names have no case: `PgHost` is read as `PGHOST`.
    if cfg!(windows) {
        name.make_ascii_uppercase();
    }
    ["PG", "MYSQL", "MARIADB"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// A program Windows ships, at `relative` under `C:\Windows\System32` (`curl.exe`, `whoami.exe`,
/// `reg.exe`, `OpenSSH\ssh.exe`), and only there. `system_root` is the `SystemRoot` variable; any
/// value but `C:\Windows` (any case, a trailing separator ignored) gives `None`: another drive, a
/// relative or UNC path, an empty or absent value. A process of the same user can set that variable
/// for the user's next programs, and a Kerno then started as administrator would run what lies in a
/// folder of its choosing (K70, D-139). A Windows installed elsewhere than `C:\Windows` gets none
/// of these programs: no update check, no installer run.
pub fn system_program(system_root: Option<&str>, relative: &str) -> Option<PathBuf> {
    let root = system_root?.trim_end_matches(['\\', '/']);
    root.eq_ignore_ascii_case(r"C:\Windows")
        .then(|| PathBuf::from(format!(r"C:\Windows\System32\{relative}")))
}

/// [`system_program`] with this process's `SystemRoot`.
pub fn windows_program(relative: &str) -> Option<PathBuf> {
    system_program(std::env::var("SystemRoot").ok().as_deref(), relative)
}

/// Windows: the child gets no console window. A release Kerno is a windowed program with no
/// console to share, so Windows would open one for each console program it starts
/// (`CREATE_NO_WINDOW`). Nothing on other systems.
pub fn no_console(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// How often [`run`] looks at its cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// Runs `command`, passing every line it writes (standard output and
/// error, as they come) to `on_line`. Returns the exit status, which may
/// be a failure; `Err` means the tool could not be started, its script was
/// refused by [`check_script`] (nothing ran), a part of its input could
/// not be read, it ended well before reading the whole of its input (part
/// of the script never ran), or [`Error::Cancelled`] when `cancel` was set
/// (the child is killed).
///
/// What the child had in its input pipe when it ended is not seen: a tool
/// that ends well with the last bytes written to the pipe still unread
/// is taken at its word.
pub async fn run(
    command: ToolCommand,
    on_line: impl Fn(String) + Send + 'static,
    cancel: Arc<AtomicBool>,
) -> Result<ExitStatus> {
    // Windows: a task's file of another computer is never opened, by Kerno
    // or by the tool (K69, D-138).
    for file in &command.files {
        crate::local_path::check(file, "task's file")?;
    }
    if let Some(check) = command.script_check.clone() {
        let cancel = cancel.clone();
        tokio::task::spawn_blocking(move || check_script(&check, &cancel))
            .await
            .map_err(|error| Error::io("checking the script", std::io::Error::other(error)))??;
    }
    let mut args = command.args;
    if let Some(hop) = &command.tunnel {
        let local = hop.tunnel.forward_once(&hop.host, hop.port).await?;
        args[hop.port_arg] = format!("--port={}", local.port());
    }
    let mut child = tokio::process::Command::new(&command.program);
    child.args(&args).kill_on_drop(true);
    no_console(child.as_std_mut());
    // Inherited libpq / MySQL variables (PGHOST, PGSERVICE, MYSQL_HOST...)
    // could send the tool to another server.
    for (name, _) in std::env::vars_os().filter(|(name, _)| is_database_variable(name)) {
        child.env_remove(name);
    }
    child.envs(command.env);
    if let Some((name, password)) = &command.password {
        child.env(name, password.expose());
    }
    // Every file checked before the child starts, and each opened only when
    // the feed reaches it, then closed: a run of a thousand parts holds one
    // open at a time (K74, F1). One that cannot be read then fails the run,
    // never a script cut short.
    let mut input = Vec::new();
    if let Some(path) = &command.stdin {
        input.push(StdinPart::File {
            path: path.clone(),
            range: 0..u64::MAX,
            counted: true,
        });
    }
    input.extend(command.stdin_parts.iter().cloned());
    for part in &input {
        if let StdinPart::File { path, .. } = part {
            tokio::fs::metadata(path)
                .await
                .map_err(|error| Error::io(format!("could not open {}", path.display()), error))?;
        }
    }
    child.stdin(if input.is_empty() {
        Stdio::null()
    } else {
        Stdio::piped()
    });
    child.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = child.spawn().map_err(|error| {
        Error::io(
            format!("could not start {}", command.program.display()),
            error,
        )
    })?;
    // Fed beside the reading of the output, and stopped with the child.
    // `fed` counts the parts written whole: a child that ends well before
    // the last one stopped reading on its own (K79).
    let parts: Vec<Option<PathBuf>> = input
        .iter()
        .map(|part| match part {
            StdinPart::File { path, .. } => Some(path.clone()),
            StdinPart::Text(_) => None,
        })
        .collect();
    let fed = Arc::new(AtomicUsize::new(0));
    let feeding = (!input.is_empty())
        .then_some(input)
        .zip(child.stdin.take())
        .map(|(input, pipe)| tokio::spawn(feed(input, pipe, command.sent.clone(), fed.clone())));
    let mut feeding = Feeding(feeding);

    let mut stdout = child.stdout.take().map(BufReader::new);
    let mut stderr = child.stderr.take().map(BufReader::new);
    let (mut out_line, mut err_line) = (Vec::new(), Vec::new());
    let mut tick = tokio::time::interval(CANCEL_POLL);
    loop {
        tokio::select! {
            // The feed first: its error wins over anything the child does
            // (K74, N1).
            biased;
            // A part that could not be read: the child must not go on with
            // what it was given (K74, F1). Its input stays open until it is
            // killed, so it never takes the cut for the end of its script.
            fed = async {
                match feeding.0.as_mut() {
                    Some(task) => task.await,
                    None => std::future::pending().await,
                }
            }, if feeding.0.is_some() => {
                feeding.0 = None;
                if let Ok(Err((error, pipe))) = fed {
                    let _ = child.kill().await;
                    drop(pipe);
                    return Err(error);
                }
            }
            read = read_line(&mut stdout, &mut out_line) => {
                if !emit(read, &mut out_line, &on_line) { stdout = None; }
            }
            read = read_line(&mut stderr, &mut err_line) => {
                if !emit(read, &mut err_line, &on_line) { stderr = None; }
            }
            // Both pipes are read to their end first, so a chatty child
            // never blocks on a full pipe.
            status = child.wait(), if stdout.is_none() && stderr.is_none() => {
                // The child is gone, so its feed ends at once (its next write
                // fails), or finds the part it cannot read: waited for, since
                // aborting it there took a feed still on its way for a child
                // that had all it needed (K79). A grandchild holding the
                // input open is not waited for longer than this.
                if let Some(mut task) = feeding.0.take() {
                    let ended = tokio::time::timeout(FEED_AFTER_EXIT, &mut task).await;
                    task.abort();
                    // A feed that failed as the child ended: its error wins.
                    if let Ok(Ok(Err((error, _)))) = ended {
                        return Err(error);
                    }
                }
                let status = status.map_err(|error| Error::io("waiting for the tool", error))?;
                // Ended well without the end of its input: it stopped reading
                // on its own (`quit`, `\q`), and the rest never ran. One that
                // stopped on an error says so by its status (K79).
                let fed = fed.load(Ordering::Acquire);
                if status.success() && fed < parts.len() {
                    return Err(Error::Config(stopped_reading(&command.program, &parts, fed)));
                }
                return Ok(status);
            }
            _ = tick.tick() => {
                if cancel.load(Ordering::Relaxed) {
                    // Not waiting for the pipes: a grandchild may hold them open.
                    let _ = child.kill().await;
                    return Err(Error::Cancelled);
                }
            }
        }
    }
}

/// How long [`run`] waits for the feed once the child ended (see there).
const FEED_AFTER_EXIT: Duration = Duration::from_secs(2);

/// What [`run`] says of a client that ended well before the end of its
/// input: it stopped at part `at`, named by its file (a text of Kerno's own
/// by the file it follows).
fn stopped_reading(program: &Path, parts: &[Option<PathBuf>], at: usize) -> String {
    let tool = program.file_stem().unwrap_or(program.as_os_str());
    let tool = tool.to_string_lossy();
    let file = parts[..=at]
        .iter()
        .rev()
        .chain(&parts[at + 1..])
        .flatten()
        .next();
    match file {
        Some(file) => format!(
            "{tool} stopped reading before the end of {} and ended without an error: the rest \
             of the script was not run.",
            file.display()
        ),
        None => format!(
            "{tool} stopped reading before the end of its input and ended without an error: \
             the rest of the script was not run."
        ),
    }
}

/// Copies `input` to the child's standard input, each file opened when its
/// turn comes and closed after, counting the bytes of the counted parts in
/// `sent` and the parts written whole in `fed`, then closes it (the end of
/// the script). Stops quietly when the child closes its end first: it
/// stopped on an error or on its own, or was killed. A file that cannot be
/// opened or read is an error: [`run`] then kills the child.
///
/// On such an error the pipe comes back with it, still open: the child
/// must never see the end of its input (it would run a last statement cut
/// short, or end well with rows missing) before [`run`] kills it (K74, N1).
async fn feed(
    input: Vec<StdinPart>,
    mut pipe: tokio::process::ChildStdin,
    sent: Arc<AtomicU64>,
    fed: Arc<AtomicUsize>,
) -> std::result::Result<(), (Error, tokio::process::ChildStdin)> {
    match feed_parts(input, &mut pipe, sent, fed).await {
        Ok(()) => Ok(()),
        Err(error) => Err((error, pipe)),
    }
}

/// [`feed`]'s work, the pipe borrowed.
async fn feed_parts(
    input: Vec<StdinPart>,
    pipe: &mut tokio::process::ChildStdin,
    sent: Arc<AtomicU64>,
    fed: Arc<AtomicUsize>,
) -> Result<()> {
    use tokio::io::AsyncSeekExt as _;
    let mut chunk = vec![0; 256 * 1024];
    for part in input {
        let (path, range, counted) = match part {
            StdinPart::Text(text) => {
                if pipe.write_all(text.as_bytes()).await.is_err() {
                    return Ok(());
                }
                fed.fetch_add(1, Ordering::Release);
                continue;
            }
            StdinPart::File {
                path,
                range,
                counted,
            } => (path, range, counted),
        };
        let failed = |error| Error::io(format!("could not read {}", path.display()), error);
        let mut file = tokio::fs::File::open(&path).await.map_err(failed)?;
        if range.start > 0 {
            file.seek(std::io::SeekFrom::Start(range.start))
                .await
                .map_err(failed)?;
        }
        let mut file = file.take(range.end.saturating_sub(range.start));
        loop {
            let read = match file.read(&mut chunk).await.map_err(failed)? {
                0 => break,
                read => read,
            };
            if pipe.write_all(&chunk[..read]).await.is_err() {
                return Ok(());
            }
            if counted {
                sent.fetch_add(read as u64, Ordering::Relaxed);
            }
        }
        fed.fetch_add(1, Ordering::Release);
    }
    Ok(())
}

/// How [`feed`] ended: well, or an error with the pipe still open.
type Fed = std::result::Result<(), (Error, tokio::process::ChildStdin)>;

/// Aborts the feeding task when [`run`] returns, however it returns.
struct Feeding(Option<tokio::task::JoinHandle<Fed>>);

impl Drop for Feeding {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// Reads up to the next newline into `line`; pending forever once the pipe
/// is closed (`None`). Partial reads stay in `line`, so being dropped by
/// `select!` loses nothing.
async fn read_line<R: AsyncRead + Unpin>(
    pipe: &mut Option<BufReader<R>>,
    line: &mut Vec<u8>,
) -> std::io::Result<usize> {
    match pipe {
        Some(pipe) => pipe.read_until(b'\n', line).await,
        None => std::future::pending().await,
    }
}

/// Hands a complete line to `on_line`. Returns `false` at the end of the
/// pipe (after the last, unterminated line if any).
fn emit(read: std::io::Result<usize>, line: &mut Vec<u8>, on_line: &impl Fn(String)) -> bool {
    let open = matches!(read, Ok(n) if n > 0);
    if !line.is_empty() {
        let text = String::from_utf8_lossy(line);
        on_line(text.trim_end_matches(['\n', '\r']).to_string());
        line.clear();
    }
    open
}

/// Refuses, with [`Error::Config`] naming the line and the command, a SQL
/// script holding a client command that `tool` ([`Tool::Psql`], or
/// [`Tool::Mysql`] for both MySQL and MariaDB) would run on this computer
/// (see the module documentation). Reads the file once in fixed-size
/// chunks, in constant memory; blocking, so call it off the UI thread.
/// [`run`] calls it before starting a restore, the restore dialog may call
/// it earlier to offer the opt-in.
///
/// The scan ignores SQL quoting: a client command written inside a string
/// or a function body is refused too, as are regular expressions such as
/// `'a\w b'` whose backslash happens to spell a command followed by a
/// space. A false alarm costs the user an explicit opt-in; reading the file
/// the way the tool does would mean following its lexer (and the encoding
/// the file can switch to), where any gap is a command run unasked.
pub fn check_script(check: &ScriptCheck, cancel: &AtomicBool) -> Result<()> {
    let path = &check.path;
    let psql = check.tool == Tool::Psql;
    let Some(found) = find_client_command(check, cancel)? else {
        return Ok(());
    };
    Err(Error::Config(format!(
        "{}, line {}: {} is a {} client command, run on this computer rather than on the \
         server (it can start programs, read or write files, or connect elsewhere). Nothing \
         was run. Restore this file only if you trust it, with client commands allowed.",
        path.display(),
        found.line,
        found.command,
        if psql { "psql" } else { "mysql" },
    )))
}

/// The first client command of the script [`check_script`] refuses, with
/// its line, or `None` when it has none: for the restore dialog, which
/// names it next to the opt-in. Blocking, like [`check_script`].
pub fn find_client_command(check: &ScriptCheck, cancel: &AtomicBool) -> Result<Option<Finding>> {
    let path = &check.path;
    let file = std::fs::File::open(path)
        .map_err(|error| Error::io(format!("could not open {}", path.display()), error))?;
    let psql = check.tool == Tool::Psql;
    let glued = psql && !check.version.as_ref().is_some_and(psql_needs_a_space);
    scan(psql, glued, file, path, cancel)
}

/// Whether this psql reads `\ofile` as the unknown command `ofile`, not as
/// `\o file`: from 8.4 on (commit 6f093f3825, "One-letter backslash
/// commands now need a space before the first argument", in the 8.4
/// release notes). Checked with psql 14.24 and 18.6: `\!touch x`,
/// `\o|cmd`, `\i/x.sql`, `\w/f`, `\s/f`, `\g|cmd`, `\cdb`, `\cd/dir`,
/// `\ir/x.sql`, `\efname`, `\copy(…)` are all "invalid command".
/// A version that does not name PostgreSQL (another tool's) proves nothing.
fn psql_needs_a_space(version: &ToolVersion) -> bool {
    version.text.contains("(PostgreSQL)") && version.number >= (8, 4, 0)
}

/// A client command found in a script, see [`find_client_command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// 1-based line number.
    pub line: u64,
    /// The command as written (`\!`, `system`…).
    pub command: String,
}

/// How much of the file [`scan`] reads at a time: all it holds of it.
const SCAN_CHUNK: usize = 64 * 1024;

/// psql meta-commands refused wherever they appear (psql 18's
/// `exec_command`): they run programs, read or write files, connect to
/// another server (with the password of the child's environment), or set
/// variables, whose value psql reads again as input when a later line
/// names it (`\set x …` then `:x` runs a command the file never spells).
/// `\copy` (any case) and every `\lo_…` are refused too.
const PSQL_REFUSED: [&str; 28] = [
    "!",
    "c",
    "connect",
    "cd",
    "e",
    "edit",
    "ef",
    "ev",
    "g",
    "gx",
    "gexec",
    "gset",
    "getenv",
    "i",
    "include",
    "ir",
    "include_relative",
    "o",
    "out",
    "password",
    "prompt",
    "pset",
    "s",
    "set",
    "setenv",
    "unset",
    "w",
    "write",
];

/// The other psql commands psql runs, harmless except that it hands
/// `` `…` `` in their arguments to the shell: a backquote after one of them
/// on its line is refused. Every name that does not start like a COPY data
/// escape (`\N`, `\t`, `\n`, `\123`, `\x41`…) is treated this way too, so a
/// command added by a later psql is covered; of those that do, these are
/// the ones that take arguments.
const PSQL_ARGUMENTS: [&str; 7] = ["bind", "bind_named", "f", "restrict", "t", "timing", "x"];

/// Bytes that start a COPY text-format escape.
const COPY_ESCAPE_STARTS: &[u8] = b"Nbfnrtvx01234567.";

/// mysql commands refused as the first word of a line (any case), and
/// their one-letter forms, refused anywhere (see the table in the module
/// documentation).
const MYSQL_REFUSED: [&str; 7] = [
    "system",
    "source",
    "tee",
    "pager",
    "edit",
    "connect",
    "ssl_session_data_print",
];
const MYSQL_REFUSED_SHORT: &[u8] = b"!.TPer";

/// psql's whitespace, which ends a command name.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// A short word kept in a fixed buffer; `cut` when it was longer.
#[derive(Clone, Copy)]
struct Word {
    bytes: [u8; 24],
    len: usize,
    cut: bool,
}

impl Word {
    fn new() -> Self {
        Self {
            bytes: [0; 24],
            len: 0,
            cut: false,
        }
    }

    fn push(&mut self, byte: u8) {
        match self.bytes.get_mut(self.len) {
            Some(slot) => {
                *slot = byte;
                self.len += 1;
            }
            None => self.cut = true,
        }
    }

    fn get(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    fn show(&self) -> String {
        let text = String::from_utf8_lossy(self.get());
        if self.cut {
            format!("{text}…")
        } else {
            text.into_owned()
        }
    }
}

/// The scan's state between two bytes.
///
/// Backslashes are read as the tools read them, wherever they are: `\\` is
/// one escaped pair (how COPY data and MySQL strings write a backslash), any
/// other backslash starts a command. In the multi-byte client encodings a
/// file may switch to (SJIS, BIG5, GBK, GB18030, UHC, JOHAB), the second
/// byte of a character can be `\`, after a first byte >= 0x80; such a
/// backslash is checked as a command start and never opens a pair, so
/// `\x81\x5c\x5c!` (a character, then `\!`) is caught. UTF-8 never has a
/// `\` inside a character: the cost is rare false alarms.
struct Scanner {
    psql: bool,
    /// psql: also refuse a refused one-letter command with its argument
    /// glued on (`\ofile`), which psql before 8.4 runs as `\o file`.
    glued: bool,
    line: u64,
    prev: u8,
    /// An unpaired backslash was the last byte; `ambiguous` when it came
    /// after a byte >= 0x80.
    backslash: Option<bool>,
    /// psql: the name of the command being read, and its line.
    name: Option<(u64, Word)>,
    /// psql: a command on this line whose arguments psql evaluates.
    arguments_of: Option<Word>,
    /// mysql: still in the leading whitespace of the line.
    line_start: bool,
    /// mysql: the first word of the line being read, after `high` leading
    /// bytes >= 0x80.
    first_word: Option<(usize, Word)>,
}

impl Scanner {
    fn new(psql: bool, glued: bool) -> Self {
        Self {
            psql,
            glued,
            line: 1,
            prev: b'\n',
            backslash: None,
            name: None,
            arguments_of: None,
            line_start: true,
            first_word: None,
        }
    }

    fn found(&self, line: u64, command: String) -> Option<Finding> {
        Some(Finding { line, command })
    }

    fn step(&mut self, byte: u8) -> Option<Finding> {
        let found = self.read(byte);
        self.prev = byte;
        found
    }

    fn read(&mut self, byte: u8) -> Option<Finding> {
        if let Some((line, mut name)) = self.name.take() {
            if !is_space(byte) && byte != b'\\' {
                name.push(byte);
                self.name = Some((line, name));
                return None;
            }
            if let Some(found) = self.end_name(line, name) {
                return Some(found);
            }
        }
        if let Some(ambiguous) = self.backslash.take() {
            if byte == b'\\' {
                // Unambiguous: an escaped pair, both bytes consumed. After
                // a high byte, the first may have been part of a character:
                // this one then starts a command.
                if ambiguous {
                    self.backslash = Some(false);
                }
                return None;
            }
            if self.psql {
                if !is_space(byte) {
                    let mut name = Word::new();
                    name.push(byte);
                    self.name = Some((self.line, name));
                    return None;
                }
            } else if MYSQL_REFUSED_SHORT.contains(&byte) {
                return self.found(self.line, format!("\\{}", char::from(byte)));
            }
        }
        if !self.psql
            && let Some(found) = self.first_word(byte)
        {
            return Some(found);
        }
        match byte {
            b'\\' => self.backslash = Some(self.prev >= 0x80),
            b'\n' => {
                self.line += 1;
                self.arguments_of = None;
                self.line_start = true;
            }
            b'`' => {
                if let Some(name) = self.arguments_of {
                    return self.found(
                        self.line,
                        format!("\\{} with a `…` shell command", name.show()),
                    );
                }
            }
            _ => {}
        }
        None
    }

    /// psql: the command `name` started on `line` has ended.
    fn end_name(&mut self, line: u64, name: Word) -> Option<Finding> {
        let text = name.get();
        let one_letter = |cmd: &&str| cmd.len() == 1 && cmd.as_bytes()[0] == text[0];
        let refused = !name.cut && PSQL_REFUSED.iter().any(|cmd| cmd.as_bytes() == text)
            || (self.glued && PSQL_REFUSED.iter().any(one_letter))
            || (!name.cut && text.eq_ignore_ascii_case(b"copy"))
            || text.starts_with(b"lo_");
        if refused {
            return self.found(line, format!("\\{}", name.show()));
        }
        let evaluates = !COPY_ESCAPE_STARTS.contains(&text[0])
            || (!name.cut && PSQL_ARGUMENTS.iter().any(|cmd| cmd.as_bytes() == text));
        if evaluates {
            self.arguments_of = Some(name);
        }
        None
    }

    /// mysql: follows the first word of the line, which mysql reads as a
    /// command name when it is one.
    fn first_word(&mut self, byte: u8) -> Option<Finding> {
        let word_byte =
            byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80;
        if let Some((mut high, mut word)) = self.first_word.take() {
            if !word_byte {
                return self.end_first_word(high, word);
            }
            if byte >= 0x80 && word.len == 0 {
                high += 1;
            } else {
                word.push(byte);
            }
            self.first_word = Some((high, word));
        } else if self.line_start && !is_space(byte) {
            self.line_start = false;
            if word_byte {
                let mut word = Word::new();
                let high = if byte >= 0x80 {
                    1
                } else {
                    word.push(byte);
                    0
                };
                self.first_word = Some((high, word));
            }
        }
        None
    }

    /// mysql: the first word of a line ended. mysql compares command names
    /// in Latin-1 without case, where accented letters can equal plain
    /// ones: a byte >= 0x80 matches any letter, and leading ones (a
    /// byte-order mark) are also tried as absent.
    fn end_first_word(&mut self, high: usize, word: Word) -> Option<Finding> {
        let body = word.get();
        let matches = |name: &[u8]| {
            body.len() == name.len()
                && body
                    .iter()
                    .zip(name)
                    .all(|(byte, letter)| *byte >= 0x80 || byte.to_ascii_lowercase() == *letter)
        };
        let refused = !word.cut
            && MYSQL_REFUSED.iter().any(|name| {
                let name = name.as_bytes();
                matches(name) || (high > 0 && high <= name.len() && matches(&name[high..]))
            });
        refused.then(|| Finding {
            line: self.line,
            command: word.show(),
        })
    }

    /// Whether the next byte matters only if it is `\`, a newline or `` ` ``.
    fn quiet(&self) -> bool {
        self.name.is_none()
            && self.backslash.is_none()
            && (self.psql || (!self.line_start && self.first_word.is_none()))
    }

    /// The end of the file.
    fn finish(&mut self) -> Option<Finding> {
        if let Some((line, name)) = self.name.take() {
            return self.end_name(line, name);
        }
        if let Some((high, word)) = self.first_word.take() {
            return self.end_first_word(high, word);
        }
        None
    }
}

/// Reads `reader` to its end (or to the first client command) in
/// [`SCAN_CHUNK`] pieces.
fn scan(
    psql: bool,
    glued: bool,
    mut reader: impl Read,
    path: &Path,
    cancel: &AtomicBool,
) -> Result<Option<Finding>> {
    let mut scanner = Scanner::new(psql, glued);
    let mut chunk = [0u8; SCAN_CHUNK];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let read = match reader.read(&mut chunk) {
            Ok(0) => return Ok(scanner.finish()),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(Error::io(
                    format!("could not read {}", path.display()),
                    error,
                ));
            }
        };
        let mut rest = &chunk[..read];
        while let Some((&byte, after)) = rest.split_first() {
            // Between commands only these bytes change anything.
            if scanner.quiet() {
                let skip = rest
                    .iter()
                    .position(|&byte| matches!(byte, b'\\' | b'\n' | b'`'))
                    .unwrap_or(rest.len());
                if skip > 0 {
                    scanner.prev = rest[skip - 1];
                    rest = &rest[skip..];
                    continue;
                }
            }
            if let Some(found) = scanner.step(byte) {
                return Ok(Some(found));
            }
            rest = after;
        }
    }
}

/// What a saved task runs: one wizard's tool and its settings (DBeaver's
/// `TaskImpl` type and properties, `org.jkiss.dbeaver.registry.task`).
/// None of the four structs holds a password: an authentication override
/// keeps its user only, and its password is asked at each run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "settings", rename_all = "snake_case")]
pub enum TaskSettings {
    Backup(BackupOptions),
    GlobalBackup(GlobalBackupOptions),
    Restore(RestoreOptions),
    ExecuteScript(ScriptOptions),
}

impl TaskSettings {
    /// The stored name of the kind.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Backup(_) => "backup",
            Self::GlobalBackup(_) => "global_backup",
            Self::Restore(_) => "restore",
            Self::ExecuteScript(_) => "execute_script",
        }
    }

    /// The kind, as the Tasks view shows it.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Backup(_) => "Backup",
            Self::GlobalBackup(_) => "Global backup",
            Self::Restore(_) => "Restore",
            Self::ExecuteScript(_) => "Execute script",
        }
    }

    /// The database the task works on; `None` for a whole server.
    pub fn database(&self) -> Option<&str> {
        match self {
            Self::Backup(options) => Some(&options.database),
            Self::Restore(options) => Some(&options.database),
            Self::ExecuteScript(options) => Some(&options.database),
            Self::GlobalBackup(_) => None,
        }
    }

    /// The user of the authentication override, whose password is asked at
    /// each run.
    pub fn auth_user(&self) -> Option<&str> {
        match self {
            Self::Backup(options) => options.auth_user.as_deref(),
            Self::GlobalBackup(options) => options.auth_user.as_deref(),
            Self::Restore(options) => options.auth_user.as_deref(),
            Self::ExecuteScript(options) => options.auth_user.as_deref(),
        }
    }

    /// Whether a run writes to the database: it is confirmed each time.
    pub fn writes(&self) -> bool {
        matches!(self, Self::Restore(_) | Self::ExecuteScript(_))
    }

    /// The files the task reads or writes, each with what it is, the empty
    /// ones left out: shown before an imported task is saved, checked where
    /// they are used (K69, D-138).
    pub fn files(&self) -> Vec<(&'static str, &Path)> {
        let file = match self {
            Self::Backup(options) => ("output folder", options.output_folder.as_path()),
            Self::GlobalBackup(options) => ("output folder", options.output_folder.as_path()),
            Self::Restore(options) => ("backup file", options.input.as_path()),
            Self::ExecuteScript(options) => ("script", options.input.as_path()),
        };
        [file]
            .into_iter()
            .filter(|(_, path)| !path.as_os_str().is_empty())
            .collect()
    }

    /// What the task does with its files, in words, for an import's
    /// preview: an imported task that restores a file of the sender's
    /// choosing into a database shows that file before it is saved. On
    /// Windows a file Kerno will not open is said too. `None` without files.
    pub fn files_note(&self) -> Option<String> {
        let (_, path) = *self.files().first()?;
        let database = self.database().unwrap_or_default();
        let mut note = match self {
            Self::Backup(_) | Self::GlobalBackup(_) => {
                format!("Writes its backups to {}", path.display())
            }
            Self::Restore(_) => format!("Restores {} into “{database}”", path.display()),
            Self::ExecuteScript(_) => format!("Runs {} in “{database}”", path.display()),
        };
        for (what, path) in self.files() {
            if let Some(refused) = crate::local_path::note(path, what) {
                note.push_str(&format!(". Note: {refused}"));
            }
        }
        Some(format!("{note}."))
    }

    /// Refuses what a task must not keep: extra arguments the tool refuses,
    /// a password among them ([`extra_args`]).
    pub fn check(&self, engine: Engine) -> Result<()> {
        let extra = match self {
            Self::Backup(options) => &options.extra_args,
            Self::GlobalBackup(options) => &options.extra_args,
            Self::Restore(options) => &options.extra_args,
            Self::ExecuteScript(options) => &options.extra_args,
        };
        extra_args(extra, ClientFamily::required(engine)?).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// K69: a task's file of another computer stops the run before the tool
    /// starts or anything opens it. Mutation seen: the loop over `files` in
    /// `run` removed (whoami.exe runs).
    #[cfg(windows)]
    #[test]
    fn a_task_file_of_another_computer_stops_the_run() {
        let command = ToolCommand {
            program: r"C:\Windows\System32\whoami.exe".into(),
            args: Vec::new(),
            env: Vec::new(),
            password: None,
            stdin: None,
            stdin_parts: Vec::new(),
            sent: Arc::default(),
            tunnel: None,
            script_check: None,
            files: vec![r"\\k69-nowhere\share\shop.sql".into()],
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(run(command, |_| {}, Arc::default()))
            .unwrap_err();
        assert!(
            error.to_string().contains("not a file of this computer"),
            "{error}"
        );
    }

    /// K69: a System32 program only under exactly `C:\Windows`.
    #[test]
    fn a_system_program_is_taken_from_c_windows_only() {
        let ssh = Some(PathBuf::from(r"C:\Windows\System32\OpenSSH\ssh.exe"));
        for root in [r"C:\Windows", r"c:\WINDOWS\", "C:\\Windows/"] {
            assert_eq!(
                system_program(Some(root), r"OpenSSH\ssh.exe"),
                ssh,
                "{root}"
            );
        }
        for root in [
            r"D:\Windows",
            "Windows",
            r"\\host\share\Windows",
            "",
            r"C:\Windows\..\Users\ada",
            r"C:\WinNT",
        ] {
            assert_eq!(
                system_program(Some(root), r"OpenSSH\ssh.exe"),
                None,
                "{root}"
            );
        }
        assert_eq!(system_program(None, "curl.exe"), None);
    }
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::sync::Mutex;
    use std::time::Instant;

    const PASSWORD: &str = "hunter2-tool-secret";

    fn params(engine: Engine) -> ConnectParams {
        ConnectParams {
            engine,
            host: "db.example.com".into(),
            port: 6543,
            tunnel: None,
            user: "ada".into(),
            password: Some(Secret::new(PASSWORD)),
            database: Some("ignored".into()),
            file: None,
            tls: TlsConfig::default(),
            read_only: false,
            options: BTreeMap::new(),
        }
    }

    const OUT: &str = "/tmp/out dir/shop.sql";

    /// A PostgreSQL plain-SQL backup of `database`.
    fn backup(database: &str) -> BackupOptions {
        BackupOptions {
            format: BackupFormat::Sql,
            ..BackupOptions::new(Engine::Postgres, database)
        }
    }

    fn my_backup(database: &str) -> BackupOptions {
        BackupOptions::new(Engine::MySql, database)
    }

    /// The backup command, written to [`OUT`].
    fn dump(tool: &Path, params: &ConnectParams, options: &BackupOptions) -> Result<ToolCommand> {
        backup_command(tool, params, options, Path::new(OUT))
    }

    fn restore(format: BackupFormat) -> RestoreOptions {
        RestoreOptions::new(Engine::Postgres, "shop_copy", "/backups/shop.dump", format)
    }

    fn my_restore() -> RestoreOptions {
        RestoreOptions::new(
            Engine::MySql,
            "shop_copy",
            "/backups/shop.dump",
            BackupFormat::Sql,
        )
    }

    fn assert_no_secret(command: &ToolCommand) {
        let shown = format!("{} {command:?}", command.display());
        assert!(!shown.contains(PASSWORD), "{shown}");
    }

    #[test]
    fn pg_dump_command_lines() {
        let pg_dump = Path::new("/opt/homebrew/bin/pg_dump");
        let command = dump(pg_dump, &params(Engine::Postgres), &backup("shop")).unwrap();
        assert_eq!(
            command.display(),
            "PGSSLMODE=prefer PGPASSWORD=••• /opt/homebrew/bin/pg_dump --no-password \
             --host=db.example.com --port=6543 --username=ada --verbose --dbname=shop \
             '--file=/tmp/out dir/shop.sql' --format=p"
        );
        assert_eq!(
            command
                .password
                .as_ref()
                .map(|(var, secret)| (*var, secret.expose())),
            Some(("PGPASSWORD", PASSWORD))
        );
        assert_no_secret(&command);

        // Windows opens a drive's path only (K69, D-138).
        let certs = if cfg!(windows) { "C:/certs" } else { "/certs" };
        let mut params = params(Engine::Postgres);
        params.tls = TlsConfig {
            mode: TlsMode::VerifyFull,
            ca_file: Some(format!("{certs}/ca.pem").into()),
            cert_file: Some(format!("{certs}/client.pem").into()),
            key_file: Some(format!("{certs}/client.key").into()),
        };
        let options = BackupOptions {
            format: BackupFormat::Custom,
            content: BackupContent::SchemaOnly,
            tables: vec![
                TableRef::new("public", "orders"),
                TableRef::new("Sales", "odd\"name"),
            ],
            ..backup("shop")
        };
        let command = dump(pg_dump, &params, &options).unwrap();
        assert_eq!(
            command.display(),
            "PGSSLMODE=verify-full PGSSLROOTCERT=/certs/ca.pem PGSSLCERT=/certs/client.pem \
             PGSSLKEY=/certs/client.key PGPASSWORD=••• /opt/homebrew/bin/pg_dump --no-password \
             --host=db.example.com --port=6543 --username=ada --verbose --dbname=shop \
             '--file=/tmp/out dir/shop.sql' --format=c --schema-only --strict-names \
             '--table=\"public\".\"orders\"' '--table=\"Sales\".\"odd\"\"name\"'"
                .replace("/certs", certs)
        );

        let options = BackupOptions {
            content: BackupContent::DataOnly,
            ..backup("a=b host=evil")
        };
        let command = dump(pg_dump, &params, &options).unwrap();
        assert!(
            command.args.contains(&"--data-only".to_string()),
            "{:?}",
            command.args
        );
        assert!(
            command
                .args
                .contains(&r"--dbname=dbname='a=b host=evil'".to_string()),
            "{:?}",
            command.args
        );
    }

    #[test]
    fn pg_restore_command_lines() {
        let command = restore_command(
            Path::new("/usr/bin/psql"),
            &params(Engine::Postgres),
            &restore(BackupFormat::Sql),
        )
        .unwrap();
        assert_eq!(
            command.display(),
            "PGSSLMODE=prefer PGPASSWORD=••• /usr/bin/psql --no-password --host=db.example.com \
             --port=6543 --username=ada --no-psqlrc --dbname=shop_copy --set=ON_ERROR_STOP=1 \
             --single-transaction --file=- < /backups/shop.dump"
        );
        assert_eq!(
            command.script_check,
            Some(ScriptCheck {
                tool: Tool::Psql,
                path: "/backups/shop.dump".into(),
                version: None,
            })
        );
        assert_no_secret(&command);

        let command = restore_command(
            Path::new("/usr/bin/pg_restore"),
            &params(Engine::Postgres),
            &restore(BackupFormat::Custom),
        )
        .unwrap();
        assert_eq!(
            command.display(),
            "PGSSLMODE=prefer PGPASSWORD=••• /usr/bin/pg_restore --no-password \
             --host=db.example.com --port=6543 --username=ada --verbose --dbname=shop_copy \
             --format=c --single-transaction /backups/shop.dump"
        );
        // A custom-format archive holds no client script.
        assert_eq!(command.script_check, None);
    }

    #[test]
    fn mysql_command_lines() {
        let mysqldump = Path::new("/usr/bin/mysqldump");
        let options = BackupOptions {
            tables: vec![
                TableRef::new("shop", "orders"),
                TableRef::new("shop", "items"),
            ],
            ..my_backup("shop")
        };
        let command = dump(mysqldump, &params(Engine::MySql), &options).unwrap();
        assert_eq!(
            command.display(),
            "MYSQL_PWD=••• /usr/bin/mysqldump --no-defaults --protocol=TCP --host=db.example.com \
             --port=6543 --user=ada --ssl-mode=PREFERRED --verbose --single-transaction --routines \
             --hex-blob '--result-file=/tmp/out dir/shop.sql' shop orders items"
        );
        assert_no_secret(&command);

        let mut params_maria = params(Engine::MariaDb);
        // Windows opens a drive's path only (K69, D-138).
        let certs = if cfg!(windows) { "C:/certs" } else { "/certs" };
        params_maria.tls = TlsConfig {
            mode: TlsMode::VerifyCa,
            ca_file: Some(format!("{certs}/ca.pem").into()),
            ..TlsConfig::default()
        };
        let options = BackupOptions {
            content: BackupContent::DataOnly,
            ..my_backup("shop")
        };
        let command = dump(Path::new("/usr/bin/mariadb-dump"), &params_maria, &options).unwrap();
        assert_eq!(
            command.display(),
            "MYSQL_PWD=••• /usr/bin/mariadb-dump --no-defaults --protocol=TCP \
             --host=db.example.com --port=6543 --user=ada --ssl --ssl-verify-server-cert \
             --ssl-ca=/certs/ca.pem --verbose --single-transaction --no-create-info \
             --skip-triggers --hex-blob '--result-file=/tmp/out dir/shop.sql' shop"
                .replace("/certs", certs)
        );

        let options = BackupOptions {
            content: BackupContent::SchemaOnly,
            ..my_backup("shop")
        };
        let command = dump(mysqldump, &params(Engine::MySql), &options).unwrap();
        assert!(command.args.ends_with(&[
            "--no-data".into(),
            "--routines".into(),
            "--hex-blob".into(),
            "--result-file=/tmp/out dir/shop.sql".into(),
            "shop".into()
        ]));

        let command = restore_command(
            Path::new("/usr/bin/mysql"),
            &params(Engine::MySql),
            &my_restore(),
        )
        .unwrap();
        assert_eq!(
            command.display(),
            "MYSQL_PWD=••• /usr/bin/mysql --no-defaults --protocol=TCP --host=db.example.com \
             --port=6543 --user=ada --ssl-mode=PREFERRED --database=shop_copy --local-infile=0 \
             < /backups/shop.dump"
        );
        // Version unknown: the file is checked before mysql starts.
        assert_eq!(
            command.script_check,
            Some(ScriptCheck {
                tool: Tool::Mysql,
                path: "/backups/shop.dump".into(),
                version: None,
            })
        );
        assert_no_secret(&command);
    }

    #[test]
    fn a_verbatim_drive_path_reaches_the_tools_plain() {
        let verbatim = Path::new(r"\\?\C:\out\k48");
        let expected = if cfg!(windows) {
            PathBuf::from(r"C:\out\k48")
        } else {
            verbatim.to_path_buf()
        };
        assert_eq!(plain_path(verbatim), expected);
        let unc = Path::new(r"\\?\UNC\host\share\k48");
        assert_eq!(plain_path(unc), unc);
        assert_eq!(plain_path(Path::new("/tmp/out")), Path::new("/tmp/out"));
        // What pg_dump is given for a folder.
        let command = backup_command(
            Path::new("pg_dump"),
            &crate::testing::params(Engine::Postgres),
            &BackupOptions {
                format: BackupFormat::Directory,
                ..BackupOptions::new(Engine::Postgres, "shop")
            },
            verbatim,
        )
        .unwrap();
        let file = format!("--file={}", expected.display());
        assert!(command.args.contains(&file), "{:?}", command.args);
        // And what pg_restore reads it back from.
        let command = restore_command(
            Path::new("pg_restore"),
            &crate::testing::params(Engine::Postgres),
            &RestoreOptions::new(
                Engine::Postgres,
                "shop",
                verbatim.to_path_buf(),
                BackupFormat::Directory,
            ),
        )
        .unwrap();
        assert_eq!(
            command.args.last().map(String::as_str),
            Some(expected.to_str().unwrap())
        );
    }

    #[test]
    fn versions_are_read_from_each_tool() {
        let cases = [
            ("psql (PostgreSQL) 18.6\n", (18, 6, 0), false),
            ("pg_dump (PostgreSQL) 14.24 (Homebrew)", (14, 24, 0), false),
            (
                "pg_dump (PostgreSQL) 17.2 (Debian 17.2-1.pgdg120+1)",
                (17, 2, 0),
                false,
            ),
            (
                "/opt/homebrew/opt/mysql-client/bin/mysql  Ver 26.10.0 for macos27.0 on arm64 \
                 (Homebrew)",
                (26, 10, 0),
                false,
            ),
            (
                "C:\\Program Files\\MySQL\\MySQL Server 8.4\\bin\\mysql.exe  Ver 8.4.13 for Win64 on \
                 x86_64 (MySQL Community Server - GPL)",
                (8, 4, 13),
                false,
            ),
            (
                "mysql  Ver 14.14 Distrib 5.7.44, for osx10.18 (x86_64) using  EditLine wrapper",
                (5, 7, 44),
                false,
            ),
            (
                "mysql  Ver 15.1 Distrib 10.6.18-MariaDB, for debian-linux-gnu (x86_64) using \
                 readline 5.2",
                (10, 6, 18),
                true,
            ),
            (
                "mariadb from 11.8.9-MariaDB, client 15.2 for debian-linux-gnu (aarch64) using  \
                 EditLine wrapper",
                (11, 8, 9),
                true,
            ),
        ];
        for (text, number, mariadb) in cases {
            let version = parse_version(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(
                (version.number, version.mariadb),
                (number, mariadb),
                "{text}"
            );
            assert_eq!(version.text, text.trim());
        }
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("usage: tool [options]"), None);
        // A binary that does not run has no version.
        assert_eq!(version(Path::new("/nonexistent/psql")), None);
    }

    #[test]
    fn the_sandbox_option_follows_the_client_version() {
        let option =
            |text: &str| sandbox_option(&parse_version(text).unwrap()).map(|all| all.join(" "));
        for (text, expected) in [
            ("mysql  Ver 8.0.42 for Linux", None),
            ("mysql  Ver 8.0.43 for Linux", Some("--commands=OFF")),
            ("mysql  Ver 8.3.0 for Linux", None),
            ("mysql  Ver 8.4.5 for Linux", None),
            ("mysql  Ver 8.4.6 for Linux", Some("--commands=OFF")),
            ("mysql  Ver 9.3.0 for Linux", None),
            ("mysql  Ver 9.4.0 for Linux", Some("--commands=OFF")),
            ("mysql  Ver 26.10.0 for macos27.0", Some("--commands=OFF")),
            ("mysql  Ver 14.14 Distrib 5.7.44, for osx", None),
            ("mysql  Ver 15.1 Distrib 10.5.25-MariaDB, for x", None),
            ("mysql  Ver 15.1 Distrib 10.6.17-MariaDB, for x", None),
            (
                "mysql  Ver 15.1 Distrib 10.6.18-MariaDB, for x",
                Some("--sandbox --binary-mode"),
            ),
            (
                "mariadb from 10.11.8-MariaDB, client 15.2",
                Some("--sandbox --binary-mode"),
            ),
            ("mariadb from 11.3.2-MariaDB, client 15.2", None),
            ("mariadb from 11.4.1-MariaDB, client 15.2", None),
            (
                "mariadb from 11.4.2-MariaDB, client 15.2",
                Some("--sandbox --binary-mode"),
            ),
            (
                "mariadb from 11.8.9-MariaDB, client 15.2",
                Some("--sandbox --binary-mode"),
            ),
            (
                "mariadb from 12.0.1-MariaDB, client 15.2",
                Some("--sandbox --binary-mode"),
            ),
        ] {
            assert_eq!(option(text).as_deref(), expected, "{text}");
        }
    }

    #[test]
    fn a_restore_uses_the_client_sandbox_or_a_check_unless_allowed() {
        let restore_with = |engine, version: &str, allow| {
            let options = RestoreOptions {
                tool_version: parse_version(version),
                allow_client_commands: allow,
                ..my_restore()
            };
            restore_command(Path::new("/usr/bin/mysql"), &params(engine), &options).unwrap()
        };
        let input = PathBuf::from("/backups/shop.dump");

        let command = restore_with(Engine::MySql, "mysql  Ver 26.10.0 for macos27.0", false);
        assert!(
            command
                .args
                .ends_with(&["--local-infile=0".into(), "--commands=OFF".into()])
        );
        assert_eq!(command.script_check, None);
        let command = restore_with(
            Engine::MariaDb,
            "mariadb from 11.8.9-MariaDB, client",
            false,
        );
        assert!(command.args.ends_with(&[
            "--local-infile=0".into(),
            "--sandbox".into(),
            "--binary-mode".into()
        ]));
        assert_eq!(command.script_check, None);
        let command = restore_with(Engine::MySql, "mysql  Ver 8.0.36 for Linux", false);
        assert!(command.args.ends_with(&["--local-infile=0".into()]));
        assert_eq!(
            command.script_check,
            Some(ScriptCheck {
                tool: Tool::Mysql,
                path: input.clone(),
                version: parse_version("mysql  Ver 8.0.36 for Linux"),
            })
        );

        // The opt-in: neither the option nor the check.
        for version in [
            "mysql  Ver 26.10.0 for macos27.0",
            "mysql  Ver 8.0.36 for Linux",
        ] {
            let command = restore_with(Engine::MySql, version, true);
            assert!(
                command.args.ends_with(&["--local-infile=0".into()]),
                "{version}"
            );
            assert_eq!(command.script_check, None);
        }
        let options = RestoreOptions {
            allow_client_commands: true,
            ..restore(BackupFormat::Sql)
        };
        let command = restore_command(
            Path::new("/usr/bin/psql"),
            &params(Engine::Postgres),
            &options,
        )
        .unwrap();
        assert_eq!(command.script_check, None);
        assert!(command.args.contains(&"--file=-".into()));
        assert_eq!(command.stdin, Some(PathBuf::from("/backups/shop.dump")));
    }

    /// What [`scan`] finds in `text`: line and command.
    /// psql known to be 8.4 or later: glued arguments are not looked at.
    fn found(psql: bool, text: &[u8]) -> Option<(u64, String)> {
        scan_with(psql, false, text)
    }

    fn scan_with(psql: bool, glued: bool, text: &[u8]) -> Option<(u64, String)> {
        scan(
            psql,
            glued,
            text,
            Path::new("test.sql"),
            &AtomicBool::new(false),
        )
        .unwrap()
        .map(|found| (found.line, found.command))
    }

    /// The psql rule used for `version`, through `check_script`.
    fn found_for(version: Option<&str>, text: &str) -> Option<(u64, String)> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dump.sql");
        std::fs::write(&path, text).unwrap();
        let check = ScriptCheck {
            tool: Tool::Psql,
            path,
            version: version.map(|text| parse_version(text).unwrap()),
        };
        match check_script(&check, &AtomicBool::new(false)) {
            Ok(()) => None,
            Err(error) => {
                let text = error.to_string();
                let (_, after) = text.split_once(", line ").unwrap();
                let (line, rest) = after.split_once(": ").unwrap();
                let (command, _) = rest.split_once(" is a psql").unwrap();
                Some((line.parse().unwrap(), command.to_string()))
            }
        }
    }

    #[test]
    fn glued_arguments_are_refused_unless_psql_is_known_to_need_a_space() {
        let glued = [
            ("\\!touch /tmp/x", "\\!touch"),
            ("SELECT 1 \\o|touch /tmp/x", "\\o|touch"),
            ("\\i/tmp/x.sql", "\\i/tmp/x.sql"),
            ("\\ir/tmp/x.sql", "\\ir/tmp/x.sql"),
            ("SELECT 1 \\w/tmp/f", "\\w/tmp/f"),
            ("\\s/tmp/f", "\\s/tmp/f"),
            ("SELECT 1 \\g|touch /tmp/x", "\\g|touch"),
            ("SELECT 1 \\gx|touch /tmp/x", "\\gx|touch"),
            ("\\cdb", "\\cdb"),
            ("\\cd/tmp", "\\cd/tmp"),
            ("\\efname", "\\efname"),
            ("\\copy(SELECT 1) TO PROGRAM 'x'", "\\copy(SELECT"),
            ("\\setx 1", "\\setx"),
            ("\\o\\! touch /tmp/x", "\\o"),
        ];
        let regex = "CREATE DOMAIN email AS text CHECK ((VALUE ~ '^[^@\\s]+@[^@\\s]+$'::text));";
        for version in [
            None,
            Some("psql (PostgreSQL) 8.3.23"),
            Some("mysql  Ver 26.10.0 for x"),
        ] {
            for (text, command) in glued {
                let text = format!("SET a = 1;\n{text}\n");
                assert_eq!(
                    found_for(version, &text),
                    Some((2, command.to_string())),
                    "{version:?} {text}"
                );
            }
            assert_eq!(
                found_for(version, &format!("SELECT 1;\n{regex}\n")),
                Some((2, "\\s]+@[^@".to_string()))
            );
        }
        for version in [
            "psql (PostgreSQL) 8.4.0",
            "psql (PostgreSQL) 14.24 (Homebrew)",
            "psql (PostgreSQL) 18.6",
        ] {
            for (text, _) in glued.iter().filter(|(text, _)| !text.contains("\\o\\")) {
                assert_eq!(
                    found_for(Some(version), &format!("{text}\n")),
                    None,
                    "{version} {text}"
                );
            }
            assert_eq!(found_for(Some(version), regex), None);
            // A refused command with a space is refused whatever the version.
            assert_eq!(
                found_for(Some(version), "SELECT 1;\n\\o /tmp/x\n"),
                Some((2, "\\o".into()))
            );
        }
        // COPY data escapes start with none of those letters.
        assert_eq!(
            scan_with(
                true,
                true,
                b"1\tC:\\\\x\\tab\\N\\n\\r\\b\\f\\v\\101\\x41\n\\.\n"
            ),
            None
        );
    }

    #[test]
    fn psql_commands_that_reach_the_computer_are_found_with_their_line() {
        let commands =
            PSQL_REFUSED
                .iter()
                .copied()
                .chain(["copy", "COPY", "lo_export", "lo_import"]);
        for command in commands {
            let expected = Some((3, format!("\\{command}")));
            for text in [
                format!("SET a = 1;\n\nSELECT 1 \\{command} /tmp/x\n"),
                format!("SET a = 1;\n\n\\{command} /tmp/x\nSELECT 2;\n"),
                format!("SET a = 1;\n\n  \\{command}\n"),
                format!("SET a = 1;\n\nSELECT 1;\\{command}"),
                format!("SET a = 1;\n\nSELECT 1;\\{command}\\\\"),
            ] {
                assert_eq!(found(true, text.as_bytes()), expected, "{text:?}");
            }
        }
        assert_eq!(
            found(true, b"SELECT 1 \\g |touch /tmp/x\n"),
            Some((1, "\\g".into()))
        );
        // After an escaped pair, a backslash starts a command again.
        assert_eq!(
            found(true, b"SELECT '\\\\\\! touch x';\n"),
            Some((1, "\\!".into()))
        );
    }

    #[test]
    fn psql_backquotes_after_a_command_are_found() {
        for text in [
            "\\encoding `touch /tmp/x`\n",
            "\\restrict `touch /tmp/x`key\n",
            "\\t `touch /tmp/x`\n",
            "\\dt+ `touch /tmp/x`\n",
            "\\if `touch /tmp/x`\n",
            "\\echo hello `touch /tmp/x`\n",
            "\\some_future_command `touch /tmp/x`\n",
        ] {
            let found = found(true, text.as_bytes());
            assert!(
                found
                    .as_ref()
                    .is_some_and(|(line, command)| *line == 1 && command.contains("shell command")),
                "{text:?}: {found:?}"
            );
        }
        // psql runs no backquote after a command without arguments, after
        // an unknown one, or in COPY data.
        for text in [
            "\\r `touch /tmp/x`\n",
            "\\N\t`a`\n",
            "a\\tb `code`\n",
            "line\\r\\nwith `code`\n",
            "SELECT '`';\n",
        ] {
            assert_eq!(found(true, text.as_bytes()), None, "{text:?}");
        }
    }

    #[test]
    fn a_multibyte_character_cannot_hide_a_backslash() {
        // In SJIS, 0x81 0x5c is one character: psql then reads `\!`.
        assert_eq!(
            found(true, b"\\encoding SJIS\nSELECT 1 \x81\x5c\x5c! touch m\n"),
            Some((2, "\\!".into()))
        );
        // 0x81 0x81 is one character: the backslash after it is real.
        assert_eq!(
            found(true, b"\\encoding SJIS\nSELECT 1 \x81\x81\x5c! touch m\n"),
            Some((2, "\\!".into()))
        );
        assert_eq!(
            found(false, b"SET NAMES sjis;\nSELECT 1; \x81\x5c\x5c! touch m\n"),
            Some((2, "\\!".into()))
        );
        // UTF-8 text before an escaped backslash in COPY data.
        assert_eq!(
            found(true, "caf\u{e9}\\\\x\tna\u{ef}ve\\\\n\n".as_bytes()),
            None
        );
    }

    #[test]
    fn what_pg_dump_writes_is_not_flagged() {
        let dump = "--\n-- PostgreSQL database dump\n--\n\n\\restrict Ab12cd\n\n\
            SET standard_conforming_strings = on;\n\\encoding UTF8\n\
            CREATE DOMAIN email AS text CHECK ((VALUE ~ '^[^@\\s]+@[^@\\s]+$'::text));\n\
            CREATE FUNCTION f() RETURNS text LANGUAGE sql AS $$ SELECT '\\d+\\w*' $$;\n\
            COPY public.notes (id, body) FROM stdin;\n\
            1\tC:\\\\Users\\\\x\\\\out.txt\n\
            2\ttab\\there\\nnew line\\r\\n\\\\! not a command \\\\o x\n\
            3\t\\N\n\
            4\t\\\\.\\\\set x\\\\\\\\\n\
            5\t\\x41\\101\\b\\f\\v caf\u{e9}\n\
            \\.\n\
            SELECT pg_catalog.setval('public.notes_id_seq', 5, true);\n\n\
            \\unrestrict Ab12cd\n";
        assert_eq!(found(true, dump.as_bytes()), None);
    }

    #[test]
    fn a_file_that_fakes_the_pg_dump_markers_is_still_refused() {
        let text = "\\restrict x\nSELECT 1;\n\\unrestrict x\n\\! touch /tmp/pwned\n";
        assert_eq!(found(true, text.as_bytes()), Some((4, "\\!".into())));
        // Setting a variable that a later line runs.
        let text = "\\restrict x\n\\set cmd '\\\\! touch /tmp/pwned'\nSELECT :cmd;\n";
        assert_eq!(found(true, text.as_bytes()), Some((2, "\\set".into())));
        let text = "SELECT chr(92) || '! touch /tmp/pwned' AS cmd \\gset\n:cmd\n";
        assert_eq!(found(true, text.as_bytes()), Some((1, "\\gset".into())));
    }

    #[test]
    fn mysql_commands_that_reach_the_computer_are_found_with_their_line() {
        for (text, expected) in [
            ("SELECT 1;\nsystem touch /tmp/x\n", (2, "system")),
            ("SELECT 1;\n  SYSTEM touch /tmp/x\n", (2, "SYSTEM")),
            ("SELECT 1;\n\tsource /tmp/x.sql\n", (2, "source")),
            ("SELECT 1;\ntee /tmp/out\n", (2, "tee")),
            ("SELECT 1;\npager;\n", (2, "pager")),
            ("SELECT 1;\nedit\n", (2, "edit")),
            ("SELECT 1;\nconnect db evil.example\n", (2, "connect")),
            ("SELECT 1;\nsystem", (2, "system")),
            ("SELECT 1;\n\\! touch /tmp/x\n", (2, "\\!")),
            ("SELECT 1; \\! touch /tmp/x\n", (1, "\\!")),
            ("SELECT 1 \\. /tmp/x.sql\n", (1, "\\.")),
            ("/*!40101 \\T /tmp/out */;\n", (1, "\\T")),
            ("SELECT 1;\n\\P cat\n", (2, "\\P")),
            ("SELECT 1\\e\n", (1, "\\e")),
            ("SELECT '\\\\\\! x';\n", (1, "\\!")),
            // connect: to another host, with the password the client holds.
            ("SELECT 1;\n\\r kerno evil.example\n", (2, "\\r")),
            ("SELECT 1; \\r kerno evil.example\n", (1, "\\r")),
            ("SELECT 1;\n\nconnect kerno evil.example\n", (3, "connect")),
            ("SELECT 1;\nCONNECT kerno evil.example;\n", (2, "CONNECT")),
            (
                "SELECT 1;\nssl_session_data_print /tmp/x\n",
                (2, "ssl_session_data_print"),
            ),
        ] {
            assert_eq!(
                found(false, text.as_bytes()),
                Some((expected.0, expected.1.into())),
                "{text:?}"
            );
        }
        // A byte-order mark, and a Latin-1 letter mysql may take for a plain
        // one.
        assert_eq!(
            found(false, b"\xef\xbb\xbfsystem touch x\n"),
            Some((1, "system".into()))
        );
        assert!(found(false, b"syst\xe9m touch x\n").is_some());
    }

    #[test]
    fn what_mysqldump_writes_is_not_flagged() {
        let dump = "/*M!999999\\- enable the sandbox mode */ \n\
            -- MySQL dump 10.13  Distrib 26.10.0, for macos27.0 (arm64)\n\
            /*!40101 SET NAMES utf8mb4 */;\n\
            INSERT INTO `notes` VALUES (1,'C:\\\\Users\\\\x \\\\! \\\\. \\\\T'),\
            (2,'Multi-line\\ntext with \\\"quotes\\\", it\\'s \\n \\0 \\Z caf\u{e9}');\n\
            INSERT INTO `blobs` VALUES (_binary '\u{de}\\\\x');\n\
            DELIMITER ;;\n\
            /*!50003 CREATE*/ /*!50003 TRIGGER `t` BEFORE INSERT ON `notes` FOR EACH ROW BEGIN\n\
              SET NEW.body = CONCAT(NEW.body, '\\\\');\n\
              system_count: LOOP LEAVE system_count; END LOOP;\n\
            END */;;\n\
            DELIMITER ;\n\
            USE `kerno`;\n\
            source_id INT,\n\
            -- Dump completed\n";
        assert_eq!(found(false, dump.as_bytes()), None);
        // The known false alarm: a carriage return in a string is written
        // `\r`, the short form of `connect`.
        assert_eq!(
            found(
                false,
                b"INSERT INTO t VALUES (1);\nINSERT INTO t VALUES (2,'a\\r\\nb');\n"
            ),
            Some((2, "\\r".into()))
        );
        // Bytes >= 0x80 before a backslash that mysqldump writes in binary
        // strings.
        assert_eq!(
            found(
                false,
                b"INSERT INTO b VALUES (_binary '\xde\xad\xbe\\\xef\\0\xff');\n"
            ),
            None
        );
    }

    /// Unix: the clean script's tool is a `/bin/sh` script.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_script_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let pwned = dir.path().join("pwned");
        let started = dir.path().join("started");
        let script = dir.path().join("dump.sql");
        std::fs::write(
            &script,
            format!("SELECT 1;\n\\! touch {}\n", pwned.display()),
        )
        .unwrap();
        // The "tool" would leave a mark if it started.
        let mut command = shell(&format!("touch {}", started.display()));
        command.script_check = Some(ScriptCheck {
            tool: Tool::Psql,
            path: script.clone(),
            version: None,
        });
        let error = run(command, |_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(matches!(error, Error::Config(_)), "{error:?}");
        assert!(
            text.contains("dump.sql, line 2: \\! is a psql client command"),
            "{text}"
        );
        assert!(!started.exists() && !pwned.exists());
        // What the dialog shows next to the opt-in.
        let check = ScriptCheck {
            tool: Tool::Psql,
            path: script.clone(),
            version: None,
        };
        assert_eq!(
            find_client_command(&check, &AtomicBool::new(false)).unwrap(),
            Some(Finding {
                line: 2,
                command: "\\!".into()
            })
        );

        // A clean script: the tool starts.
        std::fs::write(&script, "SELECT 1;\n").unwrap();
        let mut command = shell(&format!("touch {}", started.display()));
        command.script_check = Some(ScriptCheck {
            tool: Tool::Mysql,
            path: script.clone(),
            version: None,
        });
        let status = run(command, |_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert!(status.success() && started.exists());
        assert_eq!(
            find_client_command(&check, &AtomicBool::new(false)).unwrap(),
            None
        );

        let check = |path: &Path| ScriptCheck {
            tool: Tool::Psql,
            path: path.to_path_buf(),
            version: None,
        };
        let error = check_script(
            &check(&dir.path().join("absent.sql")),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.to_string().contains("absent.sql"), "{error}");
        let cancelled = check_script(&check(&script), &AtomicBool::new(true));
        assert!(matches!(cancelled, Err(Error::Cancelled)), "{cancelled:?}");
    }

    /// A file written on the fly: each pattern repeated so many times.
    /// Records the largest read the scan asks for.
    struct Synthetic {
        parts: [(&'static [u8], u64); 3],
        part: usize,
        offset: usize,
        repeated: u64,
        sent: u64,
        largest_read: usize,
    }

    impl Read for Synthetic {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.largest_read = self.largest_read.max(buf.len());
            let mut written = 0;
            while written < buf.len() && self.part < self.parts.len() {
                let (pattern, times) = self.parts[self.part];
                if self.repeated == times {
                    (self.part, self.repeated) = (self.part + 1, 0);
                    continue;
                }
                let n = (pattern.len() - self.offset).min(buf.len() - written);
                buf[written..written + n].copy_from_slice(&pattern[self.offset..self.offset + n]);
                (written, self.offset) = (written + n, self.offset + n);
                if self.offset == pattern.len() {
                    (self.offset, self.repeated) = (0, self.repeated + 1);
                }
            }
            self.sent += written as u64;
            Ok(written)
        }
    }

    #[test]
    fn a_200_mb_script_is_scanned_in_constant_memory() {
        // COPY data: 100 MB on one line, then 100 MB of short lines, then a
        // command on the last line.
        const LONG: &[u8] = b"C:\\\\Users\\\\x\\tcaf\xc3\xa9 \\N\t`code`\\\\! ";
        const SHORT: &[u8] = b"42\tC:\\\\tmp\\\\a\\nb\t\\N\n";
        const TAIL: &[u8] = b"\\! touch /tmp/never\n";
        const HALF: u64 = 100 * 1024 * 1024;
        let (long, short) = (HALF / LONG.len() as u64, HALF / SHORT.len() as u64);
        let mut source = Synthetic {
            parts: [(LONG, long), (SHORT, short), (TAIL, 1)],
            part: 0,
            offset: 0,
            repeated: 0,
            sent: 0,
            largest_read: 0,
        };
        let started = Instant::now();
        // The strict rule (unknown psql), which looks at more names.
        let found = scan(
            true,
            true,
            &mut source,
            Path::new("big.sql"),
            &AtomicBool::new(false),
        )
        .unwrap()
        .unwrap();
        eprintln!("scanned {} bytes in {:?}", source.sent, started.elapsed());
        assert!(source.sent > 2 * HALF - 1024, "{}", source.sent);
        assert_eq!(
            found,
            Finding {
                line: 1 + short,
                command: "\\!".into()
            }
        );
        // All the scan holds of the file: one read buffer, whatever the
        // length of a line, and a scanner of fixed size (no heap).
        assert_eq!(source.largest_read, SCAN_CHUNK);
        assert!(std::mem::size_of::<Scanner>() < 256);
    }

    /// Unix: a symbolic link and an executable script.
    #[cfg(unix)]
    #[test]
    fn mysql_tls_options_follow_the_binary_not_the_server() {
        let maria_server = params(Engine::MariaDb);
        let command = dump(
            Path::new("/usr/bin/mysqldump"),
            &maria_server,
            &my_backup("shop"),
        )
        .unwrap();
        assert!(command.args.contains(&"--ssl-mode=PREFERRED".into()));

        // MariaDB installs `mysqldump` as a link to `mariadb-dump`.
        let dir = tempfile::tempdir().unwrap();
        let real = fake_tool(dir.path(), "mariadb-dump", 0o755);
        let link = dir.path().join("mysqldump");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut verify = params(Engine::MySql);
        verify.tls.mode = TlsMode::Require;
        let command = dump(&link, &verify, &my_backup("shop")).unwrap();
        assert!(
            command
                .args
                .windows(2)
                .any(|pair| pair == ["--ssl", "--skip-ssl-verify-server-cert"]),
            "{:?}",
            command.args
        );
        assert!(!command.args.iter().any(|arg| arg.starts_with("--ssl-mode")));
    }

    #[test]
    fn refusals() {
        let path = Path::new("/usr/bin/tool");
        let custom = BackupOptions {
            format: BackupFormat::Custom,
            ..my_backup("shop")
        };
        assert!(matches!(
            dump(path, &params(Engine::MySql), &custom),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            restore_command(
                path,
                &params(Engine::MariaDb),
                &restore(BackupFormat::Custom)
            ),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            dump(path, &params(Engine::MySql), &my_backup("-evil")),
            Err(Error::Config(_))
        ));
        let other_db = BackupOptions {
            tables: vec![TableRef::new("other", "t")],
            ..my_backup("shop")
        };
        assert!(matches!(
            dump(path, &params(Engine::MySql), &other_db),
            Err(Error::Config(_))
        ));
        let mut read_only = params(Engine::Postgres);
        read_only.read_only = true;
        assert!(matches!(
            restore_command(path, &read_only, &restore(BackupFormat::Sql)),
            Err(Error::ReadOnly(_))
        ));
        // Backing up a read-only connection is fine.
        assert!(dump(path, &read_only, &backup("shop")).is_ok());
    }

    #[test]
    fn no_password_means_no_password_variable() {
        let mut params = params(Engine::Postgres);
        params.password = None;
        let command = dump(Path::new("/x/pg_dump"), &params, &backup("shop")).unwrap();
        assert!(command.password.is_none());
        assert!(!command.display().contains("PGPASSWORD"));
        params.password = Some(Secret::new(""));
        let command = dump(Path::new("/x/pg_dump"), &params, &backup("shop")).unwrap();
        assert!(command.password.is_none());
    }

    /// The arguments after the connection's own.
    fn own_args(command: &ToolCommand) -> Vec<String> {
        let skip = command
            .args
            .iter()
            .position(|arg| arg.starts_with("--ssl-mode=") || arg.starts_with("--username="))
            .map_or(0, |at| at + 1);
        command.args[skip..].to_vec()
    }

    fn pg_with(change: impl FnOnce(&mut BackupOptions, &mut PgBackup)) -> Result<ToolCommand> {
        let mut options = BackupOptions::new(Engine::Postgres, "shop");
        let mut pg = PgBackup::default();
        change(&mut options, &mut pg);
        options.engine = EngineBackup::Postgres(pg);
        dump(Path::new("/x/pg_dump"), &params(Engine::Postgres), &options)
    }

    #[test]
    fn each_pg_dump_option_gives_its_argument() {
        let base = own_args(&pg_with(|_, _| {}).unwrap());
        assert_eq!(
            base,
            [
                "--verbose",
                "--dbname=shop",
                "--file=/tmp/out dir/shop.sql",
                "--format=c"
            ]
        );
        type Change = fn(&mut BackupOptions, &mut PgBackup);
        let cases: [(Change, &[&str]); 16] = [
            (|o, _| o.format = BackupFormat::Sql, &["--format=p"]),
            (|o, _| o.format = BackupFormat::Directory, &["--format=d"]),
            (|o, _| o.format = BackupFormat::Tar, &["--format=t"]),
            (|_, p| p.compression = Some(7), &["--compress=7"]),
            (
                |_, p| p.encoding = Some("UTF8".into()),
                &["--encoding=UTF8"],
            ),
            (
                |o, p| {
                    o.format = BackupFormat::Directory;
                    p.jobs = Some(4)
                },
                &["--format=d", "--jobs=4"],
            ),
            (
                |o, _| o.content = BackupContent::SchemaOnly,
                &["--schema-only"],
            ),
            (|o, _| o.content = BackupContent::DataOnly, &["--data-only"]),
            (|_, p| p.inserts = true, &["--inserts"]),
            (|_, p| p.no_privileges = true, &["--no-privileges"]),
            (|_, p| p.no_owner = true, &["--no-owner"]),
            (|_, p| p.clean = true, &["--clean"]),
            (
                |_, p| {
                    p.clean = true;
                    p.if_exists = true
                },
                &["--clean", "--if-exists"],
            ),
            (|_, p| p.create = true, &["--create"]),
            (
                |o, _| o.schemas = vec!["shop".into(), "Odd\"One".into()],
                &[
                    "--strict-names",
                    "--schema=\"shop\"",
                    "--schema=\"Odd\"\"One\"",
                ],
            ),
            (
                |o, _| o.extra_args = "--exclude-table-data='shop.\"big table\"' -Z9".into(),
                &["--exclude-table-data=shop.\"big table\"", "-Z9"],
            ),
        ];
        for (change, added) in cases {
            let args = own_args(&pg_with(change).unwrap());
            let new: Vec<&str> = args
                .iter()
                .map(String::as_str)
                .filter(|arg| !base.iter().any(|b| b == arg))
                .collect();
            assert_eq!(new, added, "{args:?}");
        }
    }

    /// K48: the simple backup's "Faster" is `--format=d --jobs=N`; jobs with
    /// any other format are refused, never passed.
    #[test]
    fn parallel_jobs_go_with_the_directory_format_only() {
        let args = own_args(
            &pg_with(|o, p| {
                o.format = BackupFormat::Directory;
                p.jobs = Some(8)
            })
            .unwrap(),
        );
        let at = args.iter().position(|arg| arg == "--format=d").unwrap();
        assert_eq!(args[at + 1], "--jobs=8", "{args:?}");
        for format in [BackupFormat::Sql, BackupFormat::Custom, BackupFormat::Tar] {
            let refused = pg_with(|o, p| {
                o.format = format;
                p.jobs = Some(8)
            });
            assert!(
                matches!(&refused, Err(Error::Unsupported(text)) if text.contains("directory format")),
                "{format:?}: {refused:?}"
            );
        }
    }

    #[test]
    fn pg_dump_refuses_what_it_cannot_do() {
        type Change = fn(&mut BackupOptions, &mut PgBackup);
        let refused: [(Change, &str); 6] = [
            (|_, p| p.jobs = Some(2), "directory format"),
            (
                |o, p| {
                    o.format = BackupFormat::Tar;
                    p.compression = Some(5)
                },
                "tar format",
            ),
            (|_, p| p.compression = Some(12), "0 to 9"),
            (|_, p| p.if_exists = true, "DROP statements"),
            (
                |o, p| {
                    o.content = BackupContent::DataOnly;
                    p.clean = true
                },
                "definitions",
            ),
            (
                |o, _| {
                    o.schemas = vec!["shop".into()];
                    o.tables = vec![TableRef::new("shop", "orders")]
                },
                "name the tables only",
            ),
        ];
        for (change, why) in refused {
            match pg_with(change) {
                Err(Error::Unsupported(text)) => assert!(text.contains(why), "{text}"),
                other => panic!("{why}: {other:?}"),
            }
        }
        // MySQL's options on a PostgreSQL connection, and the reverse.
        let mysql = my_backup("shop");
        assert!(matches!(
            dump(Path::new("/x/pg_dump"), &params(Engine::Postgres), &mysql),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            dump(
                Path::new("/x/mysqldump"),
                &params(Engine::MySql),
                &backup("shop")
            ),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn each_mysqldump_option_gives_its_argument() {
        let run = |change: fn(&mut BackupOptions, &mut MySqlBackup)| {
            let mut options = my_backup("shop");
            let mut my = MySqlBackup::default();
            change(&mut options, &mut my);
            options.engine = EngineBackup::MySql(my);
            own_args(&dump(Path::new("/x/mysqldump"), &params(Engine::MySql), &options).unwrap())
        };
        let base = run(|_, _| {});
        assert_eq!(
            base,
            [
                "--verbose",
                "--single-transaction",
                "--routines",
                "--hex-blob",
                "--result-file=/tmp/out dir/shop.sql",
                "shop"
            ]
        );
        type Change = fn(&mut BackupOptions, &mut MySqlBackup);
        let cases: [(Change, &[&str], &[&str]); 13] = [
            (
                |_, m| m.method = DumpMethod::LockAllTables,
                &["--lock-all-tables"],
                &["--single-transaction"],
            ),
            (
                |_, m| m.method = DumpMethod::NoLocks,
                &["--skip-lock-tables"],
                &["--single-transaction"],
            ),
            (|_, m| m.routines = false, &[], &["--routines"]),
            (|_, m| m.events = true, &["--events"], &[]),
            (|_, m| m.triggers = false, &["--skip-triggers"], &[]),
            (
                |_, m| m.extended_inserts = false,
                &["--skip-extended-insert"],
                &[],
            ),
            (
                |_, m| m.add_drop_table = false,
                &["--skip-add-drop-table"],
                &[],
            ),
            (|_, m| m.disable_keys = false, &["--skip-disable-keys"], &[]),
            (|_, m| m.hex_blob = false, &[], &["--hex-blob"]),
            (|_, m| m.comments = false, &["--skip-comments"], &[]),
            (
                |o, _| o.content = BackupContent::SchemaOnly,
                &["--no-data"],
                &[],
            ),
            (
                |o, _| o.content = BackupContent::DataOnly,
                &["--no-create-info", "--skip-triggers"],
                &["--routines"],
            ),
            (
                |o, _| {
                    o.tables = vec![TableRef::new("shop", "orders")];
                    o.extra_args = "--ignore-table=shop.logs".into()
                },
                &["--ignore-table=shop.logs", "orders"],
                &[],
            ),
        ];
        for (change, added, removed) in cases {
            let args = run(change);
            let new: Vec<&str> = args
                .iter()
                .map(String::as_str)
                .filter(|arg| !base.iter().any(|b| b == arg))
                .collect();
            let gone: Vec<&str> = base
                .iter()
                .map(String::as_str)
                .filter(|arg| !args.iter().any(|a| a == arg))
                .collect();
            assert_eq!(
                (new.as_slice(), gone.as_slice()),
                (added, removed),
                "{args:?}"
            );
        }
        // Extra arguments come before the database and its tables.
        let args = run(|o, _| {
            o.tables = vec![TableRef::new("shop", "orders")];
            o.extra_args = "--where=id<10".into()
        });
        assert!(args.ends_with(&["--where=id<10".into(), "shop".into(), "orders".into()]));
        // MySQL's mysqldump on a MariaDB server: no column statistics.
        let command = dump(
            Path::new("/x/mysqldump"),
            &params(Engine::MariaDb),
            &my_backup("shop"),
        )
        .unwrap();
        assert!(command.args.contains(&"--column-statistics=0".into()));
    }

    #[test]
    fn each_pg_dumpall_option_gives_its_argument() {
        let run = |change: fn(&mut GlobalBackupOptions)| {
            let mut options = GlobalBackupOptions::default();
            change(&mut options);
            global_backup_command(
                Path::new("/x/pg_dumpall"),
                &params(Engine::Postgres),
                &options,
                Path::new("/b/all.sql"),
            )
        };
        let base = own_args(&run(|_| {}).unwrap());
        assert_eq!(
            base,
            ["--verbose", "--no-role-passwords", "--file=/b/all.sql"]
        );
        type Change = fn(&mut GlobalBackupOptions);
        let cases: [(Change, &[&str], &[&str]); 9] = [
            (
                |o| o.encoding = Some("LATIN1".into()),
                &["--encoding=LATIN1"],
                &[],
            ),
            (|o| o.schema_only = true, &["--schema-only"], &[]),
            (|o| o.globals_only = true, &["--globals-only"], &[]),
            (|o| o.roles_only = true, &["--roles-only"], &[]),
            (|o| o.tablespaces_only = true, &["--tablespaces-only"], &[]),
            (|o| o.no_privileges = true, &["--no-privileges"], &[]),
            (|o| o.no_owner = true, &["--no-owner"], &[]),
            (|o| o.role_passwords = true, &[], &["--no-role-passwords"]),
            (
                |o| o.exclude_databases = vec!["kerno_other".into()],
                &["--exclude-database=\"kerno_other\""],
                &[],
            ),
        ];
        for (change, added, removed) in cases {
            let args = own_args(&run(change).unwrap());
            let new: Vec<&str> = args
                .iter()
                .map(String::as_str)
                .filter(|arg| !base.iter().any(|b| b == arg))
                .collect();
            let gone: Vec<&str> = base
                .iter()
                .map(String::as_str)
                .filter(|arg| !args.iter().any(|a| a == arg))
                .collect();
            assert_eq!(
                (new.as_slice(), gone.as_slice()),
                (added, removed),
                "{args:?}"
            );
        }
        for change in [
            (|o: &mut GlobalBackupOptions| {
                o.globals_only = true;
                o.roles_only = true
            }) as Change,
            |o| {
                o.roles_only = true;
                o.exclude_databases = vec!["x".into()]
            },
        ] {
            assert!(matches!(run(change), Err(Error::Unsupported(_))));
        }
        assert!(matches!(
            global_backup_command(
                Path::new("/x/pg_dumpall"),
                &params(Engine::MySql),
                &GlobalBackupOptions::default(),
                Path::new("/b/all.sql"),
            ),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn each_restore_option_gives_its_argument() {
        let pg_restore = |change: fn(&mut RestoreOptions)| {
            let mut options = restore(BackupFormat::Custom);
            options.single_transaction = false;
            change(&mut options);
            restore_command(
                Path::new("/x/pg_restore"),
                &params(Engine::Postgres),
                &options,
            )
        };
        let base = own_args(&pg_restore(|_| {}).unwrap());
        assert_eq!(
            base,
            [
                "--verbose",
                "--dbname=shop_copy",
                "--format=c",
                "/backups/shop.dump"
            ]
        );
        type Change = fn(&mut RestoreOptions);
        let cases: [(Change, &[&str]); 10] = [
            (|o| o.format = BackupFormat::Directory, &["--format=d"]),
            (
                |o| o.use_list = Some("/tmp/k.list".into()),
                &["--use-list=/tmp/k.list"],
            ),
            (|o| o.format = BackupFormat::Tar, &["--format=t"]),
            (|o| o.clean = true, &["--clean"]),
            (
                |o| {
                    o.clean = true;
                    o.if_exists = true
                },
                &["--clean", "--if-exists"],
            ),
            (|o| o.create = true, &["--create"]),
            (|o| o.no_owner = true, &["--no-owner"]),
            (|o| o.no_privileges = true, &["--no-privileges"]),
            (|o| o.jobs = Some(3), &["--jobs=3"]),
            (|o| o.single_transaction = true, &["--single-transaction"]),
        ];
        for (change, added) in cases {
            let args = own_args(&pg_restore(change).unwrap());
            let new: Vec<&str> = args
                .iter()
                .map(String::as_str)
                .filter(|arg| !base.iter().any(|b| b == arg))
                .collect();
            assert_eq!(new, added, "{args:?}");
            assert_eq!(args.last().map(String::as_str), Some("/backups/shop.dump"));
        }
        let refused: [(Change, &str); 5] = [
            (|o| o.if_exists = true, "goes with clean"),
            (
                |o| {
                    o.jobs = Some(2);
                    o.single_transaction = true
                },
                "one transaction",
            ),
            (
                |o| {
                    o.create = true;
                    o.single_transaction = true
                },
                "in one transaction",
            ),
            (
                |o| {
                    o.format = BackupFormat::Tar;
                    o.jobs = Some(2)
                },
                "custom and directory",
            ),
            (|o| o.disable_foreign_keys = true, "MySQL's"),
        ];
        for (change, why) in refused {
            match pg_restore(change) {
                Err(Error::Unsupported(text)) => assert!(text.contains(why), "{text}"),
                other => panic!("{why}: {other:?}"),
            }
        }

        // psql runs a script as it is: pg_restore's options are refused,
        // the transaction is an option.
        let psql = |change: fn(&mut RestoreOptions)| {
            let mut options = restore(BackupFormat::Sql);
            change(&mut options);
            restore_command(Path::new("/x/psql"), &params(Engine::Postgres), &options)
        };
        let args = psql(|o| o.single_transaction = false).unwrap().args;
        assert!(!args.contains(&"--single-transaction".into()));
        for change in [
            (|o: &mut RestoreOptions| o.clean = true) as Change,
            |o| o.no_owner = true,
            |o| o.jobs = Some(2),
            |o| o.use_list = Some("/tmp/k.list".into()),
        ] {
            assert!(matches!(psql(change), Err(Error::Unsupported(_))));
        }

        // mysql: foreign-key checks off; PostgreSQL's options refused.
        let mysql = |change: fn(&mut RestoreOptions)| {
            let mut options = my_restore();
            change(&mut options);
            restore_command(Path::new("/x/mysql"), &params(Engine::MySql), &options)
        };
        let args = mysql(|o| o.disable_foreign_keys = true).unwrap().args;
        assert!(args.ends_with(&["--init-command=SET SESSION FOREIGN_KEY_CHECKS=0".into()]));
        for change in [
            (|o: &mut RestoreOptions| o.single_transaction = true) as Change,
            |o| o.no_owner = true,
        ] {
            assert!(matches!(mysql(change), Err(Error::Unsupported(_))));
        }
    }

    #[test]
    fn a_script_runs_like_a_restore_without_a_transaction() {
        let pg = script_command(
            Path::new("/x/psql"),
            &params(Engine::Postgres),
            &ScriptOptions::new(Engine::Postgres, "shop", "/s/fix.sql"),
        )
        .unwrap();
        assert_eq!(
            own_args(&pg),
            [
                "--no-psqlrc",
                "--dbname=shop",
                "--set=ON_ERROR_STOP=1",
                "--echo-errors",
                "--file=-"
            ]
        );
        // Fed through the standard input, which `run` counts.
        assert_eq!(pg.stdin, Some(PathBuf::from("/s/fix.sql")));
        assert!(pg.script_check.is_some(), "client commands are checked");
        let mut mysql_only = ScriptOptions::new(Engine::Postgres, "shop", "/s/fix.sql");
        mysql_only.disable_foreign_keys = true;
        assert!(matches!(
            script_command(Path::new("/x/psql"), &params(Engine::Postgres), &mysql_only),
            Err(Error::Unsupported(_))
        ));

        let my = |change: fn(&mut ScriptOptions)| {
            let mut options = ScriptOptions::new(Engine::MySql, "shop", "/s/fix.sql");
            change(&mut options);
            own_args(
                &script_command(Path::new("/x/mysql"), &params(Engine::MySql), &options).unwrap(),
            )
        };
        assert_eq!(
            my(|_| {}),
            ["--database=shop", "--local-infile=0", "--no-beep"]
        );
        assert!(my(|o| o.no_beep = false).ends_with(&["--local-infile=0".into()]));
        assert!(my(|o| o.log_level = ScriptLogLevel::Verbose).contains(&"--verbose".into()));
        assert!(
            my(|o| o.disable_foreign_keys = true)
                .contains(&"--init-command=SET SESSION FOREIGN_KEY_CHECKS=0".into())
        );
        let mut read_only = params(Engine::MySql);
        read_only.read_only = true;
        assert!(matches!(
            script_command(
                Path::new("/x/mysql"),
                &read_only,
                &ScriptOptions::new(Engine::MySql, "shop", "/s/fix.sql")
            ),
            Err(Error::ReadOnly(_))
        ));
    }

    #[test]
    fn extra_arguments_are_split_like_a_shell() {
        let split = |text: &str| extra_args(text, ClientFamily::Postgres).unwrap();
        assert!(split("  ").is_empty());
        assert_eq!(
            split(r#"--exclude-table='a b' --exclude-table="c\"d" --exclude-schema=e\ f"#),
            [
                "--exclude-table=a b",
                "--exclude-table=c\"d",
                "--exclude-schema=e f"
            ]
        );
        assert!(extra_args("--exclude-table='open", ClientFamily::Postgres).is_err());
        assert!(extra_args("--exclude-table=\"open", ClientFamily::Postgres).is_err());
    }

    /// K74, N3: MySQL's options that print and stop without running
    /// anything (exit 0, a run that looks done) are refused for every
    /// MySQL / MariaDB tool, in every spelling; the ones near them pass.
    #[test]
    fn options_that_print_and_stop_are_refused() {
        for arg in [
            "--print-defaults",
            "--version",
            "--vers",
            "--help",
            "--loose-help",
            "-V",
            "-?",
            "-I",
            "-NV",
        ] {
            let error = extra_args(arg, ClientFamily::MySql)
                .err()
                .unwrap_or_else(|| panic!("{arg} let through"))
                .to_string();
            assert!(
                error.contains("stop without running anything"),
                "{arg}: {error}"
            );
        }
        for arg in ["--verbose", "--hex-blob", "-N", "--force", "--no-create-db"] {
            assert!(
                extra_args(arg, ClientFamily::MySql).is_ok(),
                "{arg} refused"
            );
        }
        // PostgreSQL's tools are not concerned.
        assert!(extra_args("-V", ClientFamily::Postgres).is_ok());
    }

    #[test]
    fn extra_arguments_that_change_the_route_or_carry_a_password_are_refused() {
        let pg_refused = [
            "--host=evil",
            "--ho=evil",
            "--port=1",
            "--username=root",
            "--dbname=host=evil",
            "--password",
            "--set=ON_ERROR_STOP=0",
            "--variable=x=1",
            "--command=SELECT 1",
            "--file=/tmp/x",
            "--output=|sh",
            "--log-file=/tmp/x",
            "--service=other",
            "--sslmode=disable",
            "-h",
            "-hevil",
            "-p5432",
            "-Uroot",
            "-dpostgres",
            "-W",
            "-vx",
            "-c",
            "-o|sh",
            "-",
            "--",
            "postgres://u:p@evil/db",
            "host=evil",
            "orders",
        ];
        for arg in pg_refused {
            assert!(
                extra_args(arg, ClientFamily::Postgres).is_err(),
                "{arg} is refused"
            );
        }
        let mysql_refused = [
            "--host=evil",
            "--HOST=evil",
            "--pass=x",
            "--password=x",
            "--user=root",
            "--socket=/tmp/s",
            "--protocol=SOCKET",
            "--defaults-file=/tmp/my.cnf",
            "--defaults-extra-file=/tmp/my.cnf",
            "--login-path=x",
            "--plugin-dir=/tmp",
            "--default-auth=x",
            "--enable-cleartext-plugin",
            "--ssl-mode=DISABLED",
            "--skip-ssl",
            "--tls-version=TLSv1",
            "--commands=ON",
            "--local-infile=1",
            "--local_infile=1",
            "--loose-local-infile=1",
            "--loose-enable-local-infile",
            "--loose-skip-binary-mode",
            "--loose-skip-sandbox",
            "--loose-skip-ssl",
            "--skip-binary-mode",
            "--skip-sandbox",
            "--init-command=SELECT 1",
            "--execute=SELECT 1",
            "--pager=sh",
            "--tee=/tmp/x",
            "--result-file=/tmp/x",
            "--system-command=ON",
            "--database=other",
            "-hevil",
            "-P3306",
            "-uroot",
            "-psecret",
            "-Sx",
            "-Dother",
            "-e",
            "kerno",
        ];
        for arg in mysql_refused {
            assert!(
                extra_args(arg, ClientFamily::MySql).is_err(),
                "{arg} is refused"
            );
        }
        // Options that only shape the output pass.
        for (family, arg) in [
            (ClientFamily::Postgres, "--exclude-table=logs"),
            (ClientFamily::Postgres, "--no-comments"),
            (ClientFamily::Postgres, "--section=pre-data"),
            (ClientFamily::Postgres, "--serializable-deferrable"),
            (ClientFamily::Postgres, "-Z9"),
            (ClientFamily::MySql, "--skip-comments"),
            (ClientFamily::MySql, "--compress"),
            (ClientFamily::MySql, "--set-gtid-purged=OFF"),
            (ClientFamily::MySql, "--default-character-set=utf8mb4"),
            (ClientFamily::MySql, "--where=id<10"),
        ] {
            assert!(extra_args(arg, family).is_ok(), "{arg} passes");
        }
        // A refused argument refuses the whole command.
        let options = BackupOptions {
            extra_args: "--no-comments --host=evil".into(),
            ..backup("shop")
        };
        let refused = dump(Path::new("/x/pg_dump"), &params(Engine::Postgres), &options);
        assert!(
            matches!(&refused, Err(Error::Config(text)) if text.contains("“--host=evil”")),
            "{refused:?}"
        );
    }

    #[test]
    fn another_user_runs_the_tool_and_no_secret_is_kept() {
        let options = BackupOptions {
            auth_user: Some("backup_role".into()),
            ..backup("shop")
        };
        let command = dump(Path::new("/x/pg_dump"), &params(Engine::Postgres), &options).unwrap();
        assert!(command.args.contains(&"--username=backup_role".into()));
        let restore = RestoreOptions {
            auth_user: Some("loader".into()),
            allow_client_commands: true,
            ..my_restore()
        };
        let command =
            restore_command(Path::new("/x/mysql"), &params(Engine::MySql), &restore).unwrap();
        assert!(command.args.contains(&"--user=loader".into()));
        // The settings a task keeps: the user, never a password; the
        // per-run opt-in is not kept either.
        let json = serde_json::to_string(&restore).unwrap();
        assert!(
            json.contains("loader") && !json.contains(PASSWORD),
            "{json}"
        );
        let back: RestoreOptions = serde_json::from_str(&json).unwrap();
        assert!(!back.allow_client_commands);
        assert_eq!(back.auth_user.as_deref(), Some("loader"));
        let json = serde_json::to_string(&options).unwrap();
        let back: BackupOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back, options);
        for json in [
            serde_json::to_string(&GlobalBackupOptions::default()).unwrap(),
            serde_json::to_string(&ScriptOptions::new(Engine::MySql, "d", "/f")).unwrap(),
        ] {
            assert!(!json.to_lowercase().contains("password\""), "{json}");
        }
    }

    #[test]
    fn file_names_fill_the_variables() {
        let vars = NameVars {
            host: "db.example.com".into(),
            database: "shop".into(),
            schema: Some("sales".into()),
            table: None,
            environment: "production".into(),
            now: Stamp {
                year: 2026,
                month: 3,
                day: 9,
                hour: 7,
                minute: 5,
            },
        };
        let name = |pattern: &str| file_name(pattern, &vars).unwrap();
        assert_eq!(name("${database}-${timestamp}"), "shop-202603090705");
        assert_eq!(
            name("${host}_${schema}_${table}_${date}_${connection.type}"),
            "db.example.com_sales_null_20260309_production"
        );
        assert_eq!(name("${year}-${month}"), "2026-03");
        assert_eq!(name("${day}${hour}${minute}"), "090705");
        assert_eq!(name("${unknown}-${database"), "${unknown}-${database");
        // A value cannot leave the folder.
        let odd = NameVars {
            database: "../etc/passwd".into(),
            ..vars.clone()
        };
        assert_eq!(file_name("${database}", &odd).unwrap(), "_._etc_passwd");
        let schema_less = NameVars {
            schema: None,
            ..vars.clone()
        };
        assert_eq!(file_name("${schema}", &schema_less).unwrap(), "shop");
        assert!(file_name("a/${database}", &vars).is_err());
        assert!(file_name("  ", &vars).is_err());
        // The format's extension follows the pattern.
        let folder = Path::new("/backups");
        let path = |format| output_path(folder, DEFAULT_FILE_PATTERN, format, &vars).unwrap();
        assert_eq!(
            path(BackupFormat::Custom),
            Path::new("/backups/shop-202603090705.dump")
        );
        assert_eq!(
            path(BackupFormat::Sql),
            Path::new("/backups/shop-202603090705.sql")
        );
        assert_eq!(
            path(BackupFormat::Tar),
            Path::new("/backups/shop-202603090705.tar")
        );
        assert_eq!(
            path(BackupFormat::Directory),
            Path::new("/backups/shop-202603090705")
        );
        assert_eq!(
            output_path(folder, "shop.sql", BackupFormat::Sql, &vars).unwrap(),
            Path::new("/backups/shop.sql")
        );
        let now = Stamp::now();
        assert!(now.year >= 2026 && (1..=12).contains(&now.month), "{now:?}");
    }

    #[test]
    fn archives_are_told_from_scripts() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("a.dump");
        std::fs::write(&custom, b"PGDMP\x01\x0e\x00").unwrap();
        let script = dir.path().join("a.sql");
        std::fs::write(&script, b"SELECT 1;\n").unwrap();
        let tiny = dir.path().join("tiny.sql");
        std::fs::write(&tiny, b"PG").unwrap();
        let tar = dir.path().join("a.tar");
        let mut header = vec![0u8; 512];
        header[..7].copy_from_slice(b"toc.dat");
        header[257..262].copy_from_slice(b"ustar");
        std::fs::write(&tar, &header).unwrap();
        let archive = dir.path().join("archive");
        std::fs::create_dir(&archive).unwrap();
        std::fs::write(archive.join("toc.dat"), b"PGDMP").unwrap();
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        for (path, format) in [
            (&custom, BackupFormat::Custom),
            (&script, BackupFormat::Sql),
            (&tiny, BackupFormat::Sql),
            (&tar, BackupFormat::Tar),
            (&archive, BackupFormat::Directory),
        ] {
            assert_eq!(guess_format(path, Engine::Postgres), Ok(format), "{path:?}");
        }
        assert!(
            guess_format(&empty, Engine::Postgres)
                .unwrap_err()
                .contains("without toc.dat")
        );
        for path in [&custom, &tar, &archive] {
            assert!(
                guess_format(path, Engine::MySql)
                    .unwrap_err()
                    .contains("PostgreSQL archive")
            );
        }
        assert_eq!(guess_format(&script, Engine::MySql), Ok(BackupFormat::Sql));
        // A MySQL / MariaDB folder backup is a folder with Kerno's manifest
        // (K63); another folder is not offered as one.
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join(mysql_folder::MANIFEST), b"{}").unwrap();
        for engine in [Engine::MySql, Engine::MariaDb] {
            assert_eq!(guess_format(&folder, engine), Ok(BackupFormat::Directory));
            assert!(
                guess_format(&empty, engine)
                    .unwrap_err()
                    .contains("without kerno-backup.json")
            );
        }
        assert!(
            guess_format(&folder, Engine::Postgres)
                .unwrap_err()
                .contains("without toc.dat")
        );
        assert!(
            guess_format(&dir.path().join("absent"), Engine::Postgres)
                .unwrap_err()
                .contains("could not read")
        );
        // Each format goes to its tool.
        assert_eq!(
            restore_tool(Engine::Postgres, BackupFormat::Sql),
            Some(Tool::Psql)
        );
        for format in [
            BackupFormat::Custom,
            BackupFormat::Directory,
            BackupFormat::Tar,
        ] {
            assert_eq!(
                restore_tool(Engine::Postgres, format),
                Some(Tool::PgRestore)
            );
        }
        assert_eq!(
            restore_tool(Engine::MariaDb, BackupFormat::Sql),
            Some(Tool::Mysql)
        );
    }

    // The fake tools are `/bin/sh` scripts with an execute bit: Unix only (K64).
    /// A fake tool in `dir` that prints `version` for `--version`.
    #[cfg(unix)]
    fn versioned_tool(dir: &Path, name: &str, version: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho '{version}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A fake client install: every tool of the family, at `version`.
    #[cfg(unix)]
    fn fake_install(dir: &Path, family: ClientFamily, version: &str) {
        for tool in family.tools() {
            versioned_tool(dir, tool.name(), version);
        }
    }

    #[cfg(unix)]
    #[test]
    fn clients_are_detected_once_per_real_folder() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let cellar = root.join("brew/Cellar");
        let pg14 = cellar.join("postgresql@14/14.24_1/bin");
        let pg18 = cellar.join("postgresql@18/18.6_1/bin");
        let libpq = cellar.join("libpq/18.6_2/bin");
        let mysql = cellar.join("mysql-client/26.10.0/bin");
        fake_install(&pg14, ClientFamily::Postgres, "pg_dump (PostgreSQL) 14.24");
        fake_install(&pg18, ClientFamily::Postgres, "pg_dump (PostgreSQL) 18.6");
        fake_install(&libpq, ClientFamily::Postgres, "pg_dump (PostgreSQL) 18.6");
        fake_install(
            &mysql,
            ClientFamily::MySql,
            "mysqldump  Ver 26.10.0 for macos27.0 on arm64 (Homebrew)",
        );
        // Not a client formula: never looked at.
        versioned_tool(
            &cellar.join("redis/7.0/bin"),
            "pg_dump",
            "pg_dump (PostgreSQL) 99.0",
        );
        // Homebrew's link farm in $PATH points into the Cellar.
        let bin = root.join("brew/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(pg14.join("pg_dump"), bin.join("pg_dump")).unwrap();
        std::os::unix::fs::symlink(pg14.join("psql"), bin.join("psql")).unwrap();
        // Postgres.app and EDB versions.
        let app = root.join("Postgres.app/Contents/Versions");
        fake_install(
            &app.join("16/bin"),
            ClientFamily::Postgres,
            "pg_dump (PostgreSQL) 16.4",
        );
        let edb = root.join("Library/PostgreSQL");
        fake_install(
            &edb.join("15/bin"),
            ClientFamily::Postgres,
            "pg_dump (PostgreSQL) 15.8",
        );
        // A MariaDB client added by hand, with only `mariadb-dump`.
        let added = root.join("opt/mariadb/bin");
        versioned_tool(
            &added,
            "mariadb-dump",
            "mariadb-dump from 11.8.3-MariaDB, client 10.19",
        );
        // A folder with nothing.
        let nothing = root.join("empty");
        std::fs::create_dir_all(&nothing).unwrap();

        let roots = ClientRoots {
            path_var: Some(std::env::join_paths([&bin, &nothing, &bin]).unwrap()),
            homebrew: vec![root.join("brew")],
            postgres_app: vec![app],
            edb: vec![edb],
            linux: vec![root.join("usr/lib/postgresql")],
            program_files: Vec::new(),
            fallbacks: vec![pg18.clone()],
            added: vec![added.clone()],
        };
        let found = detect_clients(&roots);
        type Row = (ClientFamily, PathBuf, ClientSource, Option<(u32, u32, u32)>);
        let summary: Vec<Row> = found
            .iter()
            .map(|install| {
                (
                    install.family,
                    install.dir.clone(),
                    install.source,
                    install.version.as_ref().map(|v| v.number),
                )
            })
            .collect();
        use ClientFamily::{MySql, Postgres};
        assert_eq!(
            summary,
            [
                // 18.6 twice: the folder decides, libpq first.
                (
                    Postgres,
                    libpq.clone(),
                    ClientSource::Homebrew,
                    Some((18, 6, 0))
                ),
                (
                    Postgres,
                    pg18.clone(),
                    ClientSource::Homebrew,
                    Some((18, 6, 0))
                ),
                (
                    Postgres,
                    root.join("Postgres.app/Contents/Versions/16/bin"),
                    ClientSource::PostgresApp,
                    Some((16, 4, 0))
                ),
                (
                    Postgres,
                    root.join("Library/PostgreSQL/15/bin"),
                    ClientSource::Edb,
                    Some((15, 8, 0))
                ),
                // Reached through $PATH too, kept once, as found first.
                (
                    Postgres,
                    pg14.clone(),
                    ClientSource::Homebrew,
                    Some((14, 24, 0))
                ),
                (
                    MySql,
                    mysql.clone(),
                    ClientSource::Homebrew,
                    Some((26, 10, 0))
                ),
                (MySql, added.clone(), ClientSource::Added, Some((11, 8, 3))),
            ]
        );
        // Every tool of an install comes from its own folder.
        let pg14_install = &found[4];
        assert_eq!(
            pg14_install.path(Tool::PgDumpall),
            Some(pg14.join("pg_dumpall").as_path())
        );
        assert_eq!(
            found[6].path(Tool::MysqlDump),
            Some(added.join("mariadb-dump").as_path())
        );
        assert_eq!(found[6].path(Tool::Mysql), None);
        assert!(found[6].version.as_ref().unwrap().mariadb);
        assert_eq!(found[0].label(), "PostgreSQL 18.6 · Homebrew");
        assert_eq!(found[6].label(), "MariaDB 11.8.3 · added");

        // A folder probed alone.
        assert_eq!(
            probe_client(&bin, Postgres).map(|install| install.dir),
            Some(pg14.clone())
        );
        assert_eq!(probe_client(&nothing, Postgres), None);
        assert_eq!(probe_client(&added, Postgres), None);

        // The connection's folder, else the default, else the newest.
        let pick = |engine, connection: Option<&Path>, default: Option<&Path>| {
            client_for(
                &found,
                &ClientWanted {
                    engine,
                    connection_dir: connection,
                    default_dir: default,
                },
            )
            .map(|pick| (pick.install.dir.clone(), pick.ignored))
        };
        assert_eq!(
            pick(Engine::Postgres, None, None),
            Ok((libpq.clone(), None))
        );
        assert_eq!(
            pick(Engine::Postgres, None, Some(&pg14)),
            Ok((pg14.clone(), None))
        );
        assert_eq!(
            pick(
                Engine::Postgres,
                Some(&bin.join("..").join("Cellar/postgresql@18/18.6_1/bin")),
                Some(&pg14)
            ),
            Ok((pg18.clone(), None))
        );
        // A folder neither detected nor added is never used, even when it
        // holds tools: the default, else the newest, and it is reported.
        let elsewhere = root.join("elsewhere/bin");
        fake_install(&elsewhere, Postgres, "pg_dump (PostgreSQL) 99.0");
        assert_eq!(
            pick(Engine::Postgres, Some(&elsewhere), Some(&pg14)),
            Ok((pg14.clone(), Some(elsewhere.clone())))
        );
        assert_eq!(
            pick(Engine::Postgres, Some(&nothing), None),
            Ok((libpq.clone(), Some(nothing.clone())))
        );
        // MariaDB's own client for a MariaDB server, MySQL's for MySQL.
        assert_eq!(pick(Engine::MariaDb, None, None), Ok((added.clone(), None)));
        assert_eq!(pick(Engine::MySql, None, None), Ok((mysql.clone(), None)));
        let postgres_only: Vec<ClientInstall> = found
            .iter()
            .filter(|install| install.family == Postgres)
            .cloned()
            .collect();
        assert_eq!(
            client_for(
                &postgres_only,
                &ClientWanted {
                    engine: Engine::MySql,
                    connection_dir: None,
                    default_dir: None
                }
            ),
            Err(ClientMissing {
                family: Some(MySql)
            })
        );
        assert!(
            ClientMissing {
                family: Some(MySql)
            }
            .text()
            .contains("brew install mysql-client")
        );
    }

    /// K67: a relative folder of `PATH` (`.`, `bin`) names a folder of the
    /// current directory: a tool there is never found, by `find` or by
    /// detection, so none planted where Kerno starts can run.
    #[cfg(unix)]
    #[test]
    fn a_relative_path_entry_is_never_searched() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let bin = root.join("bin");
        fake_install(&bin, ClientFamily::Postgres, "pg_dump (PostgreSQL) 17.12");
        // The same folder, relative to the current directory.
        let up = std::env::current_dir().unwrap().components().count() - 1;
        let relative: PathBuf = std::iter::repeat_n(Path::new(".."), up)
            .collect::<PathBuf>()
            .join(bin.strip_prefix("/").unwrap());
        assert!(relative.join("pg_dump").is_file());
        let path_var = std::env::join_paths([&relative]).unwrap();
        assert_eq!(
            find_all_in(Tool::PgDump, Some(&path_var), &[]),
            Vec::<PathBuf>::new()
        );
        let roots = ClientRoots {
            path_var: Some(path_var),
            ..ClientRoots::default()
        };
        assert_eq!(detect_clients(&roots), Vec::new());
        // The absolute one is.
        assert_eq!(
            find_all_in(Tool::PgDump, None, std::slice::from_ref(&bin)),
            [bin.join("pg_dump")]
        );
    }

    /// K67: Windows' installers put MySQL in `MySQL\<product>\bin` and
    /// MariaDB in `MariaDB <X.Y>\bin` under `%ProgramFiles%`; PostgreSQL's
    /// (EDB's) is [`ClientRoots::edb`]. The same folder reached through
    /// `PATH` (by another name: Windows' short names) is kept once, as an
    /// installer's.
    #[cfg(unix)]
    #[test]
    fn the_windows_installers_folders_are_read_not_assumed() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let program_files = root.join("Program Files");
        let mysql = program_files.join("MySQL/MySQL Server 8.4/bin");
        fake_install(
            &mysql,
            ClientFamily::MySql,
            "mysqldump  Ver 8.4.13 for Win64 on x86_64 (MySQL Community Server - GPL)",
        );
        let mariadb = program_files.join("MariaDB 11.4/bin");
        versioned_tool(
            &mariadb,
            "mariadb-dump",
            "mariadb-dump from 11.4.5-MariaDB, client 10.19 for Win64 (AMD64)",
        );
        let postgres = program_files.join("PostgreSQL/17/bin");
        fake_install(
            &postgres,
            ClientFamily::Postgres,
            "pg_dump (PostgreSQL) 17.12",
        );
        // Another program's folder: never looked at.
        versioned_tool(
            &program_files.join("Other/bin"),
            "mysqldump",
            "mysqldump  Ver 99.0",
        );
        let short = root.join("PROGRA~1");
        std::os::unix::fs::symlink(&program_files, &short).unwrap();
        let roots = ClientRoots {
            path_var: Some(
                std::env::join_paths([short.join("MySQL/MySQL Server 8.4/bin")]).unwrap(),
            ),
            edb: vec![program_files.join("PostgreSQL")],
            program_files: vec![program_files.clone()],
            ..ClientRoots::default()
        };
        type Row = (PathBuf, ClientSource, Option<(u32, u32, u32)>);
        let found: Vec<Row> = detect_clients(&roots)
            .into_iter()
            .map(|install| {
                (
                    install.dir,
                    install.source,
                    install.version.map(|v| v.number),
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                (postgres, ClientSource::Edb, Some((17, 12, 0))),
                // Newest first.
                (mariadb, ClientSource::ProgramFiles, Some((11, 4, 5))),
                (mysql, ClientSource::ProgramFiles, Some((8, 4, 13))),
            ]
        );
    }

    #[test]
    fn a_client_older_than_a_postgresql_server_is_refused() {
        let v = |text: &str| parse_version(text).unwrap();
        let pg14 = v("pg_dump (PostgreSQL) 14.24");
        let pg18 = v("pg_dump (PostgreSQL) 18.6");
        assert_eq!(
            compatibility(Some(&pg14), Engine::Postgres, (17, 2, 0)),
            Compatibility::ClientOlder {
                client_major: 14,
                server_major: 17
            }
        );
        assert!(compatibility(Some(&pg14), Engine::Postgres, (17, 2, 0)).blocks());
        assert_eq!(
            compatibility(Some(&pg18), Engine::Postgres, (17, 2, 0)),
            Compatibility::Ok
        );
        assert_eq!(
            compatibility(Some(&pg14), Engine::Postgres, (14, 0, 0)),
            Compatibility::Ok
        );
        // MySQL and MariaDB: another version only warns.
        let mysql26 = v("mysqldump  Ver 26.10.0 for macos27.0");
        let maria = v("mariadb-dump from 11.8.3-MariaDB, client 10.19");
        let other = compatibility(Some(&mysql26), Engine::MySql, (8, 4, 3));
        assert_eq!(
            other,
            Compatibility::OtherVersion {
                client_major: 26,
                server_major: 8
            }
        );
        assert!(!other.blocks() && other.warning(Engine::MySql).is_some());
        assert_eq!(
            compatibility(Some(&maria), Engine::MariaDb, (11, 4, 0)),
            Compatibility::Ok
        );
        assert_eq!(
            compatibility(Some(&maria), Engine::MariaDb, (11, 8, 0)),
            Compatibility::Ok
        );
        assert!(matches!(
            compatibility(Some(&maria), Engine::MySql, (11, 8, 0)),
            Compatibility::OtherVersion { .. }
        ));
        assert_eq!(
            compatibility(None, Engine::Postgres, (17, 0, 0)),
            Compatibility::Unknown
        );
        assert!(
            Compatibility::ClientOlder {
                client_major: 14,
                server_major: 17
            }
            .warning(Engine::Postgres)
            .unwrap()
            .contains("older than the server")
        );
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(quote("plain-arg=1"), "plain-arg=1");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("it's here"), r"'it'\''s here'");
        assert_eq!(quote("$HOME"), "'$HOME'");
    }

    #[cfg(unix)]
    fn fake_tool(dir: &Path, name: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn find_searches_the_path_then_the_fallbacks() {
        let root = tempfile::tempdir().unwrap();
        let (first, second, fallback) = (
            root.path().join("a"),
            root.path().join("b"),
            root.path().join("c"),
        );
        for dir in [&first, &second, &fallback] {
            std::fs::create_dir(dir).unwrap();
        }
        let path_var = std::env::join_paths([&first, &second]).unwrap();
        let fallbacks = [fallback.clone()];

        assert_eq!(find_in(Tool::PgDump, Some(&path_var), &fallbacks), None);
        // Not executable: skipped.
        fake_tool(&first, "pg_dump", 0o644);
        assert_eq!(find_in(Tool::PgDump, Some(&path_var), &fallbacks), None);
        let in_fallback = fake_tool(&fallback, "pg_dump", 0o755);
        assert_eq!(
            find_in(Tool::PgDump, Some(&path_var), &fallbacks),
            Some(in_fallback.clone())
        );
        assert_eq!(find_in(Tool::PgDump, None, &fallbacks), Some(in_fallback));
        let in_path = fake_tool(&second, "pg_dump", 0o755);
        assert_eq!(
            find_in(Tool::PgDump, Some(&path_var), &fallbacks),
            Some(in_path)
        );

        // Both names are tried in a directory before the next one.
        let maria = fake_tool(&first, "mariadb-dump", 0o755);
        fake_tool(&second, "mysqldump", 0o755);
        assert_eq!(
            find_in(Tool::MysqlDump, Some(&path_var), &fallbacks),
            Some(maria)
        );
        let mysql = fake_tool(&first, "mysqldump", 0o755);
        assert_eq!(
            find_in(Tool::MysqlDump, Some(&path_var), &fallbacks),
            Some(mysql)
        );
        assert_eq!(find_in(Tool::Psql, Some(&path_var), &fallbacks), None);
    }

    #[cfg(unix)]
    #[test]
    fn find_all_lists_every_install_in_search_order_once() {
        let root = tempfile::tempdir().unwrap();
        let (first, second, fallback) = (
            root.path().join("a"),
            root.path().join("b"),
            root.path().join("c"),
        );
        for dir in [&first, &second, &fallback] {
            std::fs::create_dir(dir).unwrap();
        }
        // The same directory twice, as $PATH and as a fallback.
        let path_var = std::env::join_paths([&first, &second, &first]).unwrap();
        let fallbacks = [fallback.clone(), second.clone()];
        assert_eq!(
            find_all_in(Tool::PgDump, Some(&path_var), &fallbacks),
            Vec::<PathBuf>::new()
        );

        let old = fake_tool(&first, "pg_dump", 0o755);
        let newer = fake_tool(&fallback, "pg_dump", 0o755);
        // A link to a binary already listed is not listed again.
        std::os::unix::fs::symlink(&old, second.join("pg_dump")).unwrap();
        assert_eq!(
            find_all_in(Tool::PgDump, Some(&path_var), &fallbacks),
            vec![old.clone(), newer.clone()]
        );
        // `find_in` is the first of them.
        assert_eq!(
            find_in(Tool::PgDump, Some(&path_var), &fallbacks),
            Some(old)
        );
        // Both names of a MySQL tool, each directory in turn.
        let maria = fake_tool(&first, "mariadb", 0o755);
        let mysql = fake_tool(&fallback, "mysql", 0o755);
        assert_eq!(
            find_all_in(Tool::Mysql, Some(&path_var), &fallbacks),
            vec![maria, mysql]
        );
    }

    /// `/bin/sh -c script`, so the test never executes a file it just wrote.
    fn shell(script: &str) -> ToolCommand {
        ToolCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![("KERNO_VISIBLE".into(), "seen".into())],
            password: Some(("PGPASSWORD", Secret::new(PASSWORD))),
            stdin: None,
            stdin_parts: Vec::new(),
            sent: Arc::default(),
            tunnel: None,
            script_check: None,
            files: Vec::new(),
        }
    }

    // The run tests start `/bin/sh` scripts: Unix only (K64).
    #[cfg(unix)]
    fn collect() -> (Arc<Mutex<Vec<String>>>, impl Fn(String) + Send + 'static) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        (lines, move |line| sink.lock().unwrap().push(line))
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_streams_lines_and_reports_the_exit_status() {
        let (lines, on_line) = collect();
        let script = "echo out1; echo err1 >&2; printf 'caf\\351\\n'; \
                      echo \"$KERNO_VISIBLE ${PGPASSWORD:+password set}\"; printf 'last'; exit 3";
        let status = run(shell(script), on_line, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(status.code(), Some(3));
        let mut lines = lines.lock().unwrap().clone();
        lines.sort();
        assert_eq!(
            lines,
            ["caf\u{FFFD}", "err1", "last", "out1", "seen password set"]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_feeds_stdin_and_reports_a_missing_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.sql");
        std::fs::write(&input, "SELECT 1;\nSELECT 2;\n").unwrap();
        let mut command = shell("cat");
        command.stdin = Some(input);
        let sent = command.sent.clone();
        let (lines, on_line) = collect();
        let status = run(command, on_line, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert!(status.success());
        assert_eq!(*lines.lock().unwrap(), ["SELECT 1;", "SELECT 2;"]);
        // Counted, for the progress of a script (CLAUDE.md §4 J9).
        assert_eq!(sent.load(Ordering::Relaxed), 20);

        let mut missing = shell("true");
        missing.stdin = Some(dir.path().join("absent.sql"));
        let error = run(missing, |_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("absent.sql"), "{error}");
        assert!(!error.to_string().contains(PASSWORD));
    }

    /// K74, F1: 600 parts of 600 files feed one child in order, each counted
    /// as asked, under a limit of 64 open files: run in a child of this test
    /// binary started under `ulimit -n 64` (this process's own limit is
    /// left alone, the other tests share it). Unix only: `ulimit`, `/bin/sh`.
    #[cfg(unix)]
    #[test]
    fn parts_are_opened_one_at_a_time() {
        const CHILD: &str = "KERNO_FEED_UNDER_A_LIMIT";
        let name = "tools::tests::parts_are_opened_one_at_a_time";
        if std::env::var_os(CHILD).is_none() {
            let me = std::env::current_exe().unwrap();
            let ran = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("ulimit -n 64 && exec \"$0\" \"$@\"")
                .arg(me)
                .args(["--exact", name, "--test-threads=1", "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            let said = String::from_utf8_lossy(&ran.stdout).to_string()
                + &String::from_utf8_lossy(&ran.stderr);
            assert!(ran.status.success(), "{said}");
            assert!(said.contains("1 passed"), "{said}");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.sql");
        let mut command = shell(&format!("cat > '{}'", out.display()));
        let mut want = String::new();
        for at in 0..600 {
            let path = dir.path().join(format!("{at:04}.sql"));
            std::fs::write(&path, format!("-- head {at}\nSELECT {at};\n")).unwrap();
            // Its second line only, counted; then a text of Kerno's own.
            let head = format!("-- head {at}\n").len() as u64;
            command.stdin_parts.push(StdinPart::File {
                path,
                range: head..u64::MAX,
                counted: at % 2 == 0,
            });
            command
                .stdin_parts
                .push(StdinPart::Text(format!("-- {at}\n")));
            want.push_str(&format!("SELECT {at};\n-- {at}\n"));
        }
        let sent = command.sent.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let status = runtime
            .block_on(run(command, |_| {}, Arc::new(AtomicBool::new(false))))
            .unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), want);
        let counted: usize = (0..600)
            .filter(|at| at % 2 == 0)
            .map(|at| format!("SELECT {at};\n").len())
            .sum();
        assert_eq!(sent.load(Ordering::Relaxed), counted as u64);
    }

    /// K74, F1: a part that cannot be read once the child runs (it went
    /// away after the check) fails the run and kills the child: it never
    /// goes on with a script cut short.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_part_that_cannot_be_read_kills_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.sql");
        let gone = dir.path().join("gone.sql");
        std::fs::write(&first, "SELECT 1;\n").unwrap();
        // A folder passes the check and fails at its read.
        std::fs::create_dir(&gone).unwrap();
        let mut command = shell("cat > /dev/null; echo finished");
        for path in [first, gone.clone()] {
            command.stdin_parts.push(StdinPart::File {
                path,
                range: 0..u64::MAX,
                counted: true,
            });
        }
        let (lines, on_line) = collect();
        let error = run(command, on_line, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("gone.sql"), "{error}");
        assert!(!lines.lock().unwrap().contains(&"finished".to_string()));
        // A part missing before the start: nothing runs.
        let mut missing = shell("echo started");
        missing.stdin_parts.push(StdinPart::File {
            path: dir.path().join("absent.sql"),
            range: 0..u64::MAX,
            counted: true,
        });
        let (lines, on_line) = collect();
        let error = run(missing, on_line, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("absent.sql"), "{error}");
        assert!(lines.lock().unwrap().is_empty());
    }

    /// K74, N1: a part that fails mid-way never shows the child the end of
    /// its input (a client would run a last statement cut short, or end
    /// well), and its error always wins over the child's status: a stand-in
    /// that writes `EOF` once its input ends, 200 runs.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_feed_that_fails_never_ends_the_child_s_input() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.sql");
        std::fs::write(&first, "INSERT INTO t VALUES (1);\nINSERT INTO t VALUES (2").unwrap();
        // A folder passes the check and fails at its read.
        let broken = dir.path().join("broken.sql");
        std::fs::create_dir(&broken).unwrap();
        let out = dir.path().join("out.txt");
        for run_at in 0..200 {
            let _ = std::fs::remove_file(&out);
            let mut command = shell(&format!(
                "cat > '{out}'; echo EOF >> '{out}'; exit 0",
                out = out.display()
            ));
            for path in [first.clone(), broken.clone()] {
                command.stdin_parts.push(StdinPart::File {
                    path,
                    range: 0..u64::MAX,
                    counted: true,
                });
            }
            let ended = run(command, |_| {}, Arc::new(AtomicBool::new(false))).await;
            let error = ended.expect_err(&format!("run {run_at}: the child's status won"));
            assert!(error.to_string().contains("broken.sql"), "{error}");
            // The child, killed, never wrote that its input ended.
            tokio::time::sleep(Duration::from_millis(5)).await;
            let seen = std::fs::read_to_string(&out).unwrap_or_default();
            assert!(!seen.contains("EOF"), "run {run_at}: {seen}");
        }
        // A child that ends well on its own as its feed fails (it reads the
        // first part's bytes and stops): the feed's error wins, every time.
        let ten = dir.path().join("ten.sql");
        std::fs::write(&ten, "SELECT 1;\n").unwrap();
        for run_at in 0..500 {
            let mut command = shell("head -c 10 > /dev/null; exit 0");
            for path in [ten.clone(), broken.clone()] {
                command.stdin_parts.push(StdinPart::File {
                    path,
                    range: 0..u64::MAX,
                    counted: true,
                });
            }
            let ended = run(command, |_| {}, Arc::new(AtomicBool::new(false))).await;
            let error = ended.expect_err(&format!("run {run_at}: the child's status won"));
            // The feed's own error, not the words of a client that stopped
            // reading: the feed is waited for once the child ended (K79).
            assert!(
                error.to_string().contains("could not read")
                    && error.to_string().contains("broken.sql"),
                "run {run_at}: {error}"
            );
        }
    }

    /// K79: a client that stops reading its script on its own (`quit`,
    /// `\q`) and ends well fails the run in plain words, naming the file it
    /// stopped in (a text of Kerno's own by the file before it); one that
    /// stopped on an error keeps its status
    /// (`a_client_that_stops_reading_its_script_ends_the_run`).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_client_that_ends_well_before_the_end_of_its_input_fails_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let ten = dir.path().join("ten.sql");
        std::fs::write(&ten, "SELECT 1;\n").unwrap();
        // More than a pipe holds: its writes fail once the child is gone.
        let rest = dir.path().join("rest.sql");
        std::fs::write(&rest, vec![b'x'; 1024 * 1024]).unwrap();
        let file = |path: &Path| StdinPart::File {
            path: path.to_path_buf(),
            range: 0..u64::MAX,
            counted: true,
        };
        let mut command = shell("head -c 10 > /dev/null; exit 0");
        command.stdin_parts = vec![file(&ten), file(&rest)];
        let error = run(command, |_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        let Error::Config(text) = &error else {
            panic!("{error:?}");
        };
        assert_eq!(
            *text,
            format!(
                "sh stopped reading before the end of {} and ended without an error: the rest \
                 of the script was not run.",
                rest.display()
            )
        );
        // Stopped at a text of Kerno's own: named by the file it follows.
        let mut command = shell("head -c 10 > /dev/null; exit 0");
        command.stdin_parts = vec![file(&ten), StdinPart::Text("x".repeat(1024 * 1024))];
        let error = run(command, |_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("end of ") && error.to_string().contains("ten.sql"));
        // Everything fed: the child's status, whatever it read.
        let mut command = shell("head -c 10 > /dev/null; exit 0");
        command.stdin_parts = vec![file(&ten)];
        let status = run(command, |_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert!(status.success());
    }

    /// K79, what the Linux runner saw once: the child ended well while its
    /// feed was still on its way to the next part (here held for good, at
    /// the opening of a named pipe nobody writes): the run fails, it never
    /// takes the child's status for a whole script.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_child_that_ends_well_before_its_feed_does_fails_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let ten = dir.path().join("ten.sql");
        std::fs::write(&ten, "SELECT 1;\n").unwrap();
        let held = dir.path().join("held.sql");
        let made = std::process::Command::new("mkfifo")
            .arg(&held)
            .status()
            .unwrap();
        assert!(made.success());
        let mut command = shell("head -c 10 > /dev/null; exit 0");
        for path in [ten, held.clone()] {
            command.stdin_parts.push(StdinPart::File {
                path,
                range: 0..u64::MAX,
                counted: true,
            });
        }
        let started = Instant::now();
        let ended = run(command, |_| {}, Arc::new(AtomicBool::new(false))).await;
        // The feed's opening returns once the pipe has a writer: the
        // runtime then ends.
        drop(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&held)
                .unwrap(),
        );
        let error = ended.expect_err("the child's status won");
        assert!(error.to_string().contains("held.sql"), "{error}");
        // Waited for no longer than said.
        assert!(started.elapsed() < FEED_AFTER_EXIT + Duration::from_secs(3));
    }

    /// A client that stops reading (psql's ON_ERROR_STOP) ends the run at
    /// once, with what was fed so far; one that never reads is cancelled.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_client_that_stops_reading_its_script_ends_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("big.sql");
        std::fs::write(&input, vec![b'x'; 8 * 1024 * 1024]).unwrap();
        let mut command = shell("head -c 1000 >/dev/null; exit 3");
        command.stdin = Some(input.clone());
        let sent = command.sent.clone();
        let started = Instant::now();
        let status = run(command, |_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(status.code(), Some(3));
        assert!(sent.load(Ordering::Relaxed) < 8 * 1024 * 1024);
        assert!(started.elapsed() < Duration::from_secs(10));

        let mut stuck = shell("exec sleep 30");
        stuck.stdin = Some(input);
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn(run(stuck, |_| {}, cancel.clone()));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let started = Instant::now();
        cancel.store(true, Ordering::Relaxed);
        let outcome = task.await.unwrap();
        assert!(matches!(outcome, Err(Error::Cancelled)), "{outcome:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn inherited_database_variables_are_recognised() {
        for name in [
            "PGHOST",
            "PGSERVICE",
            "PGPASSFILE",
            "MYSQL_HOST",
            "MYSQL_PWD",
            "MARIADB_HOST",
        ] {
            assert!(is_database_variable(OsStr::new(name)), "{name}");
        }
        for name in ["PATH", "HOME", "LANG", "KERNO_VISIBLE"] {
            assert!(!is_database_variable(OsStr::new(name)), "{name}");
        }
    }

    #[tokio::test]
    async fn run_reports_a_tool_that_cannot_start() {
        let mut command = shell("true");
        command.program = "/nonexistent/pg_dump".into();
        let error = run(command, |_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains("could not start /nonexistent/pg_dump"),
            "{text}"
        );
        assert!(!text.contains(PASSWORD), "{text}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_kills_the_child() {
        let cancel = Arc::new(AtomicBool::new(false));
        let (lines, on_line) = collect();
        // `exec` keeps the pid: the line is the pid of the process to kill.
        let task = tokio::spawn(run(
            shell("echo $$; exec sleep 30"),
            on_line,
            cancel.clone(),
        ));
        let started = Instant::now();
        let pid = loop {
            if let Some(pid) = lines.lock().unwrap().first() {
                break pid.clone();
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the child never started"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let cancelled_at = Instant::now();
        cancel.store(true, Ordering::Relaxed);
        let outcome = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(outcome, Err(Error::Cancelled)), "{outcome:?}");
        assert!(cancelled_at.elapsed() < Duration::from_secs(2));
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(!alive.success(), "process {pid} still runs");
    }

    #[test]
    fn task_settings_round_trip_without_a_password_and_refuse_one_in_extra_arguments() {
        let mut backup = BackupOptions::new(Engine::Postgres, "shop");
        backup.auth_user = Some("backup_user".into());
        backup.tables = vec![TableRef::new("shop", "orders")];
        let mut restore =
            RestoreOptions::new(Engine::MySql, "shop", "/b/shop.sql", BackupFormat::Sql);
        restore.allow_client_commands = true;
        let tasks = [
            TaskSettings::Backup(backup),
            TaskSettings::GlobalBackup(GlobalBackupOptions::default()),
            TaskSettings::Restore(restore),
            TaskSettings::ExecuteScript(ScriptOptions::new(Engine::Postgres, "shop", "/s.sql")),
        ];
        for task in &tasks {
            let json = serde_json::to_string(task).unwrap();
            assert!(
                json.contains(&format!("\"kind\":\"{}\"", task.kind())),
                "{json}"
            );
            // No field holds a password (`role_passwords` is a switch).
            assert!(
                !json.contains("password\":") && !json.contains(PASSWORD),
                "{json}"
            );
            let back: TaskSettings = serde_json::from_str(&json).unwrap();
            // The opt-in to a file's client commands is asked at each run.
            match (task, &back) {
                (TaskSettings::Restore(saved), TaskSettings::Restore(read)) => {
                    assert!(saved.allow_client_commands && !read.allow_client_commands);
                    assert_eq!(read.input, saved.input);
                }
                _ => assert_eq!(&back, task),
            }
        }
        assert_eq!(tasks[0].auth_user(), Some("backup_user"));
        assert_eq!(tasks[0].database(), Some("shop"));
        assert_eq!(tasks[1].database(), None);
        assert!(!tasks[0].writes() && !tasks[1].writes());
        assert!(tasks[2].writes() && tasks[3].writes());

        // A password typed among the extra arguments is never kept.
        let mut leaky = BackupOptions::new(Engine::Postgres, "shop");
        leaky.extra_args = format!("--password={PASSWORD}");
        assert!(
            TaskSettings::Backup(leaky.clone())
                .check(Engine::Postgres)
                .is_err()
        );
        leaky.extra_args = "--lock-wait-timeout=5000".into();
        assert!(TaskSettings::Backup(leaky).check(Engine::Postgres).is_ok());
        let mut script = ScriptOptions::new(Engine::MySql, "shop", "/s.sql");
        script.extra_args = format!("--password={PASSWORD}");
        assert!(
            TaskSettings::ExecuteScript(script)
                .check(Engine::MySql)
                .is_err()
        );
    }
}

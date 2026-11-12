//! Backup and restore through the engines' own client tools (`pg_dump`,
//! `pg_restore`, `psql`, `mysqldump`, `mysql` and their MariaDB names): find
//! them, build their command lines from a connection, run them with their
//! output streamed line by line.
//!
//! The password never reaches a command line, where any local user can read
//! it in the process list: it goes to the child through its environment
//! (`PGPASSWORD`, `MYSQL_PWD`), and [`ToolCommand::display`] and `Debug` only
//! name the variable.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::config::{ConnectParams, Engine, Secret, TlsConfig, TlsMode};
use crate::error::{Error, Result};
use crate::schema::TableRef;
use crate::ssh::Tunnel;

/// A client tool Kerno runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    /// `pg_dump`.
    PgDump,
    /// `pg_restore`, for custom-format dumps.
    PgRestore,
    /// `psql`, for plain SQL dumps.
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
            Self::PgRestore => &["pg_restore"],
            Self::Psql => &["psql"],
            Self::MysqlDump => &["mysqldump", "mariadb-dump"],
            Self::Mysql => &["mysql", "mariadb"],
        }
    }
}

/// Where the tools usually are when `$PATH` does not have them (a GUI app on
/// macOS gets a short `$PATH`).
pub const FALLBACK_DIRS: [&str; 7] = [
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/opt/homebrew/opt/libpq/bin",
    "/opt/homebrew/opt/mysql-client/bin",
    "/opt/homebrew/opt/mariadb/bin",
    "/Applications/Postgres.app/Contents/Versions/latest/bin",
    "/usr/bin",
];

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
    let from_path = path_var.into_iter().flat_map(std::env::split_paths);
    from_path
        .chain(fallbacks.iter().cloned())
        .filter(|dir| !dir.as_os_str().is_empty())
        .flat_map(|dir| tool.names().iter().map(move |name| dir.join(name)))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// The tool that writes a backup of `engine`.
pub fn backup_tool(engine: Engine) -> Tool {
    match engine {
        Engine::Postgres => Tool::PgDump,
        Engine::MySql | Engine::MariaDb => Tool::MysqlDump,
    }
}

/// The tool that restores a backup of `engine` written in `format`.
pub fn restore_tool(engine: Engine, format: BackupFormat) -> Tool {
    match (engine, format) {
        (Engine::Postgres, BackupFormat::Sql) => Tool::Psql,
        (Engine::Postgres, BackupFormat::Custom) => Tool::PgRestore,
        (Engine::MySql | Engine::MariaDb, _) => Tool::Mysql,
    }
}

/// How a backup is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackupFormat {
    /// A plain SQL script (every engine).
    #[default]
    Sql,
    /// PostgreSQL's compressed custom format (`pg_dump -Fc`), restored with
    /// `pg_restore`.
    Custom,
}

/// What a backup contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackupContent {
    /// Definitions and data.
    #[default]
    All,
    /// Definitions only.
    SchemaOnly,
    /// Data only.
    DataOnly,
}

/// What to back up and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupOptions {
    /// The database to back up.
    pub database: String,
    /// The file written.
    pub output: PathBuf,
    /// Its format.
    pub format: BackupFormat,
    /// Definitions, data, or both.
    pub content: BackupContent,
    /// Only these tables; every table when empty. On MySQL they must be in
    /// `database`.
    pub tables: Vec<TableRef>,
}

/// What to restore and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOptions {
    /// The database restored into.
    pub database: String,
    /// The backup file.
    pub input: PathBuf,
    /// Its format.
    pub format: BackupFormat,
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
    /// The SSH tunnel to open before starting the child.
    pub tunnel: Option<TunnelHop>,
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

/// The command that backs up `options.database` with the dump tool at
/// `tool_path` (`pg_dump`, or `mysqldump` / `mariadb-dump`).
pub fn backup_command(
    tool_path: &Path,
    params: &ConnectParams,
    options: &BackupOptions,
) -> Result<ToolCommand> {
    let mut command = connection_command(tool_path, params);
    let args = &mut command.args;
    match params.engine {
        Engine::Postgres => {
            args.push("--verbose".into());
            args.push(format!("--dbname={}", pg_dbname(&options.database)));
            args.push(format!("--file={}", options.output.display()));
            args.push(
                match options.format {
                    BackupFormat::Sql => "--format=plain",
                    BackupFormat::Custom => "--format=custom",
                }
                .into(),
            );
            match options.content {
                BackupContent::All => {}
                BackupContent::SchemaOnly => args.push("--schema-only".into()),
                BackupContent::DataOnly => args.push("--data-only".into()),
            }
            if !options.tables.is_empty() {
                // `--table` takes a pattern; quoted parts match literally.
                // A pattern that matches nothing fails instead of dumping nothing.
                args.push("--strict-names".into());
                for table in &options.tables {
                    let ident = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
                    args.push(format!(
                        "--table={}.{}",
                        ident(&table.schema),
                        ident(&table.name)
                    ));
                }
            }
        }
        Engine::MySql | Engine::MariaDb => {
            if options.format == BackupFormat::Custom {
                return Err(Error::Unsupported(
                    "MySQL backups are SQL scripts; the custom format is PostgreSQL's".into(),
                ));
            }
            args.push("--verbose".into());
            args.push("--single-transaction".into());
            match options.content {
                BackupContent::All => args.push("--routines".into()),
                BackupContent::SchemaOnly => {
                    args.extend(["--no-data".into(), "--routines".into()]);
                }
                BackupContent::DataOnly => {
                    args.extend(["--no-create-info".into(), "--skip-triggers".into()]);
                }
            }
            args.push(format!("--result-file={}", options.output.display()));
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
    }
    Ok(command)
}

/// The command that restores `options.input` into `options.database` with
/// the tool at `tool_path` (see [`restore_tool`]). Refused on a read-only
/// connection.
pub fn restore_command(
    tool_path: &Path,
    params: &ConnectParams,
    options: &RestoreOptions,
) -> Result<ToolCommand> {
    if params.read_only {
        return Err(Error::ReadOnly(
            "restoring a backup writes to the database".into(),
        ));
    }
    let mut command = connection_command(tool_path, params);
    let args = &mut command.args;
    match (params.engine, options.format) {
        (Engine::Postgres, BackupFormat::Sql) => {
            // ~/.psqlrc could turn ON_ERROR_STOP off or run its own commands.
            args.push("--no-psqlrc".into());
            args.push(format!("--dbname={}", pg_dbname(&options.database)));
            // Without ON_ERROR_STOP psql goes on after a failed statement
            // and exits 0.
            args.push("--set=ON_ERROR_STOP=1".into());
            args.push("--single-transaction".into());
            args.push(format!("--file={}", options.input.display()));
        }
        (Engine::Postgres, BackupFormat::Custom) => {
            args.push("--verbose".into());
            args.push(format!("--dbname={}", pg_dbname(&options.database)));
            args.push("--single-transaction".into());
            args.push(options.input.display().to_string());
        }
        (Engine::MySql | Engine::MariaDb, BackupFormat::Sql) => {
            args.push(format!("--database={}", options.database));
            command.stdin = Some(options.input.clone());
        }
        (Engine::MySql | Engine::MariaDb, BackupFormat::Custom) => {
            return Err(Error::Unsupported(
                "MySQL backups are SQL scripts; the custom format is PostgreSQL's".into(),
            ));
        }
    }
    Ok(command)
}

/// The program with the arguments and environment that say which server to
/// reach, as whom, and over which TLS.
fn connection_command(tool_path: &Path, params: &ConnectParams) -> ToolCommand {
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
    }
    let port_arg = args
        .iter()
        .position(|arg| arg.starts_with("--port="))
        .unwrap_or_default();
    ToolCommand {
        program: tool_path.to_path_buf(),
        args,
        env,
        password: params
            .password
            .clone()
            .filter(|password| !password.is_empty())
            .map(|password| (password_var, password)),
        stdin: None,
        tunnel: params.tunnel.clone().map(|tunnel| TunnelHop {
            tunnel,
            host: params.host.clone(),
            port: params.port,
            port_arg,
        }),
    }
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
    let name = name.to_string_lossy();
    ["PG", "MYSQL", "MARIADB"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// How often [`run`] looks at its cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// Runs `command`, passing every line it writes (standard output and
/// error, as they come) to `on_line`. Returns the exit status, which may
/// be a failure; `Err` means the tool could not be started, or
/// [`Error::Cancelled`] when `cancel` was set (the child is killed).
pub async fn run(
    command: ToolCommand,
    on_line: impl Fn(String) + Send + 'static,
    cancel: Arc<AtomicBool>,
) -> Result<ExitStatus> {
    let mut args = command.args;
    if let Some(hop) = &command.tunnel {
        let local = hop.tunnel.forward_once(&hop.host, hop.port).await?;
        args[hop.port_arg] = format!("--port={}", local.port());
    }
    let mut child = tokio::process::Command::new(&command.program);
    child.args(&args).kill_on_drop(true);
    // Inherited libpq / MySQL variables (PGHOST, PGSERVICE, MYSQL_HOST...)
    // could send the tool to another server.
    for (name, _) in std::env::vars_os().filter(|(name, _)| is_database_variable(name)) {
        child.env_remove(name);
    }
    child.envs(command.env);
    if let Some((name, password)) = &command.password {
        child.env(name, password.expose());
    }
    child.stdin(match &command.stdin {
        Some(path) => std::fs::File::open(path)
            .map_err(|error| Error::io(format!("could not open {}", path.display()), error))?
            .into(),
        None => Stdio::null(),
    });
    child.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = child.spawn().map_err(|error| {
        Error::io(
            format!("could not start {}", command.program.display()),
            error,
        )
    })?;

    let mut stdout = child.stdout.take().map(BufReader::new);
    let mut stderr = child.stderr.take().map(BufReader::new);
    let (mut out_line, mut err_line) = (Vec::new(), Vec::new());
    let mut tick = tokio::time::interval(CANCEL_POLL);
    loop {
        tokio::select! {
            read = read_line(&mut stdout, &mut out_line) => {
                if !emit(read, &mut out_line, &on_line) { stdout = None; }
            }
            read = read_line(&mut stderr, &mut err_line) => {
                if !emit(read, &mut err_line, &on_line) { stderr = None; }
            }
            // Both pipes are read to their end first, so a chatty child
            // never blocks on a full pipe.
            status = child.wait(), if stdout.is_none() && stderr.is_none() => {
                return status.map_err(|error| Error::io("waiting for the tool", error));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
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
            tls: TlsConfig::default(),
            read_only: false,
            options: BTreeMap::new(),
        }
    }

    fn backup(database: &str) -> BackupOptions {
        BackupOptions {
            database: database.into(),
            output: "/tmp/out dir/shop.sql".into(),
            format: BackupFormat::Sql,
            content: BackupContent::All,
            tables: Vec::new(),
        }
    }

    fn restore(format: BackupFormat) -> RestoreOptions {
        RestoreOptions {
            database: "shop_copy".into(),
            input: "/backups/shop.dump".into(),
            format,
        }
    }

    fn assert_no_secret(command: &ToolCommand) {
        let shown = format!("{} {command:?}", command.display());
        assert!(!shown.contains(PASSWORD), "{shown}");
    }

    #[test]
    fn pg_dump_command_lines() {
        let pg_dump = Path::new("/opt/homebrew/bin/pg_dump");
        let command = backup_command(pg_dump, &params(Engine::Postgres), &backup("shop")).unwrap();
        assert_eq!(
            command.display(),
            "PGSSLMODE=prefer PGPASSWORD=••• /opt/homebrew/bin/pg_dump --no-password \
             --host=db.example.com --port=6543 --username=ada --verbose --dbname=shop \
             '--file=/tmp/out dir/shop.sql' --format=plain"
        );
        assert_eq!(
            command
                .password
                .as_ref()
                .map(|(var, secret)| (*var, secret.expose())),
            Some(("PGPASSWORD", PASSWORD))
        );
        assert_no_secret(&command);

        let mut params = params(Engine::Postgres);
        params.tls = TlsConfig {
            mode: TlsMode::VerifyFull,
            ca_file: Some("/certs/ca.pem".into()),
            cert_file: Some("/certs/client.pem".into()),
            key_file: Some("/certs/client.key".into()),
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
        let command = backup_command(pg_dump, &params, &options).unwrap();
        assert_eq!(
            command.display(),
            "PGSSLMODE=verify-full PGSSLROOTCERT=/certs/ca.pem PGSSLCERT=/certs/client.pem \
             PGSSLKEY=/certs/client.key PGPASSWORD=••• /opt/homebrew/bin/pg_dump --no-password \
             --host=db.example.com --port=6543 --username=ada --verbose --dbname=shop \
             '--file=/tmp/out dir/shop.sql' --format=custom --schema-only --strict-names \
             '--table=\"public\".\"orders\"' '--table=\"Sales\".\"odd\"\"name\"'"
        );

        let options = BackupOptions {
            content: BackupContent::DataOnly,
            ..backup("a=b host=evil")
        };
        let command = backup_command(pg_dump, &params, &options).unwrap();
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
             --single-transaction --file=/backups/shop.dump"
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
             --single-transaction /backups/shop.dump"
        );
    }

    #[test]
    fn mysql_command_lines() {
        let dump = Path::new("/usr/bin/mysqldump");
        let options = BackupOptions {
            tables: vec![
                TableRef::new("shop", "orders"),
                TableRef::new("shop", "items"),
            ],
            ..backup("shop")
        };
        let command = backup_command(dump, &params(Engine::MySql), &options).unwrap();
        assert_eq!(
            command.display(),
            "MYSQL_PWD=••• /usr/bin/mysqldump --no-defaults --protocol=TCP --host=db.example.com \
             --port=6543 --user=ada --ssl-mode=PREFERRED --verbose --single-transaction \
             --routines '--result-file=/tmp/out dir/shop.sql' shop orders items"
        );
        assert_no_secret(&command);

        let mut params_maria = params(Engine::MariaDb);
        params_maria.tls = TlsConfig {
            mode: TlsMode::VerifyCa,
            ca_file: Some("/certs/ca.pem".into()),
            ..TlsConfig::default()
        };
        let options = BackupOptions {
            content: BackupContent::DataOnly,
            ..backup("shop")
        };
        let command =
            backup_command(Path::new("/usr/bin/mariadb-dump"), &params_maria, &options).unwrap();
        assert_eq!(
            command.display(),
            "MYSQL_PWD=••• /usr/bin/mariadb-dump --no-defaults --protocol=TCP \
             --host=db.example.com --port=6543 --user=ada --ssl --ssl-verify-server-cert \
             --ssl-ca=/certs/ca.pem --verbose --single-transaction --no-create-info \
             --skip-triggers '--result-file=/tmp/out dir/shop.sql' shop"
        );

        let options = BackupOptions {
            content: BackupContent::SchemaOnly,
            ..backup("shop")
        };
        let command = backup_command(dump, &params(Engine::MySql), &options).unwrap();
        assert!(command.args.ends_with(&[
            "--no-data".into(),
            "--routines".into(),
            "--result-file=/tmp/out dir/shop.sql".into(),
            "shop".into()
        ]));

        let command = restore_command(
            Path::new("/usr/bin/mysql"),
            &params(Engine::MySql),
            &restore(BackupFormat::Sql),
        )
        .unwrap();
        assert_eq!(
            command.display(),
            "MYSQL_PWD=••• /usr/bin/mysql --no-defaults --protocol=TCP --host=db.example.com \
             --port=6543 --user=ada --ssl-mode=PREFERRED --database=shop_copy < /backups/shop.dump"
        );
        assert_no_secret(&command);
    }

    #[test]
    fn mysql_tls_options_follow_the_binary_not_the_server() {
        let maria_server = params(Engine::MariaDb);
        let command = backup_command(
            Path::new("/usr/bin/mysqldump"),
            &maria_server,
            &backup("shop"),
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
        let command = backup_command(&link, &verify, &backup("shop")).unwrap();
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
            ..backup("shop")
        };
        assert!(matches!(
            backup_command(path, &params(Engine::MySql), &custom),
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
            backup_command(path, &params(Engine::MySql), &backup("-evil")),
            Err(Error::Config(_))
        ));
        let other_db = BackupOptions {
            tables: vec![TableRef::new("other", "t")],
            ..backup("shop")
        };
        assert!(matches!(
            backup_command(path, &params(Engine::MySql), &other_db),
            Err(Error::Config(_))
        ));
        let mut read_only = params(Engine::Postgres);
        read_only.read_only = true;
        assert!(matches!(
            restore_command(path, &read_only, &restore(BackupFormat::Sql)),
            Err(Error::ReadOnly(_))
        ));
        // Backing up a read-only connection is fine.
        assert!(backup_command(path, &read_only, &backup("shop")).is_ok());
    }

    #[test]
    fn no_password_means_no_password_variable() {
        let mut params = params(Engine::Postgres);
        params.password = None;
        let command = backup_command(Path::new("/x/pg_dump"), &params, &backup("shop")).unwrap();
        assert!(command.password.is_none());
        assert!(!command.display().contains("PGPASSWORD"));
        params.password = Some(Secret::new(""));
        let command = backup_command(Path::new("/x/pg_dump"), &params, &backup("shop")).unwrap();
        assert!(command.password.is_none());
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(quote("plain-arg=1"), "plain-arg=1");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("it's here"), r"'it'\''s here'");
        assert_eq!(quote("$HOME"), "'$HOME'");
    }

    fn fake_tool(dir: &Path, name: &str, mode: u32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

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

    /// `/bin/sh -c script`, so the test never executes a file it just wrote.
    fn shell(script: &str) -> ToolCommand {
        ToolCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![("KERNO_VISIBLE".into(), "seen".into())],
            password: Some(("PGPASSWORD", Secret::new(PASSWORD))),
            stdin: None,
            tunnel: None,
        }
    }

    fn collect() -> (Arc<Mutex<Vec<String>>>, impl Fn(String) + Send + 'static) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        (lines, move |line| sink.lock().unwrap().push(line))
    }

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

    #[tokio::test]
    async fn run_feeds_stdin_and_reports_a_missing_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.sql");
        std::fs::write(&input, "SELECT 1;\nSELECT 2;\n").unwrap();
        let mut command = shell("cat");
        command.stdin = Some(input);
        let (lines, on_line) = collect();
        let status = run(command, on_line, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert!(status.success());
        assert_eq!(*lines.lock().unwrap(), ["SELECT 1;", "SELECT 2;"]);

        let mut missing = shell("true");
        missing.stdin = Some(dir.path().join("absent.sql"));
        let error = run(missing, |_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("absent.sql"), "{error}");
        assert!(!error.to_string().contains(PASSWORD));
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
}

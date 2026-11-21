//! Where a connection's database password comes from (J3; DBeaver #3270,
//! #9413; Beekeeper #1443, #530): the stored or asked one, `~/.pgpass`
//! (PostgreSQL) and `~/.my.cnf` (MySQL, MariaDB) when nothing is stored, or
//! a command — custom, AWS RDS IAM (`aws rds generate-db-auth-token`) or
//! 1Password (`op read`).
//!
//! A command runs without a shell, its arguments as a list (placeholders
//! are replaced inside each argument, never re-split), stdin closed, under a
//! timeout. Its output is a secret: never logged, never in an error, never
//! stored; it lives in the [`crate::Connector`] that asked for it. A command
//! that came from an imported file runs only once the user has confirmed it
//! ([`crate::config::ConnectionConfig::password_command_unconfirmed`]).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt as _;

use crate::config::{ConnectionConfig, Engine, PasswordSource, Secret};
use crate::error::{Error, Result};

/// What the password readers and commands see of the user's environment,
/// built once by the app (tests pass a temporary directory: `HOME`, `PATH`
/// and `PGPASSFILE` are never read here).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PasswordEnv {
    /// Directories a command's program is looked for in, in order; also the
    /// command's `PATH`.
    pub path: Vec<PathBuf>,
    /// The home directory: `~/.pgpass`, `~/.my.cnf`, a program's `~/`.
    pub home: Option<PathBuf>,
    /// `PGPASSFILE`, which replaces `~/.pgpass`.
    pub pgpassfile: Option<PathBuf>,
}

/// Where a GUI app's short `PATH` misses programs (Homebrew, then the
/// system).
const USUAL_DIRS: [&str; 4] = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

/// Most of a command's output that is read: a password, not a file.
const MAX_OUTPUT: u64 = 64 * 1024;

/// What a failing command's error shows of its messages (their first line).
const MAX_STDERR: u64 = 4 * 1024;

/// What every command error starts with, for [`crate::error::explain`].
pub(crate) const COMMAND_MARKER: &str = "the password command `";

/// Why an imported command does not run.
pub(crate) const UNCONFIRMED: &str = "the password command of this connection came from an \
     imported file and has not been confirmed";

/// What follows the file name in a reader's warning.
pub(crate) const IGNORED: &str = " was ignored: ";

impl PasswordEnv {
    /// The environment of the running app: `path` is the `PATH` variable,
    /// to which the usual directories are added.
    pub fn detect(
        path: Option<&OsStr>,
        home: Option<PathBuf>,
        pgpassfile: Option<PathBuf>,
    ) -> Self {
        let mut dirs: Vec<PathBuf> = path
            .map(std::env::split_paths)
            .into_iter()
            .flatten()
            .filter(|dir| dir.is_absolute())
            .collect();
        for usual in USUAL_DIRS.map(PathBuf::from) {
            if !dirs.contains(&usual) {
                dirs.push(usual);
            }
        }
        Self {
            path: dirs,
            home,
            pgpassfile,
        }
    }

    /// `path` as the user writes it: `~/…` under the home directory.
    pub fn display(&self, path: &Path) -> String {
        match self
            .home
            .as_deref()
            .and_then(|home| path.strip_prefix(home).ok())
        {
            Some(rest) => format!("~/{}", rest.display()),
            None => path.display().to_string(),
        }
    }

    /// The program a command names: a path (`~/` expanded), or the first
    /// executable file of that name in [`PasswordEnv::path`].
    fn find(&self, program: &str) -> Option<PathBuf> {
        if program.contains('/') {
            let path = match (program.strip_prefix("~/"), &self.home) {
                (Some(rest), Some(home)) => home.join(rest),
                _ => PathBuf::from(program),
            };
            return Some(path);
        }
        self.path
            .iter()
            .map(|dir| dir.join(program))
            .find(|candidate| is_executable(candidate))
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Where the password of a session came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordOrigin {
    /// The secrets handed to the connector: the secret store, or typed.
    Given,
    /// A password file (`~/.pgpass`, `~/.my.cnf`), as the user writes it.
    File(String),
    /// A command, by its program's name (`aws`, `op`).
    Command(String),
}

/// A command to run, as it will run: no shell, the arguments as they are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLine {
    pub program: String,
    pub args: Vec<String>,
    pub timeout: Duration,
}

impl CommandLine {
    /// The program's name, for messages (`aws`, not `/opt/homebrew/bin/aws`).
    pub fn name(&self) -> &str {
        self.program.rsplit('/').next().unwrap_or(&self.program)
    }

    /// The whole line, each argument quoted when it needs it, to show the
    /// user what will run. Never holds a secret (the sources hold none).
    pub fn display(&self) -> String {
        std::iter::once(&self.program)
            .chain(&self.args)
            .map(|word| {
                if !word.is_empty()
                    && word
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_./:=@,+%~${}".contains(c))
                {
                    word.clone()
                } else {
                    format!("'{}'", word.replace('\'', r"'\''"))
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The command `config`'s password source runs, `None` for
/// [`PasswordSource::Stored`]. `database` is the session's database; the
/// connection's own when `None`.
pub fn command_line(config: &ConnectionConfig, database: Option<&str>) -> Option<CommandLine> {
    let database = database.or(config.database.as_deref()).unwrap_or_default();
    let words = |words: &[&str]| words.iter().map(|word| word.to_string()).collect();
    let timeout = Duration::from_secs(crate::config::DEFAULT_COMMAND_TIMEOUT.into());
    match &config.password_source {
        PasswordSource::Stored => None,
        PasswordSource::Command {
            program,
            args,
            timeout_secs,
        } => Some(CommandLine {
            program: program.trim().to_string(),
            args: args
                .iter()
                .map(|arg| {
                    arg.replace("${host}", &config.host)
                        .replace("${port}", &config.port.to_string())
                        .replace("${user}", &config.user)
                        .replace("${database}", database)
                })
                .collect(),
            timeout: Duration::from_secs((*timeout_secs).clamp(1, 120).into()),
        }),
        PasswordSource::RdsIam { profile, region } => {
            let mut args: Vec<String> = words(&["rds", "generate-db-auth-token", "--hostname"]);
            args.extend([
                config.host.clone(),
                "--port".into(),
                config.port.to_string(),
                "--username".into(),
                config.user.clone(),
                "--region".into(),
                region.trim().to_string(),
            ]);
            if let Some(profile) = profile.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
                args.extend(["--profile".to_string(), profile.to_string()]);
            }
            Some(CommandLine {
                program: "aws".into(),
                args,
                timeout,
            })
        }
        PasswordSource::OnePassword { reference } => Some(CommandLine {
            program: "op".into(),
            args: vec!["read".into(), reference.trim().to_string()],
            timeout,
        }),
    }
}

/// Runs `line` and returns what it printed, without one trailing newline.
/// A failure says the exit status and the first line of its messages,
/// never what it printed.
pub async fn run(line: &CommandLine, env: &PasswordEnv) -> Result<Secret> {
    let name = line.name().to_string();
    let fail = |what: String| Error::Connect(format!("{COMMAND_MARKER}{name}` {what}"));
    let program = env.find(&line.program).ok_or_else(|| {
        fail(format!(
            "was not found (looked in {})",
            env.path
                .iter()
                .map(|dir| dir.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    let mut command = tokio::process::Command::new(&program);
    command
        .args(&line.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Ok(path) = std::env::join_paths(&env.path) {
        command.env("PATH", path);
    }
    if let Some(home) = &env.home {
        command.env("HOME", home);
    }
    let mut child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => fail(format!("was not found at {}", program.display())),
        _ => fail(format!("could not start: {error}")),
    })?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(fail("could not start: no pipes".into()));
    };
    let work = async {
        let read_out = async move {
            let mut out = Vec::new();
            stdout
                .take(MAX_OUTPUT + 1)
                .read_to_end(&mut out)
                .await
                .map(|_| out)
        };
        let read_err = async move {
            let mut err = Vec::new();
            let _ = stderr.take(MAX_STDERR).read_to_end(&mut err).await;
            err
        };
        let (out, err) = tokio::join!(read_out, read_err);
        (out, err, child.wait().await)
    };
    let finished = tokio::time::timeout(line.timeout, work).await;
    let Ok((out, err, status)) = finished else {
        // Killed and reaped, not left to run on.
        let _ = child.kill().await;
        return Err(fail(format!(
            "did not finish in {} s",
            line.timeout.as_secs()
        )));
    };
    let out = out.map_err(|error| fail(format!("could not be read: {error}")))?;
    let status = status.map_err(|error| fail(format!("could not be waited for: {error}")))?;
    // Cut short, it may also have died of the closed pipe: said first.
    if out.len() as u64 > MAX_OUTPUT {
        return Err(fail(
            "printed more than 64 KB: a password is expected".into(),
        ));
    }
    if !status.success() {
        let stderr = String::from_utf8_lossy(&err);
        let mut first = stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .to_string();
        // Its messages may repeat what it printed: never show that.
        let printed = String::from_utf8_lossy(&out);
        for line in printed
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            first = first.replace(line, "***");
        }
        let status = match status.code() {
            Some(code) => format!("exit status {code}"),
            None => "killed by a signal".to_string(),
        };
        return Err(fail(if first.is_empty() {
            format!("failed ({status})")
        } else {
            format!("failed ({status}): {first}")
        }));
    }
    let mut text =
        String::from_utf8(out).map_err(|_| fail("printed something that is not text".into()))?;
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    if text.is_empty() {
        return Err(fail("printed nothing".into()));
    }
    Ok(Secret::new(text))
}

// Password files ----------------------------------------------------------------

/// The password `~/.pgpass` (libpq's format) gives for these values: lines
/// `host:port:database:user:password`, `*` matching anything in the first
/// four fields, `\:` and `\\` escapes, `#` comments; the first match wins.
pub fn pgpass_password(
    text: &str,
    host: &str,
    port: u16,
    database: &str,
    user: &str,
) -> Option<String> {
    let port = port.to_string();
    let wanted = [host, port.as_str(), database, user];
    text.lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(pgpass_fields)
        .find(|fields| {
            fields.len() >= 5
                && fields
                    .iter()
                    .zip(wanted)
                    .all(|(pattern, value)| pattern == "*" || pattern == value)
        })
        .map(|fields| fields[4].clone())
}

/// The fields of a `.pgpass` line, split on unescaped colons.
fn pgpass_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => field.extend(chars.next()),
            ':' => fields.push(std::mem::take(&mut field)),
            c => field.push(c),
        }
    }
    fields.push(field);
    fields
}

/// The `password` of the `[client]` group of a MySQL option file
/// (`~/.my.cnf`): quotes, `#` and `;` comments and the escapes `\n \t \r
/// \b \s \\` handled; the last value wins, as in MySQL. `!include` is not
/// followed.
pub fn my_cnf_password(text: &str) -> Option<String> {
    let mut client = false;
    let mut found = None;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(['#', ';', '!']) {
            continue;
        }
        if let Some(group) = line.strip_prefix('[') {
            let name = group.split(']').next().unwrap_or_default().trim();
            client = name.eq_ignore_ascii_case("client");
            continue;
        }
        if !client {
            continue;
        }
        // `password` alone means "ask": nothing to use.
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "password"
        {
            found = Some(option_value(value.trim()));
        }
    }
    found.filter(|password| !password.is_empty())
}

/// An option file's value: quoted (up to the closing quote) or bare (up to
/// a `#` comment), its escapes replaced.
fn option_value(raw: &str) -> String {
    let quote = raw.chars().next().filter(|c| *c == '"' || *c == '\'');
    let body = match quote {
        Some(_) => &raw[1..],
        None => raw.split('#').next().unwrap_or_default().trim_end(),
    };
    let mut value = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some('r') => value.push('\r'),
                Some('b') => value.push('\u{8}'),
                Some('s') => value.push(' '),
                Some('\\') => value.push('\\'),
                Some(other) if Some(other) == quote => value.push(other),
                Some(other) => {
                    value.push('\\');
                    value.push(other);
                }
                None => value.push('\\'),
            },
            c if Some(c) == quote => break,
            c => value.push(c),
        }
    }
    value
}

/// A password file to read, when its permissions allow: `Ok(None)` when it
/// does not exist, `Err(warning)` when it is ignored.
async fn read_file(
    path: &Path,
    env: &PasswordEnv,
    engine: Engine,
) -> std::result::Result<Option<String>, String> {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return Ok(None);
    };
    let shown = env.display(path);
    let mode = meta.permissions().mode() & 0o777;
    if !meta.is_file() {
        return Err(format!("{shown}{IGNORED}it is not a plain file."));
    }
    match engine {
        // libpq's rule: group or world access and the file is not read.
        Engine::Postgres if mode & 0o077 != 0 => {
            return Err(format!(
                "{shown}{IGNORED}other users can read it (permissions {mode:04o}); run \
                 chmod 600 {shown}."
            ));
        }
        // MySQL's: a world-writable option file is not read.
        Engine::MySql | Engine::MariaDb if mode & 0o002 != 0 => {
            return Err(format!(
                "{shown}{IGNORED}anyone can change it (permissions {mode:04o}); run chmod 600 \
                 {shown}."
            ));
        }
        _ => {}
    }
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(Some(text)),
        Err(error) => Err(format!("{shown}{IGNORED}it could not be read ({error}).")),
    }
}

// Resolving ---------------------------------------------------------------------

/// The password of one session, where it came from, and a password file
/// that was ignored (to say why the server then asks for a password).
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    pub password: Option<Secret>,
    pub origin: Option<PasswordOrigin>,
    pub warning: Option<String>,
}

/// Finds the password of a session of `config` in `database` (the
/// connection's own when `None`). `given` is the stored or typed one; it is
/// used only by [`PasswordSource::Stored`]. Without `env` no file is read
/// and no command runs.
pub async fn resolve(
    config: &ConnectionConfig,
    given: Option<&Secret>,
    database: Option<&str>,
    env: Option<&PasswordEnv>,
) -> Result<Resolved> {
    // A database file has no password: nothing is read, run or asked.
    if config.engine.is_file() {
        return Ok(Resolved::default());
    }
    if let Some(line) = command_line(config, database) {
        if config.password_command_unconfirmed {
            return Err(Error::Config(format!(
                "{UNCONFIRMED}: open the connection's settings, check the command and confirm it"
            )));
        }
        let env =
            env.ok_or_else(|| Error::Config("password commands are not available here".into()))?;
        let password = run(&line, env).await?;
        return Ok(Resolved {
            password: Some(password),
            origin: Some(PasswordOrigin::Command(line.name().to_string())),
            warning: None,
        });
    }
    if let Some(password) = given {
        return Ok(Resolved {
            password: Some(password.clone()),
            origin: Some(PasswordOrigin::Given),
            warning: None,
        });
    }
    let (Some(env), Some(home)) = (env, env.and_then(|env| env.home.as_ref())) else {
        return Ok(Resolved::default());
    };
    let file = match config.engine {
        Engine::Postgres => env
            .pgpassfile
            .clone()
            .unwrap_or_else(|| home.join(".pgpass")),
        Engine::MySql | Engine::MariaDb => home.join(".my.cnf"),
        Engine::Sqlite => return Ok(Resolved::default()),
    };
    let text = match read_file(&file, env, config.engine).await {
        Ok(Some(text)) => text,
        Ok(None) => return Ok(Resolved::default()),
        Err(warning) => {
            // The file's name only: never its content.
            tracing::warn!(file = %env.display(&file), "password file ignored");
            return Ok(Resolved {
                warning: Some(warning),
                ..Resolved::default()
            });
        }
    };
    let password = match config.engine {
        Engine::Postgres => {
            // libpq's defaults: the database named after the user.
            let database = database
                .or(config.database.as_deref())
                .filter(|name| !name.is_empty())
                .unwrap_or(&config.user);
            pgpass_password(&text, &config.host, config.port, database, &config.user)
        }
        Engine::MySql | Engine::MariaDb => my_cnf_password(&text),
        Engine::Sqlite => None,
    };
    Ok(Resolved {
        origin: password
            .is_some()
            .then(|| PasswordOrigin::File(env.display(&file))),
        password: password.map(Secret::new),
        warning: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TlsMode;

    #[test]
    fn pgpass_matches_wildcards_escapes_and_the_first_line_wins() {
        let text = "# comment\n\
                    db.example.com:5432:*:ada:first\n\
                    *:*:*:ada:second\n\
                    h\\:x:5432:shop:bob:pa\\:ss\\\\word\n\
                    short:line\n";
        assert_eq!(
            pgpass_password(text, "db.example.com", 5432, "shop", "ada").as_deref(),
            Some("first")
        );
        assert_eq!(
            pgpass_password(text, "elsewhere", 6543, "x", "ada").as_deref(),
            Some("second")
        );
        assert_eq!(
            pgpass_password(text, "h:x", 5432, "shop", "bob").as_deref(),
            Some("pa:ss\\word")
        );
        assert_eq!(pgpass_password(text, "h:x", 5432, "other", "bob"), None);
        assert_eq!(pgpass_password("", "h", 1, "d", "u"), None);
        // A colon left unescaped ends the password, as in libpq.
        assert_eq!(
            pgpass_password("*:*:*:*:a:b\r\n", "h", 1, "d", "u").as_deref(),
            Some("a")
        );
    }

    #[test]
    fn my_cnf_reads_the_client_group_with_quotes_comments_and_escapes() {
        let text = "[mysqld]\npassword = server\n\n[client]\nuser=ada\n\
                    password = 'quoted # not a comment'\n# password = commented\n\
                    [mysql]\npassword=not-client\n[client]\npassword=bare # comment\n";
        assert_eq!(my_cnf_password(text).as_deref(), Some("bare"));
        assert_eq!(
            my_cnf_password("[client]\npassword=\"a\\sb\\\\c\\\"d\"\n").as_deref(),
            Some("a b\\c\"d")
        );
        assert_eq!(
            my_cnf_password("[CLIENT]\npassword='x'\n").as_deref(),
            Some("x")
        );
        // `password` alone asks; `!include` is not followed.
        assert_eq!(my_cnf_password("[client]\npassword\n"), None);
        assert_eq!(my_cnf_password("!include /etc/other.cnf\n[client]\n"), None);
        assert_eq!(my_cnf_password("password=top\n"), None);
    }

    fn config(source: PasswordSource) -> ConnectionConfig {
        ConnectionConfig {
            host: "db.example.com".into(),
            port: 5433,
            user: "ada lovelace".into(),
            database: Some("shop".into()),
            password_source: source,
            ..ConnectionConfig::new(Engine::Postgres)
        }
    }

    #[test]
    fn placeholders_are_replaced_inside_each_argument_never_split() {
        let line = command_line(
            &config(PasswordSource::Command {
                program: " vault ".into(),
                args: vec![
                    "read".into(),
                    "db/${host}:${port}/${user}".into(),
                    "--db=${database}".into(),
                    "$(rm -rf ~); ${unknown}".into(),
                ],
                timeout_secs: 500,
            }),
            None,
        )
        .unwrap();
        assert_eq!(line.program, "vault");
        assert_eq!(
            line.args,
            [
                "read",
                "db/db.example.com:5433/ada lovelace",
                "--db=shop",
                "$(rm -rf ~); ${unknown}"
            ]
        );
        assert_eq!(line.timeout, Duration::from_secs(120));
        assert_eq!(
            line.display(),
            "vault read 'db/db.example.com:5433/ada lovelace' --db=shop '$(rm -rf ~); ${unknown}'"
        );
        // The session's database, when it has its own.
        let other = command_line(
            &config(PasswordSource::Command {
                program: "p".into(),
                args: vec!["${database}".into()],
                timeout_secs: 5,
            }),
            Some("analytics"),
        )
        .unwrap();
        assert_eq!(other.args, ["analytics"]);
    }

    #[test]
    fn the_presets_are_fixed_command_lines() {
        let rds = command_line(
            &config(PasswordSource::RdsIam {
                profile: Some("prod".into()),
                region: "eu-west-3".into(),
            }),
            None,
        )
        .unwrap();
        assert_eq!(
            rds.display(),
            "aws rds generate-db-auth-token --hostname db.example.com --port 5433 --username \
             'ada lovelace' --region eu-west-3 --profile prod"
        );
        let rds = command_line(
            &config(PasswordSource::RdsIam {
                profile: Some(" ".into()),
                region: "eu-west-3".into(),
            }),
            None,
        )
        .unwrap();
        assert!(!rds.args.contains(&"--profile".to_string()));
        let op = command_line(
            &config(PasswordSource::OnePassword {
                reference: "op://Work/db/password".into(),
            }),
            None,
        )
        .unwrap();
        assert_eq!(op.display(), "op read op://Work/db/password");
        assert_eq!(command_line(&config(PasswordSource::Stored), None), None);
    }

    #[test]
    fn rds_iam_needs_tls_and_the_sources_are_checked() {
        let mut rds = config(PasswordSource::RdsIam {
            profile: None,
            region: "us-east-1".into(),
        });
        for mode in [TlsMode::Disable, TlsMode::Prefer] {
            rds.tls.mode = mode;
            let error = rds.validate().unwrap_err().to_string();
            assert!(error.contains("TLS"), "{error}");
        }
        rds.tls.mode = TlsMode::Require;
        rds.validate().unwrap();
        for source in [
            PasswordSource::Command {
                program: " ".into(),
                args: vec![],
                timeout_secs: 10,
            },
            PasswordSource::Command {
                program: "p".into(),
                args: vec![],
                timeout_secs: 0,
            },
            PasswordSource::RdsIam {
                profile: None,
                region: String::new(),
            },
            PasswordSource::OnePassword {
                reference: "Work/db".into(),
            },
        ] {
            let mut config = config(source);
            config.tls.mode = TlsMode::Require;
            assert!(config.validate().is_err(), "{:?}", config.password_source);
        }
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn env(dir: &Path) -> PasswordEnv {
        PasswordEnv {
            path: vec![dir.to_path_buf()],
            home: Some(dir.to_path_buf()),
            pgpassfile: None,
        }
    }

    fn line(program: &str, timeout: u64) -> CommandLine {
        CommandLine {
            program: program.into(),
            args: vec!["one two".into(), "$HOME;*".into()],
            timeout: Duration::from_secs(timeout),
        }
    }

    #[tokio::test]
    async fn a_command_runs_without_a_shell_and_one_newline_is_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let args = dir.path().join("args");
        script(
            dir.path(),
            "fake",
            &format!(
                "printf '%s\\n' \"$@\" > {}\nprintf ' s3cret \\n\\n'",
                args.display()
            ),
        );
        let secret = run(&line("fake", 5), &env(dir.path())).await.unwrap();
        assert_eq!(secret.expose(), " s3cret \n");
        assert_eq!(
            std::fs::read_to_string(&args).unwrap(),
            "one two\n$HOME;*\n"
        );
    }

    #[tokio::test]
    async fn failures_say_the_status_and_the_first_message_never_the_output() {
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "fails",
            "printf 'leaked-secret'\necho '' >&2\necho 'token leaked-secret expired' >&2\n\
             echo second >&2\nexit 3",
        );
        let error = run(&line("fails", 5), &env(dir.path()))
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "could not connect: the password command `fails` failed (exit status 3): token *** \
             expired"
        );
        script(dir.path(), "empty", "exit 0");
        let error = run(&line("empty", 5), &env(dir.path())).await.unwrap_err();
        assert!(
            error.to_string().ends_with("`empty` printed nothing"),
            "{error}"
        );
        let error = run(&line("missing", 5), &env(dir.path()))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("`missing` was not found"),
            "{error}"
        );
        script(
            dir.path(),
            "huge",
            "/usr/bin/head -c 70000 /dev/zero | /usr/bin/tr '\\0' a",
        );
        let error = run(&line("huge", 5), &env(dir.path())).await.unwrap_err();
        assert!(error.to_string().contains("more than 64 KB"), "{error}");
    }

    #[tokio::test]
    async fn a_command_that_hangs_is_stopped_at_its_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        script(
            dir.path(),
            "hangs",
            &format!("echo $$ > {}\nexec /bin/sleep 30", pid.display()),
        );
        let started = std::time::Instant::now();
        let error = run(&line("hangs", 1), &env(dir.path())).await.unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(
            error.to_string().ends_with("`hangs` did not finish in 1 s"),
            "{error}"
        );
        // The child was killed: its process is gone (after it is reaped).
        let pid = std::fs::read_to_string(&pid).unwrap();
        let gone = (0..50).any(|_| {
            let alive = std::process::Command::new("/bin/kill")
                .args(["-0", pid.trim()])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if alive {
                std::thread::sleep(Duration::from_millis(100));
            }
            !alive
        });
        assert!(gone, "the command still runs");
    }

    #[tokio::test]
    async fn an_imported_command_does_not_run_before_it_is_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        script(
            dir.path(),
            "fake",
            &format!("/usr/bin/touch {}\necho pw", ran.display()),
        );
        let mut config = config(PasswordSource::Command {
            program: "fake".into(),
            args: vec![],
            timeout_secs: 5,
        });
        config.password_command_unconfirmed = true;
        let error = resolve(&config, None, None, Some(&env(dir.path())))
            .await
            .unwrap_err();
        assert!(error.to_string().contains(UNCONFIRMED), "{error}");
        assert!(!ran.exists(), "the command ran");
        // Without an environment nothing runs either.
        config.password_command_unconfirmed = false;
        assert!(resolve(&config, None, None, None).await.is_err());
        assert!(!ran.exists());
        let resolved = resolve(
            &config,
            Some(&Secret::new("stale")),
            None,
            Some(&env(dir.path())),
        )
        .await
        .unwrap();
        assert!(ran.exists());
        assert_eq!(
            resolved.password.unwrap().expose(),
            "pw",
            "the stored one is not used"
        );
        assert_eq!(
            resolved.origin,
            Some(PasswordOrigin::Command("fake".into()))
        );
    }

    #[tokio::test]
    async fn pgpass_is_ignored_when_others_can_read_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".pgpass");
        std::fs::write(&file, "*:*:*:*:from-file\n").unwrap();
        let config = config(PasswordSource::Stored);
        let env = env(dir.path());
        for (mode, used) in [(0o600, true), (0o400, true), (0o640, false), (0o604, false)] {
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
            let resolved = resolve(&config, None, None, Some(&env)).await.unwrap();
            assert_eq!(resolved.password.is_some(), used, "{mode:o}");
            if used {
                assert_eq!(
                    resolved.origin,
                    Some(PasswordOrigin::File("~/.pgpass".into()))
                );
            } else {
                let warning = resolved.warning.unwrap();
                assert!(
                    warning.starts_with("~/.pgpass was ignored: other users"),
                    "{warning}"
                );
                assert!(!warning.contains("from-file"), "{warning}");
            }
        }
        // A stored password wins; no environment reads no file.
        let given = resolve(&config, Some(&Secret::new("stored")), None, Some(&env))
            .await
            .unwrap();
        assert_eq!(given.origin, Some(PasswordOrigin::Given));
        let none = resolve(&config, None, None, None).await.unwrap();
        assert!(none.password.is_none() && none.warning.is_none());
        // PGPASSFILE replaces ~/.pgpass.
        let other = dir.path().join("elsewhere");
        std::fs::write(&other, "*:*:*:*:other\n").unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o600)).unwrap();
        let env = PasswordEnv {
            pgpassfile: Some(other),
            ..env
        };
        let resolved = resolve(&config, None, None, Some(&env)).await.unwrap();
        assert_eq!(resolved.password.unwrap().expose(), "other");
    }

    #[test]
    fn detect_adds_the_usual_directories_once() {
        let env = PasswordEnv::detect(
            Some(OsStr::new("/custom/bin:/usr/bin:relative")),
            Some("/home/ada".into()),
            None,
        );
        assert_eq!(
            env.path,
            [
                "/custom/bin",
                "/usr/bin",
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/bin"
            ]
            .map(PathBuf::from)
        );
        assert_eq!(env.display(Path::new("/home/ada/.pgpass")), "~/.pgpass");
        assert_eq!(env.display(Path::new("/etc/x")), "/etc/x");
    }
}

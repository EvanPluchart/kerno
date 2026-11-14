//! `kerno <url>` (CLAUDE.md §4 J4, Beekeeper #361, #2002): the command
//! line, the connection a URL describes, and the one running instance.
//!
//! The first Kerno listens on a Unix socket in its data directory; a second
//! `kerno <url>` sends it the URL, one line, and exits without opening a
//! window. The socket is the user's alone (0600). A socket file left by a
//! crash refuses connections: it is replaced. A URL may hold a password:
//! nothing here logs it or puts it in an error message.

use std::io::{self, BufRead as _, BufReader, ErrorKind, Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{ConnectionConfig, Engine, ParsedUrl, Secret, Secrets, parse_connection_url};
use crate::storage::SavedConnection;

/// The socket's file name in the data directory.
pub const SOCKET: &str = "instance.sock";
/// The longest line read from the socket: a URL, not a file.
const MAX_LINE: u64 = 64 * 1024;
/// How long a peer may take to send its line.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Open the window, as always.
    Run,
    /// Open this connection URL (checked by the parser). Held as a secret:
    /// it may carry a password.
    Open(Secret),
    /// `--version`.
    Version,
    /// `--help`.
    Help,
    /// Something Kerno cannot run: the message for the user (exit 2).
    Invalid(String),
}

/// Reads the arguments that follow the program name.
pub fn parse_args(args: impl Iterator<Item = String>) -> Invocation {
    let mut url = None;
    for arg in args {
        match arg.as_str() {
            "--version" | "-V" => return Invocation::Version,
            "--help" | "-h" => return Invocation::Help,
            // Added by older macOS when the app is opened from the Finder.
            _ if arg.starts_with("-psn_") => {}
            _ if arg.starts_with('-') => {
                return Invocation::Invalid(format!("unknown option {arg}"));
            }
            _ if url.is_some() => {
                return Invocation::Invalid("give one connection URL at most".into());
            }
            // The hand-off is one line; the URL parser would drop the break
            // silently.
            _ if arg.contains(['\n', '\r']) => {
                return Invocation::Invalid("not a connection URL: it spans several lines".into());
            }
            _ => match parse_connection_url(&arg) {
                Ok(_) => url = Some(Secret::new(arg.trim())),
                Err(error) => return Invocation::Invalid(error.to_string()),
            },
        }
    }
    url.map_or(Invocation::Run, Invocation::Open)
}

/// The URL with its password replaced by `***`, for logs. Text that is not
/// a URL is not shown at all.
pub fn redact_url(input: &str) -> String {
    match url::Url::parse(input.trim()) {
        Ok(mut url) => {
            if url.password().is_some() {
                let _ = url.set_password(Some("***"));
            }
            let secret = url
                .query_pairs()
                .any(|(key, _)| key.eq_ignore_ascii_case("password"));
            if secret {
                let kept: Vec<(String, String)> = url
                    .query_pairs()
                    .map(|(key, value)| match key.eq_ignore_ascii_case("password") {
                        true => (key.into_owned(), "***".to_string()),
                        false => (key.into_owned(), value.into_owned()),
                    })
                    .collect();
                url.query_pairs_mut().clear().extend_pairs(kept);
            }
            url.to_string()
        }
        Err(_) => "(not a URL)".to_string(),
    }
}

/// The connection a URL describes, with the engine's defaults for what it
/// leaves out (as the connection dialog fills them), kept in memory under
/// a new id; the password, if any, for this run only.
pub fn unsaved_connection(url: &ParsedUrl) -> (SavedConnection, Secrets) {
    let mut config = ConnectionConfig::new(url.engine);
    if let Some(host) = &url.host {
        config.host = host.clone();
    }
    config.port = url.port.unwrap_or(config.port);
    if let Some(user) = &url.user {
        config.user = user.clone();
    }
    config.database = url.database.clone();
    config.options = url.options.clone();
    if let Some(mode) = url.tls_mode {
        config.tls.mode = mode;
    }
    // Never written to the secret store until the user saves it; without a
    // password in the URL, Kerno asks for one before connecting.
    config.save_password = false;
    let port = match config.port == config.engine.default_port() {
        true => String::new(),
        false => format!(":{}", config.port),
    };
    let database = config
        .database
        .as_ref()
        .map(|database| format!("/{database}"))
        .unwrap_or_default();
    config.name = format!("{}@{}{port}{database}", config.user, config.host);
    let secrets = Secrets {
        password: url.password.clone(),
        ..Secrets::default()
    };
    let id = uuid::Uuid::new_v4().to_string();
    (SavedConnection { id, config }, secrets)
}

/// Whether `saved` reaches the server and database `url` (from
/// [`unsaved_connection`]) names: same engine (MySQL and MariaDB speak the
/// same protocol), host, port, user and database. A connection through an
/// SSH tunnel names a host as seen from the tunnel's end: never the same.
pub fn same_target(saved: &ConnectionConfig, url: &ConnectionConfig) -> bool {
    let family = |engine| match engine {
        Engine::MariaDb => Engine::MySql,
        other => other,
    };
    let database = |config: &ConnectionConfig| {
        config
            .database
            .clone()
            .filter(|database| !database.is_empty())
    };
    saved.ssh.is_none()
        && family(saved.engine) == family(url.engine)
        && saved.host.eq_ignore_ascii_case(&url.host)
        && saved.port == url.port
        && saved.user == url.user
        && database(saved) == database(url)
}

/// The socket of the instance using `data_dir`.
pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SOCKET)
}

/// Hands `line` to the running instance. Fails when none listens.
pub fn send(path: &Path, line: &str) -> io::Result<()> {
    let mut stream = UnixStream::connect(path)?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

/// Listens on `path`, replacing a socket left by a crash. `None` when a
/// running instance already listens there: it is left alone.
pub fn listen(path: &Path) -> io::Result<Option<UnixListener>> {
    match UnixStream::connect(path) {
        Ok(_) => return Ok(None),
        // Nobody behind it: a crash left it.
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => std::fs::remove_file(path)?,
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(path)?;
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Some(listener))
}

/// Accepts peers one after the other and hands each one's line to
/// `on_line`, until `on_line` returns false. Blocking: run it on its own
/// thread.
pub fn serve(listener: UnixListener, mut on_line: impl FnMut(String) -> bool) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if let Some(line) = read_line(stream)
            && !on_line(line)
        {
            return;
        }
    }
}

/// One line from a peer, without its line break; `None` when it sent
/// nothing usable in time.
fn read_line(stream: UnixStream) -> Option<String> {
    // macOS refuses socket options once the peer has closed its end (a
    // peer that wrote its line and left): then nothing can block anyway.
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut line = String::new();
    BufReader::new(stream.take(MAX_LINE))
        .read_line(&mut line)
        .ok()?;
    let line = line.trim();
    (!line.is_empty()).then(|| line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TlsMode;

    fn args(list: &[&str]) -> Invocation {
        parse_args(list.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn arguments_open_a_url_or_say_why_not() {
        assert_eq!(args(&[]), Invocation::Run);
        assert_eq!(args(&["--version"]), Invocation::Version);
        assert_eq!(args(&["-V"]), Invocation::Version);
        assert_eq!(args(&["--help"]), Invocation::Help);
        assert_eq!(args(&["postgres://h", "--help"]), Invocation::Help);
        assert_eq!(args(&["-psn_0_1234"]), Invocation::Run);
        assert_eq!(
            args(&[" mysql://u@h/d "]),
            Invocation::Open(Secret::new("mysql://u@h/d"))
        );
        for bad in [
            &["not a url"][..],
            &["http://example.com"],
            &["--frobnicate"],
            &["postgres://a", "postgres://b"],
            &["postgres://h/d\nsecond"],
        ] {
            assert!(
                matches!(args(bad), Invocation::Invalid(_)),
                "{bad:?}: {:?}",
                args(bad)
            );
        }
        match args(&["sqlite:///x.db"]) {
            Invocation::Invalid(message) => assert!(message.contains("sqlite"), "{message}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_url_s_password_never_shows() {
        let url = "postgres://ada:hunter2@h/d?password=hunter3&application_name=x";
        let invocation = args(&[url]);
        assert!(!format!("{invocation:?}").contains("hunter"));
        let redacted = redact_url(url);
        assert!(!redacted.contains("hunter"), "{redacted}");
        assert!(redacted.contains("ada:***@h"), "{redacted}");
        assert!(redacted.contains("application_name=x"), "{redacted}");
        assert_eq!(redact_url("garbage hunter2"), "(not a URL)");
        // The parser's message names what is wrong, never the URL itself.
        match args(&["postgres://ada:hunter2@h/d?sslmode=nope"]) {
            Invocation::Invalid(message) => assert!(!message.contains("hunter"), "{message}"),
            other => panic!("{other:?}"),
        }
        let (saved, secrets) =
            unsaved_connection(&parse_connection_url("postgres://ada:hunter2@h/d").unwrap());
        assert!(!format!("{saved:?} {secrets:?}").contains("hunter"));
        assert!(
            !serde_json::to_string(&saved.config)
                .unwrap()
                .contains("hunter")
        );
    }

    #[test]
    fn a_url_gives_the_engine_s_defaults_and_a_name() {
        let url = parse_connection_url("postgres://h").unwrap();
        let (saved, secrets) = unsaved_connection(&url);
        let config = &saved.config;
        assert_eq!(
            (config.host.as_str(), config.port, config.user.as_str()),
            ("h", 5432, "postgres")
        );
        assert_eq!(config.name, "postgres@h");
        assert!(!config.save_password);
        assert_eq!(secrets, Secrets::default());

        let url =
            parse_connection_url("mariadb://ann:pw@db:3307/shop?ssl-mode=REQUIRED&charset=utf8")
                .unwrap();
        let (saved, secrets) = unsaved_connection(&url);
        let config = &saved.config;
        assert_eq!(config.name, "ann@db:3307/shop");
        assert_eq!(config.tls.mode, TlsMode::Require);
        assert_eq!(
            config.options.get("charset").map(String::as_str),
            Some("utf8")
        );
        assert_eq!(secrets.password.as_ref().map(Secret::expose), Some("pw"));
        // Each URL opened gets its own id.
        assert_ne!(saved.id, unsaved_connection(&url).0.id);
    }

    #[test]
    fn a_saved_connection_matches_on_engine_host_port_user_and_database() {
        let url = |text: &str| {
            unsaved_connection(&parse_connection_url(text).unwrap())
                .0
                .config
        };
        let saved = url("postgres://kerno@LocalHost:5432/shop");
        assert!(same_target(
            &saved,
            &url("postgresql://kerno:pw@localhost/shop")
        ));
        for other in [
            "mysql://kerno@localhost:5432/shop",
            "postgres://kerno@otherhost/shop",
            "postgres://kerno@localhost:5433/shop",
            "postgres://ada@localhost/shop",
            "postgres://kerno@localhost/other",
            "postgres://kerno@localhost",
        ] {
            assert!(!same_target(&saved, &url(other)), "{other}");
        }
        // MySQL and MariaDB are one protocol.
        assert!(same_target(&url("mariadb://u@h/d"), &url("mysql://u@h/d")));
        // An empty database is no database.
        let mut empty = url("postgres://u@h");
        empty.database = Some(String::new());
        assert!(same_target(&empty, &url("postgres://u@h")));
        // Through a tunnel, "localhost" is another machine.
        let mut tunnelled = saved.clone();
        tunnelled.ssh = Some(serde_json::from_str(r#"{"host": "bastion", "user": "u"}"#).unwrap());
        assert!(!same_target(
            &tunnelled,
            &url("postgres://kerno@localhost/shop")
        ));
    }

    #[test]
    fn a_second_endpoint_hands_its_url_to_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket_path(dir.path());
        let listener = listen(&path).unwrap().expect("nobody listens yet");
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // A second instance does not take a live socket over.
        assert!(listen(&path).unwrap().is_none());

        let (sent, received) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut left = 2;
            serve(listener, |line| {
                sent.send(line).unwrap();
                left -= 1;
                left > 0
            })
        });
        send(&path, "postgres://u:pw@h/one").unwrap();
        // A peer that says nothing does not hold the next one up.
        drop(UnixStream::connect(&path).unwrap());
        send(&path, "mysql://u@h/two").unwrap();
        server.join().unwrap();
        let lines: Vec<String> = received.try_iter().collect();
        assert_eq!(lines, ["postgres://u:pw@h/one", "mysql://u@h/two"]);
    }

    #[test]
    fn a_socket_left_by_a_crash_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket_path(&dir.path().join("first-run"));
        // No instance: the hand-off fails, the caller opens the window.
        assert!(send(&path, "postgres://h").is_err());
        // The directory is made on a first run.
        drop(listen(&path).unwrap().unwrap());
        assert!(path.exists(), "a dropped listener leaves its file");
        assert!(send(&path, "postgres://h").is_err());
        let listener = listen(&path)
            .unwrap()
            .expect("the stale socket is replaced");
        send(&path, "postgres://h").unwrap();
        let (stream, _) = listener.accept().unwrap();
        assert_eq!(read_line(stream).as_deref(), Some("postgres://h"));
    }
}

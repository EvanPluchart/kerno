//! Opening a MySQL / MariaDB connection: TLS options, direct or tunnelled
//! transport, and turning the client library's errors into Kerno's.

use std::time::Duration;

use kerno_core::config::{ConnectParams, TlsConfig, TlsMode};
use kerno_core::{DbError, Error, Result};
use mysql_async::prelude::Queryable;
use mysql_async::{ClientIdentity, Conn, DriverError, IoError, OptsBuilder, SslOpts};

/// Default time allowed for the connection to be established.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// What the driver options of a connection ask for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Settings {
    /// Time allowed for the connection to be established.
    pub(crate) timeout: Duration,
    /// Session variables to set once connected, as `(name, value)`.
    pub(crate) variables: Vec<(String, String)>,
}

/// Reads the driver options: `connect_timeout` (seconds) is the driver's
/// own; every other option is a session variable (`sql_mode`, `time_zone`,
/// `wait_timeout`, ...).
pub(crate) fn settings(params: &ConnectParams) -> Result<Settings> {
    let mut timeout = DEFAULT_CONNECT_TIMEOUT;
    let mut variables = Vec::new();
    for (key, value) in &params.options {
        if key == "connect_timeout" {
            let seconds: u64 = value.parse().map_err(|_| {
                Error::Config(format!(
                    "connect_timeout must be a number of seconds, not \"{value}\""
                ))
            })?;
            timeout = Duration::from_secs(seconds.max(1));
            continue;
        }
        // The name is written into a SET statement as it is.
        let plain = !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.');
        if !plain {
            return Err(Error::Config(format!(
                "\"{key}\" is not the name of a session variable"
            )));
        }
        variables.push((key.clone(), value.clone()));
    }
    Ok(Settings { timeout, variables })
}

fn read_error(what: &str, path: &std::path::Path, error: impl std::fmt::Display) -> Error {
    Error::Tls(format!(
        "could not read the {what} {}: {error}",
        path.display()
    ))
}

/// Checks a PEM file can be read before the client library is handed its
/// path, so a wrong path is reported with the path.
async fn readable(what: &str, path: &std::path::Path) -> Result<std::path::PathBuf> {
    tokio::fs::metadata(path)
        .await
        .map_err(|error| read_error(what, path, error))?;
    Ok(path.to_path_buf())
}

/// The system's trusted certificate authorities, as DER buffers.
fn system_roots() -> Result<Vec<Vec<u8>>> {
    let roots: Vec<Vec<u8>> = rustls_native_certs::load_native_certs()
        .certs
        .into_iter()
        // The client library gives up on the first certificate it cannot
        // parse; one odd entry in the system store must not do that.
        .filter(|cert| rustls::RootCertStore::empty().add(cert.clone()).is_ok())
        .map(|cert| cert.to_vec())
        .collect();
    if roots.is_empty() {
        return Err(Error::Tls(
            "the system has no trusted certificate authorities; choose a CA file".into(),
        ));
    }
    Ok(roots)
}

/// The client library's TLS options for a connection's TLS settings, or
/// `None` for no TLS.
///
/// - `prefer` / `require`: any certificate is accepted;
/// - `verify-ca`: the chain must lead to a trusted authority, the host name
///   is not compared;
/// - `verify-full`: both.
///
/// Trusted authorities are the configured CA file, or the system's store.
pub(crate) async fn ssl_opts(params: &ConnectParams) -> Result<Option<SslOpts>> {
    let tls: &TlsConfig = &params.tls;
    if tls.mode == TlsMode::Disable {
        return Ok(None);
    }
    // The client library builds its rustls configuration from the process
    // default provider, which rustls can only pick by itself when exactly
    // one is compiled in. Kerno uses ring everywhere.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    let mut opts = SslOpts::default();
    if tls.mode.verifies_certificate() {
        // The library takes a path or a buffer of DER or PEM.
        let roots = match &tls.ca_file {
            Some(ca_file) => vec![readable("CA certificate", ca_file).await?.into()],
            None => system_roots()?.into_iter().map(Into::into).collect(),
        };
        opts = opts
            .with_root_certs(roots)
            .with_disable_built_in_roots(true)
            .with_danger_skip_domain_validation(tls.mode == TlsMode::VerifyCa);
    } else {
        opts = opts.with_danger_accept_invalid_certs(true);
    }
    match (&tls.cert_file, &tls.key_file) {
        (Some(cert_file), Some(key_file)) => {
            opts = opts.with_client_identity(Some(ClientIdentity::new(
                readable("client certificate", cert_file).await?.into(),
                readable("client key", key_file).await?.into(),
            )));
        }
        (None, None) => {}
        _ => {
            return Err(Error::Config(
                "a client certificate needs both the certificate file and the key file".into(),
            ));
        }
    }
    if params.tunnel.is_some() {
        // The socket goes to a tunnel entrance on loopback; the certificate
        // is still the database host's.
        opts = opts.with_danger_tls_hostname_override(Some(params.host.clone()));
    }
    Ok(Some(opts))
}

/// Dials the server once, with or without TLS. Through a tunnel, every
/// call takes its own single-use entrance.
async fn dial(
    params: &ConnectParams,
    timeout: Duration,
    tls: Option<SslOpts>,
    database: Option<&str>,
) -> Result<std::result::Result<Conn, mysql_async::Error>> {
    let (host, port) = match &params.tunnel {
        None => (params.host.clone(), params.port),
        Some(tunnel) => {
            let entrance = tunnel.forward_once(&params.host, params.port).await?;
            (entrance.ip().to_string(), entrance.port())
        }
    };
    let opts = OptsBuilder::default()
        .ip_or_hostname(host)
        .tcp_port(port)
        .user(Some(params.user.as_str()))
        .pass(params.password.as_ref().map(|password| password.expose()))
        .db_name(database)
        // The default reconnects over the server's Unix socket when the
        // address is loopback: wrong for a container and for a tunnel.
        .prefer_socket(false)
        // Rows matched, not rows changed: an UPDATE that sets a value to
        // what it already was still addressed its row (see
        // `kerno_core::sql::apply_edits`).
        .client_found_rows(true)
        .tcp_keepalive(Some(Duration::from_secs(60)))
        // Statements run through the text protocol only.
        .stmt_cache_size(0)
        .connect_attribute("program_name", "Kerno")
        // AWS RDS IAM tokens (J3) go through `mysql_clear_password`, which
        // the server may ask for: allowed only inside TLS.
        .enable_cleartext_plugin(tls.is_some())
        .ssl_opts(tls);

    match tokio::time::timeout(timeout, Conn::new(opts)).await {
        Ok(Ok(conn)) => Ok(Ok(conn)),
        Ok(Err(error)) => {
            // A tunnel entrance that cannot reach the database just closes
            // the socket. Ask the SSH server directly to learn why.
            if let Some(tunnel) = &params.tunnel
                && !matches!(error, mysql_async::Error::Server(_))
            {
                tunnel.open_stream(&params.host, params.port).await?;
            }
            Ok(Err(error))
        }
        Err(_) => Err(Error::Connect(format!(
            "no answer from {}:{} after {} s",
            params.host,
            params.port,
            timeout.as_secs()
        ))),
    }
}

/// Opens a connection to `database` (none when `None`) with the TLS mode
/// of `params`. `prefer` falls back to a plain connection when the server
/// does not offer TLS, or offers only a TLS older than Kerno accepts (K22).
pub(crate) async fn open(
    params: &ConnectParams,
    timeout: Duration,
    database: Option<&str>,
) -> Result<Conn> {
    let tls = ssl_opts(params).await?;
    match dial(params, timeout, tls, database).await? {
        Ok(conn) => Ok(conn),
        // `prefer` checks no certificate and goes on without TLS when the
        // server has none: a server whose TLS cannot be used is the same
        // case, and the plain connection is no weaker than that one.
        Err(error)
            if params.tls.mode == TlsMode::Prefer
                && (matches!(
                    error,
                    mysql_async::Error::Driver(DriverError::NoClientSslFlagFromServer)
                ) || old_server_tls(&error)) =>
        {
            dial(params, timeout, None, database)
                .await?
                .map_err(|error| connect_error(error, params))
        }
        // verify-ca: the chain is what counts, not the name. The client
        // library's own switch for this does not recognise the TLS
        // library's error any more and refuses, so dial again under a name
        // the certificate carries: the chain is verified in full, exactly
        // as before, and the name check then passes.
        Err(error) if params.tls.mode == TlsMode::VerifyCa => {
            let Some(name) = presented_names(&error).and_then(|names| usable_name(&names)) else {
                return Err(connect_error(error, params));
            };
            let tls = tls_under_name(params, name).await?;
            dial(params, timeout, tls, database)
                .await?
                .map_err(|error| connect_error(error, params))
        }
        Err(error) => Err(connect_error(error, params)),
    }
}

async fn tls_under_name(params: &ConnectParams, name: String) -> Result<Option<SslOpts>> {
    Ok(ssl_opts(params)
        .await?
        .map(|opts| opts.with_danger_tls_hostname_override(Some(name))))
}

/// The TLS library's own error inside a failed connection, if that is
/// what failed.
fn rustls_error(error: &mysql_async::Error) -> Option<&rustls::Error> {
    match error {
        mysql_async::Error::Io(IoError::Tls(mysql_async::TlsError::Tls(error))) => Some(error),
        // The handshake's own failure arrives wrapped in an I/O error.
        mysql_async::Error::Io(IoError::Io(io)) => io.get_ref()?.downcast_ref::<rustls::Error>(),
        _ => None,
    }
}

/// Whether the TLS handshake failed because the server's TLS is older than
/// what Kerno's TLS library accepts (K22): TLS 1.0 or 1.1 only (MySQL 5.1
/// to 5.6 built with yaSSL answer with an unknown protocol version or a
/// pre-1.2 hello), or TLS 1.2 without an ECDHE key exchange (MySQL 5.7's
/// builds offer DHE only and end the handshake).
fn old_server_tls(error: &mysql_async::Error) -> bool {
    use rustls::{AlertDescription, InvalidMessage};
    matches!(
        rustls_error(error),
        Some(
            rustls::Error::PeerIncompatible(_)
                | rustls::Error::InvalidMessage(InvalidMessage::UnknownProtocolVersion)
                | rustls::Error::AlertReceived(
                    AlertDescription::HandshakeFailure
                        | AlertDescription::ProtocolVersion
                        | AlertDescription::InsufficientSecurity
                )
        )
    )
}

/// The names the server's certificate carries, when the only thing wrong
/// with it is that the host name is not one of them.
fn presented_names(error: &mysql_async::Error) -> Option<Vec<String>> {
    match rustls_error(error)? {
        rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForNameContext {
            presented,
            ..
        }) => Some(presented.clone()),
        _ => None,
    }
}

/// A host name or address that matches one of a certificate's names, which
/// the TLS library reports as `DnsName("db.example.com")` or
/// `IpAddress(10.0.0.5)`. A wildcard is given a first label.
fn usable_name(presented: &[String]) -> Option<String> {
    presented.iter().find_map(|name| {
        let name = name
            .strip_prefix("DnsName(\"")
            .and_then(|rest| rest.strip_suffix("\")"))
            .or_else(|| name.strip_prefix("IpAddress(")?.strip_suffix(')'))?;
        let name = match name.strip_prefix("*.") {
            Some(parent) => format!("host.{parent}"),
            None => name.to_string(),
        };
        let plain = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_'));
        plain.then_some(name)
    })
}

/// `errno (sqlstate)`, the form MySQL's own tools print.
fn code(error: &mysql_async::ServerError) -> String {
    format!("{} ({})", error.code, error.state)
}

/// The useful sentence of an I/O error, without the library's prefixes.
fn io_text(error: &IoError) -> String {
    match error {
        IoError::Io(error) => error.to_string(),
        IoError::Tls(error) => error.to_string(),
    }
}

fn connect_error(error: mysql_async::Error, params: &ConnectParams) -> Error {
    match error {
        // Wrong password, unknown database, host not allowed: the server's
        // own sentence is the useful one.
        // ER_ACCESS_DENIED_ERROR without a password: one is needed.
        mysql_async::Error::Server(server) if server.code == 1045 && params.password.is_none() => {
            Error::PasswordRequired
        }
        mysql_async::Error::Server(server) => {
            Error::Connect(format!("{} [{}]", server.message, code(&server)))
        }
        mysql_async::Error::Driver(DriverError::NoClientSslFlagFromServer) => {
            Error::Tls("the server does not offer TLS".into())
        }
        // MySQL's pre-4.1 hash: one captured login is enough to find the
        // password. The server refuses such accounts itself from 5.6.5;
        // Kerno does too.
        mysql_async::Error::Driver(DriverError::MysqlOldPasswordDisabled) => {
            Error::Connect(format!(
                "the account {} {}, which Kerno refuses",
                params.user,
                kerno_core::error::OLD_PASSWORD_HASH
            ))
        }
        ref tls if old_server_tls(tls) => Error::Tls(format!(
            "{} (TLS 1.0 or 1.1, or no ECDHE key exchange): {}",
            kerno_core::error::OLD_SERVER_TLS,
            rustls_error(tls)
                .map(ToString::to_string)
                .unwrap_or_default()
        )),
        mysql_async::Error::Io(io) => {
            // A failed handshake arrives as a TLS error or wrapped in an
            // I/O error, depending on where it was noticed.
            let text = io_text(&io);
            let about_tls = matches!(io, IoError::Tls(_))
                || ["certificate", "TLS", "tls", "handshake"]
                    .iter()
                    .any(|word| text.contains(word));
            if !about_tls {
                Error::Connect(text)
            } else if params.tls.mode == TlsMode::VerifyCa && text.contains("not valid for name") {
                // Only the name differs, and `open` found no name in the
                // certificate to dial under (or the second dial failed the
                // same way): refused rather than guessed.
                Error::Tls(format!(
                    "{text}. verify-ca could not be completed because the certificate carries \
                     no host name or address this driver can use; connect with a name the \
                     certificate carries"
                ))
            } else {
                Error::Tls(text)
            }
        }
        other => Error::Connect(other.to_string()),
    }
}

/// Server errors that mean the session itself is gone: MariaDB's
/// "Connection was killed", "Server shutdown in progress", and MySQL's
/// "disconnected by the server because of inactivity".
const DISCONNECT_CODES: [u16; 3] = [1927, 1053, 4031];

/// Converts the failure of a statement. `sql` is what was sent, to place
/// the error in it.
pub(crate) fn statement_error(error: mysql_async::Error, sql: &str) -> Error {
    match error {
        mysql_async::Error::Server(server) if DISCONNECT_CODES.contains(&server.code) => {
            Error::Closed(format!("{} [{}]", server.message, code(&server)))
        }
        mysql_async::Error::Server(server) => Error::Db(Box::new(DbError {
            position: error_position(&server.message, sql),
            code: Some(code(&server)),
            severity: Some("ERROR".into()),
            message: server.message,
            ..DbError::default()
        })),
        mysql_async::Error::Io(io) => Error::Closed(io_text(&io)),
        mysql_async::Error::Driver(DriverError::ConnectionClosed) => {
            Error::Closed("the connection to the server is closed".into())
        }
        other => Error::Closed(other.to_string()),
    }
}

/// The 1-based character offset a syntax error points at.
///
/// The parser's message ends with `near '<text>' at line <n>`, where the
/// text starts at the token it could not accept and runs to the end of the
/// statement, cut at 80 bytes. Other errors carry no position.
fn error_position(message: &str, sql: &str) -> Option<u32> {
    let (_, rest) = message.split_once(" near '")?;
    let (fragment, line) = rest.rsplit_once("' at line ")?;
    let line: usize = line.trim().parse().ok()?;
    let statement = sql.trim_end();
    let offset = if statement.ends_with(fragment) {
        statement.len() - fragment.len()
    } else {
        // Cut short: the first place it occurs on or after the line named.
        let line_start = statement
            .split_inclusive('\n')
            .take(line.saturating_sub(1))
            .map(str::len)
            .sum::<usize>()
            .min(statement.len());
        match statement[line_start..].find(fragment) {
            Some(found) => line_start + found,
            None => statement.find(fragment)?,
        }
    };
    u32::try_from(statement[..offset].chars().count() + 1).ok()
}

/// Runs `sql` and returns the rows of its first result as text: `None` for
/// NULL, anything that is not UTF-8 replaced. For metadata queries.
pub(crate) async fn text_rows(
    conn: &mut Conn,
    sql: &str,
) -> std::result::Result<Vec<Vec<Option<String>>>, mysql_async::Error> {
    let mut result = conn.query_iter(sql).await?;
    let mut rows = Vec::new();
    while let Some(row) = result.next().await? {
        rows.push(
            row.unwrap()
                .into_iter()
                .map(|value| match value {
                    mysql_async::Value::Bytes(bytes) => {
                        Some(String::from_utf8_lossy(&bytes).into_owned())
                    }
                    mysql_async::Value::NULL => None,
                    // The text protocol sends nothing else.
                    other => Some(other.as_sql(true)),
                })
                .collect(),
        );
    }
    result.drop_result().await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::Engine;
    use kerno_core::config::Secret;
    use std::collections::BTreeMap;

    fn params(tls: TlsConfig) -> ConnectParams {
        ConnectParams {
            engine: Engine::MySql,
            host: "db.example.com".into(),
            port: 3306,
            tunnel: None,
            user: "kerno".into(),
            password: Some(Secret::new("pw")),
            database: None,
            file: None,
            tls,
            read_only: false,
            options: BTreeMap::new(),
        }
    }

    #[test]
    fn a_certificate_name_is_picked_for_verify_ca() {
        let names = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
        assert_eq!(
            usable_name(&names(&[
                "DnsName(\"db.example.com\")",
                "IpAddress(10.0.0.5)"
            ])),
            Some("db.example.com".into())
        );
        assert_eq!(
            usable_name(&names(&["IpAddress(10.0.0.5)"])),
            Some("10.0.0.5".into())
        );
        assert_eq!(usable_name(&names(&["IpAddress(::1)"])), Some("::1".into()));
        assert_eq!(
            usable_name(&names(&["DnsName(\"*.example.com\")"])),
            Some("host.example.com".into())
        );
        // Anything else is not guessed at: the connection stays refused.
        assert_eq!(
            usable_name(&names(&["OtherName(1.2.3)", "DnsName(\"a b\")", ""])),
            None
        );
        assert_eq!(usable_name(&[]), None);
    }

    #[test]
    fn options_are_the_timeout_and_session_variables() {
        let mut p = params(TlsConfig::default());
        assert_eq!(
            settings(&p).unwrap(),
            Settings {
                timeout: DEFAULT_CONNECT_TIMEOUT,
                variables: vec![]
            }
        );
        p.options.insert("connect_timeout".into(), "3".into());
        p.options.insert("sql_mode".into(), "ANSI_QUOTES".into());
        p.options.insert("wait_timeout".into(), "600".into());
        let parsed = settings(&p).unwrap();
        assert_eq!(parsed.timeout, Duration::from_secs(3));
        assert_eq!(
            parsed.variables,
            [
                ("sql_mode".to_string(), "ANSI_QUOTES".to_string()),
                ("wait_timeout".to_string(), "600".to_string())
            ]
        );

        p.options.insert("connect_timeout".into(), "soon".into());
        assert!(matches!(settings(&p), Err(Error::Config(_))));
        p.options.remove("connect_timeout");
        // A name that is not a plain variable name is never put in SQL.
        p.options.insert("x = 1, autocommit".into(), "0".into());
        let error = settings(&p).unwrap_err();
        assert!(matches!(error, Error::Config(_)), "{error:?}");
    }

    #[tokio::test]
    async fn tls_options_follow_the_mode() {
        let none = ssl_opts(&params(TlsConfig {
            mode: TlsMode::Disable,
            ..TlsConfig::default()
        }))
        .await
        .unwrap();
        assert!(none.is_none());

        for mode in [TlsMode::Prefer, TlsMode::Require] {
            let opts = ssl_opts(&params(TlsConfig {
                mode,
                ..TlsConfig::default()
            }))
            .await
            .unwrap()
            .unwrap();
            assert!(opts.accept_invalid_certs(), "{mode:?}");
            assert_eq!(opts.tls_hostname_override(), None);
        }

        // Verifying modes trust the system store when no CA file is given,
        // never the library's built-in list.
        for (mode, skip_name) in [(TlsMode::VerifyCa, true), (TlsMode::VerifyFull, false)] {
            let opts = ssl_opts(&params(TlsConfig {
                mode,
                ..TlsConfig::default()
            }))
            .await
            .unwrap()
            .unwrap();
            assert!(!opts.accept_invalid_certs(), "{mode:?}");
            assert_eq!(opts.skip_domain_validation(), skip_name, "{mode:?}");
            assert!(opts.disable_built_in_roots(), "{mode:?}");
            assert!(!opts.root_certs().is_empty(), "{mode:?}");
        }
    }

    #[tokio::test]
    async fn missing_files_are_reported_with_their_path() {
        let error = ssl_opts(&params(TlsConfig {
            mode: TlsMode::VerifyFull,
            ca_file: Some("/nonexistent/kerno-ca.pem".into()),
            ..TlsConfig::default()
        }))
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Tls(_)), "{error:?}");
        assert!(
            error.to_string().contains("/nonexistent/kerno-ca.pem"),
            "{error}"
        );

        let error = ssl_opts(&params(TlsConfig {
            mode: TlsMode::Require,
            cert_file: Some("/nonexistent/client.crt".into()),
            key_file: Some("/nonexistent/client.key".into()),
            ..TlsConfig::default()
        }))
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("/nonexistent/client.crt"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_certificate_without_its_key_is_a_configuration_error() {
        let error = ssl_opts(&params(TlsConfig {
            mode: TlsMode::Require,
            cert_file: Some("/c.pem".into()),
            ..TlsConfig::default()
        }))
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Config(_)), "{error:?}");
    }

    fn server(code: u16, state: &str, message: &str) -> mysql_async::Error {
        mysql_async::Error::Server(mysql_async::ServerError {
            code,
            message: message.into(),
            state: state.into(),
        })
    }

    #[test]
    fn server_errors_keep_their_code_and_message() {
        let error = statement_error(
            server(1146, "42S02", "Table 'kerno.nope' doesn't exist"),
            "SELECT * FROM nope",
        );
        let db = error.db().expect("a server error");
        assert_eq!(db.code.as_deref(), Some("1146 (42S02)"));
        assert_eq!(db.message, "Table 'kerno.nope' doesn't exist");
        assert_eq!(db.severity.as_deref(), Some("ERROR"));
        assert_eq!(db.position, None);
    }

    #[test]
    fn a_killed_connection_is_a_disconnect_not_a_statement_error() {
        let error = statement_error(server(1927, "70100", "Connection was killed"), "SELECT 1");
        assert!(error.is_disconnect(), "{error:?}");
        let error = statement_error(
            mysql_async::Error::Driver(DriverError::ConnectionClosed),
            "SELECT 1",
        );
        assert!(error.is_disconnect(), "{error:?}");
        // An interrupted statement leaves the session alive.
        let error = statement_error(
            server(1317, "70100", "Query execution was interrupted"),
            "SELECT SLEEP(60)",
        );
        assert!(!error.is_disconnect(), "{error:?}");
        assert_eq!(
            error.db().and_then(|db| db.code.as_deref()),
            Some("1317 (70100)")
        );
    }

    #[test]
    fn connection_failures_carry_the_servers_sentence() {
        let p = params(TlsConfig::default());
        let error = connect_error(
            server(
                1045,
                "28000",
                "Access denied for user 'kerno'@'10.0.0.1' (using password: YES)",
            ),
            &p,
        );
        assert!(matches!(error, Error::Connect(_)), "{error:?}");
        assert!(error.to_string().contains("Access denied"), "{error}");
        assert!(error.to_string().contains("1045 (28000)"), "{error}");

        let error = connect_error(
            mysql_async::Error::Driver(DriverError::NoClientSslFlagFromServer),
            &p,
        );
        assert!(matches!(error, Error::Tls(_)), "{error:?}");
    }

    #[test]
    fn syntax_errors_are_placed_in_the_statement() {
        let near = |fragment: &str, line: u32| {
            format!(
                "You have an error in your SQL syntax; check the manual that corresponds to \
                 your MySQL server version for the right syntax to use near '{fragment}' at line {line}"
            )
        };
        assert_eq!(error_position(&near("SELEC 1", 1), "SELEC 1"), Some(1));
        assert_eq!(
            error_position(&near("WHERE", 2), "SELECT *\nFROM WHERE"),
            Some(15)
        );
        // Trailing whitespace is not part of what the server quotes.
        assert_eq!(
            error_position(&near("FROM", 1), "SELECT 1 FROM  \n"),
            Some(10)
        );
        // An error at the end of the input quotes nothing.
        assert_eq!(error_position(&near("", 1), "SELECT 1 +"), Some(11));
        // Characters, not bytes.
        assert_eq!(
            error_position(&near("nope 1", 1), "SELECT 'caf\u{e9}' nope 1"),
            Some(15)
        );
        // A fragment cut at 80 bytes is found from the line the server names.
        let long = format!("SELECT 1;\nSELEC {}", "x".repeat(200));
        let fragment: String = long["SELECT 1;\n".len()..].chars().take(80).collect();
        assert_eq!(error_position(&near(&fragment, 2), &long), Some(11));
        // A quote inside the fragment does not confuse the reading.
        assert_eq!(
            error_position(&near("'a' at line 9' x", 1), "SELECT 'a' at line 9' x"),
            Some(8)
        );
        assert_eq!(
            error_position("Unknown column 'x' in 'field list'", "x"),
            None
        );
        assert_eq!(error_position(&near("gone", 1), "SELECT 1"), None);
    }
}

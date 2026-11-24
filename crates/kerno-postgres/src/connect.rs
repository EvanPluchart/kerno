//! Opening a PostgreSQL connection: TLS configuration, direct or tunnelled
//! transport, and cancel requests over the same transport.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use kerno_core::config::{ConnectParams, TlsConfig, TlsMode};
use kerno_core::ssh::{Tunnel, TunnelStream};
use kerno_core::{DbError, Error, Result};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme,
};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio_postgres::config::SslMode;
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::{AsyncMessage, CancelToken, Client, Connection};
use tokio_postgres_rustls::MakeRustlsConnect;

/// Default time allowed for the connection to be established.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How to send a cancel request for a session: the same route as the
/// session itself.
pub(crate) struct CancelRoute {
    token: CancelToken,
    tls: MakeRustlsConnect,
    tunnel: Option<Arc<Tunnel>>,
    host: String,
    port: u16,
}

impl CancelRoute {
    /// Sends a cancel request and returns once the server has acted on it.
    ///
    /// The server signals the session's backend from another process: a
    /// request that is merely sent can land after the session's next
    /// statement has started, and stop that one instead. So, as libpq's
    /// `PQcancel` and pgJDBC do, wait for the server to close the cancel
    /// connection, which it does once the signal is sent.
    pub(crate) async fn cancel(&self) -> Result<()> {
        let text = |error: &dyn std::fmt::Display| error.to_string();
        let sending = async {
            match &self.tunnel {
                // A Unix socket directory, as tokio-postgres reads the host
                // (on Unix only: elsewhere such a host is a host name).
                #[cfg(unix)]
                None if self.host.starts_with('/') => {
                    let path = format!("{}/.s.PGSQL.{}", self.host, self.port);
                    let stream = tokio::net::UnixStream::connect(path)
                        .await
                        .map_err(|error| text(&error))?;
                    let connect = tls_connect_for::<UntilClosed<tokio::net::UnixStream>>(
                        &self.tls, &self.host,
                    )
                    .map_err(|e| text(&e))?;
                    self.token
                        .cancel_query_raw(UntilClosed::new(stream), connect)
                        .await
                        .map_err(|error| text(&error))
                }
                None => {
                    let stream = tokio::net::TcpStream::connect((self.host.as_str(), self.port))
                        .await
                        .map_err(|error| text(&error))?;
                    let connect = tls_connect_for::<UntilClosed<tokio::net::TcpStream>>(
                        &self.tls, &self.host,
                    )
                    .map_err(|e| text(&e))?;
                    self.token
                        .cancel_query_raw(UntilClosed::new(stream), connect)
                        .await
                        .map_err(|error| text(&error))
                }
                Some(tunnel) => {
                    let stream = tunnel
                        .open_stream(&self.host, self.port)
                        .await
                        .map_err(|error| text(&error))?;
                    let connect =
                        tls_connect_for::<UntilClosed<TunnelStream>>(&self.tls, &self.host)
                            .map_err(|e| text(&e))?;
                    self.token
                        .cancel_query_raw(UntilClosed::new(stream), connect)
                        .await
                        .map_err(|error| text(&error))
                }
            }
        };
        match tokio::time::timeout(CANCEL_TIMEOUT, sending).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(Error::Connect(format!(
                "could not send the cancel request: {error}"
            ))),
            Err(_) => Err(Error::Connect(
                "the server did not answer the cancel request".into(),
            )),
        }
    }
}

/// How long a cancel request may take, sent and acted on.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(10);

/// The transport of a cancel request: shutting it down also waits for the
/// server to close its end (see [`CancelRoute::cancel`]).
struct UntilClosed<S> {
    stream: S,
    /// The write side is shut: a stream is not asked to do it twice.
    shut: bool,
}

impl<S> UntilClosed<S> {
    fn new(stream: S) -> Self {
        Self {
            stream,
            shut: false,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for UntilClosed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for UntilClosed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if !self.shut {
            std::task::ready!(Pin::new(&mut self.stream).poll_shutdown(cx))?;
            self.shut = true;
        }
        // The server sends nothing back: read until it closes. A server on
        // Windows closes with our end's FIN unread, which Windows turns into
        // a reset: closed all the same (libpq ignores the read's errors).
        let mut scratch = [0u8; 64];
        loop {
            let mut buf = ReadBuf::new(&mut scratch);
            match std::task::ready!(Pin::new(&mut self.stream).poll_read(cx, &mut buf)) {
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {
                    return Poll::Ready(Ok(()));
                }
                result => result?,
            }
            if buf.filled().is_empty() {
                return Poll::Ready(Ok(()));
            }
        }
    }
}

fn tls_connect_for<S>(
    tls: &MakeRustlsConnect,
    host: &str,
) -> Result<<MakeRustlsConnect as MakeTlsConnect<S>>::TlsConnect>
where
    MakeRustlsConnect: MakeTlsConnect<S>,
    <MakeRustlsConnect as MakeTlsConnect<S>>::Error: std::fmt::Display,
{
    let mut make = tls.clone();
    <MakeRustlsConnect as MakeTlsConnect<S>>::make_tls_connect(&mut make, host)
        .map_err(|error| Error::Tls(error.to_string()))
}

/// What a notice or a lost connection looks like to the session.
pub(crate) enum ConnectionEvent {
    Notice(kerno_core::Notice),
    Closed(String),
}

/// Why the connection ended, set once by the task that drives it (or by
/// Kerno's own close): read without sending anything (C15).
pub(crate) type Gone = Arc<std::sync::OnceLock<DbError>>;

/// Opens a connection and starts the task that drives it.
pub(crate) async fn open(
    params: &ConnectParams,
) -> Result<(
    Client,
    mpsc::UnboundedReceiver<ConnectionEvent>,
    CancelRoute,
    Gone,
)> {
    let tls = MakeRustlsConnect::new(tls_config(&params.tls)?);
    let mut config = tokio_postgres::Config::new();
    config
        .user(&params.user)
        .dbname(
            params
                .database
                .as_deref()
                .unwrap_or(crate::DEFAULT_DATABASE),
        )
        .ssl_mode(match params.tls.mode {
            TlsMode::Disable => SslMode::Disable,
            TlsMode::Prefer => SslMode::Prefer,
            TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull => SslMode::Require,
        });
    if let Some(password) = &params.password {
        config.password(password.expose());
    }

    let mut timeout = DEFAULT_CONNECT_TIMEOUT;
    let mut application_name = "Kerno".to_string();
    // Every other option is a server setting, applied at session start.
    let mut settings = Vec::new();
    if params.read_only {
        settings.push("-c default_transaction_read_only=on".to_string());
    }
    for (key, value) in &params.options {
        match key.as_str() {
            "connect_timeout" => {
                let seconds: u64 = value.parse().map_err(|_| {
                    Error::Config(format!(
                        "connect_timeout must be a number of seconds, not \"{value}\""
                    ))
                })?;
                timeout = Duration::from_secs(seconds.max(1));
            }
            "application_name" => application_name = value.clone(),
            _ => settings.push(format!("-c {key}={}", escape_startup_option(value))),
        }
    }
    config.application_name(&application_name);
    if !settings.is_empty() {
        config.options(settings.join(" "));
    }

    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let gone = Gone::default();
    let client = match &params.tunnel {
        None => {
            config
                .host(&params.host)
                .port(params.port)
                .connect_timeout(timeout)
                .keepalives(true)
                .keepalives_idle(Duration::from_secs(60));
            let (client, connection) = config.connect(tls.clone()).await.map_err(connect_error)?;
            tokio::spawn(drive(connection, events_tx, gone.clone()));
            client
        }
        Some(tunnel) => {
            let stream = tunnel.open_stream(&params.host, params.port).await?;
            let connect = tls_connect_for::<TunnelStream>(&tls, &params.host)?;
            let (client, connection) =
                tokio::time::timeout(timeout, config.connect_raw(stream, connect))
                    .await
                    .map_err(|_| {
                        Error::Connect(format!(
                            "no answer from {}:{} after {} s",
                            params.host,
                            params.port,
                            timeout.as_secs()
                        ))
                    })?
                    .map_err(connect_error)?;
            tokio::spawn(drive(connection, events_tx, gone.clone()));
            client
        }
    };

    let route = CancelRoute {
        token: client.cancel_token(),
        tls,
        tunnel: params.tunnel.clone(),
        host: params.host.clone(),
        port: params.port,
    };
    Ok((client, events_rx, route, gone))
}

/// Startup options are split on spaces; a backslash protects a space or a
/// backslash inside a value.
fn escape_startup_option(value: &str) -> String {
    value.replace('\\', "\\\\").replace(' ', "\\ ")
}

/// Drives the connection until it ends, forwarding notices. The task reads
/// the socket all the time, so a server that closes an idle session (its
/// FATAL: a timeout, a terminate) is seen at once, nothing sent (C15).
async fn drive<S, T>(
    mut connection: Connection<S, T>,
    events: mpsc::UnboundedSender<ConnectionEvent>,
    gone: Gone,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let why = loop {
        match std::future::poll_fn(|cx| connection.poll_message(cx)).await {
            Some(Ok(AsyncMessage::Notice(notice))) => {
                let _ = events.send(ConnectionEvent::Notice(db_error(&notice).into()));
            }
            Some(Ok(_)) => {}
            // The server's words and SQLSTATE, when it said why.
            Some(Err(error)) => match error.as_db_error() {
                Some(db) => break db_error(db),
                None => break DbError::message(error_chain(&error)),
            },
            None => break DbError::message("the server closed the connection"),
        }
    };
    let _ = events.send(ConnectionEvent::Closed(why.to_string()));
    let _ = gone.set(why);
}

/// An error's message followed by its causes.
pub(crate) fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        // tokio-postgres already appends its direct cause.
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

fn connect_error(error: tokio_postgres::Error) -> Error {
    if let Some(db) = error.as_db_error() {
        // Wrong password, unknown database, pg_hba rejection: the server's
        // own sentence is the useful one.
        return Error::Connect(format!("{} [{}]", db.message(), db.code().code()));
    }
    let text = error_chain(&error);
    // The client library's words when the server asks for a password and
    // the settings carry none.
    if text.contains("password missing") {
        return Error::PasswordRequired;
    }
    if text.contains("TLS") || text.contains("certificate") {
        Error::Tls(text)
    } else {
        Error::Connect(text)
    }
}

/// Converts a statement error.
pub(crate) fn statement_error(error: tokio_postgres::Error) -> Error {
    match error.as_db_error() {
        Some(db) => Error::Db(Box::new(db_error(db))),
        None if error.is_closed() => Error::Closed(error_chain(&error)),
        None => Error::Closed(error_chain(&error)),
    }
}

pub(crate) fn db_error(db: &tokio_postgres::error::DbError) -> DbError {
    DbError {
        message: db.message().to_string(),
        code: Some(db.code().code().to_string()),
        severity: Some(db.severity().to_string()),
        detail: db.detail().map(str::to_string),
        hint: db.hint().map(str::to_string),
        position: match db.position() {
            Some(tokio_postgres::error::ErrorPosition::Original(position)) => Some(*position),
            _ => None,
        },
    }
}

// TLS ----------------------------------------------------------------------

fn read_error(what: &str, path: &std::path::Path, error: impl std::fmt::Display) -> Error {
    Error::Tls(format!(
        "could not read the {what} {}: {error}",
        path.display()
    ))
}

/// Builds the rustls configuration for a connection's TLS settings.
pub(crate) fn tls_config(tls: &TlsConfig) -> Result<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|error| Error::Tls(error.to_string()))?;

    let builder = if tls.mode.verifies_certificate() {
        let verifier = WebPkiServerVerifier::builder_with_provider(Arc::new(roots(tls)?), provider)
            .build()
            .map_err(|error| Error::Tls(error.to_string()))?;
        if tls.mode == TlsMode::VerifyFull {
            builder.with_webpki_verifier(verifier)
        } else {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AnyHostName(verifier)))
        }
    } else {
        // disable / prefer / require: encryption without authentication,
        // exactly what those modes promise.
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
    };

    match (&tls.cert_file, &tls.key_file) {
        (Some(cert_file), Some(key_file)) => {
            let certs = CertificateDer::pem_file_iter(cert_file)
                .and_then(|certs| certs.collect::<std::result::Result<Vec<_>, _>>())
                .map_err(|error| read_error("client certificate", cert_file, error))?;
            if certs.is_empty() {
                return Err(read_error(
                    "client certificate",
                    cert_file,
                    "no certificate found in the file",
                ));
            }
            let key = PrivateKeyDer::from_pem_file(key_file)
                .map_err(|error| read_error("client key", key_file, error))?;
            builder.with_client_auth_cert(certs, key).map_err(|error| {
                Error::Tls(format!(
                    "the client certificate and key cannot be used: {error}"
                ))
            })
        }
        (None, None) => Ok(builder.with_no_client_auth()),
        _ => Err(Error::Config(
            "a client certificate needs both the certificate file and the key file".into(),
        )),
    }
}

/// The certificate authorities to trust: the configured file, or the
/// system's store.
fn roots(tls: &TlsConfig) -> Result<RootCertStore> {
    let mut store = RootCertStore::empty();
    match &tls.ca_file {
        Some(ca_file) => {
            let certs = CertificateDer::pem_file_iter(ca_file)
                .and_then(|certs| certs.collect::<std::result::Result<Vec<_>, _>>())
                .map_err(|error| read_error("CA certificate", ca_file, error))?;
            let (added, _) = store.add_parsable_certificates(certs);
            if added == 0 {
                return Err(read_error(
                    "CA certificate",
                    ca_file,
                    "no certificate found in the file",
                ));
            }
        }
        None => {
            let native = rustls_native_certs::load_native_certs();
            let (added, _) = store.add_parsable_certificates(native.certs);
            if added == 0 {
                return Err(Error::Tls(
                    "the system has no trusted certificate authorities; choose a CA file".into(),
                ));
            }
        }
    }
    Ok(store)
}

/// `verify-ca`: the chain must lead to a trusted authority; the host name
/// is not compared.
#[derive(Debug)]
struct AnyHostName(Arc<WebPkiServerVerifier>);

impl ServerCertVerifier for AnyHostName {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        match self
            .0
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
        {
            // The chain is checked before the name, so reaching a name
            // error means the chain is good.
            Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

/// `prefer` / `require`: any certificate is accepted. The handshake
/// signature is still checked, so the peer holds the key of the certificate
/// it presents.
#[derive(Debug)]
struct AnyCertificate(Arc<CryptoProvider>);

impl ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The end of a cancel connection: what writing does, then what each
    /// read after the write side is shut answers.
    struct CancelEnd {
        write: Option<std::io::ErrorKind>,
        read: Option<std::io::ErrorKind>,
        written: usize,
    }

    impl AsyncRead for CancelEnd {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(self.read.map_or(Ok(()), |kind| Err(kind.into())))
        }
    }

    impl AsyncWrite for CancelEnd {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if let Some(kind) = self.write {
                return Poll::Ready(Err(kind.into()));
            }
            self.written += buf.len();
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// K68: a PostgreSQL server on Windows ends the cancel connection with a
    /// reset once it has the request: that is its close. A request that
    /// could not be written, or another error of the read, is still one.
    #[tokio::test]
    async fn a_reset_after_the_cancel_request_is_the_server_closing() {
        use std::io::ErrorKind::{BrokenPipe, ConnectionReset, TimedOut};
        use tokio::io::AsyncWriteExt as _;
        let end = |write, read| {
            UntilClosed::new(CancelEnd {
                write,
                read,
                written: 0,
            })
        };
        let request = [0u8; 16];

        let mut closed = end(None, None);
        closed.write_all(&request).await.unwrap();
        closed.shutdown().await.unwrap();

        let mut reset = end(None, Some(ConnectionReset));
        reset.write_all(&request).await.unwrap();
        reset.shutdown().await.unwrap();
        assert_eq!(reset.stream.written, request.len());

        let mut unsent = end(Some(ConnectionReset), None);
        let error = unsent.write_all(&request).await.unwrap_err();
        assert_eq!(error.kind(), ConnectionReset);
        let mut broken = end(Some(BrokenPipe), None);
        assert!(broken.write_all(&request).await.is_err());

        let mut silent = end(None, Some(TimedOut));
        silent.write_all(&request).await.unwrap();
        assert_eq!(silent.shutdown().await.unwrap_err().kind(), TimedOut);
    }

    #[test]
    fn startup_option_values_protect_spaces_and_backslashes() {
        assert_eq!(escape_startup_option("public"), "public");
        assert_eq!(escape_startup_option("a b"), "a\\ b");
        assert_eq!(escape_startup_option("a\\b c"), "a\\\\b\\ c");
    }

    #[test]
    fn tls_config_builds_for_the_non_verifying_modes() {
        for mode in [TlsMode::Disable, TlsMode::Prefer, TlsMode::Require] {
            let tls = TlsConfig {
                mode,
                ..TlsConfig::default()
            };
            assert!(tls_config(&tls).is_ok(), "{mode:?}");
        }
    }

    #[test]
    fn missing_files_are_reported_with_their_path() {
        let tls = TlsConfig {
            mode: TlsMode::VerifyFull,
            ca_file: Some("/nonexistent/kerno-ca.pem".into()),
            ..TlsConfig::default()
        };
        let error = tls_config(&tls).unwrap_err().to_string();
        assert!(error.contains("/nonexistent/kerno-ca.pem"), "{error}");

        let tls = TlsConfig {
            mode: TlsMode::Require,
            cert_file: Some("/nonexistent/client.crt".into()),
            key_file: Some("/nonexistent/client.key".into()),
            ..TlsConfig::default()
        };
        let error = tls_config(&tls).unwrap_err().to_string();
        assert!(error.contains("/nonexistent/client.crt"), "{error}");
    }

    #[test]
    fn a_certificate_without_its_key_is_a_configuration_error() {
        let tls = TlsConfig {
            mode: TlsMode::Require,
            cert_file: Some("/c.pem".into()),
            ..TlsConfig::default()
        };
        assert!(matches!(tls_config(&tls), Err(Error::Config(_))));
    }
}

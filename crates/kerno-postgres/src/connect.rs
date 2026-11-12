//! Opening a PostgreSQL connection: TLS configuration, direct or tunnelled
//! transport, and cancel requests over the same transport.

use std::sync::Arc;
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
    pub(crate) async fn cancel(&self) -> Result<()> {
        let outcome = match &self.tunnel {
            None => self.token.cancel_query(self.tls.clone()).await,
            Some(tunnel) => {
                let stream = tunnel.open_stream(&self.host, self.port).await?;
                let connect = tls_connect_for(&self.tls, &self.host)?;
                self.token.cancel_query_raw(stream, connect).await
            }
        };
        outcome
            .map_err(|error| Error::Connect(format!("could not send the cancel request: {error}")))
    }
}

fn tls_connect_for(
    tls: &MakeRustlsConnect,
    host: &str,
) -> Result<<MakeRustlsConnect as MakeTlsConnect<TunnelStream>>::TlsConnect> {
    let mut make = tls.clone();
    <MakeRustlsConnect as MakeTlsConnect<TunnelStream>>::make_tls_connect(&mut make, host)
        .map_err(|error| Error::Tls(error.to_string()))
}

/// What a notice or a lost connection looks like to the session.
pub(crate) enum ConnectionEvent {
    Notice(kerno_core::Notice),
    Closed(String),
}

/// Opens a connection and starts the task that drives it.
pub(crate) async fn open(
    params: &ConnectParams,
) -> Result<(
    Client,
    mpsc::UnboundedReceiver<ConnectionEvent>,
    CancelRoute,
)> {
    let tls = MakeRustlsConnect::new(tls_config(&params.tls)?);
    let mut config = tokio_postgres::Config::new();
    config
        .user(&params.user)
        // libpq falls back to the user name; "postgres" exists on almost
        // every server and is what people expect to land in.
        .dbname(params.database.as_deref().unwrap_or("postgres"))
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
    let client = match &params.tunnel {
        None => {
            config
                .host(&params.host)
                .port(params.port)
                .connect_timeout(timeout)
                .keepalives(true)
                .keepalives_idle(Duration::from_secs(60));
            let (client, connection) = config.connect(tls.clone()).await.map_err(connect_error)?;
            tokio::spawn(drive(connection, events_tx));
            client
        }
        Some(tunnel) => {
            let stream = tunnel.open_stream(&params.host, params.port).await?;
            let connect = tls_connect_for(&tls, &params.host)?;
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
            tokio::spawn(drive(connection, events_tx));
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
    Ok((client, events_rx, route))
}

/// Startup options are split on spaces; a backslash protects a space or a
/// backslash inside a value.
fn escape_startup_option(value: &str) -> String {
    value.replace('\\', "\\\\").replace(' ', "\\ ")
}

/// Drives the connection until it ends, forwarding notices.
async fn drive<S, T>(
    mut connection: Connection<S, T>,
    events: mpsc::UnboundedSender<ConnectionEvent>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        match std::future::poll_fn(|cx| connection.poll_message(cx)).await {
            Some(Ok(AsyncMessage::Notice(notice))) => {
                let _ = events.send(ConnectionEvent::Notice(db_error(&notice).into()));
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => {
                let _ = events.send(ConnectionEvent::Closed(error_chain(&error)));
                return;
            }
            None => {
                let _ = events.send(ConnectionEvent::Closed(
                    "the server closed the connection".into(),
                ));
                return;
            }
        }
    }
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

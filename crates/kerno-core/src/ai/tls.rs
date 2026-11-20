//! The AI client's trust (K23, D-095): the certificates the system trusts, and, when a server's
//! certificate is refused for an unknown issuer, the name of that issuer for the message.

use std::net::{TcpStream, ToSocketAddrs as _};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use ureq::tls::{Certificate, RootCerts};

/// The roots the client trusts: the system's (macOS's keychains and trust settings, Linux's
/// CA bundle), which hold an internal authority the system was told to trust; Mozilla's when
/// the system gives none. Read once, at the first `https://` request.
pub(super) fn roots() -> RootCerts {
    static ROOTS: OnceLock<RootCerts> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let native = rustls_native_certs::load_native_certs();
            if native.certs.is_empty() {
                RootCerts::WebPki
            } else {
                RootCerts::Specific(Arc::new(
                    (native.certs.iter())
                        .map(|cert| Certificate::from_der(cert.as_ref()).to_owned())
                        .collect(),
                ))
            }
        })
        .clone()
}

/// The TLS failure in `error`, if it is one: ureq gives it as is or inside an I/O error.
pub(super) fn tls_error(error: &ureq::Error) -> Option<&rustls::Error> {
    match error {
        ureq::Error::Rustls(error) => Some(error),
        ureq::Error::Io(io) => io.get_ref()?.downcast_ref::<rustls::Error>(),
        _ => None,
    }
}

/// Whether `error` is a certificate refused because its issuer is not trusted.
pub(super) fn unknown_issuer(error: &ureq::Error) -> bool {
    matches!(
        tls_error(error),
        Some(rustls::Error::InvalidCertificate(
            CertificateError::UnknownIssuer
        ))
    )
}

/// The issuer of the certificate `host:port` presents, read by a handshake that is refused
/// whatever the certificate: the verifier only notes the issuer and fails, so no connection is
/// ever made and nothing is sent but the TLS greeting.
pub(super) fn issuer(host: &str, port: u16, timeout: Duration) -> Option<String> {
    let seen = Arc::new(Mutex::new(None));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .ok()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Recorder {
            seen: seen.clone(),
            schemes: provider
                .signature_verification_algorithms
                .supported_schemes(),
        }))
        .with_no_client_auth();
    let name = ServerName::try_from(host.to_string()).ok()?;
    let mut connection = rustls::ClientConnection::new(Arc::new(config), name).ok()?;
    let address = (host, port).to_socket_addrs().ok()?.next()?;
    let mut stream = TcpStream::connect_timeout(&address, timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    while connection.is_handshaking() {
        if connection.complete_io(&mut stream).is_err() {
            break;
        }
    }
    seen.lock().ok()?.take()
}

/// The common name of a certificate's issuer, else its whole name.
pub fn issuer_name(der: &[u8]) -> Option<String> {
    use x509_cert::der::Decode as _;
    let cert = x509_cert::Certificate::from_der(der).ok()?;
    let issuer = &cert.tbs_certificate.issuer;
    issuer
        .0
        .iter()
        .flat_map(|rdn| rdn.0.iter())
        .find_map(|attribute| {
            attribute
                .to_string()
                .strip_prefix("CN=")
                .map(str::to_string)
        })
        .or_else(|| Some(issuer.to_string()))
}

/// A verifier that refuses every certificate after noting its issuer.
#[derive(Debug)]
struct Recorder {
    seen: Arc<Mutex<Option<String>>>,
    schemes: Vec<SignatureScheme>,
}

impl ServerCertVerifier for Recorder {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Ok(mut seen) = self.seen.lock() {
            *seen = issuer_name(end_entity.as_ref());
        }
        Err(rustls::Error::InvalidCertificate(
            CertificateError::UnknownIssuer,
        ))
    }

    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("not verified".into()))
    }

    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("not verified".into()))
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes.clone()
    }
}

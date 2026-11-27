//! TLS configuration: TLS 1.3 only, rustls with the ring provider, mutual authentication against
//! the relay's own CA. The verifier checks the chain and validity dates, so an expired or foreign
//! certificate never completes a handshake.

use std::path::Path;
use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};

fn err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn certs(path: &Path) -> std::io::Result<Vec<CertificateDer<'static>>> {
    CertificateDer::pem_file_iter(path)
        .map_err(|e| err(format!("{}: {e}", path.display())))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| err(format!("{}: {e}", path.display())))
}

fn key(path: &Path) -> std::io::Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_file(path).map_err(|e| err(format!("{}: {e}", path.display())))
}

fn roots(ca: &Path) -> std::io::Result<RootCertStore> {
    let mut roots = RootCertStore::empty();
    for c in certs(ca)? {
        roots.add(c).map_err(err)?;
    }
    Ok(roots)
}

/// The relay's TLS: its certificate, and client certificates required and verified against `ca`.
pub fn server(ca: &Path, cert: &Path, key_file: &Path) -> std::io::Result<Arc<ServerConfig>> {
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots(ca)?), provider())
        .build()
        .map_err(err)?;
    let config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(err)?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs(cert)?, key(key_file)?)
        .map_err(err)?;
    Ok(Arc::new(config))
}

/// A client's TLS: trusts only `ca`, presents `cert`.
pub fn client(ca: &Path, cert: &Path, key_file: &Path) -> std::io::Result<Arc<ClientConfig>> {
    let config = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(err)?
        .with_root_certificates(roots(ca)?)
        .with_client_auth_cert(certs(cert)?, key(key_file)?)
        .map_err(err)?;
    Ok(Arc::new(config))
}

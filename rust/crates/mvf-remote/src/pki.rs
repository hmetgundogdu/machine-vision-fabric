//! A small private certificate authority for the relay: one CA, one server certificate, and a
//! client certificate per operator. Every key is ECDSA P-256 and written mode 0600.
//!
//! ```text
//! <dir>/ca.pem  ca.key          the CA (keep ca.key offline in production; `issue` needs it)
//! <dir>/server.pem server.key   the relay's identity (names given at `init`)
//! <dir>/clients/<name>.pem .key one per operator or tool
//! <dir>/access.json             who may connect, with which role; revoked fingerprints
//! ```

use std::path::{Path, PathBuf};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use time::{Duration, OffsetDateTime};

use crate::access::{AccessList, ClientEntry};

/// The CA's subject: fixed, so `issue` can sign with the CA key alone.
const CA_NAME: &str = "MVF management CA";

fn err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Writes `text` to `path`, private to the user when `secret`.
pub(crate) fn write(path: &Path, text: &str, secret: bool) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = secret;
    use std::io::Write;
    options.open(path)?.write_all(text.as_bytes())
}

fn ca_params(not_before: OffsetDateTime, not_after: OffsetDateTime) -> CertificateParams {
    let mut p = CertificateParams::default();
    p.distinguished_name.push(DnType::CommonName, CA_NAME);
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    p.not_before = not_before;
    p.not_after = not_after;
    p
}

/// SHA-256 of a DER certificate, as `sha256:<hex>`.
pub fn fingerprint(der: &[u8]) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, der);
    let hex: String = d.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// Creates the CA, the relay's server certificate for `hosts` (DNS names or IP addresses) and an
/// empty access list. Refuses to overwrite an existing CA.
pub fn init(dir: &Path, hosts: &[String]) -> std::io::Result<()> {
    if dir.join("ca.key").exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already holds a CA", dir.display()),
        ));
    }
    let now = OffsetDateTime::now_utc();
    let ca_key = KeyPair::generate().map_err(err)?;
    let ca = ca_params(now - Duration::minutes(5), now + Duration::days(3650));
    let ca_cert = ca.self_signed(&ca_key).map_err(err)?;
    write(&dir.join("ca.key"), &ca_key.serialize_pem(), true)?;
    write(&dir.join("ca.pem"), &ca_cert.pem(), false)?;
    let issuer = Issuer::new(ca, ca_key);
    let key = KeyPair::generate().map_err(err)?;
    let mut p = CertificateParams::new(hosts.to_vec()).map_err(err)?;
    p.distinguished_name
        .push(DnType::CommonName, "mvf-remote relay");
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    p.not_before = now - Duration::minutes(5);
    p.not_after = now + Duration::days(397);
    let cert = p.signed_by(&key, &issuer).map_err(err)?;
    write(&dir.join("server.key"), &key.serialize_pem(), true)?;
    write(&dir.join("server.pem"), &cert.pem(), false)?;
    AccessList::default().save(&dir.join("access.json"))
}

/// An issued client certificate.
#[derive(Debug, Clone)]
pub struct Issued {
    /// Certificate file.
    pub cert: PathBuf,
    /// Key file.
    pub key: PathBuf,
    /// Its fingerprint (what the access list names).
    pub fingerprint: String,
}

/// Issues a client certificate for `name`, valid from `not_before` to `not_after`, and, with a
/// role, adds it to the access list.
pub fn issue(
    dir: &Path,
    name: &str,
    role: Option<mvf_management::Role>,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
) -> std::io::Result<Issued> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(err("a client name is 1-64 characters [A-Za-z0-9-_.]"));
    }
    let ca_key = KeyPair::from_pem(&std::fs::read_to_string(dir.join("ca.key"))?).map_err(err)?;
    let issuer = Issuer::new(ca_params(not_before, not_after), ca_key);
    let key = KeyPair::generate().map_err(err)?;
    let mut p = CertificateParams::default();
    p.distinguished_name.push(DnType::CommonName, name);
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    p.not_before = not_before;
    p.not_after = not_after;
    let cert = p.signed_by(&key, &issuer).map_err(err)?;
    let issued = Issued {
        cert: dir.join("clients").join(format!("{name}.pem")),
        key: dir.join("clients").join(format!("{name}.key")),
        fingerprint: fingerprint(cert.der()),
    };
    write(&issued.key, &key.serialize_pem(), true)?;
    write(&issued.cert, &cert.pem(), false)?;
    if let Some(role) = role {
        let path = dir.join("access.json");
        let mut list = AccessList::load(&path).unwrap_or_default();
        list.clients.push(ClientEntry {
            name: name.to_string(),
            fingerprint: issued.fingerprint.clone(),
            role: role.as_str().to_string(),
        });
        list.save(&path)?;
    }
    Ok(issued)
}

/// The usual validity: from now (minus clock skew) for `days`.
pub fn validity(days: i64) -> (OffsetDateTime, OffsetDateTime) {
    let now = OffsetDateTime::now_utc();
    (now - Duration::minutes(5), now + Duration::days(days))
}

//! Who may connect through the relay, and with which role.
//!
//! A client must present a certificate issued by the relay's CA *and* be named in the access list
//! by its certificate fingerprint. A valid certificate alone grants nothing. The list is read
//! again for every connection, so a revocation or a role change takes effect on the next connect
//! without restarting the relay.
//!
//! ```json
//! {"clients": [{"name": "alice", "fingerprint": "sha256:…", "role": "operator"}],
//!  "revoked": ["sha256:…"]}
//! ```

use std::path::Path;

use mvf_management::Role;
use serde::{Deserialize, Serialize};

/// One allowed client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientEntry {
    /// The principal name the runtime audits.
    pub name: String,
    /// `sha256:<hex>` of its certificate.
    pub fingerprint: String,
    /// `observer` or `operator`.
    pub role: String,
}

/// The access list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccessList {
    /// Allowed clients.
    #[serde(default)]
    pub clients: Vec<ClientEntry>,
    /// Fingerprints refused whatever else says.
    #[serde(default)]
    pub revoked: Vec<String>,
}

/// Why a certificate was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Its fingerprint is revoked.
    Revoked,
    /// It is not in the list.
    Unknown,
    /// The list names it with a role that does not exist.
    BadRole(String),
}

impl AccessList {
    /// Reads `path`.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        serde_json::from_slice(&std::fs::read(path)?).map_err(std::io::Error::other)
    }

    /// Writes `path` (atomically, private to the user).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        crate::pki::write(
            &tmp,
            &serde_json::to_string_pretty(self).map_err(std::io::Error::other)?,
            true,
        )?;
        std::fs::rename(tmp, path)
    }

    /// The principal and role for a certificate fingerprint.
    pub fn admit(&self, fingerprint: &str) -> Result<(String, Role), Refusal> {
        if self.revoked.iter().any(|r| r == fingerprint) {
            return Err(Refusal::Revoked);
        }
        let entry = self
            .clients
            .iter()
            .find(|c| c.fingerprint == fingerprint)
            .ok_or(Refusal::Unknown)?;
        let role = Role::parse(&entry.role).ok_or_else(|| Refusal::BadRole(entry.role.clone()))?;
        Ok((entry.name.clone(), role))
    }

    /// Revokes every certificate of `name`, or one fingerprint.
    pub fn revoke(&mut self, name_or_fingerprint: &str) -> usize {
        let hits: Vec<String> = self
            .clients
            .iter()
            .filter(|c| c.name == name_or_fingerprint || c.fingerprint == name_or_fingerprint)
            .map(|c| c.fingerprint.clone())
            .collect();
        let n = hits.len();
        for f in hits {
            if !self.revoked.contains(&f) {
                self.revoked.push(f);
            }
        }
        n
    }
}

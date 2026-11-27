//! Package signing for the runtime's signature policy (#33): where packages are built, not on the
//! edge box.
//!
//! ```text
//! mvf-remote sign keygen --dir keys                 keys/signer.pk8 (0600) and keys/signer.pub
//! mvf-remote sign package --key keys/signer.pk8 --package inspect-v2 --out inspect-v2.sig.json ./inspect-v2
//! mvf run --manage --allow-uploads --trust-signer keys/signer.pub …   (on the edge box)
//! mvf manage upload --package inspect-v2 --signature inspect-v2.sig.json ./inspect-v2
//! ```
//!
//! A signature is Ed25519 over `mvf_management::upload::signing_bytes`: the package name and every
//! file's SHA-256. The runtime verifies it with an independent implementation (ed25519-dalek).

use std::path::Path;

use mvf_management::upload::{key_id, manifest_of, signing_bytes};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{Value, json};

fn err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Creates `dir/signer.pk8` (the private key, mode 0600) and `dir/signer.pub` (its
/// `ed25519:<hex>` id). Refuses to overwrite a key.
pub fn keygen(dir: &Path) -> std::io::Result<String> {
    let private = dir.join("signer.pk8");
    if private.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", private.display()),
        ));
    }
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(err)?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(err)?;
    let id = key_id(pair.public_key().as_ref());
    std::fs::create_dir_all(dir)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options.open(&private)?.write_all(pkcs8.as_ref())?;
    std::fs::write(dir.join("signer.pub"), format!("{id}\n"))?;
    Ok(id)
}

/// Signs the package in `dir` as `package` with the PKCS#8 key at `key`: `{package, key, sig,
/// files}`.
pub fn sign(key: &Path, package: &str, dir: &Path) -> std::io::Result<Value> {
    let pair = Ed25519KeyPair::from_pkcs8(&std::fs::read(key)?).map_err(err)?;
    let files = manifest_of(dir)?;
    let sig = pair.sign(&signing_bytes(package, &files));
    Ok(json!({
        "package": package,
        "key": key_id(pair.public_key().as_ref()),
        "sig": mvf_management::preview::base64(sig.as_ref()),
        "files": files,
    }))
}

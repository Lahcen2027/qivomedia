//! Encryption at rest for secrets the server must be able to read back (SSO client secrets).
//!
//! AES-256-GCM (ring). The key comes from `QIVO_SECRET_KEY` (base64, 32 bytes) or, failing
//! that, `<data>/secret.key`, created on first run with owner-only permissions.
//! Format: `v1:<base64(nonce ‖ ciphertext+tag)>`.
//!
//! Keep `secret.key` with the database's backups (both are needed to restore), but never
//! send it with a database copy to someone who shouldn't read the secrets.

use std::{path::Path, sync::OnceLock};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use ring::{
    aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN},
    rand::{SecureRandom, SystemRandom},
};

static KEY: OnceLock<[u8; 32]> = OnceLock::new();

/// Load (or create) the key. Safe to call more than once: the first call wins.
pub fn init(data_dir: &Path) -> anyhow::Result<()> {
    if KEY.get().is_some() {
        return Ok(());
    }
    let key = load_key(data_dir)?;
    let _ = KEY.set(key);
    Ok(())
}

fn load_key(data_dir: &Path) -> anyhow::Result<[u8; 32]> {
    if let Ok(v) = std::env::var("QIVO_SECRET_KEY") {
        let bytes = B64.decode(v.trim()).map_err(|_| anyhow::anyhow!("QIVO_SECRET_KEY must be base64"))?;
        return bytes.try_into().map_err(|_| anyhow::anyhow!("QIVO_SECRET_KEY must decode to exactly 32 bytes"));
    }
    let path = data_dir.join("secret.key");
    if let Ok(bytes) = std::fs::read(&path) {
        return bytes.try_into().map_err(|_| anyhow::anyhow!("{} is damaged (expected 32 bytes)", path.display()));
    }
    let mut key = [0u8; 32];
    SystemRandom::new().fill(&mut key).map_err(|_| anyhow::anyhow!("no secure random source"))?;
    let tmp = data_dir.join("secret.key.tmp");
    std::fs::write(&tmp, key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &path)?;
    tracing::info!("created encryption key {}", path.display());
    Ok(key)
}

fn key() -> anyhow::Result<LessSafeKey> {
    let k = KEY.get().ok_or_else(|| anyhow::anyhow!("encryption key not loaded"))?;
    Ok(LessSafeKey::new(UnboundKey::new(&AES_256_GCM, k).map_err(|_| anyhow::anyhow!("bad key"))?))
}

pub fn seal(plaintext: &str) -> anyhow::Result<String> {
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new().fill(&mut nonce).map_err(|_| anyhow::anyhow!("no secure random source"))?;
    let mut buf = plaintext.as_bytes().to_vec();
    key()?
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(b"qivo-v1"), &mut buf)
        .map_err(|_| anyhow::anyhow!("encryption failed"))?;
    let mut out = nonce.to_vec();
    out.extend(buf);
    Ok(format!("v1:{}", B64.encode(out)))
}

pub fn open(sealed: &str) -> anyhow::Result<String> {
    let raw = B64.decode(sealed.strip_prefix("v1:").ok_or_else(|| anyhow::anyhow!("unknown format"))?)?;
    anyhow::ensure!(raw.len() > NONCE_LEN, "too short");
    let (nonce, ct) = raw.split_at(NONCE_LEN);
    let mut buf = ct.to_vec();
    let plain = key()?
        .open_in_place(Nonce::try_assume_unique_for_key(nonce).map_err(|_| anyhow::anyhow!("bad nonce"))?, Aad::from(b"qivo-v1"), &mut buf)
        .map_err(|_| anyhow::anyhow!("cannot decrypt: wrong key or damaged data"))?;
    Ok(String::from_utf8(plain.to_vec())?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trip_and_tamper_detection() {
        let dir = std::env::temp_dir().join(format!("qivo-crypto-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        super::init(&dir).unwrap();
        let s = super::seal("client-secret-123").unwrap();
        assert!(s.starts_with("v1:") && !s.contains("client-secret"));
        assert_eq!(super::open(&s).unwrap(), "client-secret-123");
        assert_ne!(super::seal("x").unwrap(), super::seal("x").unwrap(), "random nonce each time");
        let mut bad = s.clone();
        bad.replace_range(10..11, if &s[10..11] == "A" { "B" } else { "A" });
        assert!(super::open(&bad).is_err());
    }
}

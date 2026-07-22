use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use hkdf::Hkdf;
use sha2::Sha256;

const HKDF_INFO_AES_GCM: &[u8] = b"afpay-rpc-v1/aes-256-gcm";
const AES_GCM_NONCE_LEN: usize = 12;
const MIN_SECRET_BYTES: usize = 32;
/// Length of the per-session salt produced by the server during Handshake.
pub const HANDSHAKE_SALT_LEN: usize = 32;

#[derive(Clone)]
pub struct Cipher {
    key: [u8; 32],
}

impl Cipher {
    /// Validate that an operator-supplied RPC PSK is high-entropy enough for production use.
    pub fn validate_secret(secret: &str) -> Result<(), String> {
        let secret = secret.trim();
        if secret.len() < MIN_SECRET_BYTES {
            return Err(format!(
                "RPC secret must be at least {MIN_SECRET_BYTES} bytes; generate one with: openssl rand -base64 32"
            ));
        }
        if secret.as_bytes().windows(2).all(|w| w[0] == w[1]) {
            return Err("RPC secret must not be a repeated single character".to_string());
        }
        Ok(())
    }

    /// Derive a 32-byte AES-256 key from the PSK using HKDF-SHA256 with the
    /// caller-supplied salt. Both sides of the RPC use the salt from the most
    /// recent Handshake; restart of either daemon or session re-handshake yields
    /// a fresh salt and therefore a fresh key.
    pub fn from_secret_with_salt(secret: &str, salt: &[u8]) -> Self {
        let mut key = [0u8; 32];
        let hk = Hkdf::<Sha256>::new(Some(salt), secret.as_bytes());
        let _ = hk.expand(HKDF_INFO_AES_GCM, &mut key);
        Self { key }
    }

    /// Encrypt plaintext: zstd compress → AES-256-GCM encrypt. Returns `(nonce, ciphertext)`.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
        let compressed =
            zstd::bulk::compress(plaintext, 1).map_err(|e| format!("zstd compress: {e}"))?;
        let cipher =
            Aes256Gcm::new_from_slice(&self.key).map_err(|e| format!("cipher init: {e}"))?;
        let mut nonce_bytes = [0u8; AES_GCM_NONCE_LEN];
        getrandom::fill(&mut nonce_bytes).map_err(|e| format!("random nonce: {e}"))?;
        let nonce = aes_gcm::Nonce::from(nonce_bytes);
        let ciphertext = cipher
            .encrypt(&nonce, compressed.as_slice())
            .map_err(|e| format!("encrypt: {e}"))?;
        Ok((nonce.to_vec(), ciphertext))
    }

    /// Decrypt ciphertext: AES-256-GCM decrypt → zstd decompress.
    pub fn decrypt(&self, nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        if nonce.len() != AES_GCM_NONCE_LEN {
            return Err(format!(
                "invalid nonce length: expected {AES_GCM_NONCE_LEN}, got {}",
                nonce.len()
            ));
        }
        let cipher =
            Aes256Gcm::new_from_slice(&self.key).map_err(|e| format!("cipher init: {e}"))?;
        let nonce_bytes: [u8; AES_GCM_NONCE_LEN] = nonce
            .try_into()
            .map_err(|_| format!("invalid nonce length: expected {AES_GCM_NONCE_LEN}"))?;
        let nonce = aes_gcm::Nonce::from(nonce_bytes);
        let compressed = cipher
            .decrypt(&nonce, ciphertext)
            .map_err(|e| format!("decrypt: {e}"))?;
        // 64 MiB decompression cap to prevent zip-bomb DoS
        zstd::bulk::decompress(&compressed, 64 * 1024 * 1024)
            .map_err(|e| format!("zstd decompress: {e}"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const TEST_SALT: &[u8] = &[0xa5; HANDSHAKE_SALT_LEN];

    #[test]
    fn roundtrip() {
        let cipher = Cipher::from_secret_with_salt("test-password", TEST_SALT);
        let plaintext = b"hello world";
        let (nonce, ct) = cipher.encrypt(plaintext).ok().unwrap(); // test-only
        let decrypted = cipher.decrypt(&nonce, &ct).ok().unwrap(); // test-only
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let c1 = Cipher::from_secret_with_salt("key-a", TEST_SALT);
        let c2 = Cipher::from_secret_with_salt("key-b", TEST_SALT);
        let (nonce, ct) = c1.encrypt(b"secret").ok().unwrap(); // test-only
        assert!(c2.decrypt(&nonce, &ct).is_err());
    }

    #[test]
    fn different_salt_yields_different_key() {
        // Same PSK, different per-session salts → completely disjoint keys.
        let salt_a = [0x11; HANDSHAKE_SALT_LEN];
        let salt_b = [0x22; HANDSHAKE_SALT_LEN];
        let c1 = Cipher::from_secret_with_salt("shared-psk", &salt_a);
        let c2 = Cipher::from_secret_with_salt("shared-psk", &salt_b);
        let (nonce, ct) = c1.encrypt(b"session-a payload").ok().unwrap();
        assert!(
            c2.decrypt(&nonce, &ct).is_err(),
            "session-b must not decrypt session-a ciphertext"
        );
    }

    #[test]
    fn validates_secret_strength() {
        assert!(Cipher::validate_secret("short").is_err());
        assert!(Cipher::validate_secret("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").is_err());
        assert!(Cipher::validate_secret("0123456789abcdef0123456789abcdef").is_ok());
    }

    #[test]
    fn bad_nonce_length_fails() {
        let cipher = Cipher::from_secret_with_salt("0123456789abcdef0123456789abcdef", TEST_SALT);
        assert!(cipher.decrypt(&[], b"ciphertext").is_err());
    }
}

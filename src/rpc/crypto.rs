use aes_gcm::aead::{Aead, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, KeyInit};
use sha2::{Digest, Sha256};

pub struct Cipher {
    key: [u8; 32],
}

impl Cipher {
    /// Derive a 32-byte AES-256 key from an arbitrary secret string via SHA-256.
    pub fn from_secret(secret: &str) -> Self {
        let hash = Sha256::digest(secret.as_bytes());
        let mut key = [0u8; 32];
        key.copy_from_slice(&hash);
        Self { key }
    }

    /// Encrypt plaintext: zstd compress → AES-256-GCM encrypt. Returns `(nonce, ciphertext)`.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
        let compressed =
            zstd::bulk::compress(plaintext, 1).map_err(|e| format!("zstd compress: {e}"))?;
        let cipher =
            Aes256Gcm::new_from_slice(&self.key).map_err(|e| format!("cipher init: {e}"))?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, compressed.as_slice())
            .map_err(|e| format!("encrypt: {e}"))?;
        Ok((nonce.to_vec(), ciphertext))
    }

    /// Decrypt ciphertext: AES-256-GCM decrypt → zstd decompress.
    pub fn decrypt(&self, nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        let cipher =
            Aes256Gcm::new_from_slice(&self.key).map_err(|e| format!("cipher init: {e}"))?;
        let nonce = aes_gcm::Nonce::from_slice(nonce);
        let compressed = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| format!("decrypt: {e}"))?;
        // 64 MiB decompression cap to prevent zip-bomb DoS
        zstd::bulk::decompress(&compressed, 64 * 1024 * 1024)
            .map_err(|e| format!("zstd decompress: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let cipher = Cipher::from_secret("test-password");
        let plaintext = b"hello world";
        let (nonce, ct) = cipher.encrypt(plaintext).ok().unwrap(); // test-only
        let decrypted = cipher.decrypt(&nonce, &ct).ok().unwrap(); // test-only
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let c1 = Cipher::from_secret("key-a");
        let c2 = Cipher::from_secret("key-b");
        let (nonce, ct) = c1.encrypt(b"secret").ok().unwrap(); // test-only
        assert!(c2.decrypt(&nonce, &ct).is_err());
    }
}

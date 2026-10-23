//! Encrypted local vault — the fallback where no OS credential service exists
//! (e.g. Linux without Secret Service/KWallet).
//!
//! - Key derivation: Argon2id over a random 32-byte salt.
//! - Encryption: XChaCha20-Poly1305 with a random 24-byte nonce.
//! - The master password is never stored; only the derived key stays in
//!   memory (zeroized) while the vault is unlocked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use argon2::Argon2;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use relay_core::{CredentialRef, RelayError, Result};

use crate::CredentialStore;

const VAULT_AAD: &[u8] = b"relay-vault-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    kdf: String,
    salt: String,
    nonce: String,
    ciphertext: String,
    created_unix: u64,
}

/// Where the sealing key came from, for diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeySource {
    /// Owner-only key file beside the vault (the default).
    ProtectedFile,
    /// Argon2id derivation from a master passphrase.
    Passphrase,
}

pub struct Vault {
    path: PathBuf,
    key: Option<Zeroizing<[u8; 32]>>,
    salt: [u8; 32],
    created_unix: u64,
    /// How the sealing key was obtained; only affects the reported name.
    key_source: KeySource,
}

impl Vault {
    /// Open (locked) — an existing vault file is loaded, a missing one is
    /// created empty on first write.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Vault {
            path: path.into(),
            key: None,
            salt: random_bytes(),
            created_unix: now_unix(),
            key_source: KeySource::Passphrase,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(path: &Path) -> bool {
        path.exists()
    }

    pub fn is_locked(&self) -> bool {
        self.key.is_none()
    }

    /// Lock the vault and zeroize the derived key.
    pub fn lock(&mut self) {
        self.key = None;
    }

    /// Create a brand-new vault protected by `master_password`.
    pub fn create(&mut self, master_password: &str) -> Result<()> {
        if self.path.exists() {
            return Err(RelayError::Credential(
                "vault already exists — unlock it instead".to_string(),
            ));
        }
        self.salt = random_bytes();
        self.key = Some(derive_key(master_password, &self.salt)?);
        self.created_unix = now_unix();
        self.save(&BTreeMap::new())
    }

    /// Unlock with the master password (verifies against the stored file).
    pub fn unlock(&mut self, master_password: &str) -> Result<()> {
        if !self.path.exists() {
            self.salt = random_bytes();
            self.key = Some(derive_key(master_password, &self.salt)?);
            self.created_unix = now_unix();
            return Ok(());
        }
        let file = self.read_file()?;
        self.salt = decode_array::<32>(&file.salt)?;
        self.created_unix = file.created_unix;
        let key = derive_key(master_password, &self.salt)?;
        // Verify by decrypting.
        let entries = decrypt_entries(&key, &file)?;
        self.key = Some(key);
        let _ = entries;
        Ok(())
    }

    /// Open with an already-derived 32-byte key instead of a master password.
    ///
    /// Used when the key itself is random and lives in the OS credential
    /// service: there is no passphrase to derive from, so Argon2 is skipped
    /// and the key encrypts the entries directly. An existing vault is
    /// verified by decrypting it, so a replaced key is reported instead of
    /// silently writing a second, unreadable vault.
    pub fn open_with_key(path: impl Into<PathBuf>, key: [u8; 32]) -> Result<Self> {
        let path = path.into();
        let mut vault = Vault {
            path: path.clone(),
            key: Some(Zeroizing::new(key)),
            salt: random_bytes(),
            created_unix: now_unix(),
            key_source: KeySource::ProtectedFile,
        };
        if path.exists() {
            let file = vault.read_file()?;
            vault.salt = decode_array::<32>(&file.salt)?;
            vault.created_unix = file.created_unix;
            decrypt_entries(&key, &file)?;
        }
        Ok(vault)
    }

    fn read_file(&self) -> Result<VaultFile> {
        let text =
            std::fs::read_to_string(&self.path).map_err(|e| RelayError::io(&self.path, e))?;
        serde_json::from_str(&text).map_err(|e| RelayError::parse(&self.path, e.to_string()))
    }

    fn save(&self, entries: &BTreeMap<String, String>) -> Result<()> {
        let key = self.key.as_ref().ok_or(RelayError::VaultLocked)?;
        let plaintext =
            serde_json::to_vec(entries).map_err(|e| RelayError::other(e.to_string()))?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(key.as_ref()));
        let nonce_bytes = random_bytes::<24>();
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce_bytes),
                Payload {
                    msg: &plaintext,
                    aad: VAULT_AAD,
                },
            )
            .map_err(|_| RelayError::Credential("vault encryption failed".to_string()))?;
        let file = VaultFile {
            version: 1,
            kdf: "argon2id".to_string(),
            salt: BASE64.encode(self.salt),
            nonce: BASE64.encode(nonce_bytes),
            ciphertext: BASE64.encode(&ciphertext),
            created_unix: self.created_unix,
        };
        let text =
            serde_json::to_string_pretty(&file).map_err(|e| RelayError::other(e.to_string()))?;
        relay_config::write_file(&self.path, &text)?;
        // Ciphertext only, but no other account needs to read it either.
        let _ = relay_config::perms::owner_only(&self.path, false);
        Ok(())
    }

    fn entries(&self) -> Result<BTreeMap<String, String>> {
        if self.key.is_none() {
            return Err(RelayError::VaultLocked);
        }
        if !self.path.exists() {
            return Ok(BTreeMap::new());
        }
        let file = self.read_file()?;
        decrypt_entries(self.key.as_ref().unwrap(), &file)
    }

    fn write_entries(&self, entries: &BTreeMap<String, String>) -> Result<()> {
        self.save(entries)
    }
}

fn decrypt_entries(key: &[u8; 32], file: &VaultFile) -> Result<BTreeMap<String, String>> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = decode_array::<24>(&file.nonce)?;
    let ciphertext = BASE64
        .decode(&file.ciphertext)
        .map_err(|e| RelayError::Credential(e.to_string()))?;
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: VAULT_AAD,
            },
        )
        .map_err(|_| {
            RelayError::Credential("wrong master password or corrupted vault".to_string())
        })?;
    serde_json::from_slice(&plaintext).map_err(|e| RelayError::Credential(e.to_string()))
}

fn derive_key(master_password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::default()
        .hash_password_into(master_password.as_bytes(), salt, key.as_mut())
        .map_err(|e| RelayError::Credential(e.to_string()))?;
    Ok(key)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes
}

fn decode_array<const N: usize>(encoded: &str) -> Result<[u8; N]> {
    let bytes = BASE64
        .decode(encoded)
        .map_err(|e| RelayError::Credential(e.to_string()))?;
    let bytes: [u8; N] = bytes
        .try_into()
        .map_err(|_| RelayError::Credential("vault field has wrong length".to_string()))?;
    Ok(bytes)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl CredentialStore for Vault {
    fn name(&self) -> &'static str {
        match self.key_source {
            KeySource::ProtectedFile => "vault (encrypted, local key)",
            KeySource::Passphrase => "vault (passphrase)",
        }
    }

    fn get(&self, cred: &CredentialRef) -> Result<Option<String>> {
        Ok(self.entries()?.get(&cred.account).cloned())
    }

    fn set(&self, cred: &CredentialRef, secret: &str) -> Result<()> {
        let mut entries = self.entries()?;
        entries.insert(cred.account.clone(), secret.to_string());
        self.write_entries(&entries)
    }

    fn delete(&self, cred: &CredentialRef) -> Result<bool> {
        let mut entries = self.entries()?;
        let removed = entries.remove(&cred.account).is_some();
        if removed {
            self.write_entries(&entries)?;
        }
        Ok(removed)
    }

    fn list(&self) -> Result<Vec<CredentialRef>> {
        Ok(self
            .entries()?
            .keys()
            .map(|account| CredentialRef {
                service: "relay".to_string(),
                account: account.clone(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(account: &str) -> CredentialRef {
        CredentialRef {
            service: "relay".to_string(),
            account: account.to_string(),
        }
    }

    #[test]
    fn vault_round_trip_with_wrong_password_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.json");
        let mut vault = Vault::open(&path);
        vault.create("correct horse").unwrap();
        vault.set(&cred("provider:openai"), "sk-test-1234").unwrap();
        assert_eq!(
            vault.get(&cred("provider:openai")).unwrap().as_deref(),
            Some("sk-test-1234")
        );

        // Locked vault refuses access.
        vault.lock();
        assert!(matches!(
            vault.get(&cred("provider:openai")),
            Err(RelayError::VaultLocked)
        ));

        // Reopen with the right password.
        let mut reopened = Vault::open(&path);
        reopened.unlock("correct horse").unwrap();
        assert_eq!(
            reopened.get(&cred("provider:openai")).unwrap().as_deref(),
            Some("sk-test-1234")
        );

        // Wrong password fails.
        let mut wrong = Vault::open(&path);
        assert!(wrong.unlock("wrong").is_err());
    }

    #[test]
    fn vault_delete_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let mut vault = Vault::open(dir.path().join("vault.json"));
        vault.create("pw").unwrap();
        vault.set(&cred("a"), "1").unwrap();
        vault.set(&cred("b"), "2").unwrap();
        assert_eq!(vault.list().unwrap().len(), 2);
        assert!(vault.delete(&cred("a")).unwrap());
        assert_eq!(vault.list().unwrap().len(), 1);
    }

    #[test]
    fn os_key_vault_round_trip_and_wrong_key_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.json");
        let key = [7u8; 32];
        let vault = Vault::open_with_key(&path, key).unwrap();
        vault.set(&cred("provider:evren"), "sk-live-xyz").unwrap();

        // A second process reopens with the same OS-held key.
        let reopened = Vault::open_with_key(&path, key).unwrap();
        assert_eq!(
            reopened.get(&cred("provider:evren")).unwrap().as_deref(),
            Some("sk-live-xyz")
        );
        assert_eq!(reopened.name(), "vault (encrypted, local key)");

        // A different key must not silently produce a second vault.
        assert!(Vault::open_with_key(&path, [9u8; 32]).is_err());
        // The file on disk holds no plaintext secret.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("sk-live-xyz"));
    }

    #[test]
    fn ciphertext_does_not_contain_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vault.json");
        let mut vault = Vault::open(&path);
        vault.create("correct horse battery staple").unwrap();
        vault.set(&cred("k"), "super-secret-value").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("super-secret-value"));
        // The passphrase is never written either. Base64 output can contain a
        // short substring like "pw" by chance, so compare against the whole
        // passphrase rather than a fragment of it.
        assert!(!text.contains("correct horse battery staple"));
        let stored: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(stored.get("ciphertext").is_some());
        assert_eq!(stored["kdf"], "argon2id");
    }
}

//! Credential storage for Turnout.
//!
//! Platform order:
//! - macOS   → Keychain
//! - Windows → Credential Manager
//! - Linux   → Secret Service / KWallet, else the encrypted local [`Vault`]
//!
//! Secrets are never written to config files or logs; only [`CredentialRef`]
//! (a service + account name) travels through the rest of the tool. The
//! service id itself is the internal storage key `relay`; it must not change
//! or existing credentials files would no longer be found.

pub mod file;
pub mod vault;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use zeroize::Zeroizing;

use relay_core::{CredentialRef, RelayError, Result};

pub use file::FileStore;
pub use vault::Vault;

/// A secret held in memory. Zeroized on drop; `Debug`/`Display` never print it.
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(Zeroizing::new(value.into()))
    }

    /// Reveal the secret. Call sites should keep the borrow as short as possible.
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

/// Storage backend for provider API keys.
pub trait CredentialStore: Send + Sync {
    /// Short backend name for diagnostics (`keychain`, `vault`, `memory`, …).
    fn name(&self) -> &'static str;

    fn get(&self, cred: &CredentialRef) -> Result<Option<String>>;

    fn set(&self, cred: &CredentialRef, secret: &str) -> Result<()>;

    /// Returns whether a secret was removed.
    fn delete(&self, cred: &CredentialRef) -> Result<bool>;

    /// Enumerate stored references. OS keyrings cannot enumerate, so they
    /// return an empty list; callers track provider ids themselves.
    fn list(&self) -> Result<Vec<CredentialRef>> {
        Ok(Vec::new())
    }

    fn has(&self, cred: &CredentialRef) -> Result<bool> {
        Ok(self.get(cred)?.is_some())
    }

    fn get_secret(&self, cred: &CredentialRef) -> Result<Option<Secret>> {
        Ok(self.get(cred)?.map(Secret::new))
    }
}

/// In-memory store for tests and dry runs.
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<BTreeMap<(String, String), String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryStore {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn get(&self, cred: &CredentialRef) -> Result<Option<String>> {
        let entries = self.entries.lock().map_err(poisoned)?;
        Ok(entries
            .get(&(cred.service.clone(), cred.account.clone()))
            .cloned())
    }

    fn set(&self, cred: &CredentialRef, secret: &str) -> Result<()> {
        let mut entries = self.entries.lock().map_err(poisoned)?;
        entries.insert(
            (cred.service.clone(), cred.account.clone()),
            secret.to_string(),
        );
        Ok(())
    }

    fn delete(&self, cred: &CredentialRef) -> Result<bool> {
        let mut entries = self.entries.lock().map_err(poisoned)?;
        Ok(entries
            .remove(&(cred.service.clone(), cred.account.clone()))
            .is_some())
    }

    fn list(&self) -> Result<Vec<CredentialRef>> {
        let entries = self.entries.lock().map_err(poisoned)?;
        Ok(entries
            .keys()
            .map(|(service, account)| CredentialRef {
                service: service.clone(),
                account: account.clone(),
            })
            .collect())
    }
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> RelayError {
    RelayError::Credential("credential store lock poisoned".to_string())
}

/// The operating system credential service (Keychain, Credential Manager,
/// Secret Service).
pub struct OsKeyringStore;

impl OsKeyringStore {
    pub fn new() -> Self {
        OsKeyringStore
    }

    /// Whether the platform credential service is usable right now. A missing
    /// entry counts as available; only platform/storage failures do not.
    pub fn is_available() -> bool {
        match keyring::Entry::new("relay-probe", "availability") {
            Ok(entry) => matches!(entry.get_password(), Ok(_) | Err(keyring::Error::NoEntry)),
            Err(_) => false,
        }
    }

    fn entry(cred: &CredentialRef) -> Result<keyring::Entry> {
        keyring::Entry::new(&cred.service, &cred.account).map_err(to_error)
    }
}

impl Default for OsKeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

fn to_error(error: keyring::Error) -> RelayError {
    RelayError::Credential(error.to_string())
}

impl CredentialStore for OsKeyringStore {
    fn name(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "keychain"
        } else if cfg!(target_os = "windows") {
            "credential-manager"
        } else {
            "secret-service"
        }
    }

    fn get(&self, cred: &CredentialRef) -> Result<Option<String>> {
        match Self::entry(cred)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(to_error(error)),
        }
    }

    fn set(&self, cred: &CredentialRef, secret: &str) -> Result<()> {
        Self::entry(cred)?.set_password(secret).map_err(to_error)
    }

    fn delete(&self, cred: &CredentialRef) -> Result<bool> {
        match Self::entry(cred)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(error) => Err(to_error(error)),
        }
    }
}

/// Which backend [`open_default`] selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Os,
    Vault,
}

/// Account name of the single data key that encrypts the default vault.
const DATA_KEY_ACCOUNT: &str = "vault:data-key";
/// The data key is stored under its own service so it never collides with a
/// provider entry in the OS credential service.
const DATA_KEY_SERVICE: &str = "turnout-vault";

/// Read (or create) the key that seals `vault.json`.
///
/// Default location is `key_path`, an owner-only (`0600`) file next to the
/// vault. That is deliberate: macOS ties a Keychain item to the requesting
/// binary's code signature, and every rebuild of `turnout` produces a new
/// signature, so an unsigned or ad-hoc-signed build is asked for permission on
/// *every* build. The GUI spawns the CLI per request, so that prompt storm made
/// the tool unusable. A `0600` file in the user's profile has no such problem.
///
/// `TURNOUT_CREDENTIALS=keyring` still stores the key in the OS credential
/// service for users who prefer it; the file is used when it exists so an
/// existing install never prompts again.
fn data_key_at(key_path: &Path) -> Result<[u8; 32]> {
    if let Some(key) = read_key_file(key_path)? {
        return Ok(key);
    }
    let lock_path = PathBuf::from(format!("{}.key-lock", key_path.display()));
    let _guard = CreationLock::acquire(lock_path)?;
    // Another process may have created the key while we waited.
    if let Some(key) = read_key_file(key_path)? {
        return Ok(key);
    }
    let key = random_key();
    write_key_file(key_path, &key)?;
    Ok(key)
}

fn read_key_file(path: &Path) -> Result<Option<[u8; 32]>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(RelayError::io(path, error)),
    };
    let bytes = BASE64
        .decode(text.trim())
        .map_err(|_| RelayError::Credential("the vault key file is not valid base64".into()))?;
    bytes
        .try_into()
        .map(Some)
        .map_err(|_| RelayError::Credential("the vault key file has the wrong length".into()))
}

fn write_key_file(path: &Path, key: &[u8; 32]) -> Result<()> {
    relay_config::write_file(path, &format!("{}\n", BASE64.encode(key)))?;
    // Owner-only from the first byte; on Windows this applies the profile ACL.
    relay_config::perms::owner_only(path, false)?;
    Ok(())
}

/// Cross-process lock held while the data key is created.
///
/// The desktop UI starts several CLI processes at once. Without this, two of
/// them on a fresh install could each create a key; the one that lost the race
/// would seal `vault.json` with a key that no longer exists.
struct CreationLock(PathBuf);

impl CreationLock {
    fn acquire(path: PathBuf) -> Result<Self> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(CreationLock(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    // A crashed holder leaves the file behind; creating a key
                    // takes milliseconds, so an old lock is stale.
                    let stale = std::fs::metadata(&path)
                        .and_then(|meta| meta.modified())
                        .ok()
                        .and_then(|time| time.elapsed().ok())
                        .is_some_and(|age| age > std::time::Duration::from_secs(30));
                    if stale {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(RelayError::Credential(format!(
                            "timed out waiting for {}",
                            path.display()
                        )));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(error) => {
                    return Err(RelayError::Credential(format!(
                        "cannot create {}: {error}",
                        path.display()
                    )))
                }
            }
        }
    }
}

impl Drop for CreationLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn random_key() -> [u8; 32] {
    use rand::RngCore;
    let mut key = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    key
}

/// Key held in the OS credential service (opt-in).
fn data_key_from_os() -> Result<[u8; 32]> {
    let entry = keyring::Entry::new(DATA_KEY_SERVICE, DATA_KEY_ACCOUNT).map_err(to_error)?;
    match read_data_key(&entry)? {
        Some(key) => Ok(key),
        None => {
            let key = random_key();
            entry
                .set_password(&BASE64.encode(key))
                .map_err(to_error)?;
            read_data_key(&entry)?.ok_or_else(|| {
                RelayError::Credential("the vault key was not persisted".into())
            })
        }
    }
}

fn read_data_key(entry: &keyring::Entry) -> Result<Option<[u8; 32]>> {
    match entry.get_password() {
        Ok(encoded) => {
            let bytes = BASE64
                .decode(encoded.trim())
                .map_err(|_| RelayError::Credential("stored vault key is not valid base64".into()))?;
            let key = bytes.try_into().map_err(|_| {
                RelayError::Credential("stored vault key has the wrong length".into())
            })?;
            Ok(Some(key))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(to_error(error)),
    }
}

/// Open the default store: `vault.json` sealed with the local data key.
///
/// `key_path` is where the `0600` data-key file lives; `use_os_store` keeps the
/// key in the OS credential service instead (opt-in).
pub fn open_encrypted(
    vault_path: impl Into<PathBuf>,
    key_path: impl Into<PathBuf>,
    use_os_store: bool,
) -> Result<Box<dyn CredentialStore>> {
    let path = vault_path.into();
    let key_path = key_path.into();
    let key = if use_os_store {
        data_key_from_os()?
    } else if let Some(key) = read_key_file(&key_path)? {
        key
    } else if path.exists() {
        // An existing vault was sealed with a key from an older build that kept
        // it in the OS credential service. Recover it once and persist it to the
        // file, so the upgrade neither loses keys nor prompts again.
        let key = data_key_from_os().map_err(|error| {
            RelayError::Credential(format!(
                "{} exists but its key could not be recovered ({error});                  set TURNOUT_CREDENTIALS=keyring to keep using the OS-held key",
                path.display()
            ))
        })?;
        write_key_file(&key_path, &key)?;
        key
    } else {
        data_key_at(&key_path)?
    };
    Ok(Box::new(Vault::open_with_key(path, key)?))
}

/// Open the best available store.
///
/// The OS credential service wins when it works. Otherwise the encrypted
/// vault at `vault_path` is returned **locked**; the caller must prompt for the
/// master password and call [`Vault::unlock`] or [`Vault::create`].
pub fn open_default(
    vault_path: impl Into<PathBuf>,
) -> (Backend, Box<dyn CredentialStore>, Option<Vault>) {
    if OsKeyringStore::is_available() {
        return (Backend::Os, Box::new(OsKeyringStore::new()), None);
    }
    let vault = Vault::open(vault_path);
    (Backend::Vault, Box::new(MemoryStore::new()), Some(vault))
}

/// Default vault location: `<data dir>/Turnout/vault.json` (legacy
/// `Relay/vault.json` is migrated once at startup by the CLI).
pub fn default_vault_path() -> Result<PathBuf> {
    relay_core::brand::default_data_dir()
        .map(|dir| dir.join("vault.json"))
        .ok_or_else(|| RelayError::other("cannot locate a writable data directory"))
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
    fn creation_lock_admits_one_holder_at_a_time() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("vault.json.key-lock");
        let inside = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(AtomicUsize::new(0));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let (lock, inside, entered) = (lock.clone(), inside.clone(), entered.clone());
                std::thread::spawn(move || {
                    let _guard = CreationLock::acquire(lock).unwrap();
                    // Two holders at once is exactly the race that split the key.
                    assert_eq!(inside.fetch_add(1, Ordering::SeqCst), 0);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    inside.fetch_sub(1, Ordering::SeqCst);
                    entered.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(entered.load(Ordering::SeqCst), 16);
        assert!(!lock.exists(), "the lock file is removed on release");
    }

    #[test]
    fn stale_creation_lock_is_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("vault.json.key-lock");
        std::fs::write(&lock, "").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
        std::fs::File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(old)
            .unwrap();
        // A crashed holder must not block key creation forever.
        let started = std::time::Instant::now();
        let _guard = CreationLock::acquire(lock).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn secret_never_prints() {
        let secret = Secret::new("sk-live-abc");
        assert_eq!(format!("{secret:?}"), "Secret(***)");
        assert_eq!(format!("{secret}"), "***");
        assert_eq!(secret.expose(), "sk-live-abc");
    }

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryStore::new();
        assert!(!store.has(&cred("a")).unwrap());
        store.set(&cred("a"), "1").unwrap();
        assert_eq!(store.get(&cred("a")).unwrap().as_deref(), Some("1"));
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(store.delete(&cred("a")).unwrap());
        assert!(!store.delete(&cred("a")).unwrap());
    }
}

//! Credentials at rest (§18.7, §18.10): sealed with a per-install key.
//!
//! - **The key file** is `secrets.key` in the data directory, outside the
//!   database, so a copy of the database alone (a backup, a support bundle)
//!   doesn't reveal credentials.
//!   - **Unix:** created with mode `0600`. An existing file readable by
//!     group or others is tightened back to `0600`, with a warning.
//!   - **Windows:** the ACL is replaced, without inheritance, by full
//!     control for the file's owner (`OWNER RIGHTS`), `SYSTEM` and
//!     `Administrators`. It's applied while the file is still empty, and
//!     again every time the key is read.
//! - **Each secret** is sealed with XChaCha20-Poly1305 under a fresh random
//!   192-bit nonce. The associated data is the sealed header plus the
//!   caller's `context` (for example `"client:3:password"`), so a sealed
//!   value copied onto another record doesn't open there.
//! - **Rotation** keeps the file holding every key a stored record may use:
//!   1. [`SecretStore::rotate`] adds a new primary key.
//!   2. The caller re-seals its records with [`SecretStore::reseal`] and
//!      commits them in its own transaction.
//!   3. Only then does it call [`SecretStore::retire_old_keys`].
//!
//!   A crash between any two steps loses nothing: old records still open
//!   with an old key, and new ones with the new key.
//! - **[`Secret`]** never prints: its `Debug` is `Secret(***)`, it has no
//!   `Display` and no `Serialize`, and its memory is zeroed on drop.
//!
//! Formats (both versioned by their first bytes):
//! - **key file:** `b"LSK1"`, a count byte, then per key a little-endian
//!   `u32` id and the 32-byte key. The last key is the primary.
//! - **sealed secret:** `0x01`, the sealing key's `u32` id (little-endian),
//!   the 24-byte nonce, then the ciphertext and its 16-byte tag.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::{Zeroize, Zeroizing};

/// A credential or token in memory. It can't be printed by accident.
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(Zeroizing::new(value.into()))
    }

    /// The value, for the one place that has to send it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Secret {
        Secret::new(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Secret {
        Secret::new(value)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

const KEY_MAGIC: &[u8; 4] = b"LSK1";
const KEY_LEN: usize = 32;
const KEY_ENTRY: usize = 4 + KEY_LEN;
const MAX_KEYS: usize = 255;
const SEAL_VERSION: u8 = 1;
const HEADER_LEN: usize = 1 + 4;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;

/// A sealed secret, as stored in the database.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedSecret(Vec<u8>);

impl SealedSecret {
    /// Wraps stored bytes, checking only the version and the length; the
    /// tag is checked by [`SecretStore::open`].
    pub fn from_bytes(bytes: Vec<u8>) -> Result<SealedSecret, SecretError> {
        if bytes.len() < HEADER_LEN + NONCE_LEN + TAG_LEN || bytes[0] != SEAL_VERSION {
            return Err(SecretError::Malformed);
        }
        Ok(SealedSecret(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The id of the key that sealed it.
    pub fn key_id(&self) -> u32 {
        u32::from_le_bytes([self.0[1], self.0[2], self.0[3], self.0[4]])
    }
}

impl fmt::Debug for SealedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "SealedSecret(key {}, {} bytes)",
            self.key_id(),
            self.0.len()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// Reading, writing or protecting the key file failed.
    Io {
        op: &'static str,
        path: PathBuf,
        kind: io::ErrorKind,
    },
    /// The key file isn't a Lodestar key file.
    BadKeyFile(PathBuf),
    /// The operating system had no randomness to give.
    Random,
    /// Encryption itself failed (it can't for valid input; kept distinct
    /// from [`SecretError::Random`] so a report names the right cause).
    Seal,
    /// The stored bytes aren't a sealed secret of a known version.
    Malformed,
    /// No key with the sealed secret's id: it was retired, or the key file
    /// was replaced.
    UnknownKey(u32),
    /// Wrong key, wrong context, or the bytes were altered.
    Open,
    /// The key file already holds 255 keys; retire old ones first.
    TooManyKeys,
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretError::Io { op, path, kind } => {
                write!(
                    f,
                    "couldn't {op} the secrets key {}: {kind}",
                    path.display()
                )
            }
            SecretError::BadKeyFile(path) => {
                write!(f, "{} isn't a Lodestar secrets key", path.display())
            }
            SecretError::Random => f.write_str("the system has no randomness available"),
            SecretError::Seal => f.write_str("sealing the secret failed"),
            SecretError::Malformed => f.write_str("the stored secret is damaged"),
            SecretError::UnknownKey(id) => write!(
                f,
                "the stored secret was sealed with key {id}, which this install no longer has; enter it again"
            ),
            SecretError::Open => f.write_str(
                "the stored secret doesn't open with this install's key; enter it again",
            ),
            SecretError::TooManyKeys => {
                f.write_str("the secrets key file is full; retire old keys first")
            }
        }
    }
}

impl std::error::Error for SecretError {}

struct Key {
    id: u32,
    bytes: Zeroizing<[u8; KEY_LEN]>,
    cipher: XChaCha20Poly1305,
}

impl Key {
    fn new(id: u32, bytes: &[u8; KEY_LEN]) -> Key {
        Key {
            id,
            bytes: Zeroizing::new(*bytes),
            cipher: XChaCha20Poly1305::new(bytes.into()),
        }
    }

    fn random(id: u32) -> Result<Key, SecretError> {
        let mut bytes = Zeroizing::new([0u8; KEY_LEN]);
        getrandom::fill(&mut bytes[..]).map_err(|_| SecretError::Random)?;
        Ok(Key::new(id, &bytes))
    }
}

/// Seals and opens credentials with the install's keys.
pub struct SecretStore {
    /// Oldest first; the last is the primary, used for sealing.
    keys: Vec<Key>,
    /// The key file, or `None` for a store made with [`SecretStore::from_key`].
    path: Option<PathBuf>,
}

impl fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretStore({} keys)", self.keys.len())
    }
}

impl SecretStore {
    /// The key file's name inside the data directory.
    pub const KEY_FILE: &'static str = "secrets.key";

    /// Opens the install's keys in `data_dir`, creating the key file (and
    /// the directory) on first use.
    pub fn open_or_create(data_dir: &Path) -> Result<SecretStore, SecretError> {
        let path = data_dir.join(Self::KEY_FILE);
        match read_keys(&path) {
            Ok(keys) => {
                return Ok(SecretStore {
                    keys,
                    path: Some(path),
                });
            }
            Err(SecretError::Io {
                kind: io::ErrorKind::NotFound,
                ..
            }) => {}
            Err(e) => return Err(e),
        }
        let io_err = |op, e: io::Error| SecretError::Io {
            op,
            path: path.clone(),
            kind: e.kind(),
        };
        fs::create_dir_all(data_dir).map_err(|e| io_err("create the directory of", e))?;
        let keys = vec![Key::random(0)?];

        // Link a finished private file into place. Linking fails if the key
        // already exists, so two processes starting at once agree on one
        // key instead of overwriting each other's.
        let tmp = write_temp(&path, &keys)?;
        let linked = fs::hard_link(&tmp, &path).map_err(|e| io_err("create", e));
        let _ = fs::remove_file(&tmp);
        match linked {
            Ok(()) => {
                sync_dir(data_dir);
                Ok(SecretStore {
                    keys,
                    path: Some(path),
                })
            }
            Err(SecretError::Io {
                kind: io::ErrorKind::AlreadyExists,
                ..
            }) => SecretStore::open_or_create(data_dir),
            Err(e) => Err(e),
        }
    }

    /// A store over one known key, with id 0 and no key file (workers
    /// receive sealed credentials, §19.6; tests).
    pub fn from_key(key: &[u8; KEY_LEN]) -> SecretStore {
        SecretStore {
            keys: vec![Key::new(0, key)],
            path: None,
        }
    }

    fn primary(&self) -> &Key {
        self.keys.last().expect("a store always has a key")
    }

    /// The primary key's id: what [`seal`](Self::seal) uses.
    pub fn primary_key_id(&self) -> u32 {
        self.primary().id
    }

    /// How many keys the store holds (more than one during a rotation).
    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    /// Seals `secret` for the record named by `context`, with the primary
    /// key.
    pub fn seal(&self, context: &str, secret: &Secret) -> Result<SealedSecret, SecretError> {
        let key = self.primary();
        let mut out = Vec::with_capacity(HEADER_LEN + NONCE_LEN + secret.expose().len() + TAG_LEN);
        out.push(SEAL_VERSION);
        out.extend_from_slice(&key.id.to_le_bytes());
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| SecretError::Random)?;
        let aad = associated_data(&out, context);
        let ct = key
            .cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: secret.expose().as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| SecretError::Seal)?;
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(SealedSecret(out))
    }

    /// Opens a secret sealed for the same `context`, with whichever of the
    /// store's keys sealed it.
    pub fn open(&self, context: &str, sealed: &SealedSecret) -> Result<Secret, SecretError> {
        let id = sealed.key_id();
        let key = self
            .keys
            .iter()
            .find(|k| k.id == id)
            .ok_or(SecretError::UnknownKey(id))?;
        let (header, rest) = sealed.0.split_at(HEADER_LEN);
        let (nonce, ct) = rest.split_at(NONCE_LEN);
        let aad = associated_data(header, context);
        let mut pt = key
            .cipher
            .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad: &aad })
            .map_err(|_| SecretError::Open)?;
        let text = String::from_utf8(std::mem::take(&mut pt));
        pt.zeroize();
        match text {
            Ok(s) => Ok(Secret::new(s)),
            Err(e) => {
                e.into_bytes().zeroize();
                Err(SecretError::Malformed)
            }
        }
    }

    /// Whether `sealed` was sealed with a key other than the primary.
    pub fn needs_reseal(&self, sealed: &SealedSecret) -> bool {
        sealed.key_id() != self.primary_key_id()
    }

    /// Opens `sealed` and seals it again with the primary key.
    pub fn reseal(
        &self,
        context: &str,
        sealed: &SealedSecret,
    ) -> Result<SealedSecret, SecretError> {
        self.seal(context, &self.open(context, sealed)?)
    }

    /// Rotation, step 1: adds a fresh primary key and saves the file with
    /// every old key still in it, so records sealed before keep opening.
    /// The file is replaced atomically.
    ///
    /// One process at a time: the coordinator owns the key file.
    pub fn rotate(&mut self) -> Result<u32, SecretError> {
        if self.keys.len() >= MAX_KEYS {
            return Err(SecretError::TooManyKeys);
        }
        let id = self
            .keys
            .iter()
            .map(|k| k.id)
            .max()
            .unwrap_or(0)
            .wrapping_add(1);
        let mut keys: Vec<Key> = self.keys.iter().map(|k| Key::new(k.id, &k.bytes)).collect();
        keys.push(Key::random(id)?);
        self.save(&keys)?;
        self.keys = keys;
        Ok(id)
    }

    /// Rotation, step 3: drops every key but the primary from the file.
    /// Call it only once every stored record has been re-sealed **and
    /// committed**; a record still sealed with a retired key no longer
    /// opens ([`SecretError::UnknownKey`]). Returns how many keys were
    /// dropped.
    pub fn retire_old_keys(&mut self) -> Result<usize, SecretError> {
        let dropped = self.keys.len() - 1;
        if dropped == 0 {
            return Ok(0);
        }
        let primary = self.primary();
        let keys = vec![Key::new(primary.id, &primary.bytes)];
        self.save(&keys)?;
        self.keys = keys;
        Ok(dropped)
    }

    /// Replaces the key file with `keys` (a no-op without a file).
    fn save(&self, keys: &[Key]) -> Result<(), SecretError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let tmp = write_temp(path, keys)?;
        let renamed = fs::rename(&tmp, path).map_err(|e| SecretError::Io {
            op: "replace",
            path: path.clone(),
            kind: e.kind(),
        });
        if renamed.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        renamed?;
        if let Some(dir) = path.parent() {
            sync_dir(dir);
        }
        Ok(())
    }
}

/// The sealed header, then the caller's context.
fn associated_data(header: &[u8], context: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(header.len() + context.len());
    aad.extend_from_slice(header);
    aad.extend_from_slice(context.as_bytes());
    aad
}

fn read_keys(path: &Path) -> Result<Vec<Key>, SecretError> {
    let io_err = |op, e: io::Error| SecretError::Io {
        op,
        path: path.to_path_buf(),
        kind: e.kind(),
    };
    let mut file = File::open(path).map_err(|e| io_err("read", e))?;
    tighten_permissions(path, &file).map_err(|e| io_err("restrict access to", e))?;
    let limit = KEY_MAGIC.len() + 1 + MAX_KEYS * KEY_ENTRY + 1;
    let mut bytes = Zeroizing::new(Vec::new());
    (&mut file)
        .take(limit as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| io_err("read", e))?;
    let bad = || SecretError::BadKeyFile(path.to_path_buf());
    let rest = bytes.strip_prefix(KEY_MAGIC).ok_or_else(bad)?;
    let (&count, entries) = rest.split_first().ok_or_else(bad)?;
    if count == 0 || entries.len() != usize::from(count) * KEY_ENTRY {
        return Err(bad());
    }
    let mut keys: Vec<Key> = Vec::with_capacity(count.into());
    for entry in entries.chunks_exact(KEY_ENTRY) {
        let id = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]);
        if keys.iter().any(|k| k.id == id) {
            return Err(bad());
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        key.copy_from_slice(&entry[4..]);
        keys.push(Key::new(id, &key));
    }
    Ok(keys)
}

/// Writes `keys` to a new private file next to `path` and returns its
/// name. The file is created empty, restricted, and only then written, so
/// the key bytes never sit in a file anyone else can read.
fn write_temp(path: &Path, keys: &[Key]) -> Result<PathBuf, SecretError> {
    let mut tag = [0u8; 8];
    getrandom::fill(&mut tag).map_err(|_| SecretError::Random)?;
    let name = format!(
        ".{}.{:016x}.tmp",
        SecretStore::KEY_FILE,
        u64::from_le_bytes(tag)
    );
    let tmp = path.with_file_name(name);
    let io_err = |op, e: io::Error| SecretError::Io {
        op,
        path: path.to_path_buf(),
        kind: e.kind(),
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&tmp).map_err(|e| io_err("write", e))?;
        restrict_acl(&tmp).map_err(|e| io_err("restrict access to", e))?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(
            KEY_MAGIC.len() + 1 + keys.len() * KEY_ENTRY,
        ));
        bytes.extend_from_slice(KEY_MAGIC);
        bytes.push(keys.len() as u8);
        for key in keys {
            bytes.extend_from_slice(&key.id.to_le_bytes());
            bytes.extend_from_slice(&key.bytes[..]);
        }
        file.write_all(&bytes).map_err(|e| io_err("write", e))?;
        file.sync_all().map_err(|e| io_err("write", e))
    })();
    match result {
        Ok(()) => Ok(tmp),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(unix)]
fn tighten_permissions(path: &Path, file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = file.metadata()?.permissions().mode();
    // Only a loose file is changed, so a restrictive key on a read-only
    // mount is never touched. A loose one that can't be tightened is an
    // error: the key is exposed.
    if mode & 0o077 != 0 {
        tracing::warn!(
            path = %path.display(),
            mode = format_args!("{:o}", mode & 0o777),
            "the secrets key was readable by other users; restricting it to 0600"
        );
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Re-applies the restricted ACL on every read: reading an ACL back
/// without `unsafe` means parsing localised `icacls` output, so it's
/// enforced instead. A failure (a read-only volume, a file owned by
/// another account) is logged, not fatal: the key is still the only one
/// this install has.
#[cfg(windows)]
fn tighten_permissions(path: &Path, _file: &File) -> io::Result<()> {
    if let Err(e) = restrict_acl(path) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "couldn't re-apply the secrets key's restricted ACL"
        );
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn tighten_permissions(_path: &Path, _file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn restrict_acl(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Replaces the file's ACL with full control for its owner, `SYSTEM` and
/// `Administrators`, dropping inherited entries (`Users`, `Everyone`).
/// `icacls` ships with every supported Windows; calling it keeps this crate
/// free of `unsafe` Win32 ACL code. It's run by its absolute path, so a
/// planted `icacls.exe` in the working directory or `PATH` isn't used.
#[cfg(windows)]
fn restrict_acl(path: &Path) -> io::Result<()> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let icacls = Path::new(&root).join("System32").join("icacls.exe");
    let status = std::process::Command::new(icacls)
        .arg(path)
        .args([
            "/inheritance:r",
            "/grant:r",
            "*S-1-3-4:F",
            "*S-1-5-18:F",
            "*S-1-5-32-544:F",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "icacls failed",
        ))
    }
}

/// Makes the new directory entry durable (best effort; Unix only).
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_and_open_round_trip_with_context() {
        let store = SecretStore::from_key(&[9; 32]);
        let secret = Secret::new("hunter2-correct-horse");
        let a = store.seal("client:1:password", &secret).unwrap();
        let b = store.seal("client:1:password", &secret).unwrap();
        assert_ne!(a, b, "a fresh nonce each time");
        assert!(
            !a.as_bytes().windows(7).any(|w| w == b"hunter2"),
            "no plaintext in the sealed bytes"
        );
        assert_eq!(
            store.open("client:1:password", &a).unwrap().expose(),
            "hunter2-correct-horse"
        );
        assert_eq!(
            store.open("client:2:password", &a).unwrap_err(),
            SecretError::Open,
            "a sealed value moved to another record doesn't open"
        );
        let other = SecretStore::from_key(&[8; 32]);
        assert_eq!(
            other.open("client:1:password", &a).unwrap_err(),
            SecretError::Open
        );
        let mut tampered = a.as_bytes().to_vec();
        *tampered.last_mut().unwrap() ^= 1;
        let tampered = SealedSecret::from_bytes(tampered).unwrap();
        assert_eq!(
            store.open("client:1:password", &tampered).unwrap_err(),
            SecretError::Open
        );
    }

    #[test]
    fn malformed_sealed_bytes_are_refused() {
        assert_eq!(
            SealedSecret::from_bytes(vec![1; 10]),
            Err(SecretError::Malformed)
        );
        assert_eq!(
            SealedSecret::from_bytes(vec![2; 64]),
            Err(SecretError::Malformed)
        );
        let empty = SecretStore::from_key(&[1; 32])
            .seal("c", &Secret::new(""))
            .unwrap();
        assert_eq!(empty.as_bytes().len(), HEADER_LEN + NONCE_LEN + TAG_LEN);
        assert!(SealedSecret::from_bytes(empty.as_bytes().to_vec()).is_ok());
    }

    #[test]
    fn secrets_never_print() {
        let s = Secret::new("p@ssw0rd-value");
        let store = SecretStore::from_key(&[3; 32]);
        let sealed = store.seal("ctx", &s).unwrap();
        for text in [
            format!("{s:?}"),
            format!("{:?}", Some(s.clone())),
            format!("{store:?}"),
            format!("{sealed:?}"),
            format!("{:#?}", vec![s.clone()]),
        ] {
            assert!(!text.contains("p@ssw0rd"), "{text}");
        }
        assert_eq!(format!("{s:?}"), "Secret(***)");
    }

    #[test]
    fn the_key_file_is_created_once_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let first = SecretStore::open_or_create(&data).unwrap();
        let sealed = first.seal("ctx", &Secret::new("v")).unwrap();
        let key_path = data.join(SecretStore::KEY_FILE);
        let bytes = fs::read(&key_path).unwrap();
        assert_eq!(bytes.len(), 4 + 1 + KEY_ENTRY);
        assert_eq!(&bytes[..5], b"LSK1\x01");
        let second = SecretStore::open_or_create(&data).unwrap();
        assert_eq!(second.open("ctx", &sealed).unwrap().expose(), "v");
        assert_eq!(fs::read(&key_path).unwrap(), bytes, "not rewritten");
        let leftovers: Vec<_> = fs::read_dir(&data)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("secrets.key")]);
    }

    #[test]
    fn concurrent_first_use_agrees_on_one_key() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_path_buf();
        let stores: Vec<SecretStore> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| s.spawn(|| SecretStore::open_or_create(&data).unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let sealed = stores[0].seal("ctx", &Secret::new("same")).unwrap();
        for store in &stores {
            assert_eq!(store.open("ctx", &sealed).unwrap().expose(), "same");
        }
    }

    #[test]
    fn a_damaged_key_file_is_an_error_not_a_new_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SecretStore::KEY_FILE);
        fs::write(&path, b"LSK1short").unwrap();
        assert_eq!(
            SecretStore::open_or_create(dir.path()).unwrap_err(),
            SecretError::BadKeyFile(path.clone())
        );
        assert_eq!(fs::read(&path).unwrap(), b"LSK1short", "left untouched");
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        SecretStore::open_or_create(dir.path()).unwrap();
        let path = dir.path().join(SecretStore::KEY_FILE);
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // A key copied with loose permissions is tightened on open.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        SecretStore::open_or_create(dir.path()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn the_key_id_is_authenticated() {
        let store = SecretStore::from_key(&[5; 32]);
        let sealed = store.seal("ctx", &Secret::new("v")).unwrap();
        assert_eq!(sealed.key_id(), 0);
        let mut moved = sealed.as_bytes().to_vec();
        moved[1] = 7;
        let moved = SealedSecret::from_bytes(moved).unwrap();
        assert_eq!(
            store.open("ctx", &moved).unwrap_err(),
            SecretError::UnknownKey(7)
        );
    }

    #[test]
    fn rotation_keeps_every_record_openable_at_every_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SecretStore::open_or_create(dir.path()).unwrap();
        let old = store
            .seal("client:1:password", &Secret::new("old-pw"))
            .unwrap();

        // Step 1: a new primary; the old key stays, on disk too.
        let id = store.rotate().unwrap();
        assert_eq!(id, 1);
        assert_eq!(store.key_count(), 2);
        assert!(store.needs_reseal(&old));
        let reopened = SecretStore::open_or_create(dir.path()).unwrap();
        assert_eq!(reopened.primary_key_id(), 1);
        assert_eq!(
            reopened.open("client:1:password", &old).unwrap().expose(),
            "old-pw",
            "a crash after rotating loses nothing"
        );

        // Step 2: re-seal under the new key.
        let new = store.reseal("client:1:password", &old).unwrap();
        assert_eq!(new.key_id(), 1);
        assert!(!store.needs_reseal(&new));
        assert_eq!(
            reopened.open("client:1:password", &new).unwrap().expose(),
            "old-pw"
        );

        // Step 3: retire; only records re-sealed still open.
        assert_eq!(store.retire_old_keys().unwrap(), 1);
        assert_eq!(store.retire_old_keys().unwrap(), 0);
        let after = SecretStore::open_or_create(dir.path()).unwrap();
        assert_eq!(after.key_count(), 1);
        assert_eq!(
            after.open("client:1:password", &new).unwrap().expose(),
            "old-pw"
        );
        assert_eq!(
            after.open("client:1:password", &old).unwrap_err(),
            SecretError::UnknownKey(0)
        );
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("secrets.key")]);
    }

    #[test]
    fn malformed_key_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SecretStore::KEY_FILE);
        let entry = |id: u32| {
            let mut e = id.to_le_bytes().to_vec();
            e.extend_from_slice(&[9; KEY_LEN]);
            e
        };
        let mut duplicate = b"LSK1\x02".to_vec();
        duplicate.extend(entry(3));
        duplicate.extend(entry(3));
        let mut short = b"LSK1\x02".to_vec();
        short.extend(entry(1));
        for bytes in [b"LSK1\x00".to_vec(), b"LSK2\x01".to_vec(), duplicate, short] {
            fs::write(&path, &bytes).unwrap();
            assert_eq!(
                SecretStore::open_or_create(dir.path()).unwrap_err(),
                SecretError::BadKeyFile(path.clone())
            );
        }
        let mut two = b"LSK1\x02".to_vec();
        two.extend(entry(4));
        two.extend(entry(9));
        fs::write(&path, &two).unwrap();
        let store = SecretStore::open_or_create(dir.path()).unwrap();
        assert_eq!(store.primary_key_id(), 9);
        assert_eq!(store.key_count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn rotated_key_files_stay_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut store = SecretStore::open_or_create(dir.path()).unwrap();
        store.rotate().unwrap();
        let path = dir.path().join(SecretStore::KEY_FILE);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

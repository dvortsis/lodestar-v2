//! Credentials at rest (§18.7, §18.10): sealed with a per-install key.
//!
//! - **The key** is 32 random bytes in `secrets.key` in the data directory,
//!   outside the database, so a copy of the database alone (a backup, a
//!   support bundle) doesn't reveal credentials. It's created with mode
//!   `0600` on Unix; on Windows its ACL is replaced, without inheritance, by
//!   full control for the file's owner (`OWNER RIGHTS`), `SYSTEM` and
//!   `Administrators`. An existing key file readable by group or others on
//!   Unix is tightened back to `0600`, with a warning.
//! - **Each secret** is sealed with XChaCha20-Poly1305 under a fresh random
//!   192-bit nonce. The caller's `context` (for example
//!   `"client:3:password"`) is the associated data, so a sealed value copied
//!   onto another record doesn't open there.
//! - **[`Secret`]** never prints: its `Debug` is `Secret(***)`, it has no
//!   `Display` and no `Serialize`, and its memory is zeroed on drop.
//!
//! Formats (both versioned by their first bytes):
//! - key file: `b"LSK1"` then the 32-byte key;
//! - sealed secret: `0x01`, the 24-byte nonce, then the ciphertext and its
//!   16-byte tag.

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
const SEAL_VERSION: u8 = 1;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;

/// A sealed secret, as stored in the database.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedSecret(Vec<u8>);

impl SealedSecret {
    /// Wraps stored bytes, checking only the version and the length; the
    /// tag is checked by [`SecretStore::open`].
    pub fn from_bytes(bytes: Vec<u8>) -> Result<SealedSecret, SecretError> {
        if bytes.len() < 1 + NONCE_LEN + TAG_LEN || bytes[0] != SEAL_VERSION {
            return Err(SecretError::Malformed);
        }
        Ok(SealedSecret(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SealedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SealedSecret({} bytes)", self.0.len())
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
    /// The stored bytes aren't a sealed secret of a known version.
    Malformed,
    /// Wrong key, wrong context, or the bytes were altered.
    Open,
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
            SecretError::Malformed => f.write_str("the stored secret is damaged"),
            SecretError::Open => f.write_str(
                "the stored secret doesn't open with this install's key; enter it again",
            ),
        }
    }
}

impl std::error::Error for SecretError {}

/// Seals and opens credentials with the install's key.
pub struct SecretStore {
    cipher: XChaCha20Poly1305,
}

impl fmt::Debug for SecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretStore")
    }
}

impl SecretStore {
    /// The key file's name inside the data directory.
    pub const KEY_FILE: &'static str = "secrets.key";

    /// Opens the install's key in `data_dir`, creating it (and the
    /// directory) on first use.
    pub fn open_or_create(data_dir: &Path) -> Result<SecretStore, SecretError> {
        let path = data_dir.join(Self::KEY_FILE);
        match read_key(&path) {
            Ok(key) => return Ok(SecretStore::from_key(&key)),
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
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        getrandom::fill(&mut key[..]).map_err(|_| SecretError::Random)?;

        // Write a private temporary file, then link it into place. Linking
        // fails if the key already exists, so two processes starting at once
        // agree on one key instead of overwriting each other's.
        let mut tag = [0u8; 8];
        getrandom::fill(&mut tag).map_err(|_| SecretError::Random)?;
        let tmp = data_dir.join(format!(
            ".{}.{:016x}.tmp",
            Self::KEY_FILE,
            u64::from_le_bytes(tag)
        ));
        let written = write_private(&tmp, &key).map_err(|e| io_err("write", e));
        let linked = written.and_then(|()| {
            restrict_acl(&tmp).map_err(|e| io_err("restrict access to", e))?;
            fs::hard_link(&tmp, &path).map_err(|e| io_err("create", e))
        });
        let _ = fs::remove_file(&tmp);
        match linked {
            Ok(()) => {
                sync_dir(data_dir);
                Ok(SecretStore::from_key(&key))
            }
            Err(SecretError::Io {
                kind: io::ErrorKind::AlreadyExists,
                ..
            }) => SecretStore::open_or_create(data_dir),
            Err(e) => Err(e),
        }
    }

    /// A store over a known key (workers receive sealed credentials, §19.6;
    /// tests).
    pub fn from_key(key: &[u8; KEY_LEN]) -> SecretStore {
        SecretStore {
            cipher: XChaCha20Poly1305::new(key.into()),
        }
    }

    /// Seals `secret` for the record named by `context`.
    pub fn seal(&self, context: &str, secret: &Secret) -> Result<SealedSecret, SecretError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| SecretError::Random)?;
        let ct = self
            .cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: secret.expose().as_bytes(),
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| SecretError::Random)?;
        let mut out = Vec::with_capacity(1 + NONCE_LEN + ct.len());
        out.push(SEAL_VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(SealedSecret(out))
    }

    /// Opens a secret sealed for the same `context`.
    pub fn open(&self, context: &str, sealed: &SealedSecret) -> Result<Secret, SecretError> {
        let (nonce, ct) = sealed.0[1..].split_at(NONCE_LEN);
        let mut pt = self
            .cipher
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ct,
                    aad: context.as_bytes(),
                },
            )
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
}

fn read_key(path: &Path) -> Result<Zeroizing<[u8; KEY_LEN]>, SecretError> {
    let io_err = |op, e: io::Error| SecretError::Io {
        op,
        path: path.to_path_buf(),
        kind: e.kind(),
    };
    let mut file = File::open(path).map_err(|e| io_err("read", e))?;
    tighten_permissions(path, &file).map_err(|e| io_err("restrict access to", e))?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(KEY_MAGIC.len() + KEY_LEN + 1));
    (&mut file)
        .take((KEY_MAGIC.len() + KEY_LEN + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| io_err("read", e))?;
    if bytes.len() != KEY_MAGIC.len() + KEY_LEN || &bytes[..4] != KEY_MAGIC {
        return Err(SecretError::BadKeyFile(path.to_path_buf()));
    }
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    key.copy_from_slice(&bytes[4..]);
    Ok(key)
}

fn write_private(path: &Path, key: &[u8; KEY_LEN]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(KEY_MAGIC.len() + KEY_LEN));
    bytes.extend_from_slice(KEY_MAGIC);
    bytes.extend_from_slice(key);
    file.write_all(&bytes)?;
    file.sync_all()
}

#[cfg(unix)]
fn tighten_permissions(path: &Path, file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = file.metadata()?.permissions().mode();
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

#[cfg(not(unix))]
fn tighten_permissions(_path: &Path, _file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_acl(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Replaces the file's ACL with full control for its owner, `SYSTEM` and
/// `Administrators`, dropping inherited entries (`Users`, `Everyone`).
/// `icacls` ships with every supported Windows; calling it keeps this crate
/// free of `unsafe` Win32 ACL code.
#[cfg(windows)]
fn restrict_acl(path: &Path) -> io::Result<()> {
    let status = std::process::Command::new("icacls")
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

#[cfg(not(any(unix, windows)))]
fn restrict_acl(_path: &Path) -> io::Result<()> {
    Ok(())
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
        assert_eq!(empty.as_bytes().len(), 1 + NONCE_LEN + TAG_LEN);
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
        assert_eq!(bytes.len(), 36);
        assert_eq!(&bytes[..4], KEY_MAGIC);
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
}

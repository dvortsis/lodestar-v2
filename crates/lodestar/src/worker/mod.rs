//! A remote worker's side of the cluster (architecture §19.6, workers
//! plan WK-2): `lodestar worker`.
//!
//! - **Joining** ([`join`]): a one-time token, read from a file or stdin
//!   (never argv, §13; a first join may also take `LODESTAR_TOKEN`, plan
//!   Q2), redeemed over the join protocol. The worker's credentials (`<data>/cluster/credentials`,
//!   V6-cluster2's file, its key sealed with `<data>/secrets.key`) and the
//!   coordinator's address (`<data>/cluster/coordinator`) are saved.
//! - **Running** ([`session`]): mutual TLS to the coordinator, reconnecting
//!   with capped exponential back-off and jitter, renewing the certificate
//!   at half-life, and stopping on a drain, a revocation or an expired
//!   certificate.
//! - **The secrets-key rotation** ([`rotate_key`]): rotate, reseal the
//!   credentials, retire (V6-cluster2).
//!
//! The runtime is idle in WK-2: the worker connects and stays up but runs
//! no DHT or fetch work. Later slices add one `mod` line each here:
//! WK-3 `have` (the replicated have-filter), WK-10 `runtime` and `budget`
//! (the work itself, behind [`session::Work`]).
//!
//! **Secrets:** the token and the credentials' key never reach logs,
//! error text, a panic message or argv. Nothing here
//! formats them: `JoinToken` and `Credentials` print no secret through
//! `Debug`, and every message the worker prints is fixed text plus names,
//! paths and addresses.

pub mod session;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lodestar_cluster::{ClientError, Clock, ClusterError, Credentials, JoinToken, Joining};
use lodestar_integrations::SecretStore;

/// The cluster port's default (§19.2), used when `--join` names no port.
pub const DEFAULT_PORT: u16 = 8091;
/// How long the join exchange may take.
pub const JOIN_TIMEOUT: Duration = Duration::from_secs(30);

const ADDRESS_FILE: &str = "coordinator";
const ADDRESS_MAGIC: &str = "LWA1";
const LOCK_FILE: &str = "worker.lock";
/// The longest `--join` value taken.
const MAX_URL: usize = 300;

/// The worker's files in its data folder.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The data folder: `secrets.key` lives here.
    pub data: PathBuf,
}

impl Layout {
    pub fn new(data: impl Into<PathBuf>) -> Layout {
        Layout { data: data.into() }
    }

    /// `<data>/cluster`: the credentials, the coordinator's address and the
    /// lock.
    pub fn cluster(&self) -> PathBuf {
        self.data.join("cluster")
    }

    pub fn secrets(&self) -> Result<SecretStore, ClusterError> {
        Ok(SecretStore::open_or_create(&self.data)?)
    }

    /// Whether the folder holds credentials (it has joined), without
    /// opening them.
    pub fn is_joined(&self) -> bool {
        self.cluster().join("credentials").exists()
    }

    pub fn load(&self) -> Result<Option<Credentials>, ClusterError> {
        Credentials::load(&self.cluster(), &self.secrets()?)
    }

    /// Saves `creds`, sealed with the key file's primary key as it is now
    /// (read again each time, so a save never uses a key a rotation has
    /// since replaced).
    pub fn save(&self, creds: &Credentials) -> Result<(), ClusterError> {
        creds.save(&self.cluster(), &self.secrets()?)
    }

    /// A coordinator's data folder holds its store; a worker gets a folder
    /// of its own, so its key rotation never touches the coordinator's
    /// `secrets.key`.
    pub fn is_coordinators(&self) -> bool {
        self.data.join("lodestar.db").exists()
    }

    pub fn save_address(&self, addr: &str) -> Result<(), ClusterError> {
        let io = |op: &'static str| move |e: std::io::Error| ClusterError::Io(op, e.kind());
        let dir = self.cluster();
        fs::create_dir_all(&dir).map_err(io("create the cluster folder"))?;
        let tmp = dir.join(format!("{ADDRESS_FILE}.tmp"));
        let mut file = fs::File::create(&tmp).map_err(io("write the coordinator's address"))?;
        file.write_all(format!("{ADDRESS_MAGIC}\naddr {addr}\n").as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(io("write the coordinator's address"))?;
        fs::rename(&tmp, dir.join(ADDRESS_FILE)).map_err(io("write the coordinator's address"))?;
        // Make the rename itself durable, as the credentials' save does.
        #[cfg(unix)]
        fs::File::open(&dir)
            .and_then(|d| d.sync_all())
            .map_err(io("write the coordinator's address"))?;
        Ok(())
    }

    /// The saved address, or `None` before the first join.
    pub fn load_address(&self) -> Result<Option<String>, ClusterError> {
        let text = match fs::read_to_string(self.cluster().join(ADDRESS_FILE)) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ClusterError::Io("read the coordinator's address", e.kind())),
        };
        let corrupt = || ClusterError::Corrupt("the coordinator's address");
        let mut lines = text.lines();
        if lines.next() != Some(ADDRESS_MAGIC) {
            return Err(corrupt());
        }
        let addr = lines
            .next()
            .and_then(|l| l.strip_prefix("addr "))
            .ok_or_else(corrupt)?;
        if lines.next().is_some() {
            return Err(corrupt());
        }
        // Saved by `save_address` from a parsed value: it parses again.
        parse_coordinator(addr).map(Some).map_err(|_| corrupt())
    }

    /// One `lodestar worker` per data folder at a time, so a key rotation
    /// never races a running worker's renewal. Held until dropped; the OS
    /// releases it if the process dies.
    pub fn lock(&self) -> Result<Option<fs::File>, ClusterError> {
        let io = |op: &'static str| move |e: std::io::Error| ClusterError::Io(op, e.kind());
        let dir = self.cluster();
        fs::create_dir_all(&dir).map_err(io("create the cluster folder"))?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE))
            .map_err(io("open the worker's lock file"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => Err(io("lock the worker's folder")(e)),
        }
    }
}

/// Why a `--join` value was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressError {
    /// `http://`: the cluster port only speaks TLS.
    Plaintext,
    /// Another scheme than `https://`.
    Scheme,
    /// A path, query, fragment or user name after the host.
    Extra,
    Host,
    Port,
    TooLong,
}

impl std::fmt::Display for AddressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AddressError::Plaintext => {
                "the cluster port always uses TLS: write https://, not http://"
            }
            AddressError::Scheme => "the coordinator's address starts with https:// (or no scheme)",
            AddressError::Extra => {
                "the coordinator's address is only a host and a port, like https://lodestar.lan:8091"
            }
            AddressError::Host => "the coordinator's host name or IP address isn't valid",
            AddressError::Port => "the cluster port is a number from 1 to 65535",
            AddressError::TooLong => "the coordinator's address is too long",
        })
    }
}

/// Parses `--join`: `https://host:port`, `host:port` or either without a
/// port (8091), with `[v6]` for an IPv6 address. Returns `host:port` to
/// dial. The address isn't secret, but it's still checked strictly: it
/// goes into a file and into messages.
pub fn parse_coordinator(text: &str) -> Result<String, AddressError> {
    if text.len() > MAX_URL {
        return Err(AddressError::TooLong);
    }
    let rest = match text.split_once("://") {
        Some((scheme, rest)) => {
            if scheme.eq_ignore_ascii_case("https") {
                rest
            } else if scheme.eq_ignore_ascii_case("http") {
                return Err(AddressError::Plaintext);
            } else {
                return Err(AddressError::Scheme);
            }
        }
        None => text,
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains(['/', '?', '#', '@']) || rest.chars().any(char::is_whitespace) {
        return Err(AddressError::Extra);
    }
    let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
        let (inside, after) = v6.split_once(']').ok_or(AddressError::Host)?;
        inside
            .parse::<std::net::Ipv6Addr>()
            .map_err(|_| AddressError::Host)?;
        let port = match after {
            "" => None,
            p => Some(p.strip_prefix(':').ok_or(AddressError::Host)?),
        };
        (format!("[{inside}]"), port)
    } else {
        let (host, port) = match rest.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (rest, None),
        };
        if !valid_host(host) {
            return Err(AddressError::Host);
        }
        (host.to_ascii_lowercase(), port)
    };
    let port = match port {
        None => DEFAULT_PORT,
        Some(p) => {
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(AddressError::Port);
            }
            match p.parse::<u16>() {
                Ok(n) if n > 0 => n,
                _ => return Err(AddressError::Port),
            }
        }
    };
    Ok(format!("{host}:{port}"))
}

/// A DNS name or an IPv4 address: labels of letters, digits and hyphens,
/// 1–63 each, not starting or ending with a hyphen, 253 at most in all.
fn valid_host(host: &str) -> bool {
    (1..=253).contains(&host.len())
        && host.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

/// Why a join failed: each is one message.
#[derive(Debug)]
pub enum JoinFailure {
    /// The token was refused: mistyped, expired or already used. The
    /// coordinator gives every refusal the same answer (V6-cluster1).
    Refused,
    Unreachable(ClientError),
    /// The worker's key pair couldn't be made (before any network call).
    Key(ClusterError),
    Save(ClusterError),
}

impl std::fmt::Display for JoinFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinFailure::Refused => f.write_str(REFUSED),
            JoinFailure::Unreachable(e) => write!(f, "couldn't join: {e}"),
            JoinFailure::Key(e) => write!(f, "couldn't make the worker's key: {e}"),
            JoinFailure::Save(e) => write!(f, "joined, but couldn't save the credentials: {e}"),
        }
    }
}

/// The one message for a refused token.
pub const REFUSED: &str = "the coordinator refused the join token: it has expired (a token \
     lasts 15 minutes), was already used, or isn't this coordinator's. Make a new one in \
     Settings → Workers → Add worker, then join again.";

/// Joins the coordinator at `addr` with `token`, then saves the
/// credentials and the address. Saved credentials are replaced only once
/// the new ones are granted.
pub async fn join(
    layout: &Layout,
    addr: &str,
    token: JoinToken,
    clock: Arc<dyn Clock>,
) -> Result<Credentials, JoinFailure> {
    let joining = Joining::new(token).map_err(JoinFailure::Key)?;
    let creds = match lodestar_cluster::join(addr, joining, clock, JOIN_TIMEOUT).await {
        Ok(c) => c,
        Err(ClientError::Refused) => return Err(JoinFailure::Refused),
        Err(e) => return Err(JoinFailure::Unreachable(e)),
    };
    save_joined(layout, &creds, addr).map_err(JoinFailure::Save)?;
    Ok(creds)
}

/// Saves a join's credentials and address. "Joined" means the credentials
/// file exists. A first join writes the address first, so a crash between
/// the two leaves only an address and the folder still unjoined. A join
/// again writes the credentials first, so a crash can't pair the new
/// address with the old credentials (the new ones with the old address fail
/// their TLS check and say so).
fn save_joined(layout: &Layout, creds: &Credentials, addr: &str) -> Result<(), ClusterError> {
    if layout.is_joined() {
        layout.save(creds)?;
        layout.save_address(addr)
    } else {
        layout.save_address(addr)?;
        layout.save(creds)
    }
}

/// The worker's secrets-key rotation, worker stopped (the caller holds
/// [`Layout::lock`]): `SecretStore::rotate`, then `Credentials::reseal`,
/// then `SecretStore::retire_old_keys` (V6-cluster2). The old key is
/// retired only once the re-sealed file is on disk, opens, and is sealed
/// with the new primary key, so a failure or a crash at any step leaves
/// credentials that open (with both keys until the retire). `false` if
/// the worker hasn't joined (the key is rotated all the same).
pub fn rotate_key(layout: &Layout) -> Result<bool, ClusterError> {
    let mut secrets = layout.secrets()?;
    secrets.rotate()?;
    let dir = layout.cluster();
    let resealed = Credentials::reseal(&dir, &secrets)?;
    if resealed {
        let opens = Credentials::load(&dir, &secrets)?.is_some();
        if !opens || sealed_with(&dir)? != secrets.primary_key_id() {
            return Err(ClusterError::Corrupt("the re-sealed credentials"));
        }
    }
    secrets.retire_old_keys()?;
    Ok(resealed)
}

/// Which secrets key sealed the credentials' private key: the `key` line
/// of V6-cluster2's file (hex of a sealed secret, which names its key).
fn sealed_with(dir: &Path) -> Result<u32, ClusterError> {
    let corrupt = || ClusterError::Corrupt("the credentials' sealed key");
    let text = fs::read_to_string(dir.join("credentials"))
        .map_err(|e| ClusterError::Io("read the credentials", e.kind()))?;
    let hex = text
        .lines()
        .find_map(|l| l.strip_prefix("key "))
        .ok_or_else(corrupt)?;
    if hex.len() % 2 != 0 {
        return Err(corrupt());
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(corrupt)?;
    let sealed = lodestar_integrations::SealedSecret::from_bytes(bytes).map_err(|_| corrupt())?;
    Ok(sealed.key_id())
}

#[cfg(test)]
pub(crate) mod tests {
    use lodestar_cluster::{Coordinator, ManualClock};
    use lodestar_store::{Store, StoreConfig};
    use proptest::prelude::*;

    use super::*;

    /// A worker's credentials from a coordinator in memory, redeemed
    /// directly (no network).
    pub(crate) fn credentials(clock: &ManualClock) -> Credentials {
        let now = clock.now_secs();
        let mut store = Store::open_in_memory(StoreConfig::default()).unwrap();
        let secrets = SecretStore::from_key(&[9; 32]);
        let mut coord = Coordinator::open(&mut store, &secrets, now).unwrap();
        let token = coord.create_join_token(&mut store, "w1", now).unwrap();
        let joining = Joining::new(token).unwrap();
        let grant = coord
            .redeem(&mut store, joining.token(), &joining.public_key(), now)
            .unwrap();
        joining.finish(grant, now).unwrap()
    }

    #[test]
    fn a_first_join_writes_the_address_first_and_a_join_again_the_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let clock = ManualClock::starting_now();
        let blocker = layout.cluster().join("credentials.tmp");
        // A first join whose credentials can't be written: only the address
        // is there, and the folder isn't joined.
        fs::create_dir_all(blocker.join("x")).unwrap();
        assert!(save_joined(&layout, &credentials(&clock), "old:8091").is_err());
        assert!(!layout.is_joined());
        assert_eq!(layout.load_address().unwrap().as_deref(), Some("old:8091"));
        fs::remove_dir_all(&blocker).unwrap();
        save_joined(&layout, &credentials(&clock), "old:8091").unwrap();
        let first = layout.load().unwrap().unwrap().not_after();

        // A join again whose address can't be written: the new credentials
        // are there with the old address, never the reverse.
        clock.advance(std::time::Duration::from_secs(10));
        let addr_blocker = layout.cluster().join("coordinator.tmp");
        fs::create_dir_all(addr_blocker.join("x")).unwrap();
        assert!(save_joined(&layout, &credentials(&clock), "new:8091").is_err());
        assert_eq!(layout.load().unwrap().unwrap().not_after(), first + 10);
        assert_eq!(layout.load_address().unwrap().as_deref(), Some("old:8091"));
        fs::remove_dir_all(&addr_blocker).unwrap();

        // And a join again whose credentials can't be written changes
        // nothing.
        clock.advance(std::time::Duration::from_secs(10));
        fs::create_dir_all(blocker.join("x")).unwrap();
        assert!(save_joined(&layout, &credentials(&clock), "newer:8091").is_err());
        assert_eq!(layout.load().unwrap().unwrap().not_after(), first + 10);
        assert_eq!(layout.load_address().unwrap().as_deref(), Some("old:8091"));
    }

    #[test]
    fn reseal_then_retire_keeps_the_credentials_open() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        // Not joined: the key still rotates, and nothing is created.
        assert!(!rotate_key(&layout).unwrap());
        assert!(layout.load().unwrap().is_none());

        let clock = ManualClock::starting_now();
        let creds = credentials(&clock);
        layout.save(&creds).unwrap();
        let old = layout.secrets().unwrap().primary_key_id();
        assert!(rotate_key(&layout).unwrap());
        let secrets = layout.secrets().unwrap();
        assert_eq!(secrets.key_count(), 1, "the old key is retired");
        assert_ne!(secrets.primary_key_id(), old);
        let opened = layout.load().unwrap().expect("still opens");
        assert_eq!(
            (opened.worker(), opened.not_after()),
            (creds.worker(), creds.not_after())
        );

        assert_eq!(
            sealed_with(&layout.cluster()).unwrap(),
            secrets.primary_key_id()
        );

        // A crash after each step leaves credentials that open, and the
        // next rotation finishes the job.
        let mut secrets = layout.secrets().unwrap();
        secrets.rotate().unwrap(); // crashed after "rotate"
        assert!(layout.load().unwrap().is_some());
        Credentials::reseal(&layout.cluster(), &layout.secrets().unwrap()).unwrap();
        assert!(layout.load().unwrap().is_some()); // crashed after "reseal"
        assert_eq!(layout.secrets().unwrap().key_count(), 2);
        assert!(rotate_key(&layout).unwrap());
        assert_eq!(layout.secrets().unwrap().key_count(), 1);
        assert!(layout.load().unwrap().is_some());

        // A reseal that fails (here its temporary file can't be replaced)
        // fails the rotation and retires nothing: both keys stay, and the
        // credentials open. Once the fault is gone, it finishes.
        let blocker = layout.cluster().join("credentials.tmp");
        fs::create_dir(&blocker).unwrap();
        fs::write(blocker.join("x"), b"").unwrap();
        assert!(rotate_key(&layout).is_err());
        assert_eq!(layout.secrets().unwrap().key_count(), 2);
        assert!(layout.load().unwrap().is_some());
        fs::remove_dir_all(&blocker).unwrap();
        assert!(rotate_key(&layout).unwrap());
        assert_eq!(layout.secrets().unwrap().key_count(), 1);
        assert!(layout.load().unwrap().is_some());

        // Without the reseal step the credentials are stranded, so the
        // test above would catch a rotation that skipped it.
        let mut secrets = layout.secrets().unwrap();
        secrets.rotate().unwrap();
        secrets.retire_old_keys().unwrap();
        assert!(layout.load().is_err());
    }

    #[test]
    fn coordinator_addresses() {
        let ok = [
            ("https://coord:8091", "coord:8091"),
            ("https://Coord.LAN:9000/", "coord.lan:9000"),
            ("HTTPS://10.0.0.5", "10.0.0.5:8091"),
            ("coord", "coord:8091"),
            ("coord:1", "coord:1"),
            ("https://[::1]:8091", "[::1]:8091"),
            ("[fe80::1]", "[fe80::1]:8091"),
        ];
        for (text, want) in ok {
            assert_eq!(parse_coordinator(text).as_deref(), Ok(want), "{text}");
        }
        let bad = [
            ("http://coord:8091", AddressError::Plaintext),
            ("wss://coord", AddressError::Scheme),
            ("ftp://coord", AddressError::Scheme),
            ("https://coord:8091/cluster", AddressError::Extra),
            ("https://user@coord", AddressError::Extra),
            ("https://coord?x", AddressError::Extra),
            ("https://co ord", AddressError::Extra),
            ("https://", AddressError::Host),
            ("https://-coord", AddressError::Host),
            ("https://co_ord", AddressError::Host),
            ("https://coord..lan", AddressError::Host),
            ("https://[::1", AddressError::Host),
            ("https://[nope]:1", AddressError::Host),
            ("https://[::1]x", AddressError::Host),
            ("https://coord:0", AddressError::Port),
            ("https://coord:65536", AddressError::Port),
            ("https://coord:", AddressError::Port),
            ("https://coord:+80", AddressError::Port),
            ("https://coord:1:2", AddressError::Port),
        ];
        for (text, want) in bad {
            assert_eq!(parse_coordinator(text), Err(want), "{text}");
        }
        assert_eq!(
            parse_coordinator(&"a".repeat(MAX_URL + 1)),
            Err(AddressError::TooLong)
        );
    }

    proptest! {
        /// Whatever it's given, it never panics, and what it returns parses
        /// again to itself (what the address file relies on).
        #[test]
        fn parsing_is_total_and_idempotent(text in "\\PC{0,80}") {
            if let Ok(addr) = parse_coordinator(&text) {
                prop_assert_eq!(parse_coordinator(&addr), Ok(addr.clone()));
                prop_assert!(addr.len() <= MAX_URL);
            }
        }

        #[test]
        fn hosts_and_ports_round_trip(
            host in "[a-z0-9]([a-z0-9-]{0,10}[a-z0-9])?(\\.[a-z0-9]{1,8}){0,3}",
            port in 1u16..,
        ) {
            let want = format!("{host}:{port}");
            prop_assert_eq!(parse_coordinator(&format!("https://{host}:{port}")), Ok(want.clone()));
            prop_assert_eq!(parse_coordinator(&want), Ok(want));
        }
    }

    #[test]
    fn the_address_file_round_trips_and_refuses_junk() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        assert_eq!(layout.load_address().unwrap(), None);
        layout.save_address("coord:8091").unwrap();
        assert_eq!(
            layout.load_address().unwrap().as_deref(),
            Some("coord:8091")
        );
        for junk in [
            "",
            "LWA1\n",
            "LWA2\naddr coord:8091\n",
            "LWA1\naddr http://coord\n",
            "LWA1\naddr coord:8091\nextra\n",
        ] {
            fs::write(layout.cluster().join(ADDRESS_FILE), junk).unwrap();
            assert!(
                matches!(layout.load_address(), Err(ClusterError::Corrupt(_))),
                "{junk:?}"
            );
        }
    }

    #[test]
    fn one_worker_per_folder() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let held = layout.lock().unwrap().expect("free");
        assert!(layout.lock().unwrap().is_none(), "held by the first");
        drop(held);
        assert!(layout.lock().unwrap().is_some(), "released on drop");
    }
}

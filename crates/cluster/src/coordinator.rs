//! The coordinator's side of the cluster PKI (§19.6): the cluster CA,
//! its own server certificate, the certificates it issued and the join
//! tokens it handed out.
//!
//! **Persistence** is the store's `settings` table, under `cluster.`. No
//! migration is needed, and everything changes in one transaction per
//! operation:
//!
//! | Key | Value (space-separated, lowercase hex) |
//! |---|---|
//! | `cluster.ca.current` | `v1 <cert> <sealed key>` |
//! | `cluster.ca.previous` | `v1 <cert> <sealed key> <until>`, during a rotation |
//! | `cluster.server` | `v1 <cert> <sealed key> <not_after> <issuer fingerprint>` |
//! | `cluster.cert.<serial>` | `v1 <worker> <name> <fingerprint> <issuer> <not_after> <revoked 0/1>` |
//! | `cluster.token.<id>` | `v1 <hash> <name> <expires_at> <used 0/1> <ca fingerprint>`, then once spent `<public key> <worker> <not_after> <cert>` |
//! | `cluster.next_worker` | the next worker id |
//!
//! Private keys are sealed with the install's secrets key
//! (`lodestar_integrations::SecretStore`, outside the database), with the
//! certificate's fingerprint in the context, so a copy of the database
//! alone holds no usable key and a sealed key can't be moved to another
//! record.
//!
//! **Times** are Unix seconds from the caller, like the lease table's.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use lodestar_integrations::{SealedSecret, Secret, SecretStore};
use lodestar_store::Store;
use ring::rand::{SecureRandom, SystemRandom};
use rustls::pki_types::CertificateDer;
use rustls::sign::CertifiedKey;
use rustls::time_provider::DefaultTimeProvider;
use zeroize::Zeroizing;

use crate::join::{JoinToken, hashes_match, valid_worker_name};
use crate::tls::{CoordinatorTls, Issued, PeerIdentity, Shared, Trust, signing_key};
use crate::x509::{self, CLOCK_SKEW_SECS, Role, Spec};
use crate::{ClusterError, Fingerprint, KeyPair, WorkerId, hex};

/// A join token's lifetime (§19.6).
pub const JOIN_TOKEN_TTL_SECS: i64 = 15 * 60;
/// Leaf certificates (workers' and the coordinator's) last three days and
/// are renewed at half-life, so a worker offline for a whole spool window
/// (24 h, §19.6) still reconnects without joining again.
pub const LEAF_LIFETIME_SECS: i64 = 72 * 3600;
pub const CA_LIFETIME_SECS: i64 = 10 * 365 * 86_400;
/// How long the old CA stays trusted after a rotation. Longer than a leaf's
/// half-life, so every worker that's online renews onto the new CA in time.
pub const ROTATION_OVERLAP_SECS: i64 = 7 * 86_400;
/// Join tokens open at once.
pub const MAX_OPEN_TOKENS: usize = 64;

const PREFIX: &str = "cluster.";
const KEY_CA: &str = "cluster.ca.current";
const KEY_CA_PREVIOUS: &str = "cluster.ca.previous";
const KEY_SERVER: &str = "cluster.server";
const KEY_NEXT_WORKER: &str = "cluster.next_worker";

/// Why a redemption was refused. Until the secret matches, every refusal is
/// [`JoinError::Unknown`], so a guess learns nothing. The protocol (V6-cluster2)
/// sends one "token refused" answer for all of them.
#[derive(Debug)]
pub enum JoinError {
    /// No such token, or the secret doesn't match.
    Unknown,
    Expired,
    /// Already redeemed: a replay.
    AlreadyUsed,
    /// A live worker already has this name (revoke it first).
    NameInUse,
    Failed(ClusterError),
}

impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JoinError::Unknown => f.write_str("the join token isn't one this coordinator issued"),
            JoinError::Expired => f.write_str("the join token has expired; create a new one"),
            JoinError::AlreadyUsed => f.write_str("the join token was already used"),
            JoinError::NameInUse => f.write_str("a worker with this name is already joined"),
            JoinError::Failed(e) => write!(f, "joining failed: {e}"),
        }
    }
}

impl std::error::Error for JoinError {}

impl From<ClusterError> for JoinError {
    fn from(e: ClusterError) -> JoinError {
        JoinError::Failed(e)
    }
}

/// What a worker gets when it joins or renews: its certificate and the CA
/// bundle to trust. Its private key never leaves it (§19.6).
#[derive(Debug, Clone)]
pub struct JoinGrant {
    pub worker: WorkerId,
    pub name: String,
    pub cert: CertificateDer<'static>,
    pub not_after: i64,
    pub cas: Vec<CertificateDer<'static>>,
}

/// A certificate the coordinator issued, for Settings → Workers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedCert {
    pub serial: [u8; 16],
    pub worker: WorkerId,
    pub name: String,
    pub not_after: i64,
    pub revoked: bool,
}

struct Authority {
    cert: CertificateDer<'static>,
    key: KeyPair,
    fp: Fingerprint,
}

impl Authority {
    fn generate(now: i64) -> Result<Authority, ClusterError> {
        let key = KeyPair::generate()?;
        let name = ca_name(&key);
        let spec = Spec {
            role: Role::Ca,
            subject: &name,
            issuer: &name,
            public_key: &key.public_key(),
            not_before: now - CLOCK_SKEW_SECS,
            not_after: now + CA_LIFETIME_SECS,
        };
        let cert = x509::certificate(&spec, &x509::random_serial()?, &key);
        Ok(Authority {
            fp: Fingerprint::of(&cert),
            cert: cert.into(),
            key,
        })
    }

    fn issue(
        &self,
        role: Role<'_>,
        subject: &str,
        public_key: &[u8; 32],
        now: i64,
    ) -> Result<([u8; 16], Vec<u8>, i64), ClusterError> {
        let not_after = now + LEAF_LIFETIME_SECS;
        let spec = Spec {
            role,
            subject,
            issuer: &ca_name(&self.key),
            public_key,
            not_before: now - CLOCK_SKEW_SECS,
            not_after,
        };
        let serial = x509::random_serial()?;
        Ok((
            serial,
            x509::certificate(&spec, &serial, &self.key),
            not_after,
        ))
    }
}

/// The CA's subject name, derived from its key so it needn't be stored.
fn ca_name(key: &KeyPair) -> String {
    format!(
        "Lodestar cluster CA {}",
        hex::encode(&key.public_key()[..8])
    )
}

struct ServerLeaf {
    cert: CertificateDer<'static>,
    key: KeyPair,
    not_after: i64,
    issuer: Fingerprint,
}

#[derive(Clone)]
struct TokenRecord {
    hash: [u8; 32],
    name: String,
    expires_at: i64,
    used: bool,
    ca: Fingerprint,
    /// What the token was spent on, so a worker whose grant got lost can
    /// ask again with the same key until the token expires.
    redeemed: Option<Redeemed>,
}

#[derive(Clone)]
struct Redeemed {
    public_key: [u8; 32],
    worker: WorkerId,
    cert: CertificateDer<'static>,
    not_after: i64,
}

impl TokenRecord {
    fn open(&self, now: i64) -> bool {
        !self.used && now < self.expires_at
    }
}

/// The coordinator's cluster PKI. One owner (the coordinator's cluster
/// task) mutates it; [`Coordinator::tls`] hands out the shared, read-only
/// handshake side.
// TODO(V6-cluster2): open this in `lodestar serve`, serve the cluster port
// through `CoordinatorTls::config_for`, carry `JoinToken`/`JoinGrant` and
// renewals in the protocol's frames, and run `maintain` hourly.
pub struct Coordinator {
    current: Authority,
    previous: Option<(Authority, i64)>,
    server: ServerLeaf,
    tokens: HashMap<[u8; 8], TokenRecord>,
    next_worker: u32,
    shared: Arc<Shared>,
    tls: CoordinatorTls,
}

impl std::fmt::Debug for Coordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coordinator")
            .field("ca", &self.current.fp)
            .field("rotating", &self.previous.is_some())
            .finish_non_exhaustive()
    }
}

impl Coordinator {
    /// Loads the cluster PKI, creating the CA and the coordinator's
    /// certificate on first use, then runs [`maintain`](Self::maintain).
    pub fn open(
        store: &mut Store,
        secrets: &SecretStore,
        now: i64,
    ) -> Result<Coordinator, ClusterError> {
        let mut coordinator = match store.setting_text(KEY_CA)? {
            Some(_) => Coordinator::load(store, secrets)?,
            // Other cluster rows without a CA: something deleted it, and a
            // new CA would silently orphan every worker. Refuse, as `load`
            // refuses any other inconsistency.
            None if !store.settings_with_prefix(PREFIX)?.is_empty() => return Err(corrupt()),
            None => Coordinator::create(store, secrets, now)?,
        };
        coordinator.maintain(store, secrets, now)?;
        Ok(coordinator)
    }

    fn create(
        store: &mut Store,
        secrets: &SecretStore,
        now: i64,
    ) -> Result<Coordinator, ClusterError> {
        let current = Authority::generate(now)?;
        let server = new_server_leaf(&current, now)?;
        let ca = format!("v1 {}", authority_value(&current, secrets)?);
        let leaf = server_value(&server, secrets)?;
        store.set_settings_text(&[(KEY_CA, &ca), (KEY_SERVER, &leaf), (KEY_NEXT_WORKER, "1")])?;
        Coordinator::assemble(current, None, server, HashMap::new(), HashMap::new(), 1)
    }

    fn load(store: &Store, secrets: &SecretStore) -> Result<Coordinator, ClusterError> {
        let mut current = None;
        let mut previous = None;
        let mut server = None;
        let mut tokens = HashMap::new();
        let mut issued = HashMap::new();
        let mut next_worker = None;
        for (key, value) in store.settings_with_prefix(PREFIX)? {
            let fields: Vec<&str> = value.split_ascii_whitespace().collect();
            match (key.as_str(), fields.as_slice()) {
                (KEY_CA, ["v1", cert, sealed]) => {
                    current = Some(load_authority(cert, sealed, secrets)?);
                }
                (KEY_CA_PREVIOUS, ["v1", cert, sealed, until]) => {
                    previous = Some((load_authority(cert, sealed, secrets)?, int(until)?));
                }
                (KEY_SERVER, ["v1", cert, sealed, not_after, issuer]) => {
                    let cert = cert_from_hex(cert)?;
                    let key = open_key(secrets, &server_context(&cert), sealed)?;
                    check_certifies(&cert, &key)?;
                    server = Some(ServerLeaf {
                        cert,
                        key,
                        not_after: int(not_after)?,
                        issuer: fingerprint(issuer)?,
                    });
                }
                (KEY_NEXT_WORKER, [n]) => {
                    next_worker = Some(n.parse().map_err(|_| corrupt())?);
                }
                (k, ["v1", worker, name, fp, issuer, not_after, revoked])
                    if k.starts_with("cluster.cert.") =>
                {
                    let serial = hex::decode(&k["cluster.cert.".len()..])
                        .and_then(|s| s.try_into().ok())
                        .ok_or_else(corrupt)?;
                    issued.insert(
                        fingerprint(fp)?,
                        Issued {
                            serial,
                            worker: WorkerId(worker.parse().map_err(|_| corrupt())?),
                            name: (*name).to_owned(),
                            issuer: fingerprint(issuer)?,
                            not_after: int(not_after)?,
                            revoked: flag(revoked)?,
                        },
                    );
                }
                (k, ["v1", hash, name, expires_at, used, ca, rest @ ..])
                    if k.starts_with("cluster.token.") =>
                {
                    let redeemed = match rest {
                        [] => None,
                        [public_key, worker, not_after, cert] => Some(Redeemed {
                            public_key: fingerprint(public_key)?.0,
                            worker: WorkerId(worker.parse().map_err(|_| corrupt())?),
                            not_after: int(not_after)?,
                            cert: cert_from_hex(cert)?,
                        }),
                        _ => return Err(corrupt()),
                    };
                    let id = hex::decode(&k["cluster.token.".len()..])
                        .and_then(|s| s.try_into().ok())
                        .ok_or_else(corrupt)?;
                    tokens.insert(
                        id,
                        TokenRecord {
                            hash: fingerprint(hash)?.0,
                            name: (*name).to_owned(),
                            expires_at: int(expires_at)?,
                            used: flag(used)?,
                            ca: fingerprint(ca)?,
                            redeemed,
                        },
                    );
                }
                _ => return Err(corrupt()),
            }
        }
        match (current, server, next_worker) {
            (Some(current), Some(server), Some(next)) => {
                Coordinator::assemble(current, previous, server, tokens, issued, next)
            }
            _ => Err(corrupt()),
        }
    }

    fn assemble(
        current: Authority,
        previous: Option<(Authority, i64)>,
        server: ServerLeaf,
        tokens: HashMap<[u8; 8], TokenRecord>,
        issued: HashMap<Fingerprint, Issued>,
        next_worker: u32,
    ) -> Result<Coordinator, ClusterError> {
        let mut cas = vec![current.cert.clone()];
        cas.extend(previous.iter().map(|(a, _)| a.cert.clone()));
        let shared = Arc::new(Shared {
            trust: RwLock::new(Trust::new(cas, current.fp, issued)?),
            server: RwLock::new(Arc::new(certified_key(&server, &current, &previous)?)),
        });
        let tls = CoordinatorTls::new(shared.clone(), Arc::new(DefaultTimeProvider))?;
        Ok(Coordinator {
            current,
            previous,
            server,
            tokens,
            next_worker,
            shared,
            tls,
        })
    }

    /// The handshake side, for the cluster port's listener.
    pub fn tls(&self) -> CoordinatorTls {
        self.tls.clone()
    }

    /// The fingerprint new join tokens carry: the CA that signs the
    /// coordinator's certificate right now.
    pub fn ca_fingerprint(&self) -> Fingerprint {
        self.server.issuer
    }

    /// The CA certificates workers trust: the current one, then the one
    /// being rotated out.
    pub fn ca_certificates(&self) -> Vec<CertificateDer<'static>> {
        self.cas()
    }

    #[cfg(test)]
    pub(crate) fn server_fingerprint(&self) -> Fingerprint {
        Fingerprint::of(&self.server.cert)
    }

    #[cfg(test)]
    pub(crate) fn ca_name_for_test(&self) -> String {
        ca_name(&self.current.key)
    }

    /// A certificate from the current CA that isn't on record.
    #[cfg(test)]
    pub(crate) fn issue_for_test(
        &self,
        role: Role<'_>,
        now: i64,
    ) -> (KeyPair, CertificateDer<'static>) {
        let key = KeyPair::generate().unwrap();
        let (_, der, _) = self
            .current
            .issue(role, "test", &key.public_key(), now)
            .unwrap();
        (key, der.into())
    }

    /// Every certificate on record, revoked ones included until they
    /// expire.
    pub fn certificates(&self) -> Vec<IssuedCert> {
        let trust = self
            .shared
            .trust
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let mut out: Vec<IssuedCert> = trust
            .issued
            .values()
            .map(|i| IssuedCert {
                serial: i.serial,
                worker: i.worker,
                name: i.name.clone(),
                not_after: i.not_after,
                revoked: i.revoked,
            })
            .collect();
        out.sort_by_key(|c| (c.worker, c.not_after));
        out
    }

    /// The revocation list: serials refused at every handshake.
    pub fn revoked_serials(&self) -> Vec<[u8; 16]> {
        self.certificates()
            .into_iter()
            .filter(|c| c.revoked)
            .map(|c| c.serial)
            .collect()
    }

    /// A one-time token for joining a worker called `name`. It's returned
    /// once; only its hash is kept.
    pub fn create_join_token(
        &mut self,
        store: &mut Store,
        name: &str,
        now: i64,
    ) -> Result<JoinToken, ClusterError> {
        if !valid_worker_name(name) {
            return Err(ClusterError::InvalidName);
        }
        if self.name_in_use(name, now) {
            return Err(ClusterError::NameInUse);
        }
        if self.tokens.values().filter(|t| t.open(now)).count() >= MAX_OPEN_TOKENS {
            return Err(ClusterError::TooManyTokens);
        }
        let rng = SystemRandom::new();
        let mut id = [0u8; 8];
        let mut secret = Zeroizing::new([0u8; 32]);
        rng.fill(&mut id).map_err(|_| ClusterError::Random)?;
        rng.fill(secret.as_mut())
            .map_err(|_| ClusterError::Random)?;
        let token = JoinToken::new(id, &secret, self.server.issuer);
        let record = TokenRecord {
            hash: token.hash(),
            name: name.to_owned(),
            expires_at: now + JOIN_TOKEN_TTL_SECS,
            used: false,
            ca: self.server.issuer,
            redeemed: None,
        };
        store.set_settings_text(&[(&token_key(&id), &token_value(&record))])?;
        self.tokens.insert(id, record);
        Ok(token)
    }

    /// Redeems `token` for a certificate over `public_key`, the key pair
    /// the worker made for itself. The token is spent in the same
    /// transaction that records the certificate.
    pub fn redeem(
        &mut self,
        store: &mut Store,
        token: &JoinToken,
        public_key: &[u8; 32],
        now: i64,
    ) -> Result<JoinGrant, JoinError> {
        let id = token.id();
        let record = self.tokens.get(&id).ok_or(JoinError::Unknown)?;
        if !hashes_match(&record.hash, &token.hash()) {
            return Err(JoinError::Unknown);
        }
        if record.used {
            return self.redeem_again(record, public_key, now);
        }
        if now >= record.expires_at {
            return Err(JoinError::Expired);
        }
        let name = record.name.clone();
        if self.name_in_use(&name, now) {
            return Err(JoinError::NameInUse);
        }
        let unspent = token_value(record);
        let record = record.clone();
        // The worker id is only in the records, not the certificate, so the
        // certificate can be made before the transaction picks the id.
        let (fp, mut issued, mut grant) = self.issue_worker(WorkerId(0), &name, public_key, now)?;
        let spent = |worker| TokenRecord {
            used: true,
            redeemed: Some(Redeemed {
                public_key: *public_key,
                worker,
                cert: grant.cert.clone(),
                not_after: grant.not_after,
            }),
            ..record.clone()
        };
        let taken = spend_token(store, &token_key(&id), &unspent, |w| {
            let issued = Issued {
                worker: w,
                ..issued.clone()
            };
            [
                (token_key(&id), token_value(&spent(w))),
                (cert_key(&issued.serial), cert_value(&fp, &issued)),
            ]
        })?;
        let Some(worker) = taken else {
            // Another coordinator on the same database spent it first.
            self.tokens.insert(
                id,
                TokenRecord {
                    used: true,
                    ..record
                },
            );
            return Err(JoinError::AlreadyUsed);
        };
        self.tokens.insert(id, spent(worker));
        issued.worker = worker;
        grant.worker = worker;
        self.next_worker = self.next_worker.max(worker.0.saturating_add(1));
        self.trust_mut().issued.insert(fp, issued);
        Ok(grant)
    }

    /// A spent token presented again. With the same key, before it expires
    /// and while its certificate stands, it gets the same grant: the first
    /// answer may have been lost, and refusing would strand the name until
    /// the certificate expired. Anything else is a replay.
    fn redeem_again(
        &self,
        record: &TokenRecord,
        public_key: &[u8; 32],
        now: i64,
    ) -> Result<JoinGrant, JoinError> {
        let Some(r) = &record.redeemed else {
            return Err(JoinError::AlreadyUsed);
        };
        let standing = self
            .shared
            .trust
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .issued
            .get(&Fingerprint::of(&r.cert))
            .is_some_and(|i| !i.revoked && now <= i.not_after);
        if r.public_key != *public_key || now >= record.expires_at || !standing {
            return Err(JoinError::AlreadyUsed);
        }
        Ok(JoinGrant {
            worker: r.worker,
            name: record.name.clone(),
            cert: r.cert.clone(),
            not_after: r.not_after,
            cas: self.cas(),
        })
    }

    /// A fresh certificate for a connected worker, over a key it may have
    /// rotated. The old certificate stays valid until it expires.
    pub fn renew(
        &mut self,
        store: &mut Store,
        peer: &PeerIdentity,
        public_key: &[u8; 32],
        now: i64,
    ) -> Result<JoinGrant, ClusterError> {
        // The identity comes from the record, not the caller's copy.
        let current = self
            .shared
            .trust
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .issued
            .values()
            .find(|i| i.serial == peer.serial && i.worker == peer.worker)
            .cloned();
        let current = match current {
            Some(i) if !i.revoked => i,
            _ => return Err(ClusterError::Revoked),
        };
        if now > current.not_after {
            return Err(ClusterError::Expired);
        }
        let (fp, issued, grant) =
            self.issue_worker(current.worker, &current.name, public_key, now)?;
        store.set_settings_text(&[(&cert_key(&issued.serial), &cert_value(&fp, &issued))])?;
        self.trust_mut().issued.insert(fp, issued);
        Ok(grant)
    }

    /// Revokes every certificate of `worker`. The next handshake is
    /// refused; open connections are the protocol's to close (V6-cluster2).
    pub fn revoke(&mut self, store: &mut Store, worker: WorkerId) -> Result<usize, ClusterError> {
        let mut changed: Vec<(Fingerprint, Issued)> = self
            .shared
            .trust
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .issued
            .iter()
            .filter(|(_, i)| i.worker == worker && !i.revoked)
            .map(|(fp, i)| (*fp, i.clone()))
            .collect();
        for (_, issued) in &mut changed {
            issued.revoked = true;
        }
        let rows: Vec<(String, String)> = changed
            .iter()
            .map(|(fp, i)| (cert_key(&i.serial), cert_value(fp, i)))
            .collect();
        let rows: Vec<(&str, &str)> = rows.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        store.set_settings_text(&rows)?;
        let mut trust = self.trust_mut();
        for (fp, issued) in &changed {
            trust.issued.insert(*fp, issued.clone());
        }
        Ok(changed.len())
    }

    /// Starts a CA rotation: a new CA signs every certificate from now on,
    /// and the old one stays trusted for [`ROTATION_OVERLAP_SECS`]. The
    /// coordinator keeps presenting a certificate from the old CA until the
    /// overlap ends, so workers that haven't renewed yet still recognise it.
    pub fn rotate_ca(
        &mut self,
        store: &mut Store,
        secrets: &SecretStore,
        now: i64,
    ) -> Result<(), ClusterError> {
        if self.previous.is_some() {
            return Err(ClusterError::RotationInProgress);
        }
        let next = Authority::generate(now)?;
        let until = now + ROTATION_OVERLAP_SECS;
        let ca = format!("v1 {}", authority_value(&next, secrets)?);
        let previous = format!("v1 {} {until}", authority_value(&self.current, secrets)?);
        store.set_settings_text(&[(KEY_CA, &ca), (KEY_CA_PREVIOUS, &previous)])?;
        let old = std::mem::replace(&mut self.current, next);
        self.previous = Some((old, until));
        self.rebuild_trust()
    }

    /// Hourly upkeep:
    /// - ends a rotation whose overlap is over (once no open token still
    ///   names the old CA), dropping the old CA and its certificates;
    /// - renews the coordinator's certificate at half-life, or onto the
    ///   new CA when a rotation ends;
    /// - forgets expired tokens and expired certificates.
    pub fn maintain(
        &mut self,
        store: &mut Store,
        secrets: &SecretStore,
        now: i64,
    ) -> Result<(), ClusterError> {
        let mut set: Vec<(String, String)> = Vec::new();
        let mut remove: Vec<String> = Vec::new();

        let mut retired = None;
        if let Some((old, until)) = &self.previous {
            let pinned = self.tokens.values().any(|t| t.open(now) && t.ca == old.fp);
            if now >= *until && !pinned {
                retired = Some(old.fp);
                remove.push(KEY_CA_PREVIOUS.to_owned());
            }
        }
        let signer = match (&self.previous, retired) {
            (Some((old, _)), None) => old,
            _ => &self.current,
        };
        let renewed = if self.server.issuer != signer.fp
            || now >= self.server.not_after - LEAF_LIFETIME_SECS / 2
        {
            let leaf = new_server_leaf(signer, now)?;
            set.push((KEY_SERVER.to_owned(), server_value(&leaf, secrets)?));
            Some(leaf)
        } else {
            None
        };

        let expired_tokens: Vec<[u8; 8]> = self
            .tokens
            .iter()
            .filter(|(_, t)| now >= t.expires_at)
            .map(|(id, _)| *id)
            .collect();
        remove.extend(expired_tokens.iter().map(token_key));
        let dropped: Vec<(Fingerprint, [u8; 16])> = self
            .shared
            .trust
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .issued
            .iter()
            .filter(|(_, i)| now > i.not_after || Some(i.issuer) == retired)
            .map(|(fp, i)| (*fp, i.serial))
            .collect();
        remove.extend(dropped.iter().map(|(_, serial)| cert_key(serial)));

        if set.is_empty() && remove.is_empty() {
            return Ok(());
        }
        let set_refs: Vec<(&str, &str)> =
            set.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let remove_refs: Vec<&str> = remove.iter().map(String::as_str).collect();
        store.update_settings_text(&set_refs, &remove_refs)?;

        for id in &expired_tokens {
            self.tokens.remove(id);
        }
        {
            let mut trust = self.trust_mut();
            for (fp, _) in &dropped {
                trust.issued.remove(fp);
            }
        }
        if retired.is_some() {
            self.previous = None;
        }
        if let Some(leaf) = renewed {
            self.server = leaf;
        }
        self.rebuild_trust()
    }

    fn name_in_use(&self, name: &str, now: i64) -> bool {
        self.shared
            .trust
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .issued
            .values()
            .any(|i| i.name == name && !i.revoked && now <= i.not_after)
    }

    fn issue_worker(
        &self,
        worker: WorkerId,
        name: &str,
        public_key: &[u8; 32],
        now: i64,
    ) -> Result<(Fingerprint, Issued, JoinGrant), ClusterError> {
        let (serial, der, not_after) =
            self.current
                .issue(Role::Worker(name), name, public_key, now)?;
        let fp = Fingerprint::of(&der);
        let issued = Issued {
            serial,
            worker,
            name: name.to_owned(),
            issuer: self.current.fp,
            not_after,
            revoked: false,
        };
        let grant = JoinGrant {
            worker,
            name: name.to_owned(),
            cert: der.into(),
            not_after,
            cas: self.cas(),
        };
        Ok((fp, issued, grant))
    }

    fn cas(&self) -> Vec<CertificateDer<'static>> {
        let mut cas = vec![self.current.cert.clone()];
        cas.extend(self.previous.iter().map(|(a, _)| a.cert.clone()));
        cas
    }

    fn trust_mut(&self) -> std::sync::RwLockWriteGuard<'_, Trust> {
        self.shared
            .trust
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Re-derives the trusted CAs and the served certificate from
    /// `self` after a rotation or renewal.
    fn rebuild_trust(&mut self) -> Result<(), ClusterError> {
        let key = certified_key(&self.server, &self.current, &self.previous)?;
        {
            let mut trust = self.trust_mut();
            let issued = std::mem::take(&mut trust.issued);
            *trust = Trust::new(self.cas(), self.current.fp, issued)?;
        }
        *self
            .shared
            .server
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Arc::new(key);
        Ok(())
    }
}

/// Spends a token and records its certificate in one write transaction
/// that first re-reads the stored token row and the next worker id, so two
/// coordinators on one database (`serve` and a CLI command, say) can't
/// both spend it or hand out the same id. `rows` gives the rows to write
/// for the worker id taken. `None`: the token was already spent.
fn spend_token(
    store: &mut Store,
    key: &str,
    unspent: &str,
    rows: impl FnOnce(WorkerId) -> [(String, String); 2],
) -> Result<Option<WorkerId>, ClusterError> {
    use lodestar_store::rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
    // IMMEDIATE takes the write lock before the reads, so they can't go
    // stale before the writes.
    let tx = Transaction::new_unchecked(store.connection(), TransactionBehavior::Immediate)?;
    let read = |k: &str| -> Result<Option<String>, ClusterError> {
        Ok(tx
            .query_row("SELECT value FROM settings WHERE key = ?1", [k], |r| {
                r.get(0)
            })
            .optional()?)
    };
    if read(key)?.as_deref() != Some(unspent) {
        return Ok(None);
    }
    let worker: u32 = read(KEY_NEXT_WORKER)?
        .ok_or_else(corrupt)?
        .parse()
        .map_err(|_| corrupt())?;
    let after = worker.checked_add(1).ok_or_else(corrupt)?;
    let mut upsert = tx.prepare(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )?;
    for (k, v) in rows(WorkerId(worker)) {
        upsert.execute(params![k, v])?;
    }
    upsert.execute(params![KEY_NEXT_WORKER, after.to_string()])?;
    drop(upsert);
    tx.commit()?;
    Ok(Some(WorkerId(worker)))
}

fn new_server_leaf(ca: &Authority, now: i64) -> Result<ServerLeaf, ClusterError> {
    let key = KeyPair::generate()?;
    let (_, der, not_after) = ca.issue(
        Role::Coordinator,
        "Lodestar coordinator",
        &key.public_key(),
        now,
    )?;
    Ok(ServerLeaf {
        cert: der.into(),
        key,
        not_after,
        issuer: ca.fp,
    })
}

/// The served chain: the leaf, then its CA (the join check needs it).
fn certified_key(
    server: &ServerLeaf,
    current: &Authority,
    previous: &Option<(Authority, i64)>,
) -> Result<CertifiedKey, ClusterError> {
    let issuer = std::iter::once(current)
        .chain(previous.iter().map(|(a, _)| a))
        .find(|a| a.fp == server.issuer)
        .ok_or_else(corrupt)?;
    Ok(CertifiedKey::new(
        vec![server.cert.clone(), issuer.cert.clone()],
        signing_key(&server.key)?,
    ))
}

fn ca_context(fp: &Fingerprint) -> String {
    format!("cluster:ca:{fp}")
}

fn server_context(cert: &[u8]) -> String {
    format!("cluster:server:{}", Fingerprint::of(cert))
}

pub(crate) fn seal_key(
    secrets: &SecretStore,
    context: &str,
    key: &KeyPair,
) -> Result<String, ClusterError> {
    let plain = Secret::new(hex::encode(key.pkcs8()));
    Ok(hex::encode(secrets.seal(context, &plain)?.as_bytes()))
}

pub(crate) fn open_key(
    secrets: &SecretStore,
    context: &str,
    sealed: &str,
) -> Result<KeyPair, ClusterError> {
    let sealed = SealedSecret::from_bytes(hex::decode(sealed).ok_or_else(corrupt)?)?;
    let plain = secrets.open(context, &sealed)?;
    let pkcs8 = Zeroizing::new(hex::decode(plain.expose()).ok_or_else(corrupt)?);
    KeyPair::from_pkcs8(pkcs8)
}

fn authority_value(ca: &Authority, secrets: &SecretStore) -> Result<String, ClusterError> {
    let sealed = seal_key(secrets, &ca_context(&ca.fp), &ca.key)?;
    Ok(format!("{} {sealed}", hex::encode(&ca.cert)))
}

fn load_authority(
    cert: &str,
    sealed: &str,
    secrets: &SecretStore,
) -> Result<Authority, ClusterError> {
    let cert = cert_from_hex(cert)?;
    let fp = Fingerprint::of(&cert);
    let key = open_key(secrets, &ca_context(&fp), sealed)?;
    check_certifies(&cert, &key)?;
    Ok(Authority { cert, key, fp })
}

fn server_value(leaf: &ServerLeaf, secrets: &SecretStore) -> Result<String, ClusterError> {
    let sealed = seal_key(secrets, &server_context(&leaf.cert), &leaf.key)?;
    Ok(format!(
        "v1 {} {sealed} {} {}",
        hex::encode(&leaf.cert),
        leaf.not_after,
        leaf.issuer
    ))
}

fn token_key(id: &[u8; 8]) -> String {
    format!("cluster.token.{}", hex::encode(id))
}

fn token_value(t: &TokenRecord) -> String {
    let mut out = format!(
        "v1 {} {} {} {} {}",
        hex::encode(&t.hash),
        t.name,
        t.expires_at,
        u8::from(t.used),
        t.ca
    );
    if let Some(r) = &t.redeemed {
        out.push_str(&format!(
            " {} {} {} {}",
            hex::encode(&r.public_key),
            r.worker.0,
            r.not_after,
            hex::encode(&r.cert)
        ));
    }
    out
}

fn cert_key(serial: &[u8; 16]) -> String {
    format!("cluster.cert.{}", hex::encode(serial))
}

fn cert_value(fp: &Fingerprint, i: &Issued) -> String {
    format!(
        "v1 {} {} {fp} {} {} {}",
        i.worker.0,
        i.name,
        i.issuer,
        i.not_after,
        u8::from(i.revoked)
    )
}

pub(crate) fn cert_from_hex(text: &str) -> Result<CertificateDer<'static>, ClusterError> {
    Ok(hex::decode(text).ok_or_else(corrupt)?.into())
}

pub(crate) fn check_certifies(cert: &[u8], key: &KeyPair) -> Result<(), ClusterError> {
    if x509::certifies(cert, &key.public_key()) {
        Ok(())
    } else {
        Err(ClusterError::Corrupt("a key doesn't match its certificate"))
    }
}

fn fingerprint(text: &str) -> Result<Fingerprint, ClusterError> {
    Fingerprint::from_hex(text).ok_or_else(corrupt)
}

pub(crate) fn int(text: &str) -> Result<i64, ClusterError> {
    text.parse().map_err(|_| corrupt())
}

fn flag(text: &str) -> Result<bool, ClusterError> {
    match text {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(corrupt()),
    }
}

pub(crate) fn corrupt() -> ClusterError {
    ClusterError::Corrupt("cluster record")
}

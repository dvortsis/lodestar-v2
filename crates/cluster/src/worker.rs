//! The worker's side: joining with a token, its credentials, renewal.
//!
//! 1. [`Joining::new`] takes a parsed token and makes the worker's key
//!    pair. The private key never leaves the worker.
//! 2. The worker dials the cluster port with [`Joining::client_config`],
//!    which trusts only a chain to the CA whose fingerprint the token
//!    carries, and sends the token and its public key.
//! 3. [`Joining::finish`] checks the [`JoinGrant`] and yields
//!    [`Credentials`], used for every connection after that.
//!
//! **On disk** the credentials are `<data>/cluster/credentials`, a small
//! text file (mode 0600 on Unix), written atomically:
//!
//! ```text
//! LWC1
//! worker <id>
//! name <name>
//! not_after <unix seconds>
//! cert <hex DER>
//! ca <hex DER>            (one line per trusted CA)
//! key <hex sealed key>
//! ```
//!
//! The key is sealed with the worker's own secrets key (`secrets.key` in its
//! data directory), with the certificate's fingerprint as context.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lodestar_integrations::SecretStore;
use rustls::ClientConfig;
use rustls::pki_types::{CertificateDer, UnixTime};
use rustls::server::WebPkiClientVerifier;
use rustls::time_provider::TimeProvider;

use crate::coordinator::{
    JoinGrant, LEAF_LIFETIME_SECS, cert_from_hex, check_certifies, corrupt, int, open_key, seal_key,
};
use crate::{ClusterError, Fingerprint, JoinToken, KeyPair, WorkerId, hex, tls};

const FILE: &str = "credentials";
const MAGIC: &str = "LWC1";

/// A worker partway through joining.
#[derive(Debug)]
pub struct Joining {
    token: JoinToken,
    key: KeyPair,
}

impl Joining {
    pub fn new(token: JoinToken) -> Result<Joining, ClusterError> {
        Ok(Joining {
            token,
            key: KeyPair::generate()?,
        })
    }

    pub fn token(&self) -> &JoinToken {
        &self.token
    }

    /// What the coordinator signs.
    pub fn public_key(&self) -> [u8; 32] {
        self.key.public_key()
    }

    /// TLS for the join connection: TLS 1.3, no client certificate, and the
    /// coordinator must present a chain to the CA named in the token.
    pub fn client_config(
        &self,
        time: Option<Arc<dyn TimeProvider>>,
    ) -> Result<Arc<ClientConfig>, ClusterError> {
        tls::join_client_config(self.token.ca_fingerprint(), time)
    }

    /// Checks the grant against what was asked for: the token's CA is in
    /// the bundle, and the certificate is for this key and chains to it.
    pub fn finish(self, grant: JoinGrant, now: i64) -> Result<Credentials, ClusterError> {
        let pinned = self.token.ca_fingerprint();
        if !grant.cas.iter().any(|ca| Fingerprint::of(ca) == pinned) {
            return Err(ClusterError::BadGrant);
        }
        Credentials::accept(self.key, grant, now)
    }
}

/// A key pair waiting for its renewed certificate.
#[derive(Debug)]
pub struct Renewal {
    key: KeyPair,
}

impl Renewal {
    pub fn public_key(&self) -> [u8; 32] {
        self.key.public_key()
    }
}

/// A joined worker's identity: its key, its certificate and the CA bundle
/// it trusts. It never prints its key.
pub struct Credentials {
    worker: WorkerId,
    name: String,
    key: KeyPair,
    cert: CertificateDer<'static>,
    not_after: i64,
    cas: Vec<CertificateDer<'static>>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("worker", &self.worker)
            .field("name", &self.name)
            .field("not_after", &self.not_after)
            .finish_non_exhaustive()
    }
}

impl Credentials {
    fn accept(key: KeyPair, grant: JoinGrant, now: i64) -> Result<Credentials, ClusterError> {
        if grant.cas.is_empty() || !crate::x509::certifies(&grant.cert, &key.public_key()) {
            return Err(ClusterError::BadGrant);
        }
        let roots = {
            let mut store = rustls::RootCertStore::empty();
            for ca in &grant.cas {
                store.add(ca.clone()).map_err(|_| ClusterError::BadGrant)?;
            }
            Arc::new(store)
        };
        let verifier = WebPkiClientVerifier::builder_with_provider(roots, tls::provider())
            .build()
            .map_err(|e| ClusterError::Verifier(e.to_string()))?;
        let at = UnixTime::since_unix_epoch(std::time::Duration::from_secs(
            u64::try_from(now).map_err(|_| ClusterError::BadGrant)?,
        ));
        verifier
            .verify_client_cert(&grant.cert, &[], at)
            .map_err(|_| ClusterError::BadGrant)?;
        Ok(Credentials {
            worker: grant.worker,
            name: grant.name,
            key,
            cert: grant.cert,
            not_after: grant.not_after,
            cas: grant.cas,
        })
    }

    pub fn worker(&self) -> WorkerId {
        self.worker
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn not_after(&self) -> i64 {
        self.not_after
    }

    /// TLS for cluster connections: TLS 1.3, this worker's certificate,
    /// and only the cluster CAs as roots. Connect with
    /// [`coordinator_server_name`](crate::coordinator_server_name).
    pub fn client_config(
        &self,
        time: Option<Arc<dyn TimeProvider>>,
    ) -> Result<Arc<ClientConfig>, ClusterError> {
        tls::cluster_client_config(&self.cas, self.cert.clone(), &self.key, time)
    }

    /// Half the certificate's life is gone.
    pub fn needs_renewal(&self, now: i64) -> bool {
        now >= self.not_after - LEAF_LIFETIME_SECS / 2
    }

    /// A new key pair for the renewal; send its public key.
    pub fn begin_renewal(&self) -> Result<Renewal, ClusterError> {
        Ok(Renewal {
            key: KeyPair::generate()?,
        })
    }

    /// Takes the renewed certificate and the CA bundle that came with it,
    /// which carries a rotated CA to the worker.
    pub fn renewed(
        &self,
        renewal: Renewal,
        grant: JoinGrant,
        now: i64,
    ) -> Result<Credentials, ClusterError> {
        if grant.worker != self.worker || grant.name != self.name {
            return Err(ClusterError::BadGrant);
        }
        Credentials::accept(renewal.key, grant, now)
    }

    fn path(dir: &Path) -> PathBuf {
        dir.join(FILE)
    }

    /// Writes the credentials to `dir/credentials` (`dir` is
    /// `<data>/cluster`), replacing the old file atomically.
    pub fn save(&self, dir: &Path, secrets: &SecretStore) -> Result<(), ClusterError> {
        let sealed = seal_key(secrets, &key_context(&self.cert), &self.key)?;
        let mut text = format!(
            "{MAGIC}\nworker {}\nname {}\nnot_after {}\ncert {}\n",
            self.worker.0,
            self.name,
            self.not_after,
            hex::encode(&self.cert)
        );
        for ca in &self.cas {
            text.push_str(&format!("ca {}\n", hex::encode(ca)));
        }
        text.push_str(&format!("key {sealed}\n"));

        let io = |op: &'static str| move |e: std::io::Error| ClusterError::Io(op, e.kind());
        fs::create_dir_all(dir).map_err(io("create the cluster folder"))?;
        let tmp = dir.join(format!("{FILE}.tmp"));
        // A leftover from a crash may have looser permissions, which
        // `mode` wouldn't change on an existing file.
        match fs::remove_file(&tmp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(io("remove a stale credentials file")(e));
            }
            _ => {}
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp).map_err(io("write the credentials"))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(io("write the credentials"))?;
        fs::rename(&tmp, Credentials::path(dir)).map_err(io("replace the credentials"))?;
        // Make the rename itself durable.
        #[cfg(unix)]
        fs::File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(io("write the credentials"))?;
        Ok(())
    }

    /// Reads `dir/credentials`, or `None` if the worker hasn't joined.
    pub fn load(dir: &Path, secrets: &SecretStore) -> Result<Option<Credentials>, ClusterError> {
        let text = match fs::read_to_string(Credentials::path(dir)) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ClusterError::Io("read the credentials", e.kind())),
        };
        let mut lines = text.lines();
        if lines.next() != Some(MAGIC) {
            return Err(corrupt());
        }
        let mut field = |name: &str| -> Result<&str, ClusterError> {
            lines
                .next()
                .and_then(|l| l.strip_prefix(name))
                .and_then(|l| l.strip_prefix(' '))
                .ok_or_else(corrupt)
        };
        let worker = WorkerId(field("worker")?.parse().map_err(|_| corrupt())?);
        let name = field("name")?.to_owned();
        if !crate::join::valid_worker_name(&name) {
            return Err(corrupt());
        }
        let not_after = int(field("not_after")?)?;
        let cert = cert_from_hex(field("cert")?)?;
        let mut cas = Vec::new();
        let mut sealed = None;
        for line in lines {
            match line.split_once(' ') {
                Some(("ca", value)) if sealed.is_none() => cas.push(cert_from_hex(value)?),
                Some(("key", value)) if sealed.is_none() => sealed = Some(value),
                _ => return Err(corrupt()),
            }
        }
        let key = open_key(secrets, &key_context(&cert), sealed.ok_or_else(corrupt)?)?;
        check_certifies(&cert, &key)?;
        if cas.is_empty() {
            return Err(corrupt());
        }
        Ok(Some(Credentials {
            worker,
            name,
            key,
            cert,
            not_after,
            cas,
        }))
    }
}

fn key_context(cert: &[u8]) -> String {
    format!("cluster:worker:{}", Fingerprint::of(cert))
}

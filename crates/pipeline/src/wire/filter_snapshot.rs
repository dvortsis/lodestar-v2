//! What a remote worker judges with (architecture §19.4, §19.6, §20; plan
//! `docs/plan/2026-11-06-v06-workers.md` WK-8 and Q1).
//!
//! - **[`FilterSnapshot`]:** a versioned copy of exactly what the commit
//!   judges with: the floor (CS-1 entries, CS-8 signatures, the floor-marked
//!   lists), your core switches, packs, your rules, exceptions and
//!   overrides, classification rules, SP-4's signature rows and the
//!   subscribed content lists. It travels as the store's rows, and the
//!   worker builds its engine from them with the coordinator's own loader
//!   ([`crate::load_filter`]), so the two can't read a rule differently.
//! - **[`DictSnapshot`]:** the coordinator's active file-tree dictionary
//!   (the store's `file_dict`), whose id is its version.
//! - Both are chunked over WK-0's `Snapshot` messages ([`chunk`],
//!   [`Incoming`]), in order and paced by acknowledgements.
//! - **[`Judge`]:** judges every prepared torrent before it's sent or
//!   spooled. Per §20 action:
//!   - a **hash-list drop** (the floor's CS-1a/1b and CS-8a, a subscribed
//!     list that drops) is dropped on the worker, counted and reported as
//!     [`Loss::Blocked`]: never sent, never spooled;
//!   - **anything else the commit would hold back** (quarantine, the
//!     adult hold, a drop by one of your rules or a pack) goes to the
//!     coordinator **sealed** ([`SealedRecord`]): the whole record, name,
//!     file tree and all, under the snapshot's [`WireKey`], so it stays
//!     reviewable and correctable at commit while its name is never in
//!     clear on the wire or in the spool;
//!   - **hide, demote and flag** are only marked; the record goes as is.
//!
//! The coordinator re-judges every remote result regardless (§19.4), so a
//! stale snapshot, or a worker that lies, can't get a record past the
//! rules it has now.
//!
//! **Decoding is bounded and all-or-nothing**, like the rest of `wire`:
//! every count and length is checked against its cap and the bytes left
//! before anything is allocated, an unknown version, flag or tag is
//! refused, and a decode returns the whole snapshot or nothing. A chunk out
//! of order drops the assembly, so a half-received snapshot is never used.
//!
//! **Nothing here prints a name or a key:** the `Debug` of every type that
//! holds one shows its lengths and ids only, and every error is a fixed
//! string.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use lodestar_cluster::proto::{MAX_CHUNK, MAX_CHUNKS, SnapshotAck, SnapshotChunk, SnapshotKind};
use lodestar_cluster::{Token, WorkerId};
use lodestar_filter::{Action, Engine, Handle, ListLayer, RuleRef, Sealer};
use lodestar_metainfo::InfoHash;
use lodestar_sources::Loss;
use lodestar_store::{
    ExceptionRow, FilterState, OverrideRow, PackVersionRows, RawSetting, RuleRow, RuleSet,
    StoredText,
};

use super::{Error, Reader, Result, ResultRecord, ResultsBatch, Writer};
use crate::StoreFilter;
use crate::stages::{Closed, Outbox, Reports, Results, Weigh};

/// The filter snapshot's format version; the only one this build reads.
pub const VERSION: u8 = 1;
/// The dictionary snapshot's format version.
pub const DICT_VERSION: u8 = 1;

/// The largest filter snapshot, assembled: 64 MiB, about three million
/// listed hashes. A worker uses at most 256 MB in all (owner §1.6.5).
pub const MAX_SNAPSHOT: usize = 64 << 20;
/// The largest dictionary (a trained one is about 110 KiB).
pub const MAX_DICT: usize = 4 << 20;
/// A query, a setting's value or an override's value.
pub const MAX_TEXT: usize = 64 << 10;
/// A rule id, pack name or setting key.
pub const MAX_ID: usize = 256;
/// Rows in any one table.
pub const MAX_ROWS: usize = 1 << 20;
/// Subscribed content lists.
pub const MAX_LISTS: usize = 4096;
/// A sealed record: one record of the `Results` codec, plus the seal.
pub const MAX_SEALED: usize = super::MAX_PAYLOAD;

const _: () = assert!(MAX_SNAPSHOT.div_ceil(MAX_CHUNK) <= MAX_CHUNKS as usize);

/// The key a worker seals held-back records under. The coordinator derives
/// it per day from its own seal key (`crates/lodestar/src/cluster/filter.rs`)
/// and sends the current one in the snapshot; the worker keeps it in
/// memory only. Its `Debug` shows the id, never the key.
#[derive(Clone, PartialEq, Eq)]
pub struct WireKey {
    pub id: u32,
    key: [u8; 32],
}

impl WireKey {
    pub fn new(id: u32, key: [u8; 32]) -> WireKey {
        WireKey { id, key }
    }

    fn sealer(&self) -> Sealer {
        Sealer::new(&self.key)
    }
}

impl Drop for WireKey {
    /// Best effort without `unsafe` or a new dependency: the bytes are
    /// overwritten and the fence keeps the write from being reordered away
    /// before the memory is freed.
    fn drop(&mut self) {
        self.key.fill(0);
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
    }
}

impl std::fmt::Debug for WireKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireKey")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// A subscribed content list, as `lodestar_filter::lists::apply` applies
/// it: a floor-marked one is CS-1b, any other sits in the pack layer with
/// its action.
#[derive(Clone, PartialEq, Eq)]
pub struct ContentList {
    pub id: u32,
    pub floor: bool,
    /// The subscription's action code, as stored.
    pub action: u8,
    pub hashes: Vec<[u8; 20]>,
}

impl std::fmt::Debug for ContentList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentList")
            .field("id", &self.id)
            .field("floor", &self.floor)
            .field("action", &self.action)
            .field("hashes", &self.hashes.len())
            .finish()
    }
}

/// What the commit judges with, for a worker.
#[derive(Clone)]
pub struct FilterSnapshot {
    pub key: WireKey,
    /// The store's rows. `state.held` (tombstones and quarantine) never
    /// travels: the commit, not the worker, refuses held keys.
    pub filter: StoreFilter,
    pub lists: Vec<ContentList>,
}

impl std::fmt::Debug for FilterSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = &self.filter;
        f.debug_struct("FilterSnapshot")
            .field("key", &self.key)
            .field("cs_hashes", &s.state.cs_hashes.len())
            .field("cs_sigs", &s.state.cs_sigs.len())
            .field("switches", &s.state.switches.len())
            .field("rules", &s.rules.rules.len())
            .field("class_rules", &s.class_rules.len())
            .field("spam_sigs", &s.spam_sigs.len())
            .field("lists", &self.lists)
            .finish_non_exhaustive()
    }
}

impl FilterSnapshot {
    /// The payload. Refuses what the decoder would refuse.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let f = &self.filter;
        let r = &f.rules;
        let mut w = Writer::default();
        w.u8(VERSION);
        w.uvar(u64::from(self.key.id));
        w.fixed(&self.key.key);
        count(&mut w, f.state.cs_hashes.len())?;
        for (h, origin) in &f.state.cs_hashes {
            w.fixed(h);
            w.u8(*origin);
        }
        count(&mut w, f.state.cs_sigs.len())?;
        for s in &f.state.cs_sigs {
            w.uvar(*s);
        }
        switches(&mut w, &f.state.switches)?;
        count(&mut w, r.rules.len())?;
        for x in &r.rules {
            w.ivar(x.id);
            text(&mut w, &x.query, MAX_TEXT)?;
            w.u8(x.action);
            w.u8(x.enabled.into());
            opt_ivar(&mut w, x.shadow_until);
            opt_text(&mut w, x.unless_query.as_deref())?;
        }
        switches(&mut w, &r.pack_switches)?;
        count(&mut w, r.overrides.len())?;
        for o in &r.overrides {
            w.ivar(o.id);
            w.u8(o.kind);
            bytes(&mut w, &o.value, MAX_TEXT)?;
            w.u8(o.strength);
            opt_text(&mut w, o.scope.as_deref())?;
            w.ivar(o.created_at);
            opt_ivar(&mut w, o.expires_at);
            opt_ivar(&mut w, o.shadow_until);
            w.ivar(o.hits);
        }
        count(&mut w, r.exceptions.len())?;
        for e in &r.exceptions {
            w.ivar(e.id);
            text(&mut w, &e.rule, MAX_ID)?;
            w.u8(e.kind);
            bytes(&mut w, &e.value, MAX_TEXT)?;
        }
        count(&mut w, r.allow.len())?;
        for (id, q) in &r.allow {
            w.ivar(*id);
            text(&mut w, q, MAX_TEXT)?;
        }
        for t in r
            .pack_versions
            .applied
            .iter()
            .chain([&r.pack_versions.seeded])
        {
            stored(&mut w, t)?;
        }
        settings(&mut w, &r.pack_overrides)?;
        count(&mut w, f.class_rules.len())?;
        for (id, q, adult) in &f.class_rules {
            w.ivar(*id);
            text(&mut w, q, MAX_TEXT)?;
            w.u8((*adult).into());
        }
        settings(&mut w, &f.spam_sigs)?;
        if self.lists.len() > MAX_LISTS {
            return Err(Error::TooLarge("lists"));
        }
        let mut ids = HashSet::new();
        w.uvar(self.lists.len() as u64);
        for l in &self.lists {
            if !ids.insert(l.id) {
                return Err(Error::Invalid("duplicate list"));
            }
            w.uvar(u64::from(l.id));
            w.u8(l.floor.into());
            w.u8(l.action);
            w.uvar(l.hashes.len() as u64);
            for h in &l.hashes {
                w.fixed(h);
            }
            if w.out.len() > MAX_SNAPSHOT {
                return Err(Error::TooLarge("snapshot"));
            }
        }
        if w.out.len() > MAX_SNAPSHOT {
            return Err(Error::TooLarge("snapshot"));
        }
        Ok(w.out)
    }

    /// The whole snapshot, or an error and nothing.
    pub fn decode(payload: &[u8]) -> Result<FilterSnapshot> {
        if payload.len() > MAX_SNAPSHOT {
            return Err(Error::TooLarge("snapshot"));
        }
        let mut r = Reader { rest: payload };
        let v = r.u8()?;
        if v != VERSION {
            return Err(Error::Version(v));
        }
        let id = u32::try_from(r.uvar()?).map_err(|_| Error::Invalid("key id"))?;
        let key = WireKey::new(id, r.fixed()?);
        let n = r.count(MAX_ROWS, 21, "cs hashes")?;
        let mut cs_hashes = Vec::with_capacity(n);
        for _ in 0..n {
            cs_hashes.push((r.fixed()?, r.u8()?));
        }
        let n = r.count(MAX_ROWS, 1, "cs signatures")?;
        let mut cs_sigs = Vec::with_capacity(n);
        for _ in 0..n {
            cs_sigs.push(r.uvar()?);
        }
        let switches = read_switches(&mut r)?;
        let n = r.count(MAX_ROWS, 6, "rules")?;
        let mut rules = Vec::with_capacity(n);
        for _ in 0..n {
            rules.push(RuleRow {
                id: r.ivar()?,
                query: read_text(&mut r, MAX_TEXT, "query")?,
                action: r.u8()?,
                enabled: read_bool(&mut r)?,
                shadow_until: read_opt_ivar(&mut r)?,
                unless_query: read_opt_text(&mut r)?,
            });
        }
        let pack_switches = read_switches(&mut r)?;
        let n = r.count(MAX_ROWS, 9, "overrides")?;
        let mut overrides = Vec::with_capacity(n);
        for _ in 0..n {
            overrides.push(OverrideRow {
                id: r.ivar()?,
                kind: r.u8()?,
                value: r.bytes(MAX_TEXT, "override")?.to_vec(),
                strength: r.u8()?,
                scope: read_opt_text(&mut r)?,
                created_at: r.ivar()?,
                expires_at: read_opt_ivar(&mut r)?,
                shadow_until: read_opt_ivar(&mut r)?,
                hits: r.ivar()?,
            });
        }
        let n = r.count(MAX_ROWS, 4, "exceptions")?;
        let mut exceptions = Vec::with_capacity(n);
        for _ in 0..n {
            exceptions.push(ExceptionRow {
                id: r.ivar()?,
                rule: read_text(&mut r, MAX_ID, "rule id")?,
                kind: r.u8()?,
                value: r.bytes(MAX_TEXT, "exception")?.to_vec(),
            });
        }
        let n = r.count(MAX_ROWS, 2, "allow")?;
        let mut allow = Vec::with_capacity(n);
        for _ in 0..n {
            allow.push((r.ivar()?, read_text(&mut r, MAX_TEXT, "query")?));
        }
        let pack_versions = PackVersionRows {
            applied: [read_stored(&mut r)?, read_stored(&mut r)?],
            seeded: read_stored(&mut r)?,
        };
        let pack_overrides = read_settings(&mut r)?;
        let n = r.count(MAX_ROWS, 3, "class rules")?;
        let mut class_rules = Vec::with_capacity(n);
        for _ in 0..n {
            class_rules.push((
                r.ivar()?,
                read_text(&mut r, MAX_TEXT, "query")?,
                read_bool(&mut r)?,
            ));
        }
        let spam_sigs = read_settings(&mut r)?;
        let n = r.count(MAX_LISTS, 4, "lists")?;
        let mut lists = Vec::with_capacity(n);
        let mut ids = HashSet::with_capacity(n);
        for _ in 0..n {
            let id = u32::try_from(r.uvar()?).map_err(|_| Error::Invalid("list id"))?;
            if !ids.insert(id) {
                return Err(Error::Invalid("duplicate list"));
            }
            let floor = read_bool(&mut r)?;
            let action = r.u8()?;
            let k = r.count(MAX_SNAPSHOT / 20, 20, "list hashes")?;
            let mut hashes = Vec::with_capacity(k);
            for _ in 0..k {
                hashes.push(r.fixed()?);
            }
            lists.push(ContentList {
                id,
                floor,
                action,
                hashes,
            });
        }
        r.finish()?;
        Ok(FilterSnapshot {
            key,
            filter: StoreFilter {
                state: FilterState {
                    cs_hashes,
                    cs_sigs,
                    switches,
                    held: Vec::new(),
                },
                rules: RuleSet {
                    rules,
                    pack_switches,
                    overrides,
                    exceptions,
                    allow,
                    pack_versions,
                    pack_overrides,
                },
                class_rules,
                spam_sigs,
            },
            lists,
        })
    }

    /// The engine this snapshot describes, built as the coordinator builds
    /// its own: the shipped core, the store's rows through
    /// [`crate::load_filter`], then the content lists as
    /// `lodestar_filter::lists::apply` sets them. Returns the loader's
    /// notes for rows that don't read (each fails closed, as it does on
    /// the coordinator).
    pub fn engine(&self) -> (Engine, Vec<String>) {
        let handle = crate::default_filter();
        let notes = crate::load_filter(&handle, &self.filter);
        handle.update_engine(|e| {
            for l in &self.lists {
                let hashes = Arc::new(l.hashes.iter().copied().collect::<HashSet<_>>());
                if l.floor {
                    e.core.cs.set_list(l.id, Some(hashes));
                } else {
                    let action = Action::from_code(l.action)
                        .filter(|a| a.protective())
                        .unwrap_or(Action::Drop);
                    let layer = ListLayer {
                        id: l.id,
                        action,
                        hashes,
                    };
                    e.set_list(Some(layer), l.id);
                }
            }
        });
        (Engine::clone(&handle.current()), notes)
    }
}

/// A snapshot's version: a digest of its bytes, never 0 (`Assign`'s "none
/// yet"), so the same content always has the same version and a change
/// always a new one. Not a security check: the bytes come over mTLS.
pub fn version_of(payload: &[u8]) -> u64 {
    use std::hash::Hasher;
    let mut h = siphasher::sip::SipHasher13::new_with_keys(0x4c6f_6465_7374_6172, 0x574b_2d38);
    h.write(payload);
    h.finish().max(1)
}

/// The coordinator's active file-tree dictionary.
#[derive(Clone, PartialEq, Eq)]
pub struct DictSnapshot {
    /// Its id in the store: every blob compressed with it names it, and it
    /// is the snapshot's version.
    pub id: i64,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for DictSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DictSnapshot")
            .field("id", &self.id)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl DictSnapshot {
    /// The version `Assign` and the chunks carry: the id.
    pub fn version(&self) -> u64 {
        self.id as u64
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.id < 1 {
            return Err(Error::Invalid("dictionary id"));
        }
        let mut w = Writer::default();
        w.u8(DICT_VERSION);
        w.uvar(self.id as u64);
        bytes(&mut w, &self.bytes, MAX_DICT)?;
        Ok(w.out)
    }

    pub fn decode(payload: &[u8]) -> Result<DictSnapshot> {
        if payload.len() > MAX_DICT + 32 {
            return Err(Error::TooLarge("dictionary"));
        }
        let mut r = Reader { rest: payload };
        let v = r.u8()?;
        if v != DICT_VERSION {
            return Err(Error::Version(v));
        }
        let id = r.uvar()?;
        if id < 1 || id > i64::MAX as u64 {
            return Err(Error::Invalid("dictionary id"));
        }
        let bytes = r.bytes(MAX_DICT, "dictionary")?.to_vec();
        r.finish()?;
        Ok(DictSnapshot {
            id: id as i64,
            bytes,
        })
    }
}

/// Chunks for a payload of `len` bytes (at least one).
pub fn chunk_count(len: usize) -> u32 {
    len.div_ceil(MAX_CHUNK).max(1) as u32
}

/// Chunk `seq` of `payload`, sent as snapshot `version` of `kind`.
pub fn chunk(kind: SnapshotKind, version: u64, payload: &[u8], seq: u32) -> SnapshotChunk {
    let start = (seq as usize * MAX_CHUNK).min(payload.len());
    let end = (start + MAX_CHUNK).min(payload.len());
    SnapshotChunk {
        kind,
        version,
        seq,
        total: chunk_count(payload.len()),
        bytes: payload[start..end].to_vec(),
    }
}

/// The largest payload a worker assembles for `kind`.
fn max_for(kind: SnapshotKind) -> usize {
    match kind {
        SnapshotKind::Filter => MAX_SNAPSHOT,
        SnapshotKind::Dictionary => MAX_DICT + 32,
        // WK-3's filter travels in its own messages.
        SnapshotKind::HaveFilter => 0,
    }
}

/// What a chunk did.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Taken: send the acknowledgement.
    Ack(SnapshotAck),
    /// The last chunk: send the acknowledgement, then decode and install
    /// the payload (version, bytes). Its `Debug` shows the length only.
    Done(SnapshotAck, Complete),
}

/// An assembled payload.
#[derive(PartialEq, Eq)]
pub struct Complete {
    pub version: u64,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Complete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Complete")
            .field("version", &self.version)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// One kind's snapshot being received, chunk by chunk and strictly in
/// order. Chunk 0 starts a transfer (dropping one in progress); any chunk
/// that doesn't follow the last refuses and drops the transfer, so a
/// half-received snapshot is never used. What's installed stays in use
/// until a new one is complete.
#[derive(Debug)]
pub struct Incoming {
    kind: SnapshotKind,
    transfer: Option<Transfer>,
}

#[derive(Debug)]
struct Transfer {
    version: u64,
    total: u32,
    next: u32,
    buf: Vec<u8>,
}

impl Incoming {
    pub fn new(kind: SnapshotKind) -> Incoming {
        Incoming {
            kind,
            transfer: None,
        }
    }

    /// Takes a chunk of this kind.
    pub fn take(&mut self, c: SnapshotChunk) -> Result<Step> {
        let max = max_for(self.kind);
        if c.kind != self.kind || max == 0 {
            return Err(Error::Invalid("snapshot kind"));
        }
        if c.version == 0 {
            return Err(Error::Invalid("snapshot version"));
        }
        if c.seq == 0 {
            if c.total == 0 || c.total > chunk_count(max) {
                self.transfer = None;
                return Err(Error::TooLarge("snapshot chunks"));
            }
            self.transfer = Some(Transfer {
                version: c.version,
                total: c.total,
                next: 0,
                buf: Vec::new(),
            });
        }
        let Some(t) = self.transfer.as_mut() else {
            return Err(Error::Invalid("snapshot chunk order"));
        };
        let last = c.seq + 1 == t.total;
        let fits = if last {
            !c.bytes.is_empty() && c.bytes.len() <= MAX_CHUNK
        } else {
            c.bytes.len() == MAX_CHUNK
        };
        if c.version != t.version || c.total != t.total || c.seq != t.next || !fits {
            self.transfer = None;
            return Err(Error::Invalid("snapshot chunk order"));
        }
        if t.buf.len() + c.bytes.len() > max {
            self.transfer = None;
            return Err(Error::TooLarge("snapshot"));
        }
        t.buf.extend_from_slice(&c.bytes);
        t.next += 1;
        let ack = SnapshotAck {
            kind: self.kind,
            version: c.version,
            seq: c.seq,
        };
        if !last {
            return Ok(Step::Ack(ack));
        }
        let t = self.transfer.take().expect("a transfer");
        Ok(Step::Done(
            ack,
            Complete {
                version: t.version,
                bytes: t.buf,
            },
        ))
    }
}

/// A held-back record, sealed for the coordinator. In clear are only what
/// the commit keeps of a held-back torrent in any case and what fencing
/// needs: the leased key, the token, the rule-set version, the action and
/// the wire key's id. The rest, name, file tree, extras and enrichment
/// included, is one record of the `Results` codec sealed under a
/// [`WireKey`], bound (as AAD) to that clear header and the worker's id, so
/// it opens only under the same key, token, version, action and worker.
/// It travels in a `Results` batch under [`flag::SEALED`](super::results::flag::SEALED).
/// Its `Debug` shows the key, the action and the lengths.
#[derive(Clone, PartialEq, Eq)]
pub struct SealedRecord {
    pub key: InfoHash,
    pub token: Token,
    pub rule_version: u64,
    /// Quarantine, or a rule's drop.
    pub action: Action,
    pub key_id: u32,
    sealed: Vec<u8>,
}

impl std::fmt::Debug for SealedRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedRecord")
            .field("key", &self.key)
            .field("action", &self.action)
            .field("key_id", &self.key_id)
            .field("sealed", &self.sealed.len())
            .finish_non_exhaustive()
    }
}

/// The smallest seal: the nonce and the tag.
pub const MIN_SEAL: usize = 24 + 16;

impl SealedRecord {
    /// Seals `record` under `key` for `worker` (the id in its
    /// certificate). Refuses a record the `Results` codec would refuse, or
    /// one that isn't held back.
    pub fn seal(record: &ResultRecord, key: &WireKey, worker: WorkerId) -> Result<SealedRecord> {
        let action = record
            .action
            .filter(|a| *a >= Action::Quarantine)
            .ok_or(Error::Invalid("sealed action"))?;
        let plain = ResultsBatch {
            batch: 0,
            records: vec![record.clone()],
        }
        .encode()?;
        let mut s = SealedRecord {
            key: record.key,
            token: record.token,
            rule_version: record.rule_version,
            action,
            key_id: key.id,
            sealed: Vec::new(),
        };
        s.sealed = key
            .sealer()
            .seal_bytes(&plain, &s.aad(worker))
            .map_err(|_| Error::Invalid("seal"))?;
        if s.sealed.len() > MAX_SEALED {
            return Err(Error::TooLarge("sealed record"));
        }
        Ok(s)
    }

    /// The clear header as it's encoded, then the worker's id.
    fn aad(&self, worker: WorkerId) -> Vec<u8> {
        let mut w = Writer::default();
        self.write_header(&mut w);
        w.fixed(&worker.0.to_le_bytes());
        w.out
    }

    fn write_header(&self, w: &mut Writer) {
        w.uvar(super::results::flag::SEALED | super::results::flag::ACTION);
        w.fixed(&self.key.0);
        w.uvar(self.token.0);
        w.uvar(self.rule_version);
        w.u8(self.action.code());
        w.uvar(u64::from(self.key_id));
    }

    /// Writes the record in a `Results` batch.
    pub(super) fn write(&self, w: &mut Writer) {
        self.write_header(w);
        w.bytes(&self.sealed);
    }

    /// Reads one after its flags (`flags` already read and checked to be
    /// exactly `SEALED | ACTION`).
    pub(super) fn read(r: &mut Reader<'_>) -> Result<SealedRecord> {
        let key = InfoHash(r.fixed()?);
        let token = Token(r.uvar()?);
        let rule_version = r.uvar()?;
        let action = Action::from_code(r.u8()?)
            .filter(|a| *a >= Action::Quarantine)
            .ok_or(Error::Invalid("sealed action"))?;
        let key_id = u32::try_from(r.uvar()?).map_err(|_| Error::Invalid("key id"))?;
        let sealed = r.bytes(MAX_SEALED, "sealed record")?;
        if sealed.len() < MIN_SEAL {
            return Err(Error::Invalid("sealed record"));
        }
        Ok(SealedRecord {
            key,
            token,
            rule_version,
            action,
            key_id,
            sealed: sealed.to_vec(),
        })
    }
}

/// The wire keys the coordinator opens sealed records with: its own day's
/// and the day before (a spool holds a record for at most 24 h). Its
/// `Debug` shows the ids only.
#[derive(Default)]
pub struct WireKeys(RwLock<Vec<WireKey>>);

impl std::fmt::Debug for WireKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<u32> = self.read().iter().map(|k| k.id).collect();
        f.debug_tuple("WireKeys").field(&ids).finish()
    }
}

impl WireKeys {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Vec<WireKey>> {
        self.0.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Replaces the keys.
    pub fn set(&self, keys: Vec<WireKey>) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = keys;
    }

    /// The record inside, checked: its key id must be one of these, the
    /// seal intact under the clear header and `worker` (the authenticated
    /// session's id, never one the payload names), the inside one valid
    /// record whose key, token, rule version and action are the clear
    /// ones. Every failure is the same few words, never content. The
    /// record it returns is re-judged at commit like any remote record.
    pub fn open(&self, s: &SealedRecord, worker: WorkerId) -> Result<ResultRecord> {
        let sealer = self
            .read()
            .iter()
            .find(|k| k.id == s.key_id)
            .map(WireKey::sealer)
            .ok_or(Error::Invalid("wire key"))?;
        let plain = sealer
            .open_bytes(&s.sealed, &s.aad(worker))
            .map_err(|_| Error::Invalid("seal"))?;
        let mut batch = ResultsBatch::decode(&plain)?;
        if batch.records.len() != 1 {
            return Err(Error::Invalid("sealed record"));
        }
        let record = batch.records.pop().expect("one record");
        if record.key != s.key
            || record.token != s.token
            || record.rule_version != s.rule_version
            || record.action != Some(s.action)
        {
            return Err(Error::Invalid("sealed record"));
        }
        Ok(record)
    }
}

/// What the worker queues for the coordinator (or its spool).
#[derive(Clone, PartialEq, Eq)]
#[allow(
    clippy::large_enum_variant,
    reason = "most are records in clear; boxing them would cost an allocation each"
)]
pub enum Outbound {
    /// No decision, or only marks (hide, demote, flag).
    Record(ResultRecord),
    /// Held back at commit: quarantine, the adult hold, a rule's drop.
    Sealed(SealedRecord),
}

impl Outbound {
    pub fn key(&self) -> InfoHash {
        match self {
            Outbound::Record(r) => r.key,
            Outbound::Sealed(s) => s.key,
        }
    }
}

impl std::fmt::Debug for Outbound {
    /// The key and the shape; never a name, even of a record in clear.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outbound::Record(r) => f
                .debug_struct("Record")
                .field("key", &r.key)
                .field("action", &r.action)
                .finish_non_exhaustive(),
            Outbound::Sealed(s) => s.fmt(f),
        }
    }
}

impl Weigh for Outbound {
    fn weigh(&self) -> usize {
        size_of::<Self>()
            + match self {
                Outbound::Record(r) => {
                    let t = &r.torrent;
                    let vec = |v: &Option<Vec<u8>>| v.as_ref().map_or(0, Vec::capacity);
                    t.name.capacity()
                        + t.name_raw.as_ref().map_or(0, |(b, _)| b.capacity())
                        + t.files.data.capacity()
                        + vec(&t.extra)
                        + vec(&t.raw)
                        + t.enrichment
                            .as_ref()
                            .map_or(0, |e| e.comp.capacity() + e.data.capacity())
                }
                Outbound::Sealed(s) => s.sealed.capacity(),
            }
    }
}

/// What [`Judge::judge`] made of a prepared torrent.
#[allow(
    clippy::large_enum_variant,
    reason = "made and matched at once, one per prepared torrent"
)]
pub enum Judged {
    /// A hash-list drop: never sent or spooled. Report the lease lost as
    /// [`Loss::Blocked`].
    Dropped {
        key: InfoHash,
        token: Token,
        rule: RuleRef,
    },
    /// Queue it for the coordinator.
    Send(Outbound),
    /// It breaks the `Results` codec's rules, so the coordinator would
    /// refuse it: report the lease lost as [`Loss::Invalid`].
    Invalid { key: InfoHash, token: Token },
    /// No snapshot is installed yet, so nothing may be judged: report the
    /// lease lost as [`Loss::Blocked`] (fail closed). The worker's runtime
    /// starts fetching only once a snapshot is in (WK-10).
    NotReady { key: InfoHash, token: Token },
}

impl std::fmt::Debug for Judged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Judged::Dropped { key, rule, .. } => f
                .debug_struct("Dropped")
                .field("key", key)
                .field("rule", rule)
                .finish_non_exhaustive(),
            Judged::Send(o) => f.debug_tuple("Send").field(o).finish(),
            Judged::Invalid { key, .. } => f.debug_struct("Invalid").field("key", key).finish(),
            Judged::NotReady { key, .. } => f.debug_struct("NotReady").field("key", key).finish(),
        }
    }
}

/// The judge's counters.
#[derive(Debug, Default)]
pub struct JudgeCounts {
    /// Hash-list drops, never sent.
    pub dropped: AtomicU64,
    /// Held-back records sent sealed.
    pub sealed: AtomicU64,
    /// Records sent with a mark (hide, demote, flag).
    pub marked: AtomicU64,
    /// Records sent with no decision.
    pub clear: AtomicU64,
    /// Judged again because a new snapshot came in after prepare.
    pub rejudged: AtomicU64,
    /// Refused before a snapshot was installed, or by the codec.
    pub refused: AtomicU64,
}

#[derive(Debug, Clone)]
struct Installed {
    version: u64,
    /// The worker engine's version this snapshot runs as.
    engine: u64,
    key: WireKey,
}

/// A worker's filter: the engine its prepare stage judges with
/// ([`Judge::filter`], handed to `WorkerStages`), the snapshot it was
/// built from, and the judgment of every prepared torrent before it goes
/// out ([`Judge::judge`], or [`Judging`] around the outbound queue).
pub struct Judge {
    /// The id in this worker's certificate: sealed records are bound to it.
    worker: WorkerId,
    filter: Arc<Handle>,
    installed: RwLock<Option<Installed>>,
    pub counts: JudgeCounts,
}

/// The worker and the installed version, never the engine (its rules).
impl std::fmt::Debug for Judge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Judge")
            .field("worker", &self.worker)
            .field("version", &self.version())
            .field("counts", &self.counts)
            .finish_non_exhaustive()
    }
}

impl Judge {
    /// No snapshot yet: [`Judge::judge`] refuses everything until one is
    /// installed.
    pub fn new(worker: WorkerId) -> Judge {
        Judge {
            worker,
            filter: Arc::new(crate::default_filter()),
            installed: RwLock::new(None),
            counts: JudgeCounts::default(),
        }
    }

    /// The engine the prepare stage judges with.
    pub fn filter(&self) -> Arc<Handle> {
        self.filter.clone()
    }

    /// The installed snapshot's version, 0 before one is.
    pub fn version(&self) -> u64 {
        self.installed
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(0, |i| i.version)
    }

    /// Builds the engine from `snapshot` (version `version`) and swaps it
    /// in. Judgments already running finish on the old engine, and
    /// [`Judge::judge`] judges their results again. Returns the loader's
    /// notes for rows that don't read.
    pub fn install(&self, snapshot: &FilterSnapshot, version: u64) -> Vec<String> {
        let (built, notes) = snapshot.engine();
        let mut installed = self.installed.write().unwrap_or_else(|e| e.into_inner());
        // Keep the version counting up, so a result judged under the old
        // engine never looks current.
        self.filter.update_engine(|e| {
            let v = e.version;
            *e = built;
            e.version = v;
        });
        *installed = Some(Installed {
            version,
            engine: self.filter.current().version,
            key: snapshot.key.clone(),
        });
        notes
    }

    /// Judges one prepared torrent (see the module docs). A result the
    /// prepare stage judged under an older engine is judged again with the
    /// installed one first, as the commit would.
    pub fn judge(&self, mut r: Results) -> Judged {
        let installed = self.installed.read().unwrap_or_else(|e| e.into_inner());
        let Some(inst) = installed.as_ref() else {
            self.counts.refused.fetch_add(1, Ordering::Relaxed);
            return Judged::NotReady {
                key: r.key,
                token: r.token,
            };
        };
        if r.version != inst.engine {
            let engine = self.filter.current();
            let at = r.metadata_at;
            r.verdict = crate::writer::rejudge(&engine, &mut r, at, unix_now());
            r.version = inst.engine;
            self.counts.rejudged.fetch_add(1, Ordering::Relaxed);
        }
        let decision = r.verdict.decision.as_ref();
        if let Some(d) = decision.filter(|d| d.action == Action::Drop && by_hash(d.rule)) {
            self.counts.dropped.fetch_add(1, Ordering::Relaxed);
            return Judged::Dropped {
                key: r.key,
                token: r.token,
                rule: d.rule,
            };
        }
        let held_back = decision.is_some_and(|d| d.action >= Action::Quarantine);
        let marked = decision.is_some();
        let (key, token, version, sealing) = (r.key, r.token, inst.version, inst.key.clone());
        drop(installed);
        let mut record = r.into_record();
        record.rule_version = version;
        if !held_back {
            let c = if marked {
                &self.counts.marked
            } else {
                &self.counts.clear
            };
            c.fetch_add(1, Ordering::Relaxed);
            return Judged::Send(Outbound::Record(record));
        }
        match SealedRecord::seal(&record, &sealing, self.worker) {
            Ok(s) => {
                self.counts.sealed.fetch_add(1, Ordering::Relaxed);
                Judged::Send(Outbound::Sealed(s))
            }
            Err(e) => self.invalid(key, token, e),
        }
    }

    fn invalid(&self, key: InfoHash, token: Token, e: Error) -> Judged {
        self.counts.refused.fetch_add(1, Ordering::Relaxed);
        lodestar_log::log_event!(
            crate::log_events::PREPARE_FAILED,
            key = lodestar_log::Torrent(&key.0),
            error = lodestar_log::Error::new(&e)
        );
        Judged::Invalid { key, token }
    }
}

/// A drop by an exact hash or signature list: the floor (CS-1a, CS-1b,
/// CS-8a) or a subscribed list. These need no tombstone on the
/// coordinator: the list itself refuses the key again.
fn by_hash(rule: RuleRef) -> bool {
    match rule {
        RuleRef::Core(r) => r.floor(),
        RuleRef::List(_) => true,
        RuleRef::Pack(_) | RuleRef::User(_) | RuleRef::Override(_) => false,
    }
}

/// The judge around a remote worker's outbound queue: what the prepare
/// stage sends goes through [`Judge::judge`], and only what it lets
/// through reaches `out` (the session's queue, or the spool). Drops and
/// refusals end their leases through `reports`.
pub struct Judging<O> {
    pub judge: Arc<Judge>,
    pub reports: Arc<dyn Reports>,
    pub out: O,
}

impl<O> std::fmt::Debug for Judging<O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Judging")
            .field("judge", &self.judge)
            .finish_non_exhaustive()
    }
}

impl<O: Outbox<Outbound>> Outbox<Results> for Judging<O> {
    fn send(&self, item: Results) -> impl std::future::Future<Output = Result<(), Closed>> + Send {
        let judged = self.judge.judge(item);
        let lost = |key: &InfoHash, token: Token, loss: Loss| {
            self.reports.lost(key, token, loss);
        };
        let out = match judged {
            Judged::Send(o) => Some(o),
            Judged::Dropped { key, token, .. } | Judged::NotReady { key, token } => {
                lost(&key, token, Loss::Blocked);
                None
            }
            Judged::Invalid { key, token } => {
                lost(&key, token, Loss::Invalid);
                None
            }
        };
        let send = out.map(|o| self.out.send(o));
        async move {
            match send {
                Some(s) => s.await,
                None => Ok(()),
            }
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

// --- field encodings ---------------------------------------------------

fn count(w: &mut Writer, n: usize) -> Result<()> {
    if n > MAX_ROWS {
        return Err(Error::TooLarge("rows"));
    }
    w.uvar(n as u64);
    Ok(())
}

fn bytes(w: &mut Writer, b: &[u8], cap: usize) -> Result<()> {
    if b.len() > cap {
        return Err(Error::TooLarge("field"));
    }
    w.bytes(b);
    Ok(())
}

fn text(w: &mut Writer, s: &str, cap: usize) -> Result<()> {
    bytes(w, s.as_bytes(), cap)
}

fn opt_text(w: &mut Writer, s: Option<&str>) -> Result<()> {
    match s {
        None => w.u8(0),
        Some(s) => {
            w.u8(1);
            text(w, s, MAX_TEXT)?;
        }
    }
    Ok(())
}

fn opt_ivar(w: &mut Writer, v: Option<i64>) {
    match v {
        None => w.u8(0),
        Some(v) => {
            w.u8(1);
            w.ivar(v);
        }
    }
}

fn switches(w: &mut Writer, rows: &[(String, bool)]) -> Result<()> {
    count(w, rows.len())?;
    for (id, on) in rows {
        text(w, id, MAX_ID)?;
        w.u8((*on).into());
    }
    Ok(())
}

/// A stored value: 0 none, 1 text, 2 bytes that aren't text.
fn stored(w: &mut Writer, v: &StoredText) -> Result<()> {
    match v {
        None => w.u8(0),
        Some(v) => value(w, v)?,
    }
    Ok(())
}

fn value(w: &mut Writer, v: &std::result::Result<String, Vec<u8>>) -> Result<()> {
    match v {
        Ok(t) => {
            w.u8(1);
            text(w, t, MAX_TEXT)
        }
        Err(b) => {
            w.u8(2);
            bytes(w, b, MAX_TEXT)
        }
    }
}

fn settings(w: &mut Writer, rows: &[RawSetting]) -> Result<()> {
    count(w, rows.len())?;
    for (k, v) in rows {
        text(w, k, MAX_ID)?;
        value(w, v)?;
    }
    Ok(())
}

fn read_bool(r: &mut Reader<'_>) -> Result<bool> {
    match r.u8()? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Error::Invalid("flag")),
    }
}

fn read_text(r: &mut Reader<'_>, cap: usize, what: &'static str) -> Result<String> {
    let b = r.bytes(cap, what)?;
    std::str::from_utf8(b)
        .map(str::to_owned)
        .map_err(|_| Error::Invalid(what))
}

fn read_opt_text(r: &mut Reader<'_>) -> Result<Option<String>> {
    Ok(if read_bool(r)? {
        Some(read_text(r, MAX_TEXT, "text")?)
    } else {
        None
    })
}

fn read_opt_ivar(r: &mut Reader<'_>) -> Result<Option<i64>> {
    Ok(if read_bool(r)? { Some(r.ivar()?) } else { None })
}

fn read_switches(r: &mut Reader<'_>) -> Result<Vec<(String, bool)>> {
    let n = r.count(MAX_ROWS, 2, "switches")?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push((read_text(r, MAX_ID, "switch")?, read_bool(r)?));
    }
    Ok(out)
}

fn read_value(r: &mut Reader<'_>) -> Result<std::result::Result<String, Vec<u8>>> {
    match r.u8()? {
        1 => Ok(Ok(read_text(r, MAX_TEXT, "value")?)),
        2 => Ok(Err(r.bytes(MAX_TEXT, "value")?.to_vec())),
        _ => Err(Error::Invalid("value tag")),
    }
}

fn read_stored(r: &mut Reader<'_>) -> Result<StoredText> {
    match r.u8()? {
        0 => Ok(None),
        1 => Ok(Some(Ok(read_text(r, MAX_TEXT, "value")?))),
        2 => Ok(Some(Err(r.bytes(MAX_TEXT, "value")?.to_vec()))),
        _ => Err(Error::Invalid("value tag")),
    }
}

fn read_settings(r: &mut Reader<'_>) -> Result<Vec<RawSetting>> {
    let n = r.count(MAX_ROWS, 2, "settings")?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push((read_text(r, MAX_ID, "setting")?, read_value(r)?));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_bencode::{OwnedValue, to_vec};
    use lodestar_filter::Verdict;
    use lodestar_metainfo::Info;
    use lodestar_store::Preparer;

    const WORKER: WorkerId = WorkerId(7);
    /// A name only a rule of ours quarantines.
    const SECRET: &str = "zebraquark";

    fn rule(id: i64, query: &str, action: Action) -> RuleRow {
        RuleRow {
            id,
            query: query.into(),
            action: action.code(),
            enabled: true,
            shadow_until: None,
            unless_query: None,
        }
    }

    fn snapshot(rules: Vec<RuleRow>) -> FilterSnapshot {
        FilterSnapshot {
            key: WireKey::new(20_400, [9; 32]),
            filter: StoreFilter {
                state: FilterState::default(),
                rules: RuleSet {
                    rules,
                    ..RuleSet::default()
                },
                class_rules: Vec::new(),
                spam_sigs: Vec::new(),
            },
            lists: Vec::new(),
        }
    }

    /// Every field set, so a round trip covers every encoding.
    fn full() -> FilterSnapshot {
        let mut s = snapshot(vec![
            rule(1, SECRET, Action::Quarantine),
            RuleRow {
                shadow_until: Some(-5),
                unless_query: Some("type:book".into()),
                enabled: false,
                ..rule(2, "cam", Action::Hide)
            },
        ]);
        let f = &mut s.filter;
        f.state.cs_hashes = vec![([1; 20], 0), ([2; 20], 1)];
        f.state.cs_sigs = vec![0, u64::MAX];
        f.state.switches = vec![("CS-5a".into(), false)];
        let r = &mut f.rules;
        r.pack_switches = vec![("spam".into(), true)];
        r.overrides = vec![OverrideRow {
            id: 3,
            kind: 1,
            value: b"alpha".to_vec(),
            strength: 0,
            scope: Some("type:video".into()),
            created_at: 10,
            expires_at: None,
            shadow_until: Some(11),
            hits: 4,
        }];
        r.exceptions = vec![ExceptionRow {
            id: 4,
            rule: "U-1".into(),
            kind: 3,
            value: vec![0xff, 0],
        }];
        r.allow = vec![(5, "year:<1990".into())];
        r.pack_versions = PackVersionRows {
            applied: [Some(Ok("{}".into())), Some(Err(vec![0xc3]))],
            seeded: None,
        };
        r.pack_overrides = vec![("MW-1".into(), Err(vec![1, 2]))];
        f.class_rules = vec![(6, "studiox".into(), true)];
        f.spam_sigs = vec![("filter.spam_sig:1".into(), Ok("x".into()))];
        s.lists = vec![
            ContentList {
                id: 8,
                floor: true,
                action: 0,
                hashes: vec![[3; 20]],
            },
            ContentList {
                id: 9,
                floor: false,
                action: Action::Hide.code(),
                hashes: vec![[4; 20], [5; 20]],
            },
        ];
        s
    }

    fn same(a: &FilterSnapshot, b: &FilterSnapshot) {
        assert_eq!(a.key, b.key);
        assert_eq!(a.filter.state, b.filter.state);
        assert_eq!(a.filter.rules, b.filter.rules);
        assert_eq!(a.filter.class_rules, b.filter.class_rules);
        assert_eq!(a.filter.spam_sigs, b.filter.spam_sigs);
        assert_eq!(a.lists, b.lists);
    }

    #[test]
    fn a_snapshot_round_trips_and_is_refused_whole_when_damaged() {
        let s = full();
        let b = s.encode().unwrap();
        same(&FilterSnapshot::decode(&b).unwrap(), &s);
        assert_eq!(version_of(&b), version_of(&s.encode().unwrap()));
        // Every prefix is refused, never half-read.
        for n in 0..b.len() {
            assert!(FilterSnapshot::decode(&b[..n]).is_err(), "prefix {n}");
        }
        let mut long = b.clone();
        long.push(0);
        assert_eq!(FilterSnapshot::decode(&long).err(), Some(Error::Trailing));
        let mut v = b.clone();
        v[0] = 2;
        assert_eq!(FilterSnapshot::decode(&v).err(), Some(Error::Version(2)));
        // The held set never travels.
        let mut held = full();
        held.filter.state.held = vec![([7; 20], lodestar_store::Held::Quarantine)];
        assert!(
            FilterSnapshot::decode(&held.encode().unwrap())
                .unwrap()
                .filter
                .state
                .held
                .is_empty()
        );
        // A duplicate list is refused both ways.
        let mut dup = full();
        dup.lists[1].id = 8;
        assert_eq!(dup.encode().err(), Some(Error::Invalid("duplicate list")));
    }

    #[test]
    fn counts_are_capped_before_anything_is_allocated() {
        // A header claiming a million CS-1 entries in a few bytes.
        let mut w = Writer::default();
        w.u8(VERSION);
        w.uvar(1);
        w.fixed(&[0; 32]);
        w.uvar(1_000_000);
        assert_eq!(FilterSnapshot::decode(&w.out).err(), Some(Error::Truncated));
        let mut w = Writer::default();
        w.u8(VERSION);
        w.uvar(1);
        w.fixed(&[0; 32]);
        w.uvar(MAX_ROWS as u64 + 1);
        assert_eq!(
            FilterSnapshot::decode(&w.out).err(),
            Some(Error::TooLarge("cs hashes"))
        );
        let big = vec![0u8; MAX_SNAPSHOT + 1];
        assert_eq!(
            FilterSnapshot::decode(&big).err(),
            Some(Error::TooLarge("snapshot"))
        );
        // A bool that isn't 0 or 1 refuses.
        let mut s = snapshot(vec![rule(1, "a", Action::Hide)]);
        s.filter.class_rules = vec![(1, "b".into(), true)];
        let mut b = s.encode().unwrap();
        let last = b.len() - 2; // the class rule's flag, before the two empty counts
        assert_eq!(b[last - 1], 1);
        b[last - 1] = 2;
        assert!(FilterSnapshot::decode(&b).is_err());
    }

    #[test]
    fn debug_never_shows_a_key_or_a_rule() {
        let s = full();
        let text = format!("{s:?} {:?}", s.key);
        assert!(!text.contains(SECRET), "{text}");
        assert!(!text.contains("[9, 9"), "{text}");
        let keys = WireKeys::default();
        keys.set(vec![s.key.clone()]);
        assert_eq!(format!("{keys:?}"), "WireKeys([20400])");
    }

    #[test]
    fn a_dictionary_round_trips() {
        let d = DictSnapshot {
            id: 7,
            bytes: vec![1, 2, 3],
        };
        let b = d.encode().unwrap();
        assert_eq!(DictSnapshot::decode(&b).unwrap(), d);
        assert_eq!(d.version(), 7);
        assert!(format!("{d:?}").contains("bytes: 3"));
        for n in 0..b.len() {
            assert!(DictSnapshot::decode(&b[..n]).is_err());
        }
        let zero = DictSnapshot {
            id: 0,
            bytes: vec![],
        };
        assert!(zero.encode().is_err());
        let mut b0 = b.clone();
        b0[1] = 0;
        assert_eq!(
            DictSnapshot::decode(&b0).err(),
            Some(Error::Invalid("dictionary id"))
        );
    }

    fn chunks(kind: SnapshotKind, version: u64, payload: &[u8]) -> Vec<SnapshotChunk> {
        (0..chunk_count(payload.len()))
            .map(|i| chunk(kind, version, payload, i))
            .collect()
    }

    #[test]
    fn chunks_assemble_in_order_only() {
        let payload: Vec<u8> = (0..(2 * MAX_CHUNK + 5)).map(|i| i as u8).collect();
        let cs = chunks(SnapshotKind::Filter, 3, &payload);
        assert_eq!(cs.len(), 3);
        let mut inc = Incoming::new(SnapshotKind::Filter);
        assert!(matches!(inc.take(cs[0].clone()), Ok(Step::Ack(a)) if a.seq == 0));
        assert!(matches!(inc.take(cs[1].clone()), Ok(Step::Ack(a)) if a.seq == 1));
        match inc.take(cs[2].clone()).unwrap() {
            Step::Done(a, c) => {
                assert_eq!((a.seq, c.version), (2, 3));
                assert_eq!(c.bytes, payload);
            }
            s => panic!("{s:?}"),
        }
        // Out of order: a repeat, a gap, a chunk with no transfer.
        let mut inc = Incoming::new(SnapshotKind::Filter);
        inc.take(cs[0].clone()).unwrap();
        assert!(inc.take(cs[2].clone()).is_err());
        assert!(inc.take(cs[1].clone()).is_err(), "the transfer was dropped");
        inc.take(cs[0].clone()).unwrap();
        inc.take(cs[1].clone()).unwrap();
        assert!(inc.take(cs[1].clone()).is_err());
        // A new version's chunk 0 replaces a transfer in progress.
        let other = chunks(SnapshotKind::Filter, 4, b"tiny");
        inc.take(cs[0].clone()).unwrap();
        assert!(matches!(inc.take(other[0].clone()), Ok(Step::Done(_, c)) if c.bytes == b"tiny"));
        // A middle chunk that isn't full, an empty last one, another
        // kind, version 0, a total past the cap.
        let mut short = cs[0].clone();
        short.bytes.pop();
        assert!(Incoming::new(SnapshotKind::Filter).take(short).is_err());
        let mut empty = chunk(SnapshotKind::Filter, 5, b"x", 0);
        empty.bytes.clear();
        assert!(Incoming::new(SnapshotKind::Filter).take(empty).is_err());
        let dict = chunk(SnapshotKind::Dictionary, 5, b"x", 0);
        assert!(
            Incoming::new(SnapshotKind::Filter)
                .take(dict.clone())
                .is_err()
        );
        assert!(Incoming::new(SnapshotKind::Dictionary).take(dict).is_ok());
        let v0 = chunk(SnapshotKind::Filter, 0, b"x", 0);
        assert!(Incoming::new(SnapshotKind::Filter).take(v0).is_err());
        let mut many = chunk(SnapshotKind::Dictionary, 5, b"x", 0);
        many.total = chunk_count(MAX_DICT + 32) + 1;
        assert!(Incoming::new(SnapshotKind::Dictionary).take(many).is_err());
        let have = chunk(SnapshotKind::HaveFilter, 5, b"x", 0);
        assert!(Incoming::new(SnapshotKind::HaveFilter).take(have).is_err());
    }

    /// One prepared torrent, judged as the prepare stage judges it.
    fn prepared(engine: &Engine, name: &str, size: i64) -> Results {
        let raw = to_vec(&OwnedValue::Dict(
            [
                (b"length".to_vec(), OwnedValue::Int(size)),
                (b"name".to_vec(), OwnedValue::from(name)),
                (b"piece length".to_vec(), OwnedValue::Int(1 << 18)),
                (b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20])),
            ]
            .into_iter()
            .collect(),
        ));
        let info = Info::parse(&raw).unwrap();
        let mut torrent = Preparer::from_dict(None, 3, false)
            .prepare(&info, Some(&raw))
            .unwrap();
        let files = crate::enrich::file_infos(&info.files, info.path_encoding);
        let (enriched, class) = crate::enrich::enrich_infos(&torrent.name, &files);
        torrent.enrichment = Some(enriched);
        let mut r = Results {
            key: torrent.info_hash,
            token: Token(42),
            worker: WORKER,
            torrent,
            sources: 1,
            metadata_at: unix_now(),
            files,
            class,
            verdict: Verdict::default(),
            version: engine.version,
            trail: Default::default(),
            fix: None,
            private_source: false,
            source_adult: false,
        };
        let now = unix_now();
        r.verdict = crate::writer::rejudge(engine, &mut r, now, now);
        r
    }

    fn judge_with(s: &FilterSnapshot) -> Judge {
        let j = Judge::new(WORKER);
        assert!(j.install(s, 5).is_empty());
        j
    }

    fn keys(s: &FilterSnapshot) -> WireKeys {
        let k = WireKeys::default();
        k.set(vec![s.key.clone()]);
        k
    }

    #[test]
    fn nothing_is_judged_before_a_snapshot() {
        let j = Judge::new(WORKER);
        assert_eq!(j.version(), 0);
        let r = prepared(&j.filter().current(), "plain", 1 << 20);
        assert!(matches!(j.judge(r), Judged::NotReady { .. }));
        assert_eq!(j.counts.refused.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_hash_list_drop_never_goes_out() {
        let mut s = snapshot(Vec::new());
        // CS-8a: a repost of a CS-1 payload, specific by its size.
        let j0 = judge_with(&s);
        let r = prepared(&j0.filter().current(), "a repost", 60 << 20);
        let sig = lodestar_filter::payload_sig(&r.files).unwrap();
        assert!(sig.specific);
        s.filter.state.cs_sigs = vec![sig.sig];
        let j = judge_with(&s);
        let r = prepared(&j.filter().current(), "a repost", 60 << 20);
        match j.judge(r) {
            Judged::Dropped { rule, token, .. } => {
                assert_eq!(rule, RuleRef::Core(lodestar_filter::CoreRule::Cs8a));
                assert_eq!(token, Token(42));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(j.counts.dropped.load(Ordering::Relaxed), 1);
        // A subscribed list that drops, by the torrent's hash.
        let plain = prepared(&j.filter().current(), "listed", 1 << 20);
        s.lists = vec![ContentList {
            id: 3,
            floor: false,
            action: Action::Drop.code(),
            hashes: vec![plain.key.0],
        }];
        let j = judge_with(&s);
        let r = prepared(&j.filter().current(), "listed", 1 << 20);
        assert!(matches!(
            j.judge(r),
            Judged::Dropped {
                rule: RuleRef::List(3),
                ..
            }
        ));
    }

    #[test]
    fn a_quarantine_goes_sealed_and_opens_only_for_its_worker() {
        let s = snapshot(vec![rule(1, SECRET, Action::Quarantine)]);
        let j = judge_with(&s);
        let name = format!("{SECRET} holiday video");
        let r = prepared(&j.filter().current(), &name, 1 << 20);
        let Judged::Send(Outbound::Sealed(sealed)) = j.judge(r) else {
            panic!("not sealed");
        };
        assert_eq!(sealed.action, Action::Quarantine);
        assert_eq!(sealed.rule_version, 5);
        // On the wire: the name is nowhere in the payload, in Debug or in
        // the judge's own Debug.
        let wire = ResultsBatch {
            batch: 1,
            records: Vec::new(),
        }
        .encode_with(std::slice::from_ref(&sealed))
        .unwrap();
        assert!(!wire.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()));
        let text = format!("{sealed:?} {:?} {j:?}", Outbound::Sealed(sealed.clone()));
        assert!(!text.contains(SECRET), "{text}");
        // An intake that doesn't open sealed records refuses it.
        assert!(ResultsBatch::decode(&wire).unwrap().records.is_empty());
        let (_, got) = ResultsBatch::decode_with(&wire).unwrap();
        assert_eq!(got, vec![sealed.clone()]);
        // The coordinator opens it for this worker only, under this
        // header only.
        let k = keys(&s);
        let rec = k.open(&got[0], WORKER).unwrap();
        assert_eq!(rec.torrent.name, name);
        assert_eq!(rec.action, Some(Action::Quarantine));
        assert!(k.open(&got[0], WorkerId(8)).is_err());
        for tamper in [
            |s: &mut SealedRecord| s.token = Token(43),
            |s: &mut SealedRecord| s.rule_version = 6,
            |s: &mut SealedRecord| s.action = Action::Drop,
            |s: &mut SealedRecord| s.key = InfoHash([0; 20]),
        ] {
            let mut t = got[0].clone();
            tamper(&mut t);
            assert_eq!(k.open(&t, WORKER).err(), Some(Error::Invalid("seal")));
        }
        let mut other = got[0].clone();
        other.key_id += 1;
        assert_eq!(
            k.open(&other, WORKER).err(),
            Some(Error::Invalid("wire key"))
        );
        // Opened, it's re-judged at commit like any remote record, and the
        // commit quarantines it.
        let coordinator = judge_with(&s).filter().current();
        let mut back = Results {
            worker: WORKER,
            ..prepared(&coordinator, &name, 1 << 20)
        };
        back.verdict = Verdict::default();
        let now = unix_now();
        let v = crate::writer::rejudge(&coordinator, &mut back, now, now);
        assert_eq!(v.decision.unwrap().action, Action::Quarantine);
    }

    #[test]
    fn marks_go_in_clear_and_a_rules_drop_goes_sealed() {
        let s = snapshot(vec![
            rule(1, "cam", Action::Hide),
            rule(2, "spamword", Action::Drop),
        ]);
        let j = judge_with(&s);
        let r = prepared(&j.filter().current(), "a cam rip", 1 << 20);
        match j.judge(r) {
            Judged::Send(Outbound::Record(rec)) => {
                assert_eq!(rec.action, Some(Action::Hide));
                assert_eq!(rec.rule_version, 5);
            }
            other => panic!("{other:?}"),
        }
        let r = prepared(&j.filter().current(), "spamword offer", 1 << 20);
        assert!(matches!(
            j.judge(r),
            Judged::Send(Outbound::Sealed(SealedRecord {
                action: Action::Drop,
                ..
            }))
        ));
        let r = prepared(&j.filter().current(), "plain", 1 << 20);
        assert!(matches!(j.judge(r), Judged::Send(Outbound::Record(rec)) if rec.action.is_none()));
        let c = &j.counts;
        let n = |a: &AtomicU64| a.load(Ordering::Relaxed);
        assert_eq!((n(&c.marked), n(&c.sealed), n(&c.clear)), (1, 1, 1));
    }

    /// A worker judging with an old snapshot can't get a record past
    /// today's rules: the commit re-judges every remote result.
    #[test]
    fn a_stale_snapshot_is_rejudged_at_commit() {
        let old = judge_with(&snapshot(Vec::new()));
        let name = format!("{SECRET} holiday video");
        let r = prepared(&old.filter().current(), &name, 1 << 20);
        let Judged::Send(Outbound::Record(rec)) = old.judge(r) else {
            panic!("the old rules let it through");
        };
        assert_eq!(rec.action, None);
        // Today's rules quarantine it: the record the old worker sent,
        // re-judged as the writer re-judges every remote result.
        let today = judge_with(&snapshot(vec![rule(1, SECRET, Action::Quarantine)]))
            .filter()
            .current();
        let mut at_commit = prepared(&today, &name, 1 << 20);
        at_commit.verdict = Verdict::default();
        at_commit.version = rec.rule_version;
        let now = unix_now();
        let v = crate::writer::rejudge(&today, &mut at_commit, now, now);
        assert_eq!(v.decision.unwrap().action, Action::Quarantine);
    }

    /// A result prepared before a new snapshot came in is judged again
    /// with the new one before it goes out.
    #[test]
    fn a_result_from_before_an_install_is_judged_again() {
        let j = judge_with(&snapshot(Vec::new()));
        let name = format!("{SECRET} holiday video");
        let r = prepared(&j.filter().current(), &name, 1 << 20);
        assert!(r.verdict.decision.is_none());
        j.install(&snapshot(vec![rule(1, SECRET, Action::Quarantine)]), 6);
        assert_eq!(j.version(), 6);
        assert!(matches!(
            j.judge(r),
            Judged::Send(Outbound::Sealed(SealedRecord {
                rule_version: 6,
                ..
            }))
        ));
        assert_eq!(j.counts.rejudged.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn sealed_records_in_a_batch_follow_the_codecs_rules() {
        let s = snapshot(vec![rule(1, SECRET, Action::Quarantine)]);
        let j = judge_with(&s);
        let name = format!("{SECRET} b");
        let r = prepared(&j.filter().current(), &name, 1 << 20);
        let clear = r.into_record();
        let r = prepared(&j.filter().current(), &name, 1 << 20);
        let Judged::Send(Outbound::Sealed(sealed)) = j.judge(r) else {
            panic!()
        };
        // One key once, across clear and sealed.
        let b = ResultsBatch {
            batch: 1,
            records: vec![clear],
        };
        assert_eq!(
            b.encode_with(std::slice::from_ref(&sealed)).err(),
            Some(Error::Invalid("duplicate key"))
        );
        let wire = ResultsBatch {
            batch: 1,
            records: Vec::new(),
        }
        .encode_with(std::slice::from_ref(&sealed))
        .unwrap();
        // The flags are exactly SEALED | ACTION.
        let flags_at = 3; // version, batch 1, count 1
        let mut bad = wire.clone();
        bad[flags_at] |= super::super::results::flag::V2 as u8;
        assert_eq!(
            ResultsBatch::decode_with(&bad).err(),
            Some(Error::Invalid("flags"))
        );
        // An action that isn't held back, and a seal too short to hold a
        // nonce and tag, are refused.
        let mut w = Writer::default();
        w.u8(1);
        w.uvar(1);
        w.uvar(1);
        let mut s2 = sealed.clone();
        s2.action = Action::Hide;
        s2.write(&mut w);
        assert_eq!(
            ResultsBatch::decode_with(&w.out).err(),
            Some(Error::Invalid("sealed action"))
        );
        let mut w = Writer::default();
        w.u8(1);
        w.uvar(1);
        w.uvar(1);
        let mut s3 = sealed.clone();
        s3.sealed.truncate(MIN_SEAL - 1);
        s3.write(&mut w);
        assert!(ResultsBatch::decode_with(&w.out).is_err());
        // Every prefix of a batch with a sealed record is refused.
        for n in 0..wire.len() {
            assert!(ResultsBatch::decode_with(&wire[..n]).is_err());
        }
        // A record in clear can't be sealed with an action that isn't held
        // back.
        let r = prepared(&j.filter().current(), "plain", 1 << 20);
        let rec = r.into_record();
        assert_eq!(
            SealedRecord::seal(&rec, &s.key, WORKER).err(),
            Some(Error::Invalid("sealed action"))
        );
    }

    /// Workers judge with exactly what the commit judges with: an engine
    /// built from the snapshot decides as the coordinator's does.
    #[test]
    fn the_workers_engine_decides_as_the_coordinators() {
        let s = full();
        let coordinator = crate::default_filter();
        let _ = crate::load_filter(&coordinator, &s.filter);
        let (built, _) = FilterSnapshot::decode(&s.encode().unwrap())
            .unwrap()
            .engine();
        let c = coordinator.current();
        for h in [[1; 20], [2; 20], [3; 20], [4; 20], [6; 20]] {
            let ours = built.blocked_hash(&[h]).map(|d| d.rule);
            let core = c.blocked_hash(&[h]).map(|d| d.rule);
            // Lists are the coordinator's `lists::apply`'s; the rest match.
            if h == [3; 20] {
                assert_eq!(ours, Some(RuleRef::Core(lodestar_filter::CoreRule::Cs1b)));
            } else {
                assert_eq!(ours, core, "{h:?}");
            }
        }
        for name in [format!("{SECRET} x"), "cam".into(), "plain".into()] {
            let a = prepared(&built, &name, 1 << 20).verdict;
            let b = prepared(&c, &name, 1 << 20).verdict;
            assert_eq!(a.decision.map(|d| d.rule), b.decision.map(|d| d.rule));
        }
        assert_eq!(built.unreadable, c.unreadable);
    }

    proptest::proptest! {
        /// Arbitrary bytes never panic; whatever decodes is canonical.
        #[test]
        fn arbitrary_bytes_never_panic(body in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512)) {
            if let Ok(s) = FilterSnapshot::decode(&body) {
                proptest::prop_assert_eq!(s.encode().unwrap(), body.clone());
            }
            if let Ok(d) = DictSnapshot::decode(&body) {
                proptest::prop_assert_eq!(d.encode().unwrap(), body.clone());
            }
            if let Ok((b, sealed)) = ResultsBatch::decode_with(&body) {
                proptest::prop_assert_eq!(b.encode_with(&sealed).unwrap(), body);
            }
        }

        /// A damaged snapshot is refused whole or decodes to one that
        /// encodes to exactly the damaged bytes: never half-read.
        #[test]
        fn a_flipped_byte_never_half_reads(at in 0usize..4096, bit in 0u8..8) {
            let b = full().encode().unwrap();
            let mut d = b.clone();
            let at = at % d.len();
            d[at] ^= 1 << bit;
            if let Ok(s) = FilterSnapshot::decode(&d) {
                proptest::prop_assert_eq!(s.encode().unwrap(), d);
            }
        }
    }
}

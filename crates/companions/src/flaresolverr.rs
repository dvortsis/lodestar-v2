//! FlareSolverr, opt-in only (owner §2.47, plan 2026-11-06-v06-companions
//! §4 CMP-12 and Q21; architecture §18.10).
//!
//! FlareSolverr gets past sites' browser checks, which some sites' terms
//! forbid. So it's **never installed, detected into use or wired by
//! default:**
//! - its catalogue entry is `opt_in` and only a `challenge_solver`, which
//!   the catalogue loader enforces for anything that names it;
//! - [`crate::detect::auto_detect`] never probes it: [`find`] does, and a
//!   caller runs it only once you've switched FlareSolverr on;
//! - Prowlarr gets Lodestar's indexer proxy only from a [`Pairing`] built
//!   with [`Switch::On`], and loses it again with [`Switch::Off`], which is
//!   the default.
//!
//! **Detection and health** (checked against upstream `src/flaresolverr.py`
//! at v3.3): `GET /` answers `200` and
//! `{"msg": "FlareSolverr is ready!", "version": "3.3.21", "userAgent": …}`;
//! `GET /health` answers `200` and `{"status": "ok"}`. Neither needs a
//! credential, and FlareSolverr has none. The answers are untrusted: read
//! with the bounded JSON reader within [`Limits::PROBE`], matched against
//! the fixed marker, and only a [`Version`] in its strict grammar leaves
//! this module. The `userAgent` is passed over, never read.
//!
//! **Prowlarr's indexer proxy** (Prowlarr API v1, `IndexerProxyController`
//! and `FlareSolverrSettings`; sources in
//! `docs/decisions/2026-11-12-CMP-12.md`):
//! - Lodestar's proxy is named [`PROXY_NAME`] and carries the Prowlarr tag
//!   [`TAG_LABEL`]. A proxy is Lodestar's only if it has both and is a
//!   FlareSolverr proxy. Any other proxy, your own FlareSolverr ones
//!   included, is never changed or removed; one that only takes the name
//!   is refused ([`PROXY_NAME_TAKEN`]), never adopted.
//! - Prowlarr sends an indexer through a FlareSolverr proxy only when they
//!   share a tag, so Lodestar tags no indexer: you add [`TAG_LABEL`] to the
//!   indexers that need it.
//! - Lodestar's proxy is Lodestar's to keep as it set it: re-applying
//!   resets its timeout to [`REQUEST_TIMEOUT`] and its tags to
//!   [`TAG_LABEL`] alone. If you take that tag off it, it becomes yours:
//!   switching off leaves it, and switching on is refused until you
//!   rename or remove it.
//! - Switching off removes Lodestar's proxy and leaves the tag, which your
//!   indexers may still carry.
//! - The address is FlareSolverr's on this machine or Docker's network
//!   ([`SolverAddress`]), never a URL you entered: its API fetches any URL
//!   it's given, with no sign-in, so it stays off your network. The compose
//!   and Unraid templates refuse to publish it there for the same reason.
//!
//! **Egress** (`docs/decisions/2026-11-11-X-cmp-egress-prelaunch.md`):
//! Prowlarr reaches FlareSolverr on loopback or Docker's network, and
//! FlareSolverr itself goes direct, so an indexer it solves for leaves
//! directly, not through Prowlarr's route. Every pairing that's switched on
//! carries [`EGRESS_DIRECT`] ([`Pairing::notes`]) for the screen that shows
//! it.
//!
//! **Callers** (not in this slice): storing the switch per Prowlarr and
//! calling [`find`] and [`pairing`] from it is the companions screen and
//! endpoint's (CMP-9a's successor, placed by the runner). [`Switch`] is a
//! plain value, so "only once you switch it on" is held by that caller.
//!
//! **Pins:** the entry's artifacts and image are unpinned until CMP-13's
//! tool pins them (`TODO(CMP-13)`: a FlareSolverr recipe in
//! `tools/companion-pins/upstream.py`); until then the manager and the
//! templates refuse it as awaiting its pin.

use std::time::Duration;

use lodestar_sources_feeds::json::{self, Walk};

use crate::adapter::{
    AdapterError, BoxFuture, CheckedPlan, ConfigPlan, Configure, ConfigureInput, Detect, Detected,
    GuardedHttp, HeaderName, HeaderValue, Health, HealthStatus, Limits, ProbeRequest, RequestPath,
    SettingKey, Target,
};
use crate::catalogue::Catalogue;
use crate::configure::{
    CONTENT_TYPE, PLAN_MISMATCH, UNKNOWN_SETTING, consent, credential, failed, header, key, plain,
};
use crate::credentials::Field;
use crate::detect::{Runtime, get, health_of, not_found_or, probe_targets, status_field};
use crate::inputs::{CompanionId, ContainerName, PortNumber, Version};
use crate::log_events::CONFIG_APPLIED;

/// Its catalogue id.
pub const ID: &str = "flaresolverr";

pub(crate) const INDEX_PATH: &str = "/";
pub(crate) const HEALTH_PATH: &str = "/health";
const READY: &str = "FlareSolverr is ready!";

/// The variable a native launch sets so FlareSolverr listens on loopback
/// only; upstream's default is every address (`HOST=0.0.0.0`). For the
/// launch profile, which this build doesn't have yet: until it does, the
/// manager refuses a native install ([`crate::manager::Refusal::NoProfile`]).
pub const NATIVE_BIND: (&str, &str) = ("HOST", "127.0.0.1");

/// The note on every pairing that's switched on: indexers solved through
/// FlareSolverr leave directly, not through Prowlarr's route.
pub const EGRESS_DIRECT: &str = "flaresolverr_egress_direct";
/// The refusal when FlareSolverr's address isn't on this machine or
/// Docker's network.
pub const SOLVER_NOT_LOCAL: &str = "flaresolverr_not_local";
/// The refusal when Prowlarr can't reach FlareSolverr where Lodestar
/// found it (one on loopback, the other a container).
pub const SOLVER_UNREACHABLE: &str = "flaresolverr_unreachable_from_prowlarr";
/// A note on every pairing that's switched on: some sites' terms forbid
/// getting past their browser check (Q21).
pub const SITE_TERMS: &str = "flaresolverr_site_terms";
/// A note on every pairing that's switched on: only indexers you tag
/// [`TAG_LABEL`] go through it.
pub const TAG_INDEXERS: &str = "flaresolverr_tag_indexers";
/// The refusal when a proxy that isn't Lodestar's has its name.
pub const PROXY_NAME_TAKEN: &str = "indexer_proxy_name_taken";
/// The refusal when Prowlarr's validation refused the proxy or the tag.
pub const PROXY_REFUSED: &str = "indexer_proxy_refused";

/// Lodestar's indexer proxy's name in Prowlarr.
pub const PROXY_NAME: &str = "Lodestar FlareSolverr";
/// The Prowlarr tag on Lodestar's proxy, and on the indexers you route
/// through it. Prowlarr keeps tags lower-case.
pub const TAG_LABEL: &str = "lodestar-flaresolverr";
/// The proxy's `requestTimeout`, in seconds: upstream's default.
pub const REQUEST_TIMEOUT: u32 = 60;

const IMPLEMENTATION: &str = "FlareSolverr";
const CONTRACT: &str = "FlareSolverrSettings";

pub const TAG: SettingKey = key("indexer_proxy.tag");
pub const ADD: SettingKey = key("indexer_proxy.add");
pub const UPDATE: SettingKey = key("indexer_proxy.update");
pub const REMOVE: SettingKey = key("indexer_proxy.remove");
const ORDER: [SettingKey; 4] = [TAG, ADD, UPDATE, REMOVE];

const PROXIES: &str = "/api/v1/indexerproxy";
const TAGS: &str = "/api/v1/tag";
const API_KEY: HeaderName = header("X-Api-Key");

// ─── Detection and health ───────────────────────────────────────────────

/// Caps for `GET /`: a flat object of three short strings. The user agent
/// is skipped, so only the marker and the version are decoded.
const INDEX_LIMITS: json::Limits = json::Limits {
    max_depth: 2,
    max_values: 128,
    max_array: 8,
    max_text: 64,
};

/// `GET /`'s body: `Some` when `msg` is exactly the ready marker, with the
/// version when it's one in [`Version`]'s grammar. `None` for anything
/// else: not UTF-8, not one object, a key with an escape or given twice,
/// a non-string `msg` or `version`.
pub fn parse_index(body: &[u8]) -> Option<Option<Version>> {
    let s = std::str::from_utf8(body).ok()?;
    let mut j = json::Reader::new(s, INDEX_LIMITS);
    if j.peek() != Some(b'{') {
        return None;
    }
    let (mut msg, mut version): (Option<String>, Option<Option<String>>) = (None, None);
    j.object(|j, k| {
        let bad = json::Stop::Malformed;
        if k.contains('\\') {
            return Err(bad);
        }
        match k {
            "msg" | "version" => {
                if (k == "msg" && msg.is_some()) || (k == "version" && version.is_some()) {
                    return Err(bad);
                }
                if j.peek() != Some(b'"') {
                    return Err(bad);
                }
                let text = j.text()?.ok();
                if k == "msg" {
                    msg = Some(text.ok_or(bad)?);
                } else {
                    version = Some(text);
                }
            }
            _ => {
                j.skip()?;
            }
        }
        Ok(())
    })
    .ok()?;
    j.end().ok()?;
    if msg? != READY {
        return None;
    }
    Some(
        version
            .flatten()
            .and_then(|v| Version::parse(v.strip_prefix('v').unwrap_or(&v)).ok()),
    )
}

/// `GET /health`'s body: exactly `{"status": "ok"}` (other keys passed
/// over).
pub fn parse_health(body: &[u8]) -> bool {
    status_field(body).as_deref() == Some("ok")
}

/// The FlareSolverr detection and health adapter.
#[derive(Debug)]
pub struct FlareSolverr {
    id: CompanionId,
}

impl FlareSolverr {
    pub(crate) fn new(id: CompanionId) -> FlareSolverr {
        FlareSolverr { id }
    }
}

impl Detect for FlareSolverr {
    fn companion(&self) -> &CompanionId {
        &self.id
    }

    fn detect<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
    ) -> BoxFuture<'a, Result<Option<Detected>, AdapterError>> {
        Box::pin(async move {
            let r = match get(http, target, INDEX_PATH).await {
                Ok(r) => r,
                Err(e) => return not_found_or(e, target),
            };
            if r.status() != 200 {
                return Ok(None);
            }
            Ok(parse_index(r.body()).map(|version| Detected {
                target: target.clone(),
                version,
            }))
        })
    }
}

impl Health for FlareSolverr {
    fn companion(&self) -> &CompanionId {
        &self.id
    }

    fn check<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
    ) -> BoxFuture<'a, HealthStatus> {
        Box::pin(async move {
            match get(http, target, HEALTH_PATH).await {
                Ok(r) if r.status() == 200 && parse_health(r.body()) => HealthStatus::Healthy,
                Ok(_) => HealthStatus::Unhealthy,
                Err(e) => health_of(e),
            }
        })
    }
}

/// Looks for a running FlareSolverr, Lodestar's or your own, on the
/// catalogue's fixed targets (Q12: loopback, then its container name in a
/// container). Only for a caller whose FlareSolverr switch is on: nothing
/// else probes it. The first target that answers as FlareSolverr wins.
pub async fn find(
    http: &dyn GuardedHttp,
    catalogue: &Catalogue,
    runtime: Runtime,
) -> Option<Detected> {
    let entry = catalogue.get(ID)?;
    let adapter = FlareSolverr::new(entry.id.clone());
    for target in probe_targets(entry, runtime) {
        if let Ok(Some(d)) = adapter.detect(http, &target).await {
            return Some(d);
        }
    }
    None
}

// ─── Prowlarr's indexer proxy ───────────────────────────────────────────

/// FlareSolverr's address as Prowlarr reaches it: on this machine or by
/// its container name on Docker's network. Never anywhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SolverAddress {
    Loopback(PortNumber),
    Container(ContainerName, PortNumber),
}

impl SolverAddress {
    /// FlareSolverr's address as Prowlarr reaches it, from where Lodestar
    /// found each of them. Both on loopback means all three share this
    /// machine; both by container name means Docker's network, where
    /// Prowlarr resolves the name as Lodestar does. Any other mix is
    /// refused ([`SOLVER_UNREACHABLE`]): a loopback FlareSolverr is
    /// Lodestar's loopback, not a container Prowlarr's. A URL you entered,
    /// for either, is refused ([`SOLVER_NOT_LOCAL`]).
    pub fn for_prowlarr(
        flaresolverr: &Target,
        prowlarr: &Target,
    ) -> Result<SolverAddress, AdapterError> {
        match (flaresolverr, prowlarr) {
            (Target::Entered(_), _) | (_, Target::Entered(_)) => {
                Err(AdapterError::Refused(SOLVER_NOT_LOCAL))
            }
            (Target::Loopback(p), Target::Loopback(_)) => Ok(SolverAddress::Loopback(*p)),
            (Target::Container(n, p), Target::Container(..)) => {
                Ok(SolverAddress::Container(n.clone(), *p))
            }
            _ => Err(AdapterError::Refused(SOLVER_UNREACHABLE)),
        }
    }

    /// The `host` field Prowlarr is given: a closed name or a loopback
    /// literal, and a port.
    pub fn url(&self) -> String {
        match self {
            SolverAddress::Loopback(p) => format!("http://127.0.0.1:{}/", p.get()),
            SolverAddress::Container(n, p) => format!("http://{}:{}/", n.as_str(), p.get()),
        }
    }
}

/// Your FlareSolverr switch for Prowlarr. Off unless you turn it on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Switch {
    #[default]
    Off,
    On(SolverAddress),
}

/// Pairs FlareSolverr with Prowlarr, or unpairs it, through Prowlarr's
/// API. A [`Configure`] whose target is **Prowlarr's** and whose input is
/// Prowlarr's: its consent and its API key. Plan, then apply; each write
/// is idempotent, so a plan made after a successful apply is empty.
#[derive(Debug)]
pub struct Pairing {
    id: CompanionId,
    switch: Switch,
}

/// The pairing for FlareSolverr's catalogue id, or `None` for any other.
pub fn pairing(id: &CompanionId, switch: Switch) -> Option<Box<dyn Configure>> {
    (id.as_str() == ID).then(|| {
        Box::new(Pairing {
            id: id.clone(),
            switch,
        }) as Box<dyn Configure>
    })
}

impl Pairing {
    /// The reason codes the screen showing this pairing must carry:
    /// traffic leaves directly, some sites forbid it, and only indexers
    /// you tag use it.
    pub fn notes(switch: &Switch) -> &'static [&'static str] {
        match switch {
            Switch::Off => &[],
            Switch::On(_) => &[EGRESS_DIRECT, SITE_TERMS, TAG_INDEXERS],
        }
    }
}

fn limits() -> Limits {
    Limits::new(Duration::from_secs(10), 256 * 1024).unwrap_or(Limits::PROBE)
}

/// Caps for the proxy and tag lists. A proxy's `fields` hold objects with
/// `selectOptions` lists of objects: six levels deep at most.
const LIST_LIMITS: json::Limits = json::Limits {
    max_depth: 8,
    max_values: 16_384,
    max_array: 1024,
    max_text: 512,
};

/// The parts of one indexer proxy this module reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProxyRow {
    pub id: u32,
    /// `None` when absent, `null` or over the text cap.
    pub name: Option<String>,
    pub implementation: Option<String>,
    pub tags: Vec<u32>,
    pub host: Option<String>,
    pub timeout: Option<u32>,
}

/// One tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TagRow {
    pub id: u32,
    pub label: Option<String>,
}

/// A whole number in `0..=u32::MAX`, next.
fn uint(j: &mut json::Reader<'_>) -> json::R<u32> {
    if !matches!(j.peek(), Some(b'0'..=b'9')) {
        return Err(json::Stop::Malformed);
    }
    j.num()?.parse::<u32>().map_err(|_| json::Stop::Malformed)
}

/// A string or `null`, next: `None` for `null` or a string past the cap.
fn opt_text(j: &mut json::Reader<'_>) -> json::R<Option<String>> {
    match j.peek() {
        Some(b'"') => Ok(j.text()?.ok()),
        Some(b'n') => {
            j.skip()?;
            Ok(None)
        }
        _ => Err(json::Stop::Malformed),
    }
}

/// The most keys one object may have. Prowlarr's objects have about 15,
/// so this keeps the duplicate check in [`fresh`] small whatever the body.
const MAX_KEYS: usize = 64;

/// Refuses a key with an escape, a key seen before in this object, or
/// more than [`MAX_KEYS`] keys.
fn fresh<'a>(seen: &mut Vec<&'a str>, k: &'a str) -> json::R<()> {
    if k.contains('\\') || seen.len() >= MAX_KEYS || seen.contains(&k) {
        return Err(json::Stop::Malformed);
    }
    seen.push(k);
    Ok(())
}

/// One proxy's `fields`: `host` (a string) and `requestTimeout` (a whole
/// number), each at most once; other fields and keys are passed over.
fn read_fields(j: &mut json::Reader<'_>, row: &mut ProxyRow) -> json::R<()> {
    let (mut host_seen, mut timeout_seen) = (false, false);
    j.array(|j, _| {
        let (mut name, mut value): (Option<Option<String>>, Option<FieldValue>) = (None, None);
        let mut seen = Vec::new();
        j.object(|j, k| {
            fresh(&mut seen, k)?;
            match k {
                "name" => name = Some(opt_text(j)?),
                "value" => {
                    value = Some(match j.peek() {
                        Some(b'"') => FieldValue::Text(j.text()?.ok()),
                        Some(b'0'..=b'9') => FieldValue::Num(j.num()?.parse::<u32>().ok()),
                        _ => {
                            j.skip()?;
                            FieldValue::Other
                        }
                    })
                }
                _ => {
                    j.skip()?;
                }
            }
            Ok(())
        })?;
        match (name.flatten().as_deref(), value) {
            (Some("host"), v) => {
                if std::mem::replace(&mut host_seen, true) {
                    return Err(json::Stop::Malformed);
                }
                row.host = match v {
                    Some(FieldValue::Text(t)) => t,
                    _ => None,
                };
            }
            (Some("requestTimeout"), v) => {
                if std::mem::replace(&mut timeout_seen, true) {
                    return Err(json::Stop::Malformed);
                }
                row.timeout = match v {
                    Some(FieldValue::Num(n)) => n,
                    _ => None,
                };
            }
            _ => {}
        }
        Ok(())
    })
}

enum FieldValue {
    Text(Option<String>),
    Num(Option<u32>),
    Other,
}

/// `GET /api/v1/indexerproxy`: a list of proxies, each with a whole `id`
/// (required) and tags of whole numbers. `None` for anything else, within
/// [`LIST_LIMITS`].
pub(crate) fn parse_proxies(body: &[u8]) -> Option<Vec<ProxyRow>> {
    let s = std::str::from_utf8(body).ok()?;
    let mut j = json::Reader::new(s, LIST_LIMITS);
    if j.peek() != Some(b'[') {
        return None;
    }
    let mut rows = Vec::new();
    j.array(|j, _| {
        let mut row = ProxyRow::default();
        let mut id = None;
        let mut seen = Vec::new();
        j.object(|j, k| {
            fresh(&mut seen, k)?;
            match k {
                "id" => id = Some(uint(j)?),
                "name" => row.name = opt_text(j)?,
                "implementation" => row.implementation = opt_text(j)?,
                "tags" => j.array(|j, _| {
                    row.tags.push(uint(j)?);
                    Ok(())
                })?,
                "fields" => read_fields(j, &mut row)?,
                _ => {
                    j.skip()?;
                }
            }
            Ok(())
        })?;
        row.id = id.ok_or(json::Stop::Malformed)?;
        rows.push(row);
        Ok(())
    })
    .ok()?;
    j.end().ok()?;
    Some(rows)
}

fn read_tag(j: &mut json::Reader<'_>) -> json::R<TagRow> {
    let (mut id, mut label) = (None, None);
    let mut seen = Vec::new();
    j.object(|j, k| {
        fresh(&mut seen, k)?;
        match k {
            "id" => id = Some(uint(j)?),
            "label" => label = opt_text(j)?,
            _ => {
                j.skip()?;
            }
        }
        Ok(())
    })?;
    Ok(TagRow {
        id: id.ok_or(json::Stop::Malformed)?,
        label,
    })
}

/// `GET /api/v1/tag`: a list of `{id, label}`.
pub(crate) fn parse_tags(body: &[u8]) -> Option<Vec<TagRow>> {
    let s = std::str::from_utf8(body).ok()?;
    let mut j = json::Reader::new(s, LIST_LIMITS);
    if j.peek() != Some(b'[') {
        return None;
    }
    let mut rows = Vec::new();
    j.array(|j, _| {
        rows.push(read_tag(j)?);
        Ok(())
    })
    .ok()?;
    j.end().ok()?;
    Some(rows)
}

/// `POST /api/v1/tag`'s answer: the new tag.
pub(crate) fn parse_tag(body: &[u8]) -> Option<TagRow> {
    let s = std::str::from_utf8(body).ok()?;
    let mut j = json::Reader::new(s, LIST_LIMITS);
    if j.peek() != Some(b'{') {
        return None;
    }
    let t = read_tag(&mut j).ok()?;
    j.end().ok()?;
    Some(t)
}

/// What Prowlarr has, as far as the pairing is concerned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Have {
    /// Lodestar's tag's id.
    pub tag: Option<u32>,
    /// Lodestar's proxies: named [`PROXY_NAME`], a FlareSolverr proxy, and
    /// tagged [`TAG_LABEL`].
    pub ours: Vec<ProxyRow>,
    /// A proxy that isn't Lodestar's has [`PROXY_NAME`].
    pub name_taken: bool,
}

pub(crate) fn have(proxies: Vec<ProxyRow>, tags: &[TagRow]) -> Have {
    let tag = tags
        .iter()
        .find(|t| t.label.as_deref() == Some(TAG_LABEL))
        .map(|t| t.id);
    let mut out = Have {
        tag,
        ..Have::default()
    };
    for p in proxies {
        if p.name.as_deref() != Some(PROXY_NAME) {
            continue;
        }
        let ours = p.implementation.as_deref() == Some(IMPLEMENTATION)
            && tag.is_some_and(|t| p.tags.contains(&t));
        if ours {
            out.ours.push(p);
        } else {
            out.name_taken = true;
        }
    }
    out
}

/// The changes the switch asks for, given what Prowlarr has.
pub(crate) fn differing(switch: &Switch, have: &Have) -> Result<Vec<SettingKey>, AdapterError> {
    match switch {
        Switch::Off => Ok(if have.ours.is_empty() {
            Vec::new()
        } else {
            vec![REMOVE]
        }),
        Switch::On(addr) => {
            if have.name_taken || have.ours.len() > 1 {
                return Err(AdapterError::Refused(PROXY_NAME_TAKEN));
            }
            let mut out = Vec::new();
            if have.tag.is_none() {
                out.push(TAG);
            }
            match have.ours.first() {
                None => out.push(ADD),
                Some(p) => {
                    let tag_only = p.tags.len() == 1;
                    if p.host.as_deref() != Some(addr.url().as_str())
                        || p.timeout != Some(REQUEST_TIMEOUT)
                        || !tag_only
                    {
                        out.push(UPDATE);
                    }
                }
            }
            Ok(out)
        }
    }
}

/// Every listed key is one the switch can ask for.
fn check_plan(plan: &ConfigPlan, switch: &Switch) -> Result<(), AdapterError> {
    for k in plan.changes() {
        if !ORDER.contains(k) {
            return Err(AdapterError::Refused(UNKNOWN_SETTING));
        }
        let fits = match switch {
            Switch::Off => *k == REMOVE,
            Switch::On(_) => *k != REMOVE,
        };
        if !fits {
            return Err(AdapterError::Refused(PLAN_MISMATCH));
        }
    }
    Ok(())
}

/// Lodestar's proxy as Prowlarr takes it: its own name, FlareSolverr's
/// address, upstream's timeout and Lodestar's tag only.
fn resource(id: Option<u32>, addr: &SolverAddress, tag: u32) -> Vec<u8> {
    let mut v = serde_json::json!({
        "name": PROXY_NAME,
        "implementation": IMPLEMENTATION,
        "implementationName": IMPLEMENTATION,
        "configContract": CONTRACT,
        "fields": [
            {"name": "host", "value": addr.url()},
            {"name": "requestTimeout", "value": REQUEST_TIMEOUT},
        ],
        "tags": [tag],
    });
    if let (Some(id), Some(m)) = (id, v.as_object_mut()) {
        m.insert("id".to_owned(), id.into());
    }
    v.to_string().into_bytes()
}

fn path(p: &str) -> Result<RequestPath, AdapterError> {
    RequestPath::parse(p).map_err(|_| AdapterError::Protocol("request path"))
}

fn key_header(input: &ConfigureInput<'_>) -> Result<HeaderValue, AdapterError> {
    HeaderValue::secret(credential(input, Field::ApiKey)?)
        .map_err(|_| AdapterError::Protocol("API key"))
}

/// One request to Prowlarr with its key; `401` is a refused key, `400`
/// Prowlarr's validation, any other status outside `ok` a protocol error.
async fn call(
    http: &dyn GuardedHttp,
    req: ProbeRequest<'_>,
    input: &ConfigureInput<'_>,
    ok: &[u16],
) -> Result<Vec<u8>, AdapterError> {
    let req = req.header(API_KEY, key_header(input)?);
    let r = http.send(req).await?;
    match r.status() {
        s if ok.contains(&s) => Ok(r.body().to_vec()),
        401 => Err(AdapterError::Unauthorized),
        400 => Err(AdapterError::Refused(PROXY_REFUSED)),
        _ => Err(AdapterError::Protocol("HTTP status")),
    }
}

async fn read_have(
    http: &dyn GuardedHttp,
    target: &Target,
    input: &ConfigureInput<'_>,
) -> Result<Have, AdapterError> {
    let req = ProbeRequest::new(target, path(TAGS)?, limits());
    let tags =
        parse_tags(&call(http, req, input, &[200]).await?).ok_or(AdapterError::Protocol("tags"))?;
    let req = ProbeRequest::new(target, path(PROXIES)?, limits());
    let proxies = parse_proxies(&call(http, req, input, &[200]).await?)
        .ok_or(AdapterError::Protocol("indexer proxies"))?;
    Ok(have(proxies, &tags))
}

fn json_body(req: ProbeRequest<'_>) -> Result<ProbeRequest<'_>, AdapterError> {
    Ok(req.header(CONTENT_TYPE, plain("application/json")?))
}

impl Pairing {
    async fn plan_inner(
        &self,
        http: &dyn GuardedHttp,
        target: &Target,
        input: &ConfigureInput<'_>,
    ) -> Result<ConfigPlan, AdapterError> {
        consent(input, target)?;
        let have = read_have(http, target, input).await?;
        Ok(ConfigPlan::new(
            self.id.clone(),
            differing(&self.switch, &have)?,
        ))
    }

    async fn apply_inner(
        &self,
        http: &dyn GuardedHttp,
        target: &Target,
        input: &ConfigureInput<'_>,
        plan: &ConfigPlan,
    ) -> Result<(), AdapterError> {
        check_plan(plan, &self.switch).map_err(|e| failed(ID, "apply", e))?;
        let mut have = read_have(http, target, input)
            .await
            .map_err(|e| failed(ID, "apply", e))?;
        // What's still needed now, limited to what the plan lists: a step
        // another apply already took is skipped, and nothing unlisted is
        // done.
        let now = differing(&self.switch, &have).map_err(|e| failed(ID, "apply", e))?;
        let todo: Vec<SettingKey> = ORDER
            .into_iter()
            .filter(|k| plan.changes().contains(k) && now.contains(k))
            .collect();
        for k in &todo {
            let step = k.as_str();
            let result = match (&self.switch, k) {
                (Switch::On(_), k) if *k == TAG => {
                    let body = serde_json::json!({ "label": TAG_LABEL }).to_string();
                    let req = json_body(
                        ProbeRequest::new(target, path(TAGS)?, limits()).post(body.into_bytes()),
                    )?;
                    let created = call(http, req, input, &[200, 201]).await.and_then(|b| {
                        parse_tag(&b)
                            .filter(|t| t.label.as_deref() == Some(TAG_LABEL))
                            .ok_or(AdapterError::Protocol("tag"))
                    });
                    created.map(|t| have.tag = Some(t.id))
                }
                (Switch::On(addr), k) if *k == ADD || *k == UPDATE => {
                    let tag = have.tag.ok_or(AdapterError::Refused(PLAN_MISMATCH))?;
                    let req = if *k == ADD {
                        ProbeRequest::new(target, path(PROXIES)?, limits())
                            .post(resource(None, addr, tag))
                    } else {
                        let id = have
                            .ours
                            .first()
                            .map(|p| p.id)
                            .ok_or(AdapterError::Refused(PLAN_MISMATCH))?;
                        ProbeRequest::new(target, path(&format!("{PROXIES}/{id}"))?, limits())
                            .put(resource(Some(id), addr, tag))
                    };
                    call(http, json_body(req)?, input, &[200, 201, 202])
                        .await
                        .map(|_| ())
                }
                (Switch::Off, k) if *k == REMOVE => {
                    let mut out = Ok(());
                    for p in &have.ours {
                        let req = ProbeRequest::new(
                            target,
                            path(&format!("{PROXIES}/{}", p.id))?,
                            limits(),
                        )
                        .delete();
                        // Already gone is what was wanted.
                        if let Err(e) = call(http, req, input, &[200, 202, 204, 404]).await {
                            out = Err(e);
                            break;
                        }
                    }
                    out
                }
                _ => Err(AdapterError::Refused(PLAN_MISMATCH)),
            };
            result.map_err(|e| failed(ID, step, e))?;
            lodestar_log::log_event!(CONFIG_APPLIED, companion = ID, setting = step);
        }
        Ok(())
    }
}

impl Configure for Pairing {
    fn companion(&self) -> &CompanionId {
        &self.id
    }

    fn plan<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
    ) -> BoxFuture<'a, Result<ConfigPlan, AdapterError>> {
        Box::pin(async move {
            self.plan_inner(http, target, input)
                .await
                .map_err(|e| failed(ID, "plan", e))
        })
    }

    fn apply_plan<'a>(
        &'a self,
        http: &'a dyn GuardedHttp,
        target: &'a Target,
        input: &'a ConfigureInput<'a>,
        plan: CheckedPlan<'a>,
    ) -> BoxFuture<'a, Result<(), AdapterError>> {
        Box::pin(async move {
            consent(input, target).map_err(|e| failed(ID, "apply", e))?;
            if plan.plan().changes().is_empty() {
                return Ok(());
            }
            self.apply_inner(http, target, input, plan.plan()).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const INDEX: &[u8] = br#"{"msg": "FlareSolverr is ready!", "version": "3.3.21", "userAgent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36"}"#;

    #[test]
    fn index_answers() {
        let v = parse_index(INDEX).unwrap().unwrap();
        assert_eq!(v.as_str(), "3.3.21");
        assert_eq!(
            parse_index(br#"{"msg":"FlareSolverr is ready!","version":"v3.3.0"}"#)
                .unwrap()
                .unwrap()
                .as_str(),
            "3.3.0"
        );
        // A version outside the grammar is no version, not no FlareSolverr.
        assert_eq!(
            parse_index(br#"{"msg":"FlareSolverr is ready!","version":"3.3.21-beta"}"#),
            Some(None)
        );
        assert_eq!(
            parse_index(br#"{"msg":"FlareSolverr is ready!"}"#),
            Some(None)
        );
        for bad in [
            &b""[..],
            b"{}",
            b"[]",
            br#"{"msg":"flaresolverr is ready!"}"#,
            br#"{"msg":"FlareSolverr is ready! "}"#,
            br#"{"msg":1}"#,
            br#"{"msg":"FlareSolverr is ready!","msg":"FlareSolverr is ready!"}"#,
            br#"{"m\u0073g":"x","msg":"FlareSolverr is ready!"}"#,
            br#"{"msg":"FlareSolverr is ready!","version":3}"#,
            br#"{"msg":"FlareSolverr is ready!"} {}"#,
            br#"{"msg":"FlareSolverr is ready!","#,
            b"{\"msg\":\"FlareSolverr is ready!\xff\"}",
        ] {
            assert_eq!(parse_index(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
        let long = format!(r#"{{"msg":"{}"}}"#, "A".repeat(10_000));
        assert_eq!(parse_index(long.as_bytes()), None);
    }

    #[test]
    fn health_answers() {
        assert!(parse_health(br#"{"status": "ok"}"#));
        for bad in [
            &b""[..],
            br#"{"status":"OK"}"#,
            br#"{"status":"error"}"#,
            b"ok",
        ] {
            assert!(!parse_health(bad));
        }
    }

    fn proxy(id: u32, name: &str, imp: &str, tags: &[u32], host: &str) -> ProxyRow {
        ProxyRow {
            id,
            name: Some(name.into()),
            implementation: Some(imp.into()),
            tags: tags.to_vec(),
            host: Some(host.into()),
            timeout: Some(REQUEST_TIMEOUT),
        }
    }

    fn tag(id: u32, label: &str) -> TagRow {
        TagRow {
            id,
            label: Some(label.into()),
        }
    }

    #[test]
    fn only_lodestars_proxy_is_its_own() {
        let tags = [tag(1, "mine"), tag(7, TAG_LABEL)];
        let lo = SolverAddress::Loopback(PortNumber::new(8191).unwrap());
        let url = lo.url();
        let on = Switch::On(lo);
        // Yours, with another name or tag, is never Lodestar's.
        let yours = vec![
            proxy(1, "FlareSolverr", IMPLEMENTATION, &[1], &url),
            proxy(2, "Other", IMPLEMENTATION, &[7], &url),
        ];
        let h = have(yours.clone(), &tags);
        assert!(h.ours.is_empty() && !h.name_taken);
        assert_eq!(differing(&on, &h).unwrap(), vec![ADD]);
        assert!(differing(&Switch::Off, &h).unwrap().is_empty());
        // The name without the tag, or another implementation, is refused.
        for taken in [
            proxy(3, PROXY_NAME, IMPLEMENTATION, &[1], &url),
            proxy(3, PROXY_NAME, "Http", &[7], &url),
        ] {
            let h = have(vec![taken], &tags);
            assert!(h.name_taken);
            assert_eq!(
                differing(&on, &h),
                Err(AdapterError::Refused(PROXY_NAME_TAKEN))
            );
            assert!(differing(&Switch::Off, &h).unwrap().is_empty());
        }
        // Lodestar's: nothing to do, or an update, or a removal.
        let ours = proxy(4, PROXY_NAME, IMPLEMENTATION, &[7], &url);
        let h = have(vec![ours.clone()], &tags);
        assert!(differing(&on, &h).unwrap().is_empty());
        assert_eq!(differing(&Switch::Off, &h).unwrap(), vec![REMOVE]);
        let moved = ProxyRow {
            host: Some("http://flaresolverr:8191/".into()),
            ..ours.clone()
        };
        assert_eq!(
            differing(&on, &have(vec![moved], &tags)).unwrap(),
            vec![UPDATE]
        );
        let extra_tag = ProxyRow {
            tags: vec![7, 1],
            ..ours
        };
        assert_eq!(
            differing(&on, &have(vec![extra_tag], &tags)).unwrap(),
            vec![UPDATE]
        );
        // No tag yet: make it, then the proxy.
        assert_eq!(
            differing(&on, &have(Vec::new(), &[tag(1, "mine")])).unwrap(),
            vec![TAG, ADD]
        );
    }

    #[test]
    fn plans_fit_their_switch() {
        let on = Switch::On(SolverAddress::Loopback(PortNumber::new(8191).unwrap()));
        let id = CompanionId::parse(ID).unwrap();
        let p = |k: Vec<SettingKey>| ConfigPlan::new(id.clone(), k);
        assert!(check_plan(&p(vec![TAG, ADD]), &on).is_ok());
        assert!(check_plan(&p(vec![REMOVE]), &Switch::Off).is_ok());
        assert_eq!(
            check_plan(&p(vec![REMOVE]), &on),
            Err(AdapterError::Refused(PLAN_MISMATCH))
        );
        assert_eq!(
            check_plan(&p(vec![ADD]), &Switch::Off),
            Err(AdapterError::Refused(PLAN_MISMATCH))
        );
        assert_eq!(
            check_plan(&p(vec![key("proxy.enabled")]), &on),
            Err(AdapterError::Refused(UNKNOWN_SETTING))
        );
    }

    #[test]
    fn addresses_are_local_only() {
        let lo = Target::Loopback(PortNumber::new(8191).unwrap());
        let ct = Target::Container(
            ContainerName::parse("flaresolverr").unwrap(),
            PortNumber::new(8191).unwrap(),
        );
        let en =
            Target::Entered(crate::adapter::EnteredUrl::parse("http://10.0.0.2:8191").unwrap());
        assert_eq!(
            SolverAddress::for_prowlarr(&lo, &lo).unwrap().url(),
            "http://127.0.0.1:8191/"
        );
        assert_eq!(
            SolverAddress::for_prowlarr(&ct, &ct).unwrap().url(),
            "http://flaresolverr:8191/"
        );
        for (f, p) in [(&lo, &ct), (&ct, &lo)] {
            assert_eq!(
                SolverAddress::for_prowlarr(f, p),
                Err(AdapterError::Refused(SOLVER_UNREACHABLE))
            );
        }
        for (f, p) in [(&en, &lo), (&lo, &en), (&en, &en)] {
            assert_eq!(
                SolverAddress::for_prowlarr(f, p),
                Err(AdapterError::Refused(SOLVER_NOT_LOCAL))
            );
        }
        assert_eq!(NATIVE_BIND, ("HOST", "127.0.0.1"));
        assert_eq!(Switch::default(), Switch::Off);
        assert!(Pairing::notes(&Switch::Off).is_empty());
        assert_eq!(
            Pairing::notes(&Switch::On(SolverAddress::Loopback(
                PortNumber::new(8191).unwrap()
            ))),
            &[EGRESS_DIRECT, SITE_TERMS, TAG_INDEXERS]
        );
    }

    #[test]
    fn lists() {
        let body = br#"[{"id":3,"name":"Lodestar FlareSolverr","implementation":"FlareSolverr","implementationName":"FlareSolverr","configContract":"FlareSolverrSettings","fields":[{"order":0,"name":"host","label":"Host","value":"http://127.0.0.1:8191/","type":"textbox","advanced":false,"selectOptions":[{"value":1,"name":"a"}]},{"name":"requestTimeout","value":60}],"tags":[7],"link":null}]"#;
        let rows = parse_proxies(body).unwrap();
        assert_eq!(
            rows,
            vec![ProxyRow {
                id: 3,
                name: Some(PROXY_NAME.into()),
                implementation: Some("FlareSolverr".into()),
                tags: vec![7],
                host: Some("http://127.0.0.1:8191/".into()),
                timeout: Some(60),
            }]
        );
        assert_eq!(parse_proxies(b"[]"), Some(Vec::new()));
        for bad in [
            &b""[..],
            b"{}",
            br#"[{"name":"x"}]"#,
            br#"[{"id":-1}]"#,
            br#"[{"id":1,"id":2}]"#,
            br#"[{"id":1,"tags":["a"]}]"#,
            br#"[{"id":1,"tags":[1.5]}]"#,
            br#"[{"id":1,"fields":[{"name":"host","value":"a"},{"name":"host","value":"b"}]}]"#,
            br#"[{"\u0069d":1}]"#,
            br#"[{"id":1}],"#,
        ] {
            assert_eq!(
                parse_proxies(bad),
                None,
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
        assert_eq!(
            parse_tags(br#"[{"label":"lodestar-flaresolverr","id":7}]"#),
            Some(vec![tag(7, TAG_LABEL)])
        );
        assert_eq!(parse_tags(br#"[{"label":"x"}]"#), None);
        assert_eq!(parse_tag(br#"{"label":"x","id":2}"#), Some(tag(2, "x")));
        assert_eq!(parse_tag(br#"[{"label":"x","id":2}]"#), None);
    }

    #[test]
    fn objects_have_at_most_64_keys() {
        let obj = |n: usize| {
            let keys: Vec<String> = (0..n).map(|i| format!(r#""k{i}":0"#)).collect();
            format!(r#"[{{"id":1,{}}}]"#, keys.join(","))
        };
        assert!(parse_proxies(obj(MAX_KEYS - 1).as_bytes()).is_some());
        assert_eq!(parse_proxies(obj(MAX_KEYS).as_bytes()), None);
        let tags = |n: usize| {
            let keys: Vec<String> = (0..n).map(|i| format!(r#""k{i}":0"#)).collect();
            format!(r#"[{{"id":1,"label":"x",{}}}]"#, keys.join(","))
        };
        assert!(parse_tags(tags(MAX_KEYS - 2).as_bytes()).is_some());
        assert_eq!(parse_tags(tags(MAX_KEYS - 1).as_bytes()), None);
    }

    #[test]
    fn resource_is_lodestars_own() {
        let addr = SolverAddress::Loopback(PortNumber::new(8191).unwrap());
        let v: serde_json::Value = serde_json::from_slice(&resource(Some(4), &addr, 7)).unwrap();
        assert_eq!(v["id"], 4);
        assert_eq!(v["name"], PROXY_NAME);
        assert_eq!(v["implementation"], "FlareSolverr");
        assert_eq!(v["configContract"], "FlareSolverrSettings");
        assert_eq!(v["fields"][0]["value"], "http://127.0.0.1:8191/");
        assert_eq!(v["fields"][1]["value"], 60);
        assert_eq!(v["tags"], serde_json::json!([7]));
        let v: serde_json::Value = serde_json::from_slice(&resource(None, &addr, 7)).unwrap();
        assert!(v.get("id").is_none());
    }

    proptest! {
        #[test]
        fn parsers_are_total(body in proptest::collection::vec(any::<u8>(), 0..512)) {
            let _ = parse_index(&body);
            let _ = parse_health(&body);
            let _ = parse_proxies(&body);
            let _ = parse_tags(&body);
            let _ = parse_tag(&body);
        }

        #[test]
        fn json_shaped_noise_never_panics(s in r#"[\[\]{}",:0-9a-z\\ .-]{0,256}"#) {
            let _ = parse_index(s.as_bytes());
            let _ = parse_proxies(s.as_bytes());
            let _ = parse_tags(s.as_bytes());
        }

        #[test]
        fn index_versions_round_trip(a in 0u32..1000, b in 0u32..1000, c in 0u32..1000) {
            let body = format!(r#"{{"version":"{a}.{b}.{c}","msg":"FlareSolverr is ready!"}}"#);
            let v = parse_index(body.as_bytes()).unwrap().unwrap();
            prop_assert_eq!(v.as_str(), format!("{a}.{b}.{c}"));
        }
    }
}

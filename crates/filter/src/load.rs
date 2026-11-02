//! Builds the engine's rule layers from the stored rule set (§20.7). A
//! row that no longer reads (a query from a newer version after a
//! rollback, a corrupted row) is never fatal, and never loosens
//! filtering: a row that withholds fails closed, a row that releases is
//! skipped, and either is listed in [`Engine::unreadable`] for the
//! health exception (docs/decisions/2026-11-01-F-filter-load-fail-closed.md).

use lodestar_search::{Node, parse};
use lodestar_store::RuleSet;

use crate::core::Action;
use crate::engine::{Engine, RuleRef};
use crate::packs::{Pack, PackRule, PackSwitches};
use crate::rules::{Allow, ClassRule, ClassRules, Exception, Match, Override, Strength, UserRule};

/// `override_list.kind` / `filter_exceptions.kind`.
pub const KIND_HASHES: u8 = 0;
pub const KIND_KEYWORD: u8 = 1;
pub const KIND_QUERY: u8 = 2;
pub const KIND_PAYLOAD: u8 = 3;

/// The stored form of a match: its kind and value.
pub fn encode_match(m: &Match) -> (u8, Vec<u8>) {
    match m {
        Match::Hashes(hs) => (KIND_HASHES, hs.concat()),
        Match::Keyword(k) => (KIND_KEYWORD, k.as_bytes().to_vec()),
        Match::Query(q) => (KIND_QUERY, q.to_query().into_bytes()),
        Match::Payload(sig) => (KIND_PAYLOAD, sig.to_be_bytes().to_vec()),
    }
}

pub fn decode_match(kind: u8, value: &[u8]) -> Option<Match> {
    let text = || std::str::from_utf8(value).ok();
    Some(match kind {
        KIND_HASHES if value.len().is_multiple_of(20) && !value.is_empty() => Match::Hashes(
            value
                .chunks_exact(20)
                .map(|c| c.try_into().expect("20 bytes"))
                .collect(),
        ),
        KIND_KEYWORD => Match::Keyword(text()?.to_owned()),
        KIND_QUERY => Match::Query(parse(text()?).ok()?),
        KIND_PAYLOAD => Match::Payload(u64::from_be_bytes(value.try_into().ok()?)),
        _ => return None,
    })
}

/// The stored switches over no pack on: packs are off until chosen, in
/// the first-run wizard or in Settings (§20.6, owner's decision §2.8).
/// An install that never chose stays without packs.
pub fn pack_switches(stored: &[(String, bool)]) -> PackSwitches {
    let mut s = PackSwitches::default();
    for (id, on) in stored {
        if let Some(p) = Pack::from_name(id) {
            if *on {
                s.on.insert(p);
            } else {
                s.on.remove(&p);
            }
        } else if let Some(r) = PackRule::from_id(id) {
            if *on {
                s.rules_off.remove(&r);
            } else {
                s.rules_off.insert(r);
            }
        }
    }
    s
}

/// The kind of a stored filter row that couldn't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowKind {
    /// One of your rules: its query or its action.
    Rule,
    /// A rule's `unless` query.
    Unless,
    /// A classification rule.
    ClassRule,
    /// A CSX-5 entry.
    Override,
    /// An exception to one rule.
    Exception,
    /// An *Always allow* entry.
    Allow,
}

impl RowKind {
    pub const ALL: [RowKind; 6] = [
        RowKind::Rule,
        RowKind::Unless,
        RowKind::ClassRule,
        RowKind::Override,
        RowKind::Exception,
        RowKind::Allow,
    ];

    /// The `/metrics` label.
    pub fn as_str(self) -> &'static str {
        match self {
            RowKind::Rule => "rule",
            RowKind::Unless => "unless",
            RowKind::ClassRule => "class_rule",
            RowKind::Override => "override",
            RowKind::Exception => "exception",
            RowKind::Allow => "allow",
        }
    }
}

/// A stored row the engine couldn't read (docs/decisions/
/// 2026-11-01-F-filter-load-fail-closed.md). It stays listed until a
/// reload reads it, so the health exception clears once it's fixed or
/// removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    pub kind: RowKind,
    pub id: i64,
    /// Unix seconds it was first found unreadable, kept across reloads.
    pub since: i64,
    /// Losing it would have loosened filtering, so it fails closed: an
    /// enabled rule that withholds, an `unless` of an enabled rule, an Is
    /// adult classification rule. The rest are skipped, which only
    /// tightens.
    pub fails_closed: bool,
}

impl Unreadable {
    /// The id the Filters & Safety screens show: `U-3`, `O-4`, … A
    /// display name only; it's never stored.
    pub fn label(&self) -> String {
        let prefix = match self.kind {
            RowKind::Rule | RowKind::Unless => "U",
            RowKind::ClassRule => "C",
            RowKind::Override => "O",
            RowKind::Exception => "E",
            RowKind::Allow => "A",
        };
        format!("{prefix}-{}", self.id)
    }
}

/// The `settings` key of the [`StandIns`] record.
pub const STAND_INS_KEY: &str = "filter.stand_ins";

/// The rows that failed closed, kept across restarts in the store's
/// `settings` so their stand-ins' holds can be re-judged once the rows
/// read again, after an upgrade back or a repair
/// (docs/decisions/2026-11-02-F-filter-standin-undo.md). One entry per
/// row, as its label (`U-3` for a rule or its `unless`, `C-2` for an Is
/// adult classification rule) and when it was first found, in Unix
/// seconds: every hold its stand-in made is that time or later. Stored
/// as `U-3@1700000000 C-2@1700000100`. Labels this version doesn't know
/// are kept as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StandIns(pub Vec<(String, i64)>);

impl StandIns {
    /// `None` when the text isn't a record.
    pub fn decode(text: &str) -> Option<StandIns> {
        text.split_whitespace()
            .map(|t| {
                let (label, since) = t.split_once('@')?;
                Some((label.to_owned(), since.parse().ok()?))
            })
            .collect::<Option<Vec<_>>>()
            .map(StandIns)
    }

    pub fn encode(&self) -> String {
        let parts: Vec<String> = self.0.iter().map(|(l, s)| format!("{l}@{s}")).collect();
        parts.join(" ")
    }

    /// Merges a load's unreadable rows into the record. Returns the new
    /// record and the entries whose row no longer fails closed (it reads
    /// again, was removed or switched off): their holds are to be
    /// re-judged, and they stay in the record until that's done, so a stop
    /// part-way leaves it to redo. A row that still fails closed keeps the
    /// earliest time it was found.
    pub fn reconcile(&self, unreadable: &[Unreadable]) -> (StandIns, Vec<(String, i64)>) {
        let mut current: Vec<(String, i64)> = Vec::new();
        for u in unreadable.iter().filter(|u| u.fails_closed) {
            let label = u.label();
            let since = self
                .0
                .iter()
                .filter(|(l, _)| *l == label)
                .map(|(_, s)| *s)
                .fold(u.since, i64::min);
            match current.iter_mut().find(|(l, _)| *l == label) {
                Some(e) => e.1 = e.1.min(since),
                None => current.push((label, since)),
            }
        }
        let (mut recovered, mut other) = (Vec::new(), Vec::new());
        for e in &self.0 {
            if current.iter().any(|(l, _)| *l == e.0) {
                continue;
            }
            if StandIns::scope(&e.0).is_some() {
                recovered.push(e.clone());
            } else {
                other.push(e.clone());
            }
        }
        current.extend(recovered.iter().cloned());
        current.extend(other);
        (StandIns(current), recovered)
    }

    /// The record without one entry, once its holds are re-judged. An
    /// entry found again since (another time) stays.
    pub fn without(&self, label: &str, since: i64) -> StandIns {
        StandIns(
            self.0
                .iter()
                .filter(|(l, s)| !(l == label && *s == since))
                .cloned()
                .collect(),
        )
    }

    /// Which holds an entry's stand-in may have made: `Some(Some(rule))`
    /// for a rule's (`U-3`), `Some(None)` for every adult hold (an Is
    /// adult classification rule's, `C-2`), `None` for a label this
    /// version doesn't know.
    pub fn scope(label: &str) -> Option<Option<&str>> {
        let (kind, id) = label.split_once('-')?;
        id.parse::<u32>().ok()?;
        match kind {
            "U" => Some(Some(label)),
            "C" => Some(None),
            _ => None,
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// What a rule stands for when its query or action can't be read, or
/// `None` when it's skipped: an enabled rule that withholds (hide,
/// quarantine, drop, or an action this version doesn't know) quarantines
/// every torrent it could have matched. With the query unreadable that's
/// every torrent that reaches your rules outside its `unless`; a known
/// query keeps its own scope. Exceptions to the rule, *Always allow* and
/// CSX-5 still release from it, as they would from the real rule, and it
/// never pre-empts a stronger decision from the layers after it
/// ([`Engine::stand_ins`]). Quarantine, not drop or hide: it holds nothing
/// back for good, expires in 30 days, and fixing the rule releases it.
fn fail_closed(
    id: u32,
    query: Option<Node>,
    action: Option<Action>,
    enabled: bool,
    shadow_until: Option<i64>,
    unless: Option<Node>,
) -> Option<UserRule> {
    let withholds = action.is_none_or(|a| a >= Action::Hide);
    (enabled && withholds).then(|| UserRule {
        id,
        // `unless` still narrows it: everything but `unless` covers what
        // the real rule matched.
        unless,
        query: query.unwrap_or(Node::All),
        action: Action::Quarantine,
        enabled,
        shadow_until,
    })
}

impl Engine {
    /// Records the rows of `kinds` found unreadable by this load, keeping
    /// when each was first found.
    fn set_unreadable(&mut self, kinds: &[RowKind], found: Vec<(RowKind, i64, bool)>) {
        let now = unix_now();
        let before = std::mem::take(&mut self.unreadable);
        let (mut keep, before): (Vec<_>, Vec<_>) =
            before.into_iter().partition(|u| !kinds.contains(&u.kind));
        for (kind, id, fails_closed) in found {
            let since = before
                .iter()
                .find(|u| u.kind == kind && u.id == id)
                .map_or(now, |u| u.since);
            keep.push(Unreadable {
                kind,
                id,
                since,
                fails_closed,
            });
        }
        self.unreadable = keep;
    }

    /// Loads the classification rules (`(id, query, adult)` rows, oldest
    /// first). Returns notes about rows that couldn't be read.
    ///
    /// An unreadable Is adult rule matches everything in its place, so a
    /// torrent is adult unless a newer Not adult rule says otherwise: never
    /// less than the real rule would have marked. An unreadable Not adult
    /// rule is skipped, which only marks more adult.
    pub fn load_class_rules(&mut self, rows: &[(i64, String, bool)]) -> Vec<String> {
        let mut notes = Vec::new();
        let mut found = Vec::new();
        let rules: Vec<ClassRule> = rows
            .iter()
            .filter_map(|(id, q, adult)| {
                let query = match parse(q) {
                    Ok(query) => query,
                    Err(_) => {
                        found.push((RowKind::ClassRule, *id, *adult));
                        notes.push(format!(
                            "classification rule {id} {}: it no longer parses",
                            if *adult { "fails closed" } else { "skipped" }
                        ));
                        if !*adult {
                            return None;
                        }
                        Node::All
                    }
                };
                Some(ClassRule {
                    id: *id as u32,
                    query,
                    adult: *adult,
                })
            })
            .collect();
        self.class_rules = ClassRules::new(rules);
        self.set_unreadable(&[RowKind::ClassRule], found);
        notes
    }

    /// Replaces the rule layers with the stored set. Returns notes about
    /// rows that couldn't be read: your rules fail closed (an unreadable
    /// `unless` is dropped, which only narrows the exception; see
    /// [`fail_closed`] for the query and action), and unreadable entries
    /// that release (CSX-5, exceptions, *Always allow*) are skipped.
    pub fn load_rules(&mut self, set: &RuleSet) -> Vec<String> {
        let mut notes = Vec::new();
        let mut found = Vec::new();
        let mut stand_ins = Vec::new();
        self.packs = pack_switches(&set.pack_switches);
        self.rules = set
            .rules
            .iter()
            .filter_map(|r| {
                let query = parse(&r.query).ok();
                let unless = r.unless_query.as_deref().map(parse);
                let action = Action::from_code(r.action);
                let unless_lost = matches!(unless, Some(Err(_)));
                let unless = match unless {
                    None => None,
                    Some(Ok(u)) => Some(u),
                    Some(Err(_)) => {
                        found.push((RowKind::Unless, r.id, r.enabled));
                        notes.push(format!(
                            "rule U-{} {}: its unless no longer parses",
                            r.id,
                            if r.enabled { "fails closed" } else { "skipped" }
                        ));
                        None
                    }
                };
                match (query, action) {
                    (Some(query), Some(action)) => {
                        // Without its `unless` it matches more than the
                        // real rule, so it must not stop the layers after
                        // it from deciding where the real `unless` would
                        // have let them: it's a stand-in too.
                        if unless_lost {
                            stand_ins.push(r.id as u32);
                        }
                        Some(UserRule {
                            id: r.id as u32,
                            query,
                            action,
                            enabled: r.enabled,
                            shadow_until: r.shadow_until,
                            unless,
                        })
                    }
                    (query, action) => {
                        let query_read = query.is_some();
                        let rule = fail_closed(
                            r.id as u32,
                            query,
                            action,
                            r.enabled,
                            r.shadow_until,
                            unless,
                        );
                        found.push((RowKind::Rule, r.id, rule.is_some()));
                        notes.push(format!(
                            "rule U-{} {}: {}",
                            r.id,
                            if rule.is_some() {
                                "fails closed"
                            } else {
                                "skipped"
                            },
                            if query_read {
                                "unreadable"
                            } else {
                                "it no longer parses"
                            }
                        ));
                        stand_ins.extend(rule.as_ref().map(|r| r.id));
                        rule
                    }
                }
            })
            .collect();
        self.stand_ins = stand_ins;
        self.overrides = set
            .overrides
            .iter()
            .filter_map(|o| {
                let what = decode_match(o.kind, &o.value);
                let strength = match o.strength {
                    0 => Some(Strength::Override),
                    1 => Some(Strength::Neutralize),
                    _ => None,
                };
                let scope = match o.scope.as_deref().map(parse) {
                    None => Some(None),
                    Some(Ok(q)) => Some(Some(q)),
                    Some(Err(_)) => None,
                };
                match (what, strength, scope) {
                    (Some(what), Some(strength), Some(scope)) => Some(Override {
                        id: o.id as u32,
                        what,
                        strength,
                        scope,
                        expires_at: o.expires_at,
                        shadow_until: o.shadow_until,
                    }),
                    _ => {
                        found.push((RowKind::Override, o.id, false));
                        notes.push(format!("override O-{} skipped: unreadable", o.id));
                        None
                    }
                }
            })
            .collect();
        self.exceptions = set
            .exceptions
            .iter()
            .filter_map(|e| {
                let rule = RuleRef::from_id(&e.rule);
                match (rule, decode_match(e.kind, &e.value)) {
                    (Some(rule), Some(what)) => Some(Exception {
                        id: e.id as u32,
                        rule,
                        what,
                    }),
                    _ => {
                        found.push((RowKind::Exception, e.id, false));
                        notes.push(format!("exception {} skipped: unreadable", e.id));
                        None
                    }
                }
            })
            .collect();
        self.allow = set
            .allow
            .iter()
            .filter_map(|(id, q)| match parse(q) {
                Ok(query) => Some(Allow {
                    id: *id as u32,
                    query,
                }),
                Err(_) => {
                    found.push((RowKind::Allow, *id, false));
                    notes.push(format!("always-allow {id} skipped: it no longer parses"));
                    None
                }
            })
            .collect();
        self.set_unreadable(
            &[
                RowKind::Rule,
                RowKind::Unless,
                RowKind::Override,
                RowKind::Exception,
                RowKind::Allow,
            ],
            found,
        );
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_store::{ExceptionRow, OverrideRow, RuleRow};

    #[test]
    fn matches_round_trip() {
        for m in [
            Match::Hashes(vec![[1; 20], [2; 20]]),
            Match::Keyword("alpha beta".into()),
            Match::Query(parse("type:movie res:2160p").unwrap()),
            Match::Payload(u64::MAX - 3),
        ] {
            let (k, v) = encode_match(&m);
            assert_eq!(decode_match(k, &v), Some(m));
        }
        assert_eq!(decode_match(KIND_HASHES, &[1; 19]), None);
        assert_eq!(decode_match(9, b"x"), None);
    }

    #[test]
    fn loads_and_fails_closed_on_bad_rows() {
        let set = RuleSet {
            rules: vec![
                RuleRow {
                    id: 1,
                    query: "cam".into(),
                    action: Action::Hide.code(),
                    enabled: true,
                    shadow_until: None,
                    unless_query: Some("year:<1990".into()),
                },
                RuleRow {
                    id: 2,
                    query: "size:>>".into(),
                    action: 2,
                    enabled: true,
                    shadow_until: None,
                    unless_query: None,
                },
            ],
            pack_switches: vec![
                ("malware".into(), true),
                ("spam".into(), false),
                ("MW-1".into(), false),
            ],
            overrides: vec![OverrideRow {
                id: 4,
                kind: KIND_KEYWORD,
                value: b"alpha".to_vec(),
                strength: 1,
                scope: Some("type:adult".into()),
                created_at: 0,
                expires_at: None,
                shadow_until: None,
                hits: 0,
            }],
            exceptions: vec![ExceptionRow {
                id: 5,
                rule: "CS-4a".into(),
                kind: KIND_HASHES,
                value: vec![9; 20],
            }],
            allow: vec![(6, "group:goodgroup".into())],
        };
        let lists = crate::Lists {
            sexual: crate::TermSet::default(),
            child_roles: crate::TermSet::default(),
            codes: crate::TermSet::default(),
        };
        let mut e = Engine::new(crate::Core::new(
            lists,
            crate::CsList::default(),
            crate::Switches::default(),
        ));
        let notes = e.load_rules(&set);
        assert_eq!(notes.len(), 1, "{notes:?}");
        // The unreadable hide rule fails closed (tests/load_fail_closed.rs).
        assert_eq!(e.rules.len(), 2);
        assert!(e.rules[0].unless.is_some());
        assert_eq!(e.rules[1].action, Action::Quarantine);
        assert_eq!(e.unreadable.len(), 1);
        assert_eq!(e.unreadable[0].label(), "U-2");
        assert!(!e.packs.on.contains(&Pack::Spam));
        assert!(!e.packs.is_on(PackRule::Mw1) && e.packs.is_on(PackRule::Mw2));
        assert_eq!(e.overrides[0].strength, Strength::Neutralize);
        assert_eq!(e.exceptions[0].rule, RuleRef::Core(crate::CoreRule::Cs4a));
        assert_eq!(e.allow.len(), 1);
    }

    #[test]
    fn stand_ins_record_reconciles_with_each_load() {
        let u = |kind, id, since, fails_closed| Unreadable {
            kind,
            id,
            since,
            fails_closed,
        };
        assert_eq!(StandIns::decode(""), Some(StandIns::default()));
        assert_eq!(StandIns::decode("U-3@x"), None);
        let before = StandIns::decode("U-3@100 C-2@50 X-9@7").unwrap();
        assert_eq!(StandIns::decode(&before.encode()), Some(before.clone()));
        // U-3 still fails closed (its query and its `unless`: one row),
        // C-2 reads again, U-4 is new, the skipped O-5 isn't recorded.
        let (record, recovered) = before.reconcile(&[
            u(RowKind::Rule, 3, 900, true),
            u(RowKind::Unless, 3, 900, true),
            u(RowKind::Rule, 4, 900, true),
            u(RowKind::Override, 5, 900, false),
        ]);
        assert_eq!(recovered, vec![("C-2".to_owned(), 50)]);
        assert_eq!(
            record.encode(),
            "U-3@100 U-4@900 C-2@50 X-9@7",
            "the earliest time, and the recovered entry until it's done"
        );
        assert_eq!(record.without("C-2", 50).encode(), "U-3@100 U-4@900 X-9@7");
        assert_eq!(record.without("C-2", 51), record);
        assert_eq!(StandIns::scope("U-3"), Some(Some("U-3")));
        assert_eq!(StandIns::scope("C-2"), Some(None));
        assert_eq!(StandIns::scope("X-9"), None);
        assert_eq!(StandIns::scope("U-x"), None);
    }

    #[test]
    fn packs_are_off_until_chosen() {
        // Nothing stored: a fresh install, or one that skipped the wizard.
        let s = pack_switches(&[]);
        assert_eq!(s, PackSwitches::default());
        assert!(PackRule::ALL.iter().all(|r| !s.is_on(*r)));
        // A pack rule switched on doesn't turn its pack on.
        assert!(!pack_switches(&[("MW-2".into(), true)]).is_on(PackRule::Mw2));
        let s = pack_switches(&[("lures".into(), true)]);
        assert!(s.is_on(PackRule::Lu1) && !s.is_on(PackRule::Mw1));
    }
}

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

    /// Losing a row of this kind would loosen filtering, so it fails
    /// closed instead of being skipped. The others only release, so
    /// skipping them tightens.
    pub fn loosening(self) -> bool {
        matches!(self, RowKind::Rule | RowKind::Unless | RowKind::ClassRule)
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
/// CSX-5 still release from it, as they would from the real rule. Quarantine, not drop or hide: it holds nothing back for good,
/// expires in 30 days, and fixing the rule releases it.
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
    fn set_unreadable(&mut self, kinds: &[RowKind], found: Vec<(RowKind, i64)>) {
        let now = unix_now();
        let before = std::mem::take(&mut self.unreadable);
        let (mut keep, before): (Vec<_>, Vec<_>) =
            before.into_iter().partition(|u| !kinds.contains(&u.kind));
        for (kind, id) in found {
            let since = before
                .iter()
                .find(|u| u.kind == kind && u.id == id)
                .map_or(now, |u| u.since);
            keep.push(Unreadable { kind, id, since });
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
                        found.push((RowKind::ClassRule, *id));
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
        self.packs = pack_switches(&set.pack_switches);
        self.rules = set
            .rules
            .iter()
            .filter_map(|r| {
                let query = parse(&r.query).ok();
                let unless = r.unless_query.as_deref().map(parse);
                let action = Action::from_code(r.action);
                let unless = match unless {
                    None => None,
                    Some(Ok(u)) => Some(u),
                    Some(Err(_)) => {
                        found.push((RowKind::Unless, r.id));
                        notes.push(format!(
                            "rule U-{} fails closed: its unless no longer parses",
                            r.id
                        ));
                        None
                    }
                };
                match (query, action) {
                    (Some(query), Some(action)) => Some(UserRule {
                        id: r.id as u32,
                        query,
                        action,
                        enabled: r.enabled,
                        shadow_until: r.shadow_until,
                        unless,
                    }),
                    (query, action) => {
                        found.push((RowKind::Rule, r.id));
                        let query_read = query.is_some();
                        let rule = fail_closed(
                            r.id as u32,
                            query,
                            action,
                            r.enabled,
                            r.shadow_until,
                            unless,
                        );
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
                        rule
                    }
                }
            })
            .collect();
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
                        found.push((RowKind::Override, o.id));
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
                        found.push((RowKind::Exception, e.id));
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
                    found.push((RowKind::Allow, *id));
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

//! Builds the engine's rule layers from the stored rule set (§20.7). A
//! row that no longer parses (a query from a newer version, a removed
//! pack rule) is skipped with a note, never fatal: filtering keeps
//! working with everything else.

use lodestar_search::parse;
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

impl Engine {
    /// Loads the classification rules (`(id, query, adult)` rows, oldest
    /// first). Returns notes about rows that were skipped.
    pub fn load_class_rules(&mut self, rows: &[(i64, String, bool)]) -> Vec<String> {
        let mut notes = Vec::new();
        let rules: Vec<ClassRule> = rows
            .iter()
            .filter_map(|(id, q, adult)| match parse(q) {
                Ok(query) => Some(ClassRule {
                    id: *id as u32,
                    query,
                    adult: *adult,
                }),
                Err(_) => {
                    notes.push(format!(
                        "classification rule {id} skipped: it no longer parses"
                    ));
                    None
                }
            })
            .collect();
        self.class_rules = ClassRules::new(rules);
        notes
    }

    /// Replaces the rule layers with the stored set. Returns notes about
    /// rows that were skipped.
    pub fn load_rules(&mut self, set: &RuleSet) -> Vec<String> {
        let mut notes = Vec::new();
        self.packs = pack_switches(&set.pack_switches);
        self.rules = set
            .rules
            .iter()
            .filter_map(|r| {
                let query = parse(&r.query).ok();
                let unless = r.unless_query.as_deref().map(parse);
                let action = Action::from_code(r.action);
                match (query, unless, action) {
                    (Some(query), None | Some(Ok(_)), Some(action)) => Some(UserRule {
                        id: r.id as u32,
                        query,
                        action,
                        enabled: r.enabled,
                        shadow_until: r.shadow_until,
                        unless: r.unless_query.as_deref().and_then(|u| parse(u).ok()),
                    }),
                    _ => {
                        notes.push(format!("rule U-{} skipped: it no longer parses", r.id));
                        None
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
                    notes.push(format!("always-allow {id} skipped: it no longer parses"));
                    None
                }
            })
            .collect();
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
    fn loads_and_skips_bad_rows() {
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
        assert_eq!(e.rules.len(), 1);
        assert!(e.rules[0].unless.is_some());
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

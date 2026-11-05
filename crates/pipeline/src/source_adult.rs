//! Sources whose every find is adult (owner Q7, Sukebei;
//! `docs/decisions/2026-11-05-X-source-adult.md`).
//!
//! A plugin whose manifest says [`always_adult`] has every item it finds
//! classified Adult, as an Is adult correction makes it (§20.9): the
//! nearest adult kind, at full confidence. The signal is the source, never
//! an item's claims, so a feed can't turn it off, and it only ever makes
//! an item adult:
//! - **Stricter only.** Filtering judges detection's own reading *and* the
//!   adult one, and keeps the stricter verdict, as for an Is adult rule:
//!   the floor and the child-safety core first, then the Adult-only core
//!   (CS-5a and the rest) on the adult reading.
//! - **Every classification point.** The worker's prepare, the writer's
//!   re-judge and promotion, an unverified item's claims when stored and
//!   swept, and the background enrichment of a stored torrent all apply
//!   it, from the torrent's `sources` bitset, so a later clean name or a
//!   newer parser can't un-adult it. A torrent the DHT also found stays
//!   adult: any always-adult source is enough.
//! - **Your own Not adult correction wins.** It's your explicit act on one
//!   torrent. A Not adult *classification rule* doesn't: rules match many
//!   torrents, and the source is a stronger signal than a name pattern.
//!
//! [`always_adult`]: lodestar_sources::Manifest::always_adult

use lodestar_classify::{ContentType, Correction};
use lodestar_filter::reclassify::rule_fix;
use lodestar_filter::{Engine, Facts, Item, Verdict};
use lodestar_store::SourceId;

/// The registered sources whose manifests say `always_adult`, as a bitset
/// over [`SourceId`]s (the same form as a torrent's `sources`). Empty
/// when none is registered, which costs nothing anywhere.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdultSources(pub u64);

impl AdultSources {
    /// The always-adult ones among registered `(id, manifest)` pairs.
    pub fn of<'a>(
        registered: impl IntoIterator<Item = (SourceId, &'a lodestar_sources::Manifest)>,
    ) -> AdultSources {
        AdultSources(
            registered
                .into_iter()
                .filter(|(_, m)| m.always_adult)
                .fold(0, |bits, (id, _)| bits | bit(id)),
        )
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Any source in the bitset `sources` is always adult.
    pub fn any(self, sources: u64) -> bool {
        self.0 & sources != 0
    }

    pub fn has(self, source: SourceId) -> bool {
        self.any(bit(source))
    }
}

fn bit(source: SourceId) -> u64 {
    1u64.checked_shl(u32::from(source.get())).unwrap_or(0)
}

/// The correction a stored torrent is classified with: its own correction,
/// with Is adult added when one of its `sources` is always adult and you
/// haven't said Not adult.
pub fn correction(own: Correction, sources: u64, adult: AdultSources) -> Correction {
    match own.adult {
        None if adult.any(sources) => Correction {
            adult: Some(true),
            ..own
        },
        _ => own,
    }
}

/// What filtering makes of an item an always-adult source found, and the
/// fix to store with it: `Some(true)` when detection didn't call it adult,
/// else `None` (a Not adult rule never applies to it).
///
/// The engine's verdict with your classification rules
/// ([`Engine::judge_classified`]), and, unless that already judged the
/// adult reading, the adult reading too; the stricter of the two stands.
/// The floor's hit comes back alone, as it does from the engine.
pub fn judge(
    engine: &Engine,
    item: &Item<'_>,
    facts: &Facts<'_>,
    now: i64,
) -> (Verdict, Option<bool>) {
    let (v, says) = engine.judge_classified(item, Some(facts), now);
    let detected = facts.class.content_type == ContentType::Adult;
    let fix = (!detected).then_some(true);
    // The engine judged it as adult already: detection or an Is adult
    // rule said so.
    if detected || item.content_type == ContentType::Adult || says == Some(true) {
        return (v, fix);
    }
    // A floor hit is final, as the engine returns it.
    if v.decision
        .as_ref()
        .is_some_and(|d| matches!(d.rule, lodestar_filter::RuleRef::Core(r) if r.floor()))
    {
        return (v, fix);
    }
    let adult_class = facts.class.as_adult();
    let adult_facts = Facts {
        class: &adult_class,
        ..*facts
    };
    let adult_item = Item {
        content_type: ContentType::Adult,
        ..*item
    };
    let as_adult = engine.judge(&adult_item, Some(&adult_facts), now);
    (stricter(v, as_adult), fix)
}

/// [`judge`] when `adult`, else the engine's verdict with your
/// classification rules and what they change ([`rule_fix`]).
pub fn judge_maybe(
    engine: &Engine,
    item: &Item<'_>,
    facts: &Facts<'_>,
    adult: bool,
    now: i64,
) -> (Verdict, Option<bool>) {
    if adult {
        judge(engine, item, facts, now)
    } else {
        let (v, says) = engine.judge_classified(item, Some(facts), now);
        (v, rule_fix(says, facts.class))
    }
}

/// The heavier decision of two verdicts (a tie keeps `a`'s), and both
/// verdicts' shadow decisions: the engine's own rule for two readings.
pub fn stricter(a: Verdict, b: Verdict) -> Verdict {
    let decision = match (a.decision, b.decision) {
        (Some(x), Some(y)) => Some(if y.action > x.action { y } else { x }),
        (x, y) => x.or(y),
    };
    let mut shadow = a.shadow;
    for d in b.shadow {
        if !shadow.contains(&d) {
            shadow.push(d);
        }
    }
    Verdict { decision, shadow }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestar_classify::{FileInfo, classify};
    use lodestar_filter::{Action, Core, CoreRule, CsList, Lists, RuleRef, Switches, TermSet};

    /// The writer tests' terms: a role, a sexual term, a studio.
    const NAME: &str = "Studiox Rolea one two three four five six seven eight nine Lexa";

    fn id(n: u8) -> SourceId {
        SourceId::new(n).unwrap()
    }

    fn manifest(always_adult: bool) -> lodestar_sources::Manifest {
        lodestar_sources::Manifest {
            id: "m",
            name: "m",
            version: "1",
            kinds: &[],
            networks: &[],
            endpoints: &[],
            risk: lodestar_sources::Risk::Low,
            consent: lodestar_sources::Consent::None,
            default_priority: lodestar_sources::Priority::Normal,
            enabled_by_default: false,
            always_adult,
        }
    }

    #[test]
    fn the_mask_holds_only_always_adult_sources() {
        let (a, n) = (manifest(true), manifest(false));
        let mask = AdultSources::of([(id(3), &a), (id(5), &n), (id(9), &a)]);
        assert_eq!(mask.0, (1 << 3) | (1 << 9));
        assert!(mask.has(id(3)) && mask.has(id(9)) && !mask.has(id(5)));
        assert!(mask.any((1 << 5) | (1 << 9)) && !mask.any(1 << 5));
        assert!(AdultSources::default().is_empty() && !mask.is_empty());
    }

    #[test]
    fn the_correction_adds_is_adult_but_never_overrides_yours() {
        let mask = AdultSources(1 << 4);
        let none = Correction::default();
        assert_eq!(correction(none, 1 << 4 | 1, mask).adult, Some(true));
        assert_eq!(correction(none, 1, mask).adult, None);
        let not_adult = Correction {
            adult: Some(false),
            not_junk: true,
        };
        assert_eq!(correction(not_adult, 1 << 4, mask), not_adult);
        let junk = Correction {
            adult: None,
            not_junk: true,
        };
        assert_eq!(
            correction(junk, 1 << 4, mask),
            Correction {
                adult: Some(true),
                not_junk: true
            }
        );
    }

    fn engine() -> Engine {
        let lists = Lists {
            sexual: TermSet::of(&["lexa"]),
            child_roles: TermSet::of(&["rolea"]),
            codes: TermSet::of(&["codex"]),
        };
        Engine::new(Core::new(lists, CsList::default(), Switches::default()))
    }

    fn judged(e: &Engine, name: &str, files: &[FileInfo], adult: bool) -> (Verdict, Option<bool>) {
        let class = classify(name, files);
        let hashes = [[7u8; 20]];
        let item = Item {
            hashes: &hashes,
            name,
            files,
            content_type: class.content_type,
        };
        let facts = Facts {
            class: &class,
            size: files.iter().map(|f| f.size).sum(),
            piece_len: 0,
            flags: 0,
            discovered_at: 0,
            now: 0,
        };
        judge_maybe(e, &item, &facts, adult, 0)
    }

    #[test]
    fn a_clean_name_is_made_adult_and_judged_as_adult() {
        let e = engine();
        let files = [FileInfo::new(
            vec!["Holiday.2019.1080p.mkv".into()],
            4 << 30,
        )];
        let (v, fix) = judged(&e, "Holiday.2019.1080p", &files, false);
        assert_eq!((v.decision, fix), (None, None));
        let (v, fix) = judged(&e, "Holiday.2019.1080p", &files, true);
        assert_eq!((v.decision, fix), (None, Some(true)));
    }

    #[test]
    fn the_adult_only_core_applies_to_the_adult_reading() {
        // CS-5a checks only adult items: the same name from a non-adult
        // source isn't adult, so it isn't held.
        let e = engine();
        let files = [FileInfo::new(vec![format!("{NAME}.mp4")], 900 << 20)];
        assert_ne!(classify(NAME, &files).content_type, ContentType::Adult);
        assert_eq!(judged(&e, NAME, &files, false).0.decision, None);
        let (v, fix) = judged(&e, NAME, &files, true);
        let d = v.decision.expect("held on the adult reading");
        assert_eq!(d.rule, RuleRef::Core(CoreRule::Cs5a));
        assert_eq!(d.action, Action::Quarantine);
        assert_eq!(fix, Some(true));
    }

    #[test]
    fn a_not_adult_rule_never_un_adults_it() {
        let mut e = engine();
        e.load_class_rules(&[(1, "holiday".into(), false)]);
        let files = [FileInfo::new(
            vec!["Holiday.2019.1080p.mkv".into()],
            4 << 30,
        )];
        let (_, fix) = judged(&e, "Holiday.2019.1080p", &files, true);
        assert_eq!(fix, Some(true));
    }

    #[test]
    fn stricter_keeps_the_heavier_decision_and_every_shadow() {
        let d = |action| lodestar_filter::Decision {
            rule: RuleRef::Core(CoreRule::Cs5a),
            action,
            evidence: vec![],
        };
        let a = Verdict {
            decision: Some(d(Action::Flag)),
            shadow: vec![d(Action::Hide)],
        };
        let b = Verdict {
            decision: Some(d(Action::Quarantine)),
            shadow: vec![d(Action::Hide), d(Action::Drop)],
        };
        let s = stricter(a, b);
        assert_eq!(s.decision.unwrap().action, Action::Quarantine);
        assert_eq!(s.shadow.len(), 2);
        let none = stricter(Verdict::default(), Verdict::default());
        assert!(none.decision.is_none());
    }
}

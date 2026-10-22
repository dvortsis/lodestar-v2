//! Layer precedence (§20.1) with neutral stand-in terms: `lexa` for a
//! sexual term, `rolea` for a child role.

use lodestar_classify::{ContentType, FileInfo, classify};
use lodestar_filter::{
    Action, Allow, Core, CoreRule, CsList, Engine, Exception, Facts, Item, Lists, Match, Origin,
    Override, Pack, PackRule, PackSwitches, RuleRef, Strength, Switches, TermSet, UserRule,
};
use lodestar_search::parse;

fn engine() -> Engine {
    let lists = Lists {
        sexual: TermSet::of(&["lexa"]),
        child_roles: TermSet::of(&["rolea"]),
        codes: TermSet::of(&["codex"]),
    };
    let mut e = Engine::new(Core::new(lists, CsList::default(), Switches::default()));
    e.packs = PackSwitches::all();
    e
}

const H: [u8; 20] = [7; 20];

fn judge(e: &Engine, name: &str, paths: &[(&str, u64)], now: i64) -> lodestar_filter::Verdict {
    let f: Vec<FileInfo> = paths
        .iter()
        .map(|(p, s)| FileInfo::new(p.split('/').map(str::to_owned).collect(), *s))
        .collect();
    let class = classify(name, &f);
    let facts = Facts {
        class: &class,
        size: f.iter().map(|x| x.size).sum(),
        piece_len: 1 << 20,
        flags: 0,
        discovered_at: now,
        now,
    };
    let item = Item {
        hashes: &[H],
        name,
        files: &f,
        content_type: class.content_type,
    };
    e.judge(&item, Some(&facts), now)
}

fn rule(e: &Engine, name: &str, paths: &[(&str, u64)]) -> Option<RuleRef> {
    judge(e, name, paths, 1000).decision.map(|d| d.rule)
}

const VIDEO: &[(&str, u64)] = &[("a.mkv", 2 << 30)];

fn keyword(k: &str, strength: Strength) -> Override {
    Override {
        id: 1,
        what: Match::Keyword(k.into()),
        strength,
        scope: None,
        expires_at: None,
        shadow_until: None,
    }
}

#[test]
fn the_floor_is_final() {
    let mut e = engine();
    e.core.cs.add_hash(H, Origin::Confirmed);
    e.overrides.push(Override {
        what: Match::Hashes(vec![H]),
        ..keyword("x", Strength::Override)
    });
    e.allow.push(Allow {
        id: 1,
        query: parse("anything").unwrap(),
    });
    assert_eq!(
        rule(&e, "Anything", VIDEO),
        Some(RuleRef::Core(CoreRule::Cs1a))
    );
}

#[test]
fn overrides_pass_the_core_and_neutralize_only_their_word() {
    let mut e = engine();
    let name = "Series rolea lexa";
    assert_eq!(rule(&e, name, VIDEO), Some(RuleRef::Core(CoreRule::Cs4a)));
    e.overrides = vec![keyword("series", Strength::Override)];
    assert_eq!(rule(&e, name, VIDEO), None);
    // Scoped to adult: no longer covers this video.
    e.overrides[0].scope = Some(parse("type:adult").unwrap());
    assert_eq!(rule(&e, name, VIDEO), Some(RuleRef::Core(CoreRule::Cs4a)));
    // Neutralize: the word stops counting, other evidence still does.
    e.overrides = vec![keyword("lexa", Strength::Neutralize)];
    assert_eq!(rule(&e, name, VIDEO), None);
    let with_age = "Series 15yo lexa rolea";
    e.overrides = vec![keyword("rolea", Strength::Neutralize)];
    assert_eq!(
        rule(&e, with_age, VIDEO),
        Some(RuleRef::Core(CoreRule::Cs2a))
    );
    // Expired entries and shadow entries don't release.
    e.overrides = vec![Override {
        expires_at: Some(500),
        ..keyword("series", Strength::Override)
    }];
    assert_eq!(rule(&e, name, VIDEO), Some(RuleRef::Core(CoreRule::Cs4a)));
    e.overrides = vec![Override {
        shadow_until: Some(2000),
        ..keyword("series", Strength::Override)
    }];
    let v = judge(&e, name, VIDEO, 1000);
    assert_eq!(
        v.decision.map(|d| d.rule),
        Some(RuleRef::Core(CoreRule::Cs4a))
    );
    assert_eq!(v.shadow[0].rule, RuleRef::Override(1));
}

#[test]
fn exceptions_cover_one_rule_only() {
    let mut e = engine();
    e.exceptions.push(Exception {
        id: 1,
        rule: RuleRef::Core(CoreRule::Cs4a),
        what: Match::Hashes(vec![H]),
    });
    assert_eq!(rule(&e, "Series rolea lexa", VIDEO), None);
    // CS-2a still checks it.
    assert_eq!(
        rule(&e, "Series rolea lexa 15yo", VIDEO),
        Some(RuleRef::Core(CoreRule::Cs2a))
    );
}

#[test]
fn your_rules_with_unless_and_shadow() {
    let mut e = engine();
    e.rules.push(UserRule {
        id: 3,
        query: parse("cam | telesync").unwrap(),
        action: Action::Hide,
        enabled: true,
        shadow_until: None,
        unless: Some(parse("year:<1990").unwrap()),
    });
    assert_eq!(rule(&e, "Movie 2023 CAM", VIDEO), Some(RuleRef::User(3)));
    assert_eq!(rule(&e, "Movie 1985 CAM", VIDEO), None);
    e.rules[0].shadow_until = Some(5000);
    let v = judge(&e, "Movie 2023 CAM", VIDEO, 1000);
    assert!(v.decision.is_none());
    assert_eq!(v.shadow[0].rule, RuleRef::User(3));
    e.rules[0].enabled = false;
    assert!(judge(&e, "Movie 2023 CAM", VIDEO, 1000).shadow.is_empty());
}

#[test]
fn always_allow_skips_rules_and_packs_but_not_the_core() {
    let mut e = engine();
    let exe = &[("Movie.2019.mp4.exe", 2_000_000u64)][..];
    assert_eq!(
        rule(&e, "Movie 2019", exe),
        Some(RuleRef::Pack(PackRule::Mw1))
    );
    e.allow.push(Allow {
        id: 1,
        query: parse("movie").unwrap(),
    });
    assert_eq!(rule(&e, "Movie 2019", exe), None);
    assert_eq!(
        rule(&e, "Movie rolea lexa", VIDEO),
        Some(RuleRef::Core(CoreRule::Cs4a))
    );
}

#[test]
fn packs_switch_and_take_exceptions() {
    let mut e = engine();
    let exe = &[("Movie.2019.mp4.exe", 2_000_000u64)][..];
    e.packs.rules_off.insert(PackRule::Mw1);
    assert_eq!(rule(&e, "Movie 2019", exe), None);
    e.packs = PackSwitches::all();
    e.packs.on.remove(&Pack::Malware);
    assert_eq!(rule(&e, "Movie 2019", exe), None);
    e.packs = PackSwitches::all();
    e.exceptions.push(Exception {
        id: 2,
        rule: RuleRef::Pack(PackRule::Mw1),
        what: Match::Query(parse("movie").unwrap()),
    });
    assert_eq!(rule(&e, "Movie 2019", exe), None);
}

#[test]
fn rule_ids_round_trip() {
    for r in [
        RuleRef::Core(CoreRule::Cs2a),
        RuleRef::Pack(PackRule::Sc1),
        RuleRef::List(4),
        RuleRef::User(12),
        RuleRef::Override(9),
    ] {
        assert_eq!(RuleRef::from_id(&r.id()), Some(r));
    }
    assert_eq!(RuleRef::from_id("X-1"), None);
    assert_eq!(RuleRef::from_id("U-x"), None);
}

#[test]
fn prefetch_refusal_stays_conservative() {
    use std::collections::HashSet;
    use std::sync::Arc;
    let mut e = engine();
    e.set_list(
        Some(lodestar_filter::ListLayer {
            id: 1,
            action: Action::Drop,
            hashes: Arc::new(HashSet::from([H])),
        }),
        1,
    );
    assert!(e.blocked_hash(&[H]).is_some());
    // An allow query might match once the name is known: judge later.
    e.allow.push(Allow {
        id: 1,
        query: parse("x").unwrap(),
    });
    assert!(e.blocked_hash(&[H]).is_none());
    let _ = ContentType::Video;
}

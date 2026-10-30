# Lodestar v0.5 code review: Area 3 results

Date: 2026-10-24

Review instructions: [codex-review-v0.5.md](codex-review-v0.5.md).

Reviewed a fresh clone of `main` at `2d5e761d30cb1b7947fb09493959668eabcb84ab`, newer than the instructions' v0.5 gate commit `4667277`. Locations below refer to that commit. This session covered **Area 3: content filtering and adult detection**. All three requested areas are now reviewed; earlier results are in [Area 1](codex-review-v0.5-area-1-results.md) and [Area 2](codex-review-v0.5-area-2-results.md).

No source files were changed and no GitHub actions were taken. This report was saved at the owner's request.

Validation and scope:

- Reviewed the ten scoped source files, architecture §20, owner decisions §1.8 and relevant content-safety decisions in §2, and runner standards. Traced candidate findings through subscription loading, filter-change and classification APIs, ingest and background enrichment. Inspected search, facets, Discover and event visibility, quarantine sealing, and classification-generation checks.
- `cargo test --locked --offline -p lodestar-filter -p lodestar-classify` passed on Windows: **147 passed, 0 failed, 3 ignored**. The ignored tests measure release-mode performance; those benchmarks were not run.
- Four isolated `rustc --test` probes, supplied through stdin and linked against the compiled production crates, failed their expected-state assertions and confirmed the three findings below. They use neutral fixture words and an in-memory store. No test files were added to the clone; its final `git status --short` was empty.
- Checked existing unit, integration and property tests before reporting. The existing prefilter/matcher equivalence tests passed. No fuzz campaign or live network crawl was run.
- Excluded documented token/file examination bounds and the deliberate distinction between core normalization and the search tokenizer used by query rules. These are not new findings.

### 1. An exception to one subscription also suppresses other matching lists

Severity: medium  
Location: `crates/filter/src/engine.rs:313` (selection at lines 362–367)  
Input / scenario: Two non-floor content subscriptions contain hash H: L-1 quarantines it and L-2 drops it. Through `POST /api/v1/filters/apply`, add an exact-hash exception for H under L-2 only, with the required confirmation. When H is subsequently discovered, the L-2 exception permits metadata fetching. There is no matching core rule, user rule or pack in this example. Both subscription actions are accepted by the API; `lists.rs:596` installs their layers in the engine.  
Expected: Architecture §20.12 limits an exception to its named rule, and §20.6 says the strongest applicable pack/list action wins. L-1 must still quarantine H.  
Actual: `listed()` first runs `.max_by_key(|l| l.action)?`, collapsing all matching lists to L-2. The caller then executes `.chain(self.listed(item.hashes)).filter(|d| !self.excepted(d.rule, item, doc))`, removing that single decision without considering L-1 again. The probe confirmed that the initial verdict is L-2/Drop, `blocked_hash()` permits fetching after the exception, and the final metadata verdict becomes `None` instead of L-1/Quarantine. Ingest consequently accepts the torrent. Existing exception tests cover core-rule isolation and an individual pack; the prefetch test has only one list.  
Confidence: high  
Suggested fix: Evaluate exceptions for each matching list before choosing the strongest surviving action. Preserve conservative prefetch behavior when an exception or allow rule needs metadata, then evaluate every applicable list after fetching.

### 2. Stored classification rules evaluate hash predicates against an empty hash set

Severity: medium  
Location: `crates/filter/src/reclassify.rs:154`  
Input / scenario: An already stored, ordinary video with hash H has no per-torrent correction. Submit `POST /api/v1/classify/rules` with `{"query":"hash:<H>","adult":true}`. The query is accepted: `class_query()` rejects type/adult predicates and an empty query, but permits hashes. `add_class_rule()` queues matching stored rows, and background enrichment calls `reclassify_row()`. The probe uses a valid single-file torrent named `Amber Beacon.mkv`.  
Expected: Architecture §20.7's V9 implementation description and §20.9 require the newest matching classification rule to apply both at ingest and whenever a stored torrent is classified again, unless its own correction takes precedence. H must become Adult and receive the corresponding visibility and filtering treatment.  
Actual: `rule_says()` constructs `Item { hashes: &[], ... }`. The hash matcher at `matcher.rs:975` tests `self.item.hashes.iter().any(...)`, which is always false here. The same production engine returns `Some(true)` for the ingest item containing H, but stored reclassification returns `Video`. The row therefore remains eligible for normal search with adult content hidden. Negated hash predicates also receive the wrong truth value. Existing API and pipeline classification-rule tests use text predicates; they do not cover stored hash predicates.  
Confidence: high  
Suggested fix: Populate the classification-rule item with `row_hashes(row)`, including the hybrid swarm hash, as the later adult-filtering pass already does. Cover positive and negated hash predicates during stored reclassification, with ingest parity and per-torrent correction precedence.

### 3. Neutralized words still supply evidence inside phrases and NEAR expressions

Severity: medium  
Location: `crates/filter/src/matcher.rs:879` (also line 882)  
Input / scenario: A user Hide rule is either `"amber beacon"` or `amber NEAR/1 beacon`. Add a validated, active CSX-5 **Neutralize term** override for the single keyword `amber`, without scope, expiry or shadow mode. A torrent named `Amber Beacon` still matches the Hide rule. These are normal rule and override entries accepted through the filter-change API and loaded into `Engine`.  
Expected: Architecture §20.6 explicitly says Neutralize term removes that word's evidence from core and query rules. The matcher test `neutralized_words_never_count` establishes that a neutralized required word becomes false while other words remain available. Neither expression can use `amber` as its required evidence after neutralization.  
Actual: The phrase branch evaluates `!neutral_word(fold) && texts(*field, &|t| sequence(t, parts))`: it compares the neutralized word to the fold of the entire phrase, not each required word. The NEAR branch is `texts(*field, &|t| has_near(t, words, *slop))` and never consults neutralization. Both probes still return `Some(Decision { rule: User(1), action: Hide, evidence: [Query] })`. The existing neutralization tests exercise bare words and their negation, not neutralized components of phrases or NEAR expressions.  
Confidence: high  
Suggested fix: Apply neutralization when evaluating the words that supply phrase and proximity evidence, keeping positional semantics intact. Add phrase and NEAR regressions through the full engine, including their use under negation and `unless`.

Reproduction evidence (observed output, with panic boilerplate omitted):

```text
Excepted L-2 drop; unexcepted L-1 quarantine: None
list_exception_does_not_cancel_another_list ... FAILED
  left: None
 right: Some((List(1), Quarantine))

Neutralized amber, rule amber NEAR/1 beacon:
Some(Decision { rule: User(1), action: Hide, evidence: [Query] })
neutralize_applies_inside_near ... FAILED

Neutralized amber, rule "amber beacon":
Some(Decision { rule: User(1), action: Hide, evidence: [Query] })
neutralize_applies_inside_phrase ... FAILED

hash rule: ingest says adult=true; stored reclassification=Video
stored_hash_classification_matches_ingest ... FAILED
  left: Video
 right: Adult

test result: FAILED. 0 passed; 4 failed; 0 ignored
```

The probes supplied to `rustc --test` are preserved below for reproduction against the reviewed commit. They call production methods directly; API reachability was traced in the source rather than exercised by these four probes.

<details>
<summary>Reproduction source</summary>

```rust
use lodestar_filter::{Action, Core, CsList, Engine, Exception, Item, Lists, Match, RuleRef, Switches, TermSet, ListLayer, Override, Strength, UserRule, ClassRule, ClassRules, Facts};
use lodestar_classify::{ContentType, FileInfo, Correction, classify};
use std::{collections::HashSet, sync::Arc};
fn engine() -> Engine {
 Engine::new(Core::new(Lists { sexual: TermSet::default(), child_roles: TermSet::default(), codes: TermSet::default() }, CsList::default(), Switches::default()))
}
#[test]
fn list_exception_does_not_cancel_another_list() {
 let mut e=engine();
 let hash=[7;20];
 for (id, action) in [(1,Action::Quarantine),(2,Action::Drop)] {
  e.set_list(Some(ListLayer{id,action,hashes:Arc::new(HashSet::from([hash]))}),id);
 }
 let item=Item{hashes:&[hash],name:"Amber Beacon",files:&[],content_type:ContentType::Other};
 assert_eq!(e.judge(&item,None,0).decision.unwrap().rule,RuleRef::List(2));
 let ex=Exception{id:1,rule:RuleRef::List(2),what:Match::Hashes(vec![hash])};
 ex.validate().unwrap();
 e.exceptions.push(ex);
 assert!(e.blocked_hash(&[hash]).is_none(), "exception permits fetching so metadata can be judged");
 let got=e.judge(&item,None,0).decision.map(|d|(d.rule,d.action));
 eprintln!("Excepted L-2 drop; unexcepted L-1 quarantine: {got:?}");
 assert_eq!(got,Some((RuleRef::List(1),Action::Quarantine)));
}
fn neutral_case(query:&str) {
 let mut e=engine();
 e.rules.push(UserRule{id:1,query:lodestar_search::parse(query).unwrap(),action:Action::Hide,enabled:true,shadow_until:None,unless:None});
 let item=Item{hashes:&[[7;20]],name:"Amber Beacon",files:&[],content_type:ContentType::Other};
 assert!(e.judge(&item,None,0).decision.is_some());
 let o=Override{id:1,what:Match::Keyword("amber".into()),strength:Strength::Neutralize,scope:None,expires_at:None,shadow_until:None};
 o.validate().unwrap(); e.overrides.push(o);
 let got=e.judge(&item,None,0).decision;
 eprintln!("Neutralized amber, rule {query}: {got:?}");
 assert!(got.is_none(),"neutralized word must not supply required phrase/NEAR evidence");
}
#[test] fn neutralize_applies_inside_phrase(){neutral_case("\"amber beacon\"")}
#[test] fn neutralize_applies_inside_near(){neutral_case("amber NEAR/1 beacon")}
#[test]
fn stored_hash_classification_matches_ingest() {
 use lodestar_store::{Store,StoreConfig,NewTorrent,SourceId};
 let mut store=Store::open_in_memory(StoreConfig::default()).unwrap();
 let raw=b"d6:lengthi1e4:name16:Amber Beacon.mkv12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";
 let info=lodestar_metainfo::Info::parse(raw).unwrap();
 let id=store.insert(&[NewTorrent{info:&info,raw_info:Some(raw),discovered_at:0,metadata_at:0,extra_flags:0,source:SourceId::new(0).unwrap()}]).unwrap()[0].id();
 let row=store.torrent(id).unwrap().unwrap();
 let files=vec![FileInfo::new(vec![row.name.clone()],row.size)];
 let hash=row.info_hash.0;
 let hex:String=hash.iter().map(|b|format!("{b:02x}")).collect();
 let mut e=engine();
 e.class_rules=ClassRules::new(vec![ClassRule{id:1,query:lodestar_search::parse(&format!("hash:{hex}")).unwrap(),adult:true}]);
 let class=classify(&row.name,&files);
 assert_ne!(class.content_type,ContentType::Adult);
 let facts=Facts{class:&class,size:row.size,piece_len:row.piece_len,flags:row.flags,discovered_at:row.discovered_at,now:1000};
 let item=Item{hashes:&[hash],name:&row.name,files:&files,content_type:class.content_type};
 assert_eq!(e.judge_classified(&item,Some(&facts),1000).1,Some(true));
 let r=lodestar_filter::reclassify::reclassify_row(&row,&files,Correction::default(),false,Some(&e),1000);
 eprintln!("hash rule: ingest says adult=true; stored reclassification={:?}",r.class.content_type);
 assert_eq!(r.class.content_type,ContentType::Adult);
}
```

</details>

Checked and clean: `crates/filter/src/core.rs`, `crates/filter/src/rules.rs`, `crates/filter/src/normalize.rs`, `crates/filter/src/prefilter.rs`, `crates/filter/src/held.rs`, `crates/filter/src/seal.rs`, `crates/classify/src/adult.rs`.


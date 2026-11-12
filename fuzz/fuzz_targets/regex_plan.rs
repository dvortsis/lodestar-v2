//! The regex parser and trigram planner (RX-0, `lodestar_search::regex`)
//! never panic on any input, stay within their limits, and are
//! deterministic. Time and memory are bounded by the limits themselves;
//! CI runs libFuzzer without a short `-timeout`, so the target fails any
//! parse slower than a second itself (release builds take milliseconds),
//! and libFuzzer's RSS limit catches memory.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_search::regex::{MAX_PATTERN_BYTES, MAX_PLAN_NODES, Plan, parse};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let started = std::time::Instant::now();
    let parsed = parse(text);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "{text:?} took {:?}",
        started.elapsed()
    );
    match parsed {
        Ok(p) => {
            assert!(text.len() <= MAX_PATTERN_BYTES);
            assert!(p.plan().size() <= MAX_PLAN_NODES, "{text:?}");
            // Grams are three lowercased characters each.
            for g in p.plan().grams() {
                let n = g.chars().count();
                assert!((3..=9).contains(&n), "{text:?}: {g:?}");
            }
            // The plan is a function of the pattern, and spelling the
            // default flags out doesn't change it.
            assert_eq!(parse(text).as_ref(), Ok(&p));
            if let Ok(v) = parse(&p.verify_pattern()) {
                assert_eq!(v.plan(), p.plan(), "{text:?}");
            }
            // `All` and `Nothing` are only ever the whole plan.
            if let Plan::And(kids) | Plan::Or(kids) = p.plan() {
                assert!(kids.iter().all(normal), "{text:?}");
            }
        }
        Err(e) => assert!(e.reason.code().starts_with("regex_")),
    }
});

fn normal(p: &Plan) -> bool {
    match p {
        Plan::All | Plan::Nothing => false,
        Plan::Gram(_) => true,
        Plan::And(kids) | Plan::Or(kids) => kids.len() >= 2 && kids.iter().all(normal),
    }
}

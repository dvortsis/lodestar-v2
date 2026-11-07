//! Pipeline helpers shared by this crate's gate tests (`safety.rs`,
//! `log_scan.rs`), which include it by path.
#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lodestar_filter::{Action, Core, CsList, Engine, Handle, Lists, Sealer, Switches, TermSet};
use lodestar_pipeline::PipelineConfig;

/// The child-safety core with stand-in term lists, as the pipeline's own
/// tests build it: a child role next to a sexual term is a CS-4 pattern.
pub fn core_with(sexual: &[&str], roles: &[&str]) -> Arc<Handle> {
    let lists = Lists {
        sexual: TermSet::of(sexual),
        child_roles: TermSet::of(roles),
        codes: TermSet::of(&["codex"]),
    };
    Arc::new(Handle::new(Engine::new(Core::new(
        lists,
        CsList::default(),
        Switches::default(),
    ))))
}

pub fn user_rule(filter: &Handle, query: &str, action: Action) {
    filter.update_engine(|e| {
        let notes = e.load_rules(&lodestar_store::RuleSet {
            rules: vec![lodestar_store::RuleRow {
                id: 1,
                query: query.into(),
                action: action.code(),
                enabled: true,
                shadow_until: None,
                unless_query: None,
            }],
            ..Default::default()
        });
        assert!(notes.is_empty(), "{notes:?}");
        e.version += 1;
    });
}

pub fn pipeline_config(filter: Arc<Handle>) -> PipelineConfig {
    PipelineConfig {
        flush_every: Duration::from_millis(100),
        batch_wait: Duration::from_millis(20),
        filter,
        sealer: Some(Arc::new(Sealer::new(&Sealer::generate_key().unwrap()))),
        ..PipelineConfig::default()
    }
}

/// Polls for at least 10 s (standards §2), so the slowest runner passes.
pub async fn until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..600 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

pub fn n(c: &AtomicU64) -> u64 {
    c.load(Ordering::Relaxed)
}

//! The Overview's privacy summary (§10.10.7 Privacy, §10.10.12): whether
//! searches are saved and until when the log writes a data kind that
//! identifies torrents or people. Booleans and a deadline only, so the
//! banner can't tell anyone which kinds, what was searched or where the
//! files are.

use lodestar_api::dto::insights::PrivacySummary;

use super::insights::Insights;
use super::logging::{Logging, UNTIL_OFF};
use crate::logging::state::Applied;

/// The summary at `now` (Unix seconds). A deadline that has passed counts
/// as off even before the runtime's switch re-applies the section.
pub fn summary(logging: &Logging, insights: &Insights, now: i64) -> PrivacySummary {
    // The same rule the runtime applies: a passed deadline is 0.
    let on: Vec<i64> = Applied::compute(logging, now)
        .kinds
        .into_iter()
        .filter(|k| (k.kind.identifying() || k.kind.opt_in()) && k.until != 0)
        .map(|k| k.until)
        .collect();
    let identifying_until = match on.iter().max() {
        None => None,
        Some(_) if on.contains(&UNTIL_OFF) => Some(UNTIL_OFF),
        Some(&latest) => Some(latest),
    };
    PrivacySummary {
        identifying_until,
        query_logging: insights.query_logging,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    fn with_kind(kind: &str, until: i64) -> Logging {
        let mut l = Logging::default();
        l.kinds.insert(kind.into(), until);
        l
    }

    #[test]
    fn off_by_default() {
        let s = summary(&Logging::default(), &Insights::default(), NOW);
        assert_eq!(s, PrivacySummary::default());
        assert_eq!(
            serde_json::to_value(s).unwrap(),
            serde_json::json!({ "identifying_until": null, "query_logging": false })
        );
    }

    #[test]
    fn a_kind_on_for_an_hour_gives_its_deadline_until_it_passes() {
        let l = with_kind("name", NOW + 3600);
        let i = Insights::default();
        assert_eq!(summary(&l, &i, NOW).identifying_until, Some(NOW + 3600));
        assert_eq!(
            summary(&l, &i, NOW + 3599).identifying_until,
            Some(NOW + 3600)
        );
        // Expired: off, without a save.
        assert_eq!(summary(&l, &i, NOW + 3600).identifying_until, None);
    }

    #[test]
    fn until_turned_off_wins_over_a_deadline() {
        let mut l = with_kind("hash", NOW + 60);
        l.kinds.insert("url".into(), UNTIL_OFF);
        let s = summary(&l, &Insights::default(), NOW);
        assert_eq!(s.identifying_until, Some(UNTIL_OFF));
    }

    #[test]
    fn presets_and_opt_ins_count_and_private_kinds_do_not() {
        let l = Logging {
            data: super::super::logging::DataPreset::Hashes,
            data_until: NOW + 600,
            ..Logging::default()
        };
        assert_eq!(
            summary(&l, &Insights::default(), NOW).identifying_until,
            Some(NOW + 600)
        );
        let l = Logging {
            peer_activity_until: NOW + 900,
            ..Logging::default()
        };
        assert_eq!(
            summary(&l, &Insights::default(), NOW).identifying_until,
            Some(NOW + 900)
        );
        // Sizes and client addresses are on by default and don't identify.
        let l = with_kind("size", UNTIL_OFF);
        assert_eq!(
            summary(&l, &Insights::default(), NOW).identifying_until,
            None
        );
    }

    #[test]
    fn query_logging_is_its_own_flag() {
        let i = Insights {
            query_logging: true,
            ..Insights::default()
        };
        let s = summary(&Logging::default(), &i, NOW);
        assert_eq!((s.query_logging, s.identifying_until), (true, None));
    }

    #[test]
    fn the_registry_follows_each_save_and_the_clock() {
        use lodestar_api::settings::SettingsApi;
        use serde_json::json;

        use crate::settings::tests::{config, patch, registry};

        let dir = tempfile::tempdir().unwrap();
        let r = registry(dir.path(), &config(dir.path(), None, &[]));
        let now = crate::logging::unix_now();
        assert_eq!(r.privacy(now), PrivacySummary::default());
        r.patch("logging", patch(json!({ "kinds": { "name": now + 3600 } })))
            .unwrap();
        assert_eq!(r.privacy(now).identifying_until, Some(now + 3600));
        r.patch("insights", patch(json!({ "query_logging": true })))
            .unwrap();
        let s = r.privacy(now);
        assert_eq!(
            (s.identifying_until, s.query_logging),
            (Some(now + 3600), true)
        );
        // The deadline passes with no save.
        let later = r.privacy(now + 3600);
        assert_eq!((later.identifying_until, later.query_logging), (None, true));
    }
}

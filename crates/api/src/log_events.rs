//! The API crate's log events (architecture §12.1).
//!
//! `filters.change_detail` is the owner's opt-in (§2.11): on only while
//! Filter change details is, written only to the file and the live log,
//! and never naming an item a change released (sealed, §20.11). Without it,
//! a change logs only `filters.rule_set_updated` and the new version.

use crate::dto::filters::{Change, MatchInput};

lodestar_log::catalogue! {
    "lodestar-api";
    /// A request failed inside the server; the response says only
    /// "Something went wrong".
    API_INTERNAL_ERROR {
        id: "api.internal_error",
        component: Api,
        level: Error,
        message: "An API request failed inside the server",
        fields: { error: Error },
    }
    /// One API request (`request_log`): the method, the matched route
    /// template (never the raw path or query), the status, the time to the
    /// response's start (`took_ms`; a stream or download goes on after it)
    /// and the client's TCP peer. Never a header. Off by default: it joins
    /// at Detailed.
    API_REQUEST {
        id: "api.request",
        component: Api,
        level: Debug,
        default_on: false,
        message: "API request",
        fields: { method: Label, route: Label, status: Label, took_ms: Size, client: Client },
    }
    /// A filter change was applied: the engine's new version only.
    FILTERS_RULE_SET_UPDATED {
        id: "filters.rule_set_updated",
        component: Filters,
        level: Info,
        message: "filters: rule set updated",
        fields: { version: Label },
    }
    /// What the change was (the opt-in).
    FILTERS_CHANGE_DETAIL {
        id: "filters.change_detail",
        component: Filters,
        level: Info,
        opt_in: FilterChange,
        message: "filters: change applied",
        fields: { change: FilterChange, version: Label },
    }
    /// A stored rule didn't load and was skipped.
    FILTERS_RULE_SKIPPED {
        id: "filters.rule_skipped",
        component: Filters,
        level: Warn,
        message: "A filter rule couldn't be loaded and was skipped",
        fields: {},
    }
    /// Rows that failed closed read again, and what their stand-ins held
    /// was judged again by the rules as they are: counts only, never an
    /// item (§20.11).
    FILTERS_STAND_INS_REJUDGED {
        id: "filters.stand_ins_rejudged",
        component: Filters,
        level: Info,
        message: "Filter rows that couldn't be read do again; what their stand-ins held was judged again",
        fields: { released: Size, kept: Size },
    }
    /// A core rule's hashed example couldn't be read.
    FILTERS_FIXTURE_UNREADABLE {
        id: "filters.fixture_unreadable",
        component: Filters,
        level: Warn,
        message: "An unreadable rule example was skipped",
        fields: { rule: Label },
    }
}

/// The integrations crate's events, for the binary's list (the binary
/// reaches that crate through this one).
pub use lodestar_integrations::log_events::CATALOGUE as INTEGRATIONS;

fn what(m: &MatchInput) -> String {
    match m {
        MatchInput {
            hashes: Some(h), ..
        } => format!("{} hashes", h.len()),
        MatchInput {
            keyword: Some(k), ..
        } => format!("keyword={k:?}"),
        MatchInput { query: Some(q), .. } => format!("query={q:?}"),
        MatchInput {
            payload: Some(_), ..
        } => "a payload signature".to_string(),
        _ => "nothing".to_string(),
    }
}

fn opt<T: std::fmt::Debug>(name: &str, v: &Option<T>) -> String {
    match v {
        Some(v) => format!(" {name}={v:?}"),
        None => String::new(),
    }
}

/// What a change did, for `filters.change_detail`: the rule, term,
/// exception, pack or list and how it changed. Items (info hashes, payload
/// signatures, the torrents a change releases) are only counted, never
/// named, and a list's URL loses its credentials.
pub fn change_detail(change: &Change) -> String {
    match change {
        Change::CoreSwitch { rule, on } => format!("core_switch rule={rule} on={on}"),
        Change::PackSwitch { id, on } => format!("pack_switch id={id} on={on}"),
        Change::AddRule {
            query,
            action,
            unless,
            shadow_days,
        } => format!(
            "add_rule query={query:?} action={action}{}{}",
            opt("unless", unless),
            opt("shadow_days", shadow_days)
        ),
        Change::UpdateRule {
            id,
            query,
            action,
            enabled,
            unless,
            shadow_days,
        } => format!(
            "update_rule id={id}{}{}{}{}{}",
            opt("query", query),
            opt("action", action),
            opt("enabled", enabled),
            opt("unless", unless),
            opt("shadow_days", shadow_days)
        ),
        Change::RemoveRule { id } => format!("remove_rule id={id}"),
        Change::AddOverride {
            what: m,
            neutralize,
            scope,
            expires_days,
            shadow_days,
        } => format!(
            "add_override {} neutralize={neutralize}{}{}{}",
            what(m),
            opt("scope", scope),
            opt("expires_days", expires_days),
            opt("shadow_days", shadow_days)
        ),
        Change::RemoveOverride { id } => format!("remove_override id={id}"),
        Change::AddException { rule, what: m } => {
            format!("add_exception rule={rule} {}", what(m))
        }
        Change::RemoveException { id } => format!("remove_exception id={id}"),
        Change::AddAllow { query } => format!("add_allow query={query:?}"),
        Change::RemoveAllow { id } => format!("remove_allow id={id}"),
        Change::ReportAbuse { hashes } => format!("report_abuse {} hashes", hashes.len()),
        Change::RemoveCs1 { hashes } => format!("remove_cs1 {} hashes", hashes.len()),
        Change::FalsePositive { .. } => "false_positive 1 item".to_string(),
        Change::Release { .. } => "release 1 item".to_string(),
        Change::Confirm { .. } => "confirm 1 item".to_string(),
        Change::RevealName { .. } => "reveal_name 1 item".to_string(),
        Change::Subscribe {
            url,
            name,
            kind,
            format,
            action,
            floor,
            refresh_hours,
        } => format!(
            "subscribe url={}{} kind={kind}{}{} floor={floor}{}",
            lodestar_log::url::scrub(url, lodestar_log::url::Form::Absolute),
            opt("name", name),
            opt("format", format),
            opt("action", action),
            opt("refresh_hours", refresh_hours)
        ),
        Change::Unsubscribe { id } => format!("unsubscribe id={id}"),
        Change::PauseSubscription { id, enabled } => {
            format!("pause_subscription id={id} enabled={enabled}")
        }
        Change::ResolveHeld { id, accept } => format!("resolve_held id={id} accept={accept}"),
        Change::ResolveFloorRemoval {
            source,
            hashes,
            remove,
        } => format!(
            "resolve_floor_removal source={source:?}{} remove={remove}",
            match hashes {
                Some(h) => format!(" {} hashes", h.len()),
                None => " all".to_string(),
            }
        ),
        Change::Undo => "undo".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_never_name_items() {
        let hash = "c9e15763f722f23e98a29decdfae341b98d53056".to_string();
        let changes = [
            Change::ReportAbuse {
                hashes: vec![hash.clone()],
            },
            Change::RemoveCs1 {
                hashes: vec![hash.clone()],
            },
            Change::FalsePositive { hash: hash.clone() },
            Change::Release { hash: hash.clone() },
            Change::Confirm { hash: hash.clone() },
            Change::RevealName { hash: hash.clone() },
            Change::AddOverride {
                what: MatchInput {
                    hashes: Some(vec![hash.clone()]),
                    ..Default::default()
                },
                neutralize: false,
                scope: None,
                expires_days: None,
                shadow_days: None,
            },
            Change::AddException {
                rule: "CS-2".into(),
                what: MatchInput {
                    payload: Some("0123456789abcdef".into()),
                    ..Default::default()
                },
            },
            Change::ResolveFloorRemoval {
                source: "list:3".into(),
                hashes: Some(vec![hash.clone()]),
                remove: true,
            },
        ];
        for c in &changes {
            let d = change_detail(c);
            assert!(
                !d.contains(&hash[..12]) && !d.contains("0123456789abcdef"),
                "{d}"
            );
        }
        let d = change_detail(&Change::Subscribe {
            url: "https://lists.example/x.txt?apikey=SECRET".into(),
            name: Some("Example".into()),
            kind: "content".into(),
            format: None,
            action: None,
            floor: false,
            refresh_hours: None,
        });
        assert!(!d.contains("SECRET") && d.contains("lists.example"), "{d}");
        assert_eq!(
            change_detail(&Change::AddAllow {
                query: "jailbait".into()
            }),
            "add_allow query=\"jailbait\""
        );
    }
}

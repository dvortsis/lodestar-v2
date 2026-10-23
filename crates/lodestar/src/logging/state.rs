//! What a `logging` section means at a moment: which events are on, what
//! each output may show, the outputs' levels, the `tracing` filter for the
//! lines the code didn't type, and the next deadline to wake up for.

use std::path::PathBuf;

use lodestar_log::{Component, Event, Kind, KindStatus, Level, Mask, Output, OutputStatus, Status};

use crate::settings::logging::{
    ConsoleDetail, ConsoleFormat, DataPreset, Logging, UNTIL_OFF, Verbosity, ahead,
};

/// Where the file goes and how it's rotated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileConfig {
    pub dir: PathBuf,
    pub size: u64,
    pub count: usize,
    pub max_age_secs: u64,
    pub total: u64,
}

/// A section, applied at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    /// Each component's level for its events and its untyped lines.
    pub levels: [Level; Component::ALL.len()],
    pub file_mask: Mask,
    pub console_mask: Mask,
    pub console_json: bool,
    /// "Same as the file" rather than Private.
    pub console_same_as_file: bool,
    pub console_level: Level,
    pub file_enabled: bool,
    pub file_level: Level,
    pub file: FileConfig,
    /// Every data kind and its deadline, for the catalogue and the
    /// switch events.
    pub kinds: Vec<KindStatus>,
    pub verbosity: &'static str,
    pub temporary: Option<(&'static str, i64)>,
    pub data: &'static str,
    /// The `tracing` filter for untyped lines.
    pub filter: String,
    /// The soonest deadline ahead, to re-apply at.
    pub next_deadline: Option<i64>,
    /// Events turned on or off against the preset, as saved.
    overrides: Vec<(String, bool)>,
}

/// Each kind's deadline in `l` (0 off, -1 until turned off, else Unix
/// seconds), whether or not it's still ahead.
fn deadline(l: &Logging, kind: Kind) -> i64 {
    match kind {
        Kind::PeerActivity => l.peer_activity_until,
        Kind::FilterChange => l.change_details_until,
        _ => match l.kinds.get(kind.name()) {
            Some(until) => *until,
            None if !l.data.has(kind) => 0,
            None if kind.identifying() => l.data_until,
            None => UNTIL_OFF,
        },
    }
}

impl Applied {
    pub fn compute(l: &Logging, now: i64) -> Applied {
        let mut file_mask = Mask::NONE;
        let mut kinds = Vec::new();
        let mut next: Option<i64> = None;
        let mut soon = |until: i64| {
            if until > now {
                next = Some(next.map_or(until, |n| n.min(until)));
            }
        };
        for kind in Kind::ALL {
            let until = deadline(l, kind);
            let on = ahead(until, now) && until != 0;
            if on {
                file_mask = file_mask.with(kind);
                soon(until);
            }
            kinds.push((kind, on, if on { until } else { 0 }));
        }
        // The console and the Event Log never get the opt-ins, and get no
        // more than Private unless "same as the file" is chosen.
        let console_mask = match l.console_detail {
            ConsoleDetail::File => file_mask.without_opt_ins(),
            ConsoleDetail::Private => Mask(file_mask.0 & Mask::PRIVATE.0),
        };
        let event_log_mask = Mask(file_mask.0 & Mask::PRIVATE.0);
        let kinds = kinds
            .into_iter()
            .map(|(kind, on, until)| KindStatus {
                kind,
                outputs: [
                    on && console_mask.has(kind),
                    on,
                    on,
                    on && event_log_mask.has(kind),
                ],
                until,
            })
            .collect();
        let temporary = (l.temporary_until > now).then_some(l.temporary_verbosity);
        if temporary.is_some() {
            soon(l.temporary_until);
        }
        let base = l.verbosity.level();
        let mut levels = [base; Component::ALL.len()];
        for (i, c) in Component::ALL.iter().enumerate() {
            let own = l
                .components
                .get(c.name())
                .and_then(|s| Level::parse(s))
                .unwrap_or(base);
            levels[i] = match temporary {
                Some(t) => own.max(t.level()),
                None => own,
            };
        }
        let overrides: Vec<(String, bool)> =
            l.events.iter().map(|(k, v)| (k.clone(), *v)).collect();
        let custom_events = overrides.iter().any(|(id, on)| {
            super::find(id).is_some_and(|e| !e.required && e.on_under(base) != *on)
        });
        let data_custom = l
            .kinds
            .iter()
            .any(|(k, until)| Kind::parse(k).is_some_and(|k| (*until != 0) != l.data.has(k)));
        let data_on = l.data == DataPreset::Private || ahead(l.data_until, now);
        Applied {
            levels,
            file_mask,
            console_mask,
            console_json: l.console_format == ConsoleFormat::Json,
            console_same_as_file: l.console_detail == ConsoleDetail::File,
            console_level: l.console_level.level(),
            file_enabled: l.file_enabled,
            file_level: l.file_level.level(),
            file: FileConfig {
                dir: PathBuf::from(&l.file_folder),
                size: u64::from(l.file_size_mib) << 20,
                count: l.file_count as usize,
                max_age_secs: u64::from(l.file_max_age_days) * 86_400,
                total: u64::from(l.file_total_mib) << 20,
            },
            kinds,
            verbosity: if custom_events {
                "custom"
            } else {
                l.verbosity.name()
            },
            temporary: temporary.map(|t| (t.name(), l.temporary_until)),
            data: match (data_custom, data_on) {
                (true, _) => "custom",
                (false, true) => l.data.name(),
                (false, false) => DataPreset::Private.name(),
            },
            filter: filter(l, &levels),
            next_deadline: next,
            overrides,
        }
    }

    pub fn level_of(&self, c: Component) -> Level {
        self.levels[c as usize]
    }

    /// Whether `event` is on under this state.
    pub fn event_on(&self, event: &Event) -> bool {
        if event.required {
            return true;
        }
        if let Some(k) = event.opt_in {
            return self.file_mask.has(k);
        }
        if let Some((_, on)) = self.overrides.iter().find(|(id, _)| id == event.id) {
            return *on;
        }
        event.on_under(self.level_of(event.component))
    }

    /// Sets every registered event's switch.
    pub fn switch(&self, events: impl Iterator<Item = &'static Event>) {
        for e in events {
            e.set_on(self.event_on(e));
        }
    }

    pub fn status(&self) -> Status {
        Status {
            verbosity: self.verbosity,
            temporary: self.temporary,
            data: self.data,
            kinds: self.kinds.clone(),
            outputs: vec![
                OutputStatus {
                    output: Output::Console,
                    enabled: true,
                    level: self.console_level,
                    format: Some(if self.console_json { "json" } else { "text" }),
                    detail: Some(if self.console_same_as_file {
                        "file"
                    } else {
                        "private"
                    }),
                },
                OutputStatus {
                    output: Output::File,
                    enabled: self.file_enabled,
                    level: self.file_level,
                    format: Some("jsonl"),
                    detail: None,
                },
            ],
        }
    }
}

/// The `tracing` filter for untyped lines: the Libraries filter, then each
/// first-party target at its component's level.
fn filter(l: &Logging, levels: &[Level; Component::ALL.len()]) -> String {
    let mut parts = vec![l.libraries.clone()];
    for (target, c) in super::TARGETS {
        parts.push(format!("{target}={}", levels[*c as usize].name()));
    }
    parts.join(",")
}

/// The verbosity that a timed choice raises things to, for the preview.
pub fn effective_verbosity(l: &Logging, now: i64) -> Verbosity {
    match l.temporary_until > now {
        true => l.verbosity.max(l.temporary_verbosity),
        false => l.verbosity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::events;

    fn at(f: impl FnOnce(&mut Logging)) -> Logging {
        let mut l = Logging::default();
        f(&mut l);
        l
    }

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn private_by_default() {
        let a = Applied::compute(&Logging::default(), NOW);
        assert_eq!(a.file_mask, Mask::PRIVATE);
        assert_eq!(a.console_mask, Mask::PRIVATE);
        assert_eq!((a.verbosity, a.data), ("normal", "private"));
        assert_eq!(a.next_deadline, None);
        assert!(
            a.filter.starts_with("warn,") && a.filter.contains(",lodestar=info,"),
            "{}",
            a.filter
        );
    }

    #[test]
    fn presets_set_the_switches_and_custom_is_detected() {
        let hashes = Applied::compute(
            &at(|l| {
                l.data = DataPreset::Hashes;
                l.data_until = NOW + 3600;
            }),
            NOW,
        );
        assert_eq!(hashes.file_mask, Mask::PRIVATE.with(Kind::Hash));
        assert_eq!(hashes.data, "hashes");
        assert_eq!(hashes.next_deadline, Some(NOW + 3600));
        let full = Applied::compute(
            &at(|l| {
                l.data = DataPreset::Full;
                l.data_until = UNTIL_OFF;
            }),
            NOW,
        );
        for k in Kind::ALL {
            assert_eq!(full.file_mask.has(k), !k.opt_in(), "{k:?}");
        }
        assert_eq!(full.next_deadline, None, "until turned off");
        // Full leaves both opt-ins off; the console stays Private.
        assert_eq!(full.console_mask, Mask::PRIVATE);
        let custom = Applied::compute(
            &at(|l| {
                l.kinds.insert("name".into(), NOW + 60);
            }),
            NOW,
        );
        assert_eq!(custom.data, "custom");
        assert!(custom.file_mask.has(Kind::Name));
        // An override equal to the preset isn't Custom.
        let same = Applied::compute(
            &at(|l| {
                l.kinds.insert("size".into(), -1);
            }),
            NOW,
        );
        assert_eq!(same.data, "private");
    }

    #[test]
    fn deadlines_expire_without_a_save() {
        let l = at(|l| {
            l.data = DataPreset::Full;
            l.data_until = NOW + 10;
            l.peer_activity_until = NOW + 5;
        });
        let before = Applied::compute(&l, NOW);
        assert!(before.file_mask.has(Kind::Name) && before.file_mask.has(Kind::PeerActivity));
        assert_eq!(before.next_deadline, Some(NOW + 5));
        let between = Applied::compute(&l, NOW + 5);
        assert!(!between.file_mask.has(Kind::PeerActivity));
        assert_eq!(between.next_deadline, Some(NOW + 10));
        let after = Applied::compute(&l, NOW + 10);
        assert_eq!(after.file_mask, Mask::PRIVATE);
        assert_eq!(after.data, "private", "an expired preset reads as Private");
        assert_eq!(after.next_deadline, None);
    }

    #[test]
    fn opt_ins_never_reach_the_console() {
        let l = at(|l| {
            l.data = DataPreset::Full;
            l.data_until = UNTIL_OFF;
            l.peer_activity_until = NOW + 60;
            l.change_details_until = NOW + 60;
            l.console_detail = ConsoleDetail::File;
        });
        let a = Applied::compute(&l, NOW);
        assert!(a.file_mask.has(Kind::PeerActivity) && a.file_mask.has(Kind::FilterChange));
        assert!(!a.console_mask.has(Kind::PeerActivity) && !a.console_mask.has(Kind::FilterChange));
        assert!(a.console_mask.has(Kind::Name), "same as the file");
        assert_eq!(a.status().outputs[0].detail, Some("file"));
        assert!(a.event_on(&events::APP_START::EVENT));
    }

    #[test]
    fn events_follow_the_verbosity_overrides_and_components() {
        let quiet = Applied::compute(&at(|l| l.verbosity = Verbosity::Errors), NOW);
        assert!(!quiet.event_on(&events::LOGGING_APPLIED::EVENT));
        assert!(quiet.event_on(&events::APP_START::EVENT), "required");
        let over = Applied::compute(
            &at(|l| {
                l.verbosity = Verbosity::Errors;
                l.events.insert("logging.applied".into(), true);
            }),
            NOW,
        );
        assert!(over.event_on(&events::LOGGING_APPLIED::EVENT));
        assert_eq!(over.verbosity, "custom");
        let comp = Applied::compute(
            &at(|l| drop(l.components.insert("dht".into(), "error".into()))),
            NOW,
        );
        assert!(!comp.event_on(&events::SETTINGS_INTENSITY::EVENT));
        assert!(
            comp.filter.contains("lodestar_dht=error"),
            "{}",
            comp.filter
        );
        // "Detailed for 30 minutes" raises every component until it ends.
        let timed = at(|l| {
            l.temporary_verbosity = Verbosity::Detailed;
            l.temporary_until = NOW + 1800;
        });
        let a = Applied::compute(&timed, NOW);
        assert_eq!(a.level_of(Component::Fetch), Level::Debug);
        assert_eq!(a.temporary, Some(("detailed", NOW + 1800)));
        assert_eq!(a.next_deadline, Some(NOW + 1800));
        assert_eq!(
            Applied::compute(&timed, NOW + 1800).level_of(Component::Fetch),
            Level::Info
        );
        // The opt-in event follows its opt-in, not the verbosity.
        let detail = &lodestar_api::log_events::FILTERS_CHANGE_DETAIL::EVENT;
        assert!(
            !Applied::compute(&at(|l| l.verbosity = Verbosity::Everything), NOW).event_on(detail)
        );
        assert!(Applied::compute(&at(|l| l.change_details_until = NOW + 9), NOW).event_on(detail));
    }
}

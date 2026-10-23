//! What the Logging screen shows before a save (§10.10.12): warnings while
//! an identifying kind or an opt-in would be on, the estimated volume per
//! day for each output (from the counters, which count events that are off
//! too), and example lines for the staged choices, rendered from sample
//! data, never from real torrents.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use lodestar_log::{Event, FieldKind, Kind, Output, Record, Value};
use serde_json::{Value as Json, json};

use super::format::{self, Stamp};
use super::state::{Applied, effective_verbosity};
use crate::settings::Preview;
use crate::settings::logging::{Logging, Verbosity};

/// How many example lines the preview gives.
const EXAMPLES: usize = 6;
/// Line lengths assumed before an output has written any.
const CONSOLE_LINE: u64 = 140;
const FILE_LINE: u64 = 260;

/// The sample torrent's hash: made up, so it can't be anyone's.
pub const SAMPLE_HASH: [u8; 20] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
    0x00, 0x11, 0x22, 0x33,
];
pub const SAMPLE_NAME: &str = "Example.Open.Source.Distro.2026.x64";
pub const SAMPLE_PATH: &str = "Example.Open.Source.Distro.2026.x64/disc1.iso";
pub const SAMPLE_URL: &str =
    "udp://tracker.example:1337/announce?passkey=0f1e2d3c4b5a69788796a5b4c3d2e1f0";
pub const SAMPLE_QUERY: &str = "example distro";
pub const SAMPLE_DETAIL: &str = "add_rule query=\"example term\" action=hide";
/// Documentation addresses (RFC 5737).
const SAMPLE_CLIENT: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
const SAMPLE_PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)), 51413);

fn sample(kind: FieldKind) -> Value<'static> {
    match kind {
        FieldKind::Data(k) => match k {
            Kind::Hash => Value::Torrent(&SAMPLE_HASH),
            Kind::Name => Value::Name(SAMPLE_NAME),
            Kind::Path => Value::Path(std::path::Path::new(SAMPLE_PATH)),
            Kind::Url => Value::Url(SAMPLE_URL, lodestar_log::url::Form::Absolute),
            Kind::Query => Value::Query(SAMPLE_QUERY),
            Kind::Client => Value::Client(SAMPLE_CLIENT),
            Kind::Peer => Value::Peer(SAMPLE_PEER),
            Kind::Size => Value::Size(1_048_576),
            Kind::FilterChange | Kind::PeerActivity => Value::Detail(SAMPLE_DETAIL),
        },
        FieldKind::Error => Value::Error(lodestar_log::Error::kind("timeout")),
        FieldKind::Label => Value::Label("example"),
    }
}

/// A list of the identifying kinds a mask holds, for the warnings.
fn kind_list(a: &Applied) -> Vec<&'static str> {
    a.file_mask
        .kinds()
        .filter(|k| k.identifying())
        .map(Kind::name)
        .collect()
}

fn hours(until: i64, now: i64) -> &'static str {
    match until {
        -1 => "until you turn them off",
        u if u - now <= 3_900 => "for the next hour",
        _ => "for the next 24 hours",
    }
}

fn mib(bytes: u64) -> f64 {
    (bytes as f64 / f64::from(1 << 20) * 10.0).round() / 10.0
}

fn events() -> impl Iterator<Item = &'static Event> {
    super::CATALOGUES
        .iter()
        .flat_map(|c| c.events.iter().copied())
}

/// The whole preview for a staged section.
pub fn preview(l: &Logging, now: i64) -> Preview {
    let a = Applied::compute(l, now);
    let mut p = Preview::default();
    let identifying = kind_list(&a);
    if !identifying.is_empty() {
        let until = a
            .kinds
            .iter()
            .filter(|k| k.kind.identifying() && k.outputs[1])
            .map(|k| if k.until == -1 { i64::MAX } else { k.until })
            .min()
            .unwrap_or(0);
        let until = if until == i64::MAX { -1 } else { until };
        let message = format!(
            "Torrent details ({}) will be written to the log file {}. {}",
            identifying.join(", "),
            hours(until, now),
            if a.console_same_as_file {
                "The console gets them too, and Lodestar can't delete what docker logs or the journal keep."
            } else {
                "The console and docker logs stay without them."
            }
        );
        p = p.warn("identifying_kinds", &message);
    }
    if a.file_mask.has(Kind::PeerActivity) {
        p = p.warn(
            "peer_activity",
            &format!(
                "Lodestar normally never keeps which peer had which torrent. {}, the log file will record it: a list of people's addresses next to what they share.",
                capitalize(hours(l.peer_activity_until, now))
            ),
        );
    }
    if a.file_mask.has(Kind::FilterChange) {
        p = p.warn(
            "filter_changes",
            &format!(
                "Lodestar normally keeps no record of filtering changes. {}, the log file will record each change you make to rules, terms, exceptions and lists.",
                capitalize(hours(l.change_details_until, now))
            ),
        );
    }
    if effective_verbosity(l, now) >= Verbosity::Detailed
        || l.components
            .values()
            .any(|v| matches!(v.as_str(), "debug" | "trace"))
    {
        p = p.warn(
            "verbose_logs",
            "Debug and trace logs grow quickly; turn them back down when you're done.",
        );
    }
    let (console, file) = volume(&a);
    p = p
        .estimate("console_mib_per_day", mib(console))
        .estimate("file_mib_per_day", mib(file));
    let days = match file {
        0 => Json::Null,
        f => json!(((a.file.total as f64 / f as f64) * 10.0).round() / 10.0),
    };
    p.estimate("file_days_kept", days)
        .estimate("examples", examples(&a))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Bytes a day each output would write at today's rate.
fn volume(a: &Applied) -> (u64, u64) {
    let console_line = lodestar_log::average_line(Output::Console, CONSOLE_LINE);
    let file_line = lodestar_log::average_line(Output::File, FILE_LINE);
    let (mut console, mut file) = (0u64, 0u64);
    for e in events().filter(|e| a.event_on(e)) {
        let per_day = lodestar_log::rate_per_hour(e).saturating_mul(24);
        if e.opt_in.is_none() && (e.required || e.level <= a.console_level) {
            console = console.saturating_add(per_day.saturating_mul(console_line));
        }
        if a.file_enabled && (e.required || e.level <= a.file_level) {
            file = file.saturating_add(per_day.saturating_mul(file_line));
        }
    }
    (console, file)
}

/// Example lines of the events that would be on, those carrying data
/// first, each as the console and the file would show it.
fn examples(a: &Applied) -> Json {
    let mut on: Vec<&'static Event> = events().filter(|e| a.event_on(e)).collect();
    on.sort_by_key(|e| {
        (
            e.kinds().iter().all(|k| !k.identifying() && !k.opt_in()),
            e.id,
        )
    });
    let rt = super::runtime();
    let refs = rt.and_then(|r| r.refs.as_ref());
    let reference = refs.map(|r| move |h: &[u8; 20]| r.of(h));
    let reference = reference
        .as_ref()
        .map(|f| f as &dyn Fn(&[u8; 20]) -> String);
    let stamp = Stamp::now();
    on.into_iter()
        .take(EXAMPLES)
        .map(|e| {
            let fields: Vec<(&'static str, Value<'static>)> =
                e.fields.iter().map(|(n, k)| (*n, sample(*k))).collect();
            let record = Record { event: e, fields: &fields };
            let console = (e.opt_in.is_none()).then(|| match a.console_json {
                true => format::event_json(stamp, &record, a.console_mask, reference),
                false => format::event_text(stamp, &record, a.console_mask, reference),
            });
            json!({
                "event": e.id,
                "console": console,
                "file": a.file_enabled.then(|| format::event_json(stamp, &record, a.file_mask, reference)),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::logging::{ConsoleDetail, DataPreset};

    const NOW: i64 = 1_800_000_000;

    fn codes(p: &Preview) -> Vec<&str> {
        p.warnings.iter().map(|w| w.code.as_str()).collect()
    }

    fn estimate<'a>(p: &'a Preview, name: &str) -> &'a Json {
        &p.estimates.iter().find(|e| e.name == name).unwrap().value
    }

    #[test]
    fn private_warns_of_nothing_and_shows_no_sample_data() {
        let p = preview(&Logging::default(), NOW);
        assert!(codes(&p).is_empty(), "{:?}", p.warnings);
        let examples = estimate(&p, "examples").to_string();
        for sample in [
            SAMPLE_NAME,
            SAMPLE_PATH,
            SAMPLE_QUERY,
            "0011223344",
            "198.51.100.7",
            "0f1e2d3c",
        ] {
            assert!(!examples.contains(sample), "{sample} in {examples}");
        }
        assert!(estimate(&p, "file_mib_per_day").is_number());
    }

    #[test]
    fn identifying_kinds_and_opt_ins_warn() {
        let mut l = Logging::default();
        l.data = DataPreset::Full;
        l.data_until = NOW + 3600;
        l.peer_activity_until = NOW + 3600;
        l.change_details_until = NOW + 86_400;
        let p = preview(&l, NOW);
        assert_eq!(
            codes(&p),
            ["identifying_kinds", "peer_activity", "filter_changes"]
        );
        assert!(p.warnings[0].message.contains("for the next hour"));
        assert!(p.warnings[0].message.contains("stay without them"));
        assert!(p.warnings[1].message.starts_with(
            "Lodestar normally never keeps which peer had which torrent. For the next hour,"
        ));
        assert!(p.warnings[2].message.contains("For the next 24 hours"));
        l.console_detail = ConsoleDetail::File;
        assert!(
            preview(&l, NOW).warnings[0]
                .message
                .contains("can't delete")
        );
        // The sample name shows in the file's example lines only.
        let examples = estimate(&preview(&l, NOW), "examples").clone();
        for ex in examples.as_array().unwrap() {
            if let Some(c) = ex["console"].as_str() {
                assert!(!c.contains(SAMPLE_DETAIL), "{c}");
            }
            // The passkey never shows, URLs on or off.
            assert!(!ex.to_string().contains("0f1e2d3c4b5a"), "{ex}");
        }
    }

    #[test]
    fn verbose_choices_warn() {
        let mut l = Logging::default();
        l.verbosity = Verbosity::Detailed;
        assert_eq!(codes(&preview(&l, NOW)), ["verbose_logs"]);
        let mut l = Logging::default();
        l.temporary_until = NOW + 1800;
        assert_eq!(codes(&preview(&l, NOW)), ["verbose_logs"]);
    }
}

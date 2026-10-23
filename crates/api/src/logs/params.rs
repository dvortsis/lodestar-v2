//! The `/logs` query parameters: each given at most once, checked before
//! anything is read.

use std::collections::BTreeMap;

use lodestar_log::{Component, Level};

use crate::error::{ApiError, ApiResult};

/// The filters the stream and history share, applied on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineFilter {
    /// The least severe level shown: `trace` shows everything.
    pub level: Level,
    /// `None`: every component.
    pub components: Option<Vec<Component>>,
    /// A case-insensitive substring, lower-cased, matched against the
    /// line as shown.
    pub text: Option<String>,
}

impl Default for LineFilter {
    fn default() -> Self {
        LineFilter {
            level: Level::Trace,
            components: None,
            text: None,
        }
    }
}

impl LineFilter {
    pub fn level_ok(&self, l: Level) -> bool {
        l <= self.level
    }

    pub fn component_ok(&self, c: Component) -> bool {
        self.components.as_ref().is_none_or(|cs| cs.contains(&c))
    }

    /// A stable description of the filter, for cursors.
    pub fn describe(&self) -> String {
        let comps = match &self.components {
            None => "*".to_string(),
            Some(cs) => {
                let mut names: Vec<&str> = cs.iter().map(|c| c.name()).collect();
                names.sort_unstable();
                names.join(",")
            }
        };
        format!(
            "{}|{comps}|{}",
            self.level.name(),
            self.text.as_deref().unwrap_or("")
        )
    }
}

/// The most characters a text filter may have.
pub const MAX_TEXT: usize = 200;
/// The longest cursor accepted.
pub const MAX_CURSOR: usize = 512;
/// History's default and largest page.
pub const DEFAULT_LIMIT: usize = 200;
pub const MAX_LIMIT: usize = 1_000;

/// The query's parameters by name, each given at most once.
pub(super) fn parse(query: Option<&str>) -> ApiResult<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (k, v) in form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
        let known = [
            "level",
            "components",
            "text",
            "after",
            "since",
            "until",
            "limit",
            "cursor",
        ];
        let Some(name) = known.iter().find(|n| **n == k) else {
            continue;
        };
        if out.insert(name.to_string(), v.into_owned()).is_some() {
            return Err(
                ApiError::bad_request("bad_parameter", format!("Give `{name}` once.")).field(*name),
            );
        }
    }
    Ok(out)
}

/// The shared filters.
pub(super) fn filter(p: &BTreeMap<String, String>) -> ApiResult<LineFilter> {
    let level = match p.get("level") {
        None => Level::Trace,
        Some(l) => Level::ALL
            .into_iter()
            .find(|x| x.name() == l)
            .ok_or_else(|| {
                ApiError::bad_request(
                    "bad_parameter",
                    "`level` is one of error, warn, info, debug or trace.",
                )
                .field("level")
            })?,
    };
    let components = match p.get("components") {
        None => None,
        Some(list) => {
            let mut cs = Vec::new();
            for name in list.split(',') {
                let c = Component::parse(name).ok_or_else(|| {
                    let known: Vec<&str> = Component::ALL.iter().map(|c| c.name()).collect();
                    ApiError::bad_request(
                        "unknown_component",
                        format!(
                            "Unknown component; use a comma-separated list of {}.",
                            known.join(", ")
                        ),
                    )
                    .field("components")
                })?;
                if !cs.contains(&c) {
                    cs.push(c);
                }
            }
            Some(cs)
        }
    };
    let text = match p.get("text") {
        None => None,
        Some(t) if t.chars().count() > MAX_TEXT => {
            return Err(ApiError::bad_request(
                "bad_parameter",
                format!("`text` is at most {MAX_TEXT} characters."),
            )
            .field("text"));
        }
        Some(t) if t.is_empty() => None,
        Some(t) => Some(t.to_lowercase()),
    };
    Ok(LineFilter {
        level,
        components,
        text,
    })
}

/// `since` or `until`: RFC 3339 in UTC, as the files write it.
pub(super) fn time(p: &BTreeMap<String, String>, name: &'static str) -> ApiResult<Option<i64>> {
    match p.get(name) {
        None => Ok(None),
        Some(t) => parse_rfc3339(t).map(Some).ok_or_else(|| {
            ApiError::bad_request(
                "bad_parameter",
                format!("`{name}` is a UTC time such as 2026-10-23T10:15:00Z."),
            )
            .field(name)
        }),
    }
}

pub(super) fn limit(p: &BTreeMap<String, String>) -> ApiResult<usize> {
    match p.get("limit") {
        None => Ok(DEFAULT_LIMIT),
        Some(l) => l
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_LIMIT).contains(n))
            .ok_or_else(|| {
                ApiError::bad_request(
                    "bad_parameter",
                    format!("`limit` is a number from 1 to {MAX_LIMIT}."),
                )
                .field("limit")
            }),
    }
}

/// `2026-10-23T10:15:00Z` or `…:00.123Z` (also `+00:00`) to Unix millis.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &s[19..];
    let mut millis = 0;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || digits > 9 {
            return None;
        }
        let f = &frac[..digits];
        let padded = format!("{f:0<3}");
        millis = padded[..3].parse::<i64>().ok()?;
        rest = &frac[digits..];
    }
    if !(rest == "Z" || rest == "z" || rest == "+00:00") {
        return None;
    }
    if !(1..=12).contains(&mo) || d < 1 || d > days_in(y, mo) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some((days * 86_400 + h * 3600 + mi * 60 + sec.min(59)) * 1000 + millis)
}

fn days_in(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Howard Hinnant's days-from-civil.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339("2026-10-23T10:15:00.123Z"),
            Some(1_792_750_500_123)
        );
        assert_eq!(
            parse_rfc3339("2000-02-29T00:00:00.5Z"),
            Some(951_782_400_500)
        );
        for bad in [
            "2026-10-23",
            "2026-10-23T10:15:00",
            "2026-10-23T10:15:00+02:00",
            "2026-13-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2026-10-23T10:15:00.Z",
            "2026-1O-23T10:15:00Z",
            "２026-10-23T10:15:00Z",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }

    #[test]
    fn filters() {
        let p = |q: &str| parse(Some(q));
        assert!(p("level=info&level=warn").is_err());
        let f = filter(&p("level=warn&components=dht,libraries&text=AbC").unwrap()).unwrap();
        assert_eq!(f.level, Level::Warn);
        assert_eq!(
            f.components,
            Some(vec![Component::Dht, Component::Libraries])
        );
        assert_eq!(f.text.as_deref(), Some("abc"));
        assert_eq!(f.describe(), "warn|dht,libraries|abc");
        let e = filter(&p("components=dht,nope").unwrap()).unwrap_err();
        assert_eq!(e.body()["error"]["code"], "unknown_component");
        assert!(filter(&p("level=WARN").unwrap()).is_err());
        let long = "x".repeat(201);
        assert!(filter(&p(&format!("text={long}")).unwrap()).is_err());
        assert!(filter(&p(&format!("text={}", "é".repeat(200))).unwrap()).is_ok());
        assert_eq!(limit(&p("limit=1000").unwrap()).unwrap(), 1000);
        assert!(limit(&p("limit=0").unwrap()).is_err());
        assert!(limit(&p("limit=1001").unwrap()).is_err());
    }
}

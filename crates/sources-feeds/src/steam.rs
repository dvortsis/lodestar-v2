//! Steam's app list (ENR-steam; architecture §18.2, §18.7;
//! `docs/decisions/2026-11-16-ENR-steam.md`): one page of
//! `IStoreService/GetAppList/v1`, read with caps.
//!
//! The plugin fetches the whole list page by page and matches titles
//! locally, so no title is ever sent. Each page is parsed here and folded
//! into the plugin's index before the next is asked for: memory is one
//! page's body plus the index, never the whole list as text.
//!
//! The answer's shape:
//!
//! ```json
//! {"response":{"apps":[{"appid":10,"name":"Counter-Strike",
//!   "last_modified":1666823513,"price_change_number":21319021}],
//!   "have_more_results":true,"last_appid":10}}
//! ```
//!
//! Unknown keys are passed over. An app with no id, a zero or oversize
//! id, no name, or a name that's empty, over [`MAX_NAME`] bytes or holds
//! a control character is skipped and counted. Nothing here panics.

use crate::common::Skipped;
pub use crate::json::Stop;
use crate::json::{self, R, Reader, Walk};

/// The longest app name kept, in bytes.
pub const MAX_NAME: usize = 256;

/// One page's caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Bytes read; the rest is ignored ([`Stop::Bytes`]).
    pub max_bytes: usize,
    /// Open objects and arrays at once ([`Stop::Depth`]).
    pub max_depth: usize,
    /// Values in the document, containers included ([`Stop::Elements`]).
    pub max_values: usize,
    /// Apps kept from one page ([`Stop::Items`]).
    pub max_items: usize,
}

/// A page asks for at most 10,000 apps (the plugin's `max_results`); the
/// caps allow some slack. An app is about 100 bytes and 6 values.
pub const LIMITS: Limits = Limits {
    max_bytes: 4 << 20,
    max_depth: 8,
    max_values: 200_000,
    max_items: 12_000,
};

/// One app: its id and name, as the service wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    pub id: u32,
    pub name: String,
}

/// One page of the list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Page {
    pub apps: Vec<App>,
    /// The service says there are more apps after this page.
    pub more: bool,
    /// Where the next page starts (`last_appid`).
    pub last_appid: Option<u32>,
    pub skipped: Skipped,
    /// The first fault.
    pub stop: Option<Stop>,
    /// The root held a `response` object.
    pub seen: bool,
}

impl Page {
    /// Read to the end with no fault: only such a page is used.
    pub fn whole(&self) -> bool {
        self.stop.is_none() && self.seen
    }
}

struct P<'a> {
    r: Reader<'a>,
    l: Limits,
    page: Page,
}

impl<'a> Walk<'a> for P<'a> {
    fn json(&mut self) -> &mut Reader<'a> {
        &mut self.r
    }
}

/// Parses one page. Keeps the apps read before any fault.
pub fn parse_page(body: &[u8], l: &Limits) -> Page {
    let mut page = Page::default();
    let mut body = body;
    if body.len() > l.max_bytes {
        body = &body[..l.max_bytes];
        page.stop = Some(Stop::Bytes);
    }
    let text = match std::str::from_utf8(body) {
        Ok(t) => t,
        Err(e) => {
            page.stop.get_or_insert(Stop::NotUtf8);
            std::str::from_utf8(&body[..e.valid_up_to()]).unwrap_or_default()
        }
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut p = P {
        r: Reader::new(
            text,
            json::Limits {
                max_depth: l.max_depth,
                max_values: l.max_values,
                max_array: l.max_items,
                max_text: MAX_NAME * 6,
            },
        ),
        l: *l,
        page,
    };
    let res = root(&mut p).and_then(|()| p.r.end());
    if let Err(stop) = res {
        p.page.stop.get_or_insert(stop);
    }
    p.page
}

fn root(p: &mut P<'_>) -> R<()> {
    if p.r.peek() != Some(b'{') {
        return Err(Stop::Malformed);
    }
    p.object(|p, key| {
        if key != "response" || p.r.peek() != Some(b'{') {
            return p.r.skip().map(drop);
        }
        p.page.seen = true;
        p.object(|p, key| match key {
            "apps" if p.r.peek() == Some(b'[') => p.array(|p, _| app(p)),
            "have_more_results" => {
                p.page.more = boolean(&mut p.r)?;
                Ok(())
            }
            "last_appid" if p.r.peek().is_some_and(|b| b.is_ascii_digit()) => {
                p.page.last_appid = id(p.r.num()?);
                Ok(())
            }
            _ => p.r.skip().map(drop),
        })
    })
}

/// `true` or `false`; anything else is malformed, so a page whose paging
/// flag isn't a boolean is never read as the last one.
fn boolean(r: &mut Reader<'_>) -> R<bool> {
    let t = match r.peek() {
        Some(b't') => true,
        Some(b'f') => false,
        _ => return Err(Stop::Malformed),
    };
    r.skip()?;
    Ok(t)
}

/// A positive app id that fits in `u32`.
fn id(raw: &str) -> Option<u32> {
    raw.parse::<u32>().ok().filter(|n| *n > 0)
}

fn app(p: &mut P<'_>) -> R<()> {
    if p.r.peek() != Some(b'{') {
        p.r.skip()?;
        p.page.skipped.malformed += 1;
        return Ok(());
    }
    let (mut appid, mut name, mut bad) = (None, None, false);
    p.object(|p, key| match key {
        "appid" if p.r.peek().is_some_and(|b| b.is_ascii_digit()) => {
            appid = id(p.r.num()?);
            Ok(())
        }
        "name" if p.r.peek() == Some(b'"') => {
            match p.r.text()? {
                Ok(s) => name = Some(s),
                Err(_) => bad = true,
            }
            Ok(())
        }
        _ => p.r.skip().map(drop),
    })?;
    let name = name.filter(|n| {
        !bad && !n.trim().is_empty() && n.len() <= MAX_NAME && !n.chars().any(char::is_control)
    });
    match (appid, name) {
        (Some(id), Some(name)) => {
            if p.page.apps.len() >= p.l.max_items {
                return Err(Stop::Items);
            }
            p.page.apps.push(App {
                id,
                name: name.trim().to_owned(),
            });
        }
        _ => p.page.skipped.malformed += 1,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_page() {
        let b = br#"{"response":{"apps":[{"appid":10,"name":"Counter-Strike","last_modified":1},{"appid":0,"name":"x"},{"appid":20,"name":""}],"have_more_results":true,"last_appid":20}}"#;
        let p = parse_page(b, &LIMITS);
        assert!(p.whole() && p.more);
        assert_eq!(p.last_appid, Some(20));
        assert_eq!(
            p.apps,
            vec![App {
                id: 10,
                name: "Counter-Strike".into()
            }]
        );
        assert_eq!(p.skipped.malformed, 2);
    }

    #[test]
    fn caps_items() {
        let b = br#"{"response":{"apps":[{"appid":1,"name":"a"},{"appid":2,"name":"b"}]}}"#;
        let p = parse_page(
            b,
            &Limits {
                max_items: 1,
                ..LIMITS
            },
        );
        assert_eq!(p.stop, Some(Stop::Items));
        assert_eq!(p.apps.len(), 1);
        assert!(!p.whole());
    }
}

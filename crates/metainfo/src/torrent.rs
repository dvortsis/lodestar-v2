use lodestar_bencode::{self as bencode, DecodeOptions, Dict, Value};

use crate::error::Error;
use crate::info::{Info, Limits, ParseOptions};
use crate::text::TextEncoding;

/// A whole `.torrent` file. The fields outside the info dictionary exist only
/// for imported files, never for DHT-discovered torrents (architecture §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TorrentFile {
    pub info: Info,
    /// Tracker tiers from `announce-list`, or `announce` as a single tier.
    pub announce: Vec<Vec<String>>,
    /// Seconds since the Unix epoch, as the creator claims.
    pub creation_date: Option<i64>,
    pub created_by: Option<String>,
    pub comment: Option<String>,
    /// BEP 19 web seeds.
    pub url_list: Vec<String>,
}

impl TorrentFile {
    pub fn parse(bytes: &[u8]) -> Result<TorrentFile, Error> {
        TorrentFile::parse_with(bytes, &Limits::DEFAULT)
    }

    pub fn parse_with(bytes: &[u8], limits: &Limits) -> Result<TorrentFile, Error> {
        if bytes.len() > limits.max_torrent_bytes {
            return Err(Error::TooLarge {
                size: bytes.len(),
                limit: limits.max_torrent_bytes,
            });
        }
        let value = bencode::decode_with(
            bytes,
            &DecodeOptions::lenient().with_limits(limits.bencode()),
        )?;
        let top = value.as_dict().ok_or(Error::WrongType {
            field: "torrent",
            expected: "dictionary",
        })?;
        let info_raw = top
            .get(b"info")
            .ok_or(Error::MissingField("info"))?
            .as_dict()
            .ok_or(Error::WrongType {
                field: "info",
                expected: "dictionary",
            })?
            .raw();
        let hint = top
            .get(b"encoding")
            .and_then(Value::as_bytes)
            .and_then(TextEncoding::for_label);
        let info = Info::parse_with(
            info_raw,
            &ParseOptions {
                limits: *limits,
                encoding_hint: hint,
            },
        )?;

        let text = |v: &Value<'_>| v.as_bytes().map(|b| decode_text(b, hint));
        let mut announce: Vec<Vec<String>> = top
            .get(b"announce-list")
            .and_then(Value::as_list)
            .map(|tiers| {
                tiers
                    .iter()
                    .filter_map(Value::as_list)
                    .map(|tier| tier.iter().filter_map(text).collect::<Vec<_>>())
                    .filter(|tier| !tier.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if announce.is_empty()
            && let Some(url) = top.get(b"announce").and_then(text)
        {
            announce.push(vec![url]);
        }

        let url_list = match top.get(b"url-list") {
            Some(Value::List(l)) => l.iter().filter_map(text).collect(),
            Some(v) => text(v).into_iter().collect(),
            None => Vec::new(),
        };

        Ok(TorrentFile {
            info,
            announce,
            creation_date: top.get(b"creation date").and_then(Value::as_int),
            created_by: utf8_or(top, "created by", hint),
            comment: utf8_or(top, "comment", hint),
            url_list,
        })
    }
}

/// Prefers the `<key>.utf-8` variant, falling back to `<key>`.
fn utf8_or(top: &Dict<'_>, key: &str, hint: Option<TextEncoding>) -> Option<String> {
    let utf8_key = format!("{key}.utf-8");
    top.get(utf8_key.as_bytes())
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            top.get(key.as_bytes())
                .and_then(Value::as_bytes)
                .map(|b| decode_text(b, hint))
        })
}

fn decode_text(raw: &[u8], hint: Option<TextEncoding>) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_owned(),
        Err(_) => hint.unwrap_or(TextEncoding::UTF8).decode(raw).into_owned(),
    }
}

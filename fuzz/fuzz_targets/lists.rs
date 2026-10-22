//! Blocklist downloads and torrent text through the content filter
//! (§20.4, §20.13): all untrusted. Nothing may panic, and IP sets must
//! survive their stored encoding.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_classify::{ContentType, FileInfo};
use lodestar_filter::lists::{self, Format, IpSet};
use lodestar_filter::{Core, CsList, Item, Lists, Switches};

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = lists::decode(data, 1 << 20) {
        let _ = lists::detect(&text);
        let _ = lists::parse_hashes(&text);
        for f in [Format::P2p, Format::Dat, Format::Cidr] {
            let set = IpSet::new(lists::parse_ips(&text, f).entries);
            assert_eq!(IpSet::decode(&set.encode()), Some(set));
        }
        let core = Core::new(Lists::shipped().unwrap(), CsList::default(), Switches::default());
        let files: Vec<FileInfo> = text
            .lines()
            .take(64)
            .map(|l| FileInfo::new(l.split('/').map(str::to_owned).collect(), l.len() as u64))
            .collect();
        for content_type in [ContentType::Video, ContentType::Adult] {
            let item = Item {
                hashes: &[[0; 20]],
                name: &text,
                files: &files,
                content_type,
            };
            assert_eq!(core.verdict(&item), core.hits(&item).into_iter().find(|h| h.on));
        }
    }
});

//! The Magnetico row normaliser never panics on any stored values, and
//! what it returns keeps the adapter's promises: a complete file list
//! adds up, nothing is silently truncated, and names stay within limits.
#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestar_sources_importers::magnetico::{
    Findings, Normaliser, Raw, RawFile, RawTorrent, Record,
};
use lodestar_store::import::Completeness;

/// Reads SQLite values from the input: a tag byte picks the storage class,
/// then a length byte and that many bytes, or eight bytes of number.
struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (b, rest) = self.0.split_first()?;
        self.0 = rest;
        Some(*b)
    }

    fn take(&mut self, n: usize) -> Vec<u8> {
        let n = n.min(self.0.len());
        let (a, rest) = self.0.split_at(n);
        self.0 = rest;
        a.to_vec()
    }

    fn int(&mut self) -> i64 {
        let mut b = self.take(8);
        b.resize(8, 0);
        i64::from_le_bytes(b.try_into().unwrap())
    }

    fn raw(&mut self) -> Raw {
        match self.byte().unwrap_or(0) % 5 {
            0 => Raw::Null,
            1 => Raw::Integer(self.int()),
            2 => Raw::Real(f64::from_bits(self.int() as u64)),
            3 => {
                let n = usize::from(self.byte().unwrap_or(0)) * 24;
                Raw::Text(self.take(n))
            }
            _ => {
                let n = usize::from(self.byte().unwrap_or(0)) * 24;
                Raw::Blob(self.take(n))
            }
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input(data);
    let now = input.int();
    let row = RawTorrent {
        id: 1,
        info_hash: input.raw(),
        name: input.raw(),
        total_size: input.raw(),
        discovered_on: input.raw(),
    };
    let overflow = u32::from(input.byte().unwrap_or(0));
    let mut files = Vec::new();
    while !input.0.is_empty() && files.len() < 256 {
        files.push(RawFile {
            path: input.raw(),
            size: input.raw(),
        });
    }
    let mut findings = Findings::default();
    match (Normaliser { now }).torrent(&row, &files, overflow, &mut findings) {
        Record::Torrent(t) => {
            assert!(!t.name.is_empty() && t.name.len() <= 4096);
            assert!(t.size > 0);
            assert_eq!(
                t.files.len() as u64 + u64::from(t.files_dropped),
                files.len() as u64 + u64::from(overflow)
            );
            if t.completeness == Completeness::ReportedComplete {
                assert_eq!(t.repair, 0);
                assert_eq!(t.files_dropped, 0);
                assert_eq!(
                    t.files.iter().map(|f| u128::from(f.size)).sum::<u128>(),
                    u128::from(t.size)
                );
            } else {
                assert_ne!(t.repair, 0);
            }
            for f in &t.files {
                assert!(
                    !f.path.is_empty() && f.path.iter().all(|s| !s.is_empty() && !s.contains(&0))
                );
            }
            let _ = t.info();
        }
        Record::Candidate(c) => {
            assert!(c.name.is_none() || c.size.is_none());
            assert!(
                c.name
                    .as_ref()
                    .is_none_or(|n| !n.is_empty() && n.len() <= 4096)
            );
        }
        Record::Invalid(_) => {}
    }
});

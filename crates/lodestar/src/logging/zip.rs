//! A small streaming zip writer for `GET /logs/archive.zip`, on `flate2`
//! (deflate and CRC-32): each member is a local header with a data
//! descriptor, so nothing is buffered beyond what deflate holds, then the
//! central directory.
//!
//! **Limits, without zip64:** a member stops at 4 GiB less 64 MiB of
//! uncompressed bytes, ending with a `[truncated …]` line, and no member
//! starts once the archive is past 3.75 GiB; the files left out are named
//! in a last `archive-incomplete.txt`. Log files are at most 1 GiB each,
//! so only an unusually large folder meets either.

use std::collections::VecDeque;
use std::io::{self, Write};

use flate2::Compression;
use flate2::Crc;
use flate2::write::DeflateEncoder;

/// The most uncompressed bytes in one member.
pub const MEMBER_MAX: u64 = (4 << 30) - (64 << 20);
/// No member starts past this offset.
pub const ARCHIVE_MAX: u64 = (3 << 30) + (3 << 28);

/// The archive never passes this.
const OFFSET_MAX: u64 = 0xFFE0_0000;

const TRUNCATED: &[u8] = b"[truncated: a zip member holds at most 4 GiB]\n";
const UNREADABLE_REST: &[u8] = b"[unreadable: the rest of this file couldn't be read]\n";

const NO_ROOM: &str = "left out: the archive reached its size limit";
const NOT_OPENED: &str = "left out: it couldn't be opened (deleted, or unreadable)";
const CUT_SHORT: &str = "cut short: the rest couldn't be read";

struct Central {
    name: String,
    crc: u32,
    csize: u64,
    size: u64,
    offset: u64,
}

struct Open {
    name: String,
    offset: u64,
    crc: Crc,
    size: u64,
    csize: u64,
    enc: DeflateEncoder<Vec<u8>>,
    full: bool,
}

/// Writes a zip into chunks the caller takes.
pub struct ZipWriter {
    out: Vec<u8>,
    offset: u64,
    entries: Vec<Central>,
    open: Option<Open>,
    time: u16,
    date: u16,
    /// [`ARCHIVE_MAX`] and [`OFFSET_MAX`] (tests lower them).
    archive_max: u64,
    offset_max: u64,
}

/// MS-DOS time and date of `millis` (UTC).
fn dos(millis: i64) -> (u16, u16) {
    let stamp = super::format::Stamp { millis }.rfc3339();
    let n = |r: std::ops::Range<usize>| stamp[r].parse::<u16>().unwrap_or(0);
    let year = n(0..4).max(1980);
    let date = ((year - 1980) << 9) | (n(5..7) << 5) | n(8..10);
    let time = (n(11..13) << 11) | (n(14..16) << 5) | (n(17..19) / 2);
    (time, date)
}

impl ZipWriter {
    pub fn new(millis: i64) -> ZipWriter {
        let (time, date) = dos(millis);
        ZipWriter {
            out: Vec::new(),
            offset: 0,
            entries: Vec::new(),
            open: None,
            time,
            date,
            archive_max: ARCHIVE_MAX,
            offset_max: OFFSET_MAX,
        }
    }

    fn put(&mut self, b: &[u8]) {
        self.out.extend_from_slice(b);
        self.offset += b.len() as u64;
    }

    /// The last member ended.
    fn last_name(&self) -> Option<String> {
        self.entries.last().map(|e| e.name.clone())
    }

    /// Whether another member may start.
    pub fn room(&self) -> bool {
        self.offset < self.archive_max
    }

    /// Starts a member (ASCII names only).
    pub fn start(&mut self, name: &str) {
        debug_assert!(self.open.is_none());
        let offset = self.offset;
        let mut h = Vec::with_capacity(30 + name.len());
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes()); // version needed
        h.extend_from_slice(&0x0008u16.to_le_bytes()); // data descriptor
        h.extend_from_slice(&8u16.to_le_bytes()); // deflate
        h.extend_from_slice(&self.time.to_le_bytes());
        h.extend_from_slice(&self.date.to_le_bytes());
        h.extend_from_slice(&[0; 12]); // crc, sizes: in the descriptor
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(name.as_bytes());
        self.put(&h);
        self.open = Some(Open {
            name: name.to_string(),
            offset,
            crc: Crc::new(),
            size: 0,
            csize: 0,
            enc: DeflateEncoder::new(Vec::new(), Compression::default()),
            full: false,
        });
    }

    /// Adds to the open member. Returns `false` once it's full (the rest
    /// is dropped and a `[truncated …]` line ends it).
    pub fn write(&mut self, data: &[u8]) -> io::Result<bool> {
        let Some(mut m) = self.open.take() else {
            return Ok(false);
        };
        if m.full {
            self.open = Some(m);
            return Ok(false);
        }
        // Within the member's limit, and the archive's offsets in 32 bits
        // (deflate never grows data by much; the slack covers that and the
        // central directory).
        let room = (MEMBER_MAX - m.size).min(self.offset_max.saturating_sub(self.offset));
        let (take, full) = match data.len() as u64 > room {
            true => (room as usize, true),
            false => (data.len(), false),
        };
        let mut piece = &data[..take];
        if full {
            // At a line's end where there is one.
            if let Some(i) = piece.iter().rposition(|b| *b == b'\n') {
                piece = &piece[..=i];
            }
        }
        m.crc.update(piece);
        m.enc.write_all(piece)?;
        m.size += piece.len() as u64;
        if full {
            m.crc.update(TRUNCATED);
            m.enc.write_all(TRUNCATED)?;
            m.size += TRUNCATED.len() as u64;
            m.full = true;
        }
        let out = std::mem::take(m.enc.get_mut());
        m.csize += out.len() as u64;
        self.put(&out);
        self.open = Some(m);
        Ok(!full)
    }

    /// Ends the open member with its data descriptor.
    pub fn end(&mut self) -> io::Result<()> {
        let Some(m) = self.open.take() else {
            return Ok(());
        };
        let rest = m.enc.finish()?;
        let csize = m.csize + rest.len() as u64;
        self.put(&rest);
        let crc = m.crc.sum();
        let mut d = Vec::with_capacity(16);
        d.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
        d.extend_from_slice(&crc.to_le_bytes());
        d.extend_from_slice(&(csize as u32).to_le_bytes());
        d.extend_from_slice(&(m.size as u32).to_le_bytes());
        self.put(&d);
        self.entries.push(Central {
            name: m.name,
            crc,
            csize,
            size: m.size,
            offset: m.offset,
        });
        Ok(())
    }

    /// Writes the central directory and the end record.
    pub fn finish(&mut self) -> io::Result<()> {
        self.end()?;
        let start = self.offset;
        let entries = std::mem::take(&mut self.entries);
        for e in &entries {
            let mut c = Vec::with_capacity(46 + e.name.len());
            c.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            c.extend_from_slice(&20u16.to_le_bytes()); // made by
            c.extend_from_slice(&20u16.to_le_bytes()); // needed
            c.extend_from_slice(&0x0008u16.to_le_bytes());
            c.extend_from_slice(&8u16.to_le_bytes());
            c.extend_from_slice(&self.time.to_le_bytes());
            c.extend_from_slice(&self.date.to_le_bytes());
            c.extend_from_slice(&e.crc.to_le_bytes());
            c.extend_from_slice(&(e.csize as u32).to_le_bytes());
            c.extend_from_slice(&(e.size as u32).to_le_bytes());
            c.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            c.extend_from_slice(&[0; 8]); // extra, comment, disk, internal attrs
            c.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            c.extend_from_slice(&(e.offset as u32).to_le_bytes());
            c.extend_from_slice(e.name.as_bytes());
            self.put(&c);
        }
        let size = self.offset - start;
        let mut end = Vec::with_capacity(22);
        end.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        end.extend_from_slice(&[0; 4]); // disks
        end.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        end.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        end.extend_from_slice(&(size as u32).to_le_bytes());
        end.extend_from_slice(&(start as u32).to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        self.put(&end);
        Ok(())
    }

    /// The bytes written since the last take.
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }
}

/// A member's content, opened when its turn comes.
pub type Content = Box<dyn Iterator<Item = io::Result<Vec<u8>>> + Send>;
pub type Opener = Box<dyn FnOnce() -> io::Result<Content> + Send>;

/// A zip as a stream of chunks: each member opened in turn, its content
/// deflated as it's read.
pub struct ZipStream {
    w: ZipWriter,
    members: VecDeque<(String, Opener)>,
    current: Option<Content>,
    /// Files left out or cut short, and why.
    left_out: Vec<(String, &'static str)>,
    done: bool,
}

impl ZipStream {
    pub fn new(millis: i64, members: Vec<(String, Opener)>) -> ZipStream {
        ZipStream {
            w: ZipWriter::new(millis),
            members: members.into(),
            current: None,
            left_out: Vec::new(),
            done: false,
        }
    }

    fn step(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(content) = &mut self.current {
                match content.next() {
                    Some(Ok(chunk)) => {
                        if !self.w.write(&chunk)? {
                            self.current = None;
                            self.w.end()?;
                        }
                    }
                    // A file that fails partway (a corrupt `.gz`) ends its
                    // member with a line saying so; the archive goes on.
                    Some(Err(_)) => {
                        self.current = None;
                        self.w.write(UNREADABLE_REST)?;
                        self.w.end()?;
                        if let Some(name) = self.w.last_name() {
                            self.left_out.push((name, CUT_SHORT));
                        }
                    }
                    None => {
                        self.current = None;
                        self.w.end()?;
                    }
                }
            } else if let Some((name, open)) = self.members.pop_front() {
                if !self.w.room() {
                    self.left_out.push((name, NO_ROOM));
                    continue;
                }
                match open() {
                    Ok(content) => {
                        self.w.start(&name);
                        self.current = Some(content);
                    }
                    // Deleted meanwhile, or unreadable: named, not silent.
                    Err(_) => self.left_out.push((name, NOT_OPENED)),
                }
            } else if !self.done {
                if !self.left_out.is_empty() {
                    self.w.start("archive-incomplete.txt");
                    let text = format!(
                        "Some log files aren't whole in this archive. Download them one at a time:\n{}\n",
                        self.left_out
                            .iter()
                            .map(|(name, why)| format!("{name}: {why}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    self.w.write(text.as_bytes())?;
                    self.w.end()?;
                }
                self.w.finish()?;
                self.done = true;
            } else {
                let rest = self.w.take();
                return Ok((!rest.is_empty()).then_some(rest));
            }
            if self.w.out.len() >= 64 << 10 {
                return Ok(Some(self.w.take()));
            }
        }
    }
}

impl Iterator for ZipStream {
    type Item = io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<io::Result<Vec<u8>>> {
        self.step().transpose()
    }
}

/// Reads a zip back, checking every member's CRC and sizes (tests).
#[cfg(test)]
pub fn read(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
    use std::io::Read;
    let u16_at = |i: usize| u16::from_le_bytes([zip[i], zip[i + 1]]) as usize;
    let u32_at = |i: usize| u32::from_le_bytes(zip[i..i + 4].try_into().unwrap()) as usize;
    let end = zip.len() - 22;
    assert_eq!(u32_at(end), 0x0605_4b50, "end record");
    let count = u16_at(end + 10);
    let mut at = u32_at(end + 16);
    let mut out = Vec::new();
    for _ in 0..count {
        assert_eq!(u32_at(at), 0x0201_4b50, "central entry");
        let crc = u32_at(at + 16) as u32;
        let csize = u32_at(at + 20);
        let size = u32_at(at + 24);
        let nlen = u16_at(at + 28);
        let local = u32_at(at + 42);
        let name = String::from_utf8(zip[at + 46..at + 46 + nlen].to_vec()).unwrap();
        assert_eq!(u32_at(local), 0x0403_4b50, "local header");
        let lnlen = u16_at(local + 26);
        let data = local + 30 + lnlen + u16_at(local + 28);
        let mut plain = Vec::new();
        flate2::read::DeflateDecoder::new(&zip[data..data + csize])
            .read_to_end(&mut plain)
            .unwrap();
        assert_eq!(plain.len(), size, "{name}: size");
        let mut c = Crc::new();
        c.update(&plain);
        assert_eq!(c.sum(), crc, "{name}: crc");
        let d = data + csize;
        assert_eq!(u32_at(d), 0x0807_4b50, "descriptor");
        assert_eq!(u32_at(d + 4) as u32, crc);
        out.push((name, plain));
        at += 46 + nlen + u16_at(at + 30) + u16_at(at + 32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(parts: Vec<&'static str>) -> Opener {
        Box::new(move || {
            Ok(Box::new(parts.into_iter().map(|p| Ok(p.as_bytes().to_vec()))) as Content)
        })
    }

    #[test]
    fn round_trip() {
        let big: &'static str = Box::leak("line of log text\n".repeat(50_000).into_boxed_str());
        let zip: Vec<u8> = ZipStream::new(
            1_792_750_500_123,
            vec![
                (
                    "a.jsonl".into(),
                    content(vec!["{\"x\":1}\n", "{\"x\":2}\n"]),
                ),
                ("empty.txt".into(), content(vec![])),
                (
                    "gone.txt".into(),
                    Box::new(|| Err(io::Error::other("gone"))),
                ),
                ("big.txt".into(), content(vec![big, big])),
            ],
        )
        .collect::<io::Result<Vec<Vec<u8>>>>()
        .unwrap()
        .concat();
        let members = read(&zip);
        let names: Vec<&str> = members.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["a.jsonl", "empty.txt", "big.txt", "archive-incomplete.txt"]
        );
        assert!(
            String::from_utf8_lossy(&members[3].1).contains(&format!("gone.txt: {NOT_OPENED}"))
        );
        assert_eq!(members[0].1, b"{\"x\":1}\n{\"x\":2}\n");
        assert!(members[1].1.is_empty());
        assert_eq!(members[2].1.len(), big.len() * 2);
        assert!(zip.len() < big.len() / 4, "deflated: {}", zip.len());
        // 1980-01-01, the earliest a zip can say.
        assert_eq!(dos(0), (0, (1 << 5) | 1));
    }

    #[test]
    fn a_full_member_ends_with_a_note() {
        let mut w = ZipWriter::new(0);
        w.start("m");
        // Pretend it's nearly full.
        w.open.as_mut().unwrap().size = MEMBER_MAX - 10;
        assert!(!w.write(b"0123\n56789abcdef\n").unwrap());
        assert!(!w.write(b"more").unwrap(), "the rest is dropped");
        let m = w.open.as_ref().unwrap();
        assert_eq!(m.size, MEMBER_MAX - 10 + 5 + TRUNCATED.len() as u64);
    }

    fn zip_of(s: ZipStream) -> Vec<(String, Vec<u8>)> {
        read(&s.collect::<io::Result<Vec<Vec<u8>>>>().unwrap().concat())
    }

    #[test]
    fn files_past_the_limit_or_unreadable_are_named_not_silent() {
        let failing: Opener = Box::new(|| {
            Ok(
                Box::new([Ok(b"first\n".to_vec()), Err(io::Error::other("corrupt"))].into_iter())
                    as Content,
            )
        });
        let mut s = ZipStream::new(
            0,
            vec![
                ("cut.jsonl".into(), failing),
                (
                    "gone.jsonl".into(),
                    Box::new(|| Err(io::Error::other("gone"))),
                ),
                ("a.jsonl".into(), content(vec!["a\n"])),
                ("b.jsonl".into(), content(vec!["b\n"])),
            ],
        );
        // Room for the first members only.
        s.w.archive_max = 1;
        let members = zip_of(s);
        let names: Vec<&str> = members.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["cut.jsonl", "archive-incomplete.txt"]);
        assert_eq!(
            members[0].1,
            [b"first\n".as_slice(), UNREADABLE_REST].concat()
        );
        let note = String::from_utf8(members[1].1.clone()).unwrap();
        for line in [
            format!("cut.jsonl: {CUT_SHORT}"),
            format!("a.jsonl: {NO_ROOM}"),
            format!("b.jsonl: {NO_ROOM}"),
        ] {
            assert!(note.contains(&line), "{note}");
        }
        // `gone` came after the limit was reached: it was never opened.
        assert!(note.contains("gone.jsonl: left out"), "{note}");
    }

    #[test]
    fn a_member_stops_before_the_offsets_pass_32_bits() {
        // Lines deflate can't shrink, so the offsets track what's written.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let text: String = (0..4000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                format!("{x:016x}{:016x}\n", x.rotate_left(29))
            })
            .collect();
        let text: &'static str = Box::leak(text.into_boxed_str());
        let mut s = ZipStream::new(0, vec![("big.jsonl".into(), content(vec![text]))]);
        s.w.offset_max = 50_000;
        let zip = s.collect::<io::Result<Vec<Vec<u8>>>>().unwrap().concat();
        assert!(zip.len() < 50_000 + (1 << 16), "{}", zip.len());
        let members = read(&zip);
        let body = &members[0].1;
        assert!(body.ends_with(TRUNCATED));
        let kept = &body[..body.len() - TRUNCATED.len()];
        assert!(
            kept.len() > 30_000 && kept.len() < text.len(),
            "{}",
            kept.len()
        );
        assert!(text.as_bytes().starts_with(kept) && kept.ends_with(b"\n"));
    }
}

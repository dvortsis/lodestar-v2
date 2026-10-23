//! Reading log files back (history, downloads, the archive): newest first
//! for history, front to back for downloads, gzip streamed and never
//! inflated whole.
//!
//! **Rotated files are gzip in members of about 1 MiB of whole lines**,
//! with a last, empty member whose header's extra field lists every
//! member's offsets ([`write_gz`]). Any gzip reader reads the file as one
//! stream; history uses the list to start at the member it needs, so
//! paging back through a file costs each member once. A file without the
//! list (an older build's, or one that fails the checks) is read in 4 MiB
//! windows from its start.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};

use flate2::Compression;
use flate2::bufread::{GzDecoder, MultiGzDecoder};
use flate2::write::GzEncoder;

/// Lines over this are unreadable (skipped in history, replaced in a
/// redacted download).
pub const MAX_LINE: usize = 1 << 20;
/// A member's target size, uncompressed.
pub const MEMBER: u64 = 1 << 20;
/// Bytes read per step backwards through a plain file.
const BLOCK: u64 = 64 << 10;
/// An older gzip file's read window.
const WINDOW: u64 = 4 << 20;
/// The member index's extra-field id.
const SI: [u8; 2] = *b"Lx";

/// A line read back: where it starts (uncompressed), and its bytes, or
/// `None` when it's over [`MAX_LINE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLine {
    pub start: u64,
    pub bytes: Option<Vec<u8>>,
}

/// Reads lines newest first.
pub trait Backward {
    /// The line before the last one returned, or `None` at the start.
    fn prev(&mut self) -> io::Result<Option<RawLine>>;
    /// Bytes read or inflated so far.
    fn cost(&self) -> u64;
}

/// One member: compressed offset, uncompressed offset and length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    pub at: u64,
    pub from: u64,
    pub len: u64,
}

/// Where the last newline at or before `len` ends: a file being written
/// is read only up to its last whole line.
pub fn whole_lines(f: &mut File, len: u64) -> io::Result<u64> {
    let mut end = len;
    let mut buf = vec![0u8; BLOCK as usize];
    while end > 0 {
        let start = end.saturating_sub(BLOCK);
        let n = (end - start) as usize;
        f.seek(SeekFrom::Start(start))?;
        f.read_exact(&mut buf[..n])?;
        if let Some(i) = buf[..n].iter().rposition(|b| *b == b'\n') {
            return Ok(start + i as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// A plain file, read backwards from `end` in blocks.
pub struct PlainBack {
    file: File,
    /// Everything at or after `pos` has been read.
    pos: u64,
    /// The bytes of the partial line after `pos` (before the last
    /// newline found), in order.
    pending: Vec<u8>,
    over: bool,
    /// Lines found in the last block, oldest first.
    found: VecDeque<RawLine>,
    cost: u64,
}

impl PlainBack {
    pub fn new(file: File, end: u64) -> PlainBack {
        PlainBack {
            file,
            pos: end,
            pending: Vec::new(),
            over: false,
            found: VecDeque::new(),
            cost: 0,
        }
    }

    fn take_pending(&mut self, start: u64) -> Option<RawLine> {
        let bytes = std::mem::take(&mut self.pending);
        let over = std::mem::replace(&mut self.over, false);
        if bytes.is_empty() && !over {
            return None;
        }
        Some(RawLine {
            start,
            bytes: (!over).then_some(bytes),
        })
    }
}

impl Backward for PlainBack {
    fn prev(&mut self) -> io::Result<Option<RawLine>> {
        loop {
            if let Some(l) = self.found.pop_back() {
                return Ok(Some(l));
            }
            if self.pos == 0 {
                return Ok(self.take_pending(0));
            }
            let start = self.pos.saturating_sub(BLOCK);
            let mut block = vec![0u8; (self.pos - start) as usize];
            self.file.seek(SeekFrom::Start(start))?;
            self.file.read_exact(&mut block)?;
            self.cost += block.len() as u64;
            // Split at the newlines, last first.
            let mut end = block.len();
            let mut found = Vec::new();
            while let Some(i) = block[..end].iter().rposition(|b| *b == b'\n') {
                let piece = &block[i + 1..end];
                self.prepend(piece);
                let line_start = start + i as u64 + 1;
                if let Some(l) = self.take_pending(line_start) {
                    found.push(l);
                }
                end = i;
            }
            self.prepend(&block[..end]);
            self.pos = start;
            found.reverse();
            self.found = found.into();
        }
    }

    fn cost(&self) -> u64 {
        self.cost
    }
}

impl PlainBack {
    fn prepend(&mut self, piece: &[u8]) {
        if self.over || piece.is_empty() {
            return;
        }
        if self.pending.len() + piece.len() > MAX_LINE {
            self.over = true;
            self.pending.clear();
            return;
        }
        let mut v = Vec::with_capacity(self.pending.len() + piece.len());
        v.extend_from_slice(piece);
        v.extend_from_slice(&self.pending);
        self.pending = v;
    }
}

/// Reads one line of `r` into `line` (without its newline), holding at
/// most [`MAX_LINE`] bytes. Returns its length on disk (newline included)
/// and whether it was over, or `None` at the end.
pub fn read_line(r: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<Option<(u64, bool)>> {
    line.clear();
    let mut over = false;
    let mut len = 0u64;
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let (take, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        let body = if done { &buf[..take - 1] } else { &buf[..take] };
        if !over {
            if line.len() + body.len() > MAX_LINE {
                over = true;
                line.clear();
            } else {
                line.extend_from_slice(body);
            }
        }
        len += take as u64;
        r.consume(take);
        if done {
            break;
        }
    }
    Ok((len > 0).then_some((len, over)))
}

/// Reads the lines of `r` front to back, each with its offset from
/// `base`, skipping (as `None`) those over [`MAX_LINE`] without holding
/// them, and empty ones. `each` says whether to stop.
pub fn forward(
    r: &mut impl BufRead,
    base: u64,
    mut each: impl FnMut(RawLine) -> io::Result<bool>,
) -> io::Result<u64> {
    let mut pos = base;
    let mut line = Vec::new();
    while let Some((len, over)) = read_line(r, &mut line)? {
        let start = pos;
        pos += len;
        if line.is_empty() && !over {
            continue;
        }
        let raw = RawLine {
            start,
            bytes: (!over).then(|| line.clone()),
        };
        if each(raw)? {
            break;
        }
    }
    Ok(pos)
}

/// Counts bytes passing through.
struct Counted<R> {
    inner: R,
    n: u64,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.n += n as u64;
        Ok(n)
    }
}

/// A gzip file with a member index, read backwards a member at a time.
pub struct GzBack {
    file: File,
    members: Vec<Member>,
    /// The next member to read (exclusive).
    next: usize,
    end: u64,
    found: Vec<RawLine>,
    cost: u64,
}

impl GzBack {
    fn load(&mut self, i: usize) -> io::Result<()> {
        let m = self.members[i];
        self.file.seek(SeekFrom::Start(m.at))?;
        let mut d = Counted {
            inner: GzDecoder::new(BufReader::new(&mut self.file)),
            n: 0,
        };
        let mut r = BufReader::with_capacity(64 << 10, &mut d);
        let end = self.end;
        let mut found = Vec::new();
        forward(&mut r, m.from, |l| {
            if l.start >= end {
                return Ok(true);
            }
            found.push(l);
            Ok(false)
        })?;
        drop(r);
        self.cost += d.n;
        self.found = found;
        Ok(())
    }
}

impl Backward for GzBack {
    fn prev(&mut self) -> io::Result<Option<RawLine>> {
        loop {
            if let Some(l) = self.found.pop() {
                return Ok(Some(l));
            }
            if self.next == 0 {
                return Ok(None);
            }
            self.next -= 1;
            let i = self.next;
            self.load(i)?;
        }
    }

    fn cost(&self) -> u64 {
        self.cost
    }
}

/// A gzip file without an index: each window is a pass from the start.
pub struct GzWindows {
    file: File,
    /// Lines before this are still to be read.
    end: u64,
    found: Vec<RawLine>,
    cost: u64,
}

impl Backward for GzWindows {
    fn prev(&mut self) -> io::Result<Option<RawLine>> {
        loop {
            if let Some(l) = self.found.pop() {
                return Ok(Some(l));
            }
            if self.end == 0 {
                return Ok(None);
            }
            let from = self.end.saturating_sub(WINDOW);
            self.file.seek(SeekFrom::Start(0))?;
            let mut d = Counted {
                inner: MultiGzDecoder::new(BufReader::new(&mut self.file)),
                n: 0,
            };
            let mut r = BufReader::with_capacity(64 << 10, &mut d);
            let end = self.end;
            let mut found = Vec::new();
            let mut first_start = end;
            forward(&mut r, 0, |l| {
                if l.start >= end {
                    return Ok(true);
                }
                if l.start >= from {
                    first_start = first_start.min(l.start);
                    found.push(l);
                }
                Ok(false)
            })?;
            drop(r);
            self.cost += d.n;
            if found.is_empty() {
                // No line starts in [from, end): one line spans the window.
                // Read from the start instead, once, to the end.
                self.end = from;
                if from == 0 {
                    return Ok(None);
                }
                continue;
            }
            self.end = first_start;
            self.found = found;
        }
    }

    fn cost(&self) -> u64 {
        self.cost
    }
}

/// A reader of `file`'s lines before `end` (uncompressed), newest first.
pub fn backward(mut file: File, gz: bool, end: u64) -> io::Result<Box<dyn Backward + Send>> {
    if !gz {
        return Ok(Box::new(PlainBack::new(file, end)));
    }
    match members(&mut file)? {
        Some(members) => {
            let next = members.partition_point(|m| m.from < end);
            Ok(Box::new(GzBack {
                file,
                members,
                next,
                end,
                found: Vec::new(),
                cost: 0,
            }))
        }
        None => Ok(Box::new(GzWindows {
            file,
            end,
            found: Vec::new(),
            cost: 0,
        })),
    }
}

/// A file's uncompressed length (gzip: from its index, or by inflating).
pub fn plain_len(file: &mut File, gz: bool) -> io::Result<u64> {
    if !gz {
        return Ok(file.metadata()?.len());
    }
    if let Some(m) = members(file)? {
        return Ok(m.last().map_or(0, |m| m.from + m.len));
    }
    file.seek(SeekFrom::Start(0))?;
    io::copy(
        &mut MultiGzDecoder::new(BufReader::new(&mut *file)),
        &mut io::sink(),
    )
}

/// The member index at the end of a gzip file, checked: `None` when
/// there's none or it doesn't hold together (then the file is read from
/// its start).
pub fn members(f: &mut File) -> io::Result<Option<Vec<Member>>> {
    let size = f.metadata()?.len();
    if size < 14 + 26 {
        return Ok(None);
    }
    let mut tail = [0u8; 14];
    f.seek(SeekFrom::Start(size - 14))?;
    f.read_exact(&mut tail)?;
    if tail[4..6] != [0x03, 0x00] || tail[6..].iter().any(|b| *b != 0) {
        return Ok(None);
    }
    let total = u64::from(u32::from_le_bytes(tail[..4].try_into().expect("4")));
    if total < 26 + 4 || total > size || total > 70_000 {
        return Ok(None);
    }
    let start = size - total;
    let mut m = vec![0u8; total as usize];
    f.seek(SeekFrom::Start(start))?;
    f.read_exact(&mut m)?;
    let header_ok = m[..4] == [0x1f, 0x8b, 0x08, 0x04] && m[12..14] == SI;
    let xlen = u16::from_le_bytes([m[10], m[11]]) as usize;
    let dlen = u16::from_le_bytes([m[14], m[15]]) as usize;
    if !header_ok || xlen != dlen + 4 || dlen + 26 != total as usize {
        return Ok(None);
    }
    let data = &m[16..16 + dlen];
    // version, count, entries, then the member's own length.
    if data.len() < 9 || data[0] != 1 {
        return Ok(None);
    }
    let count = u32::from_le_bytes(data[1..5].try_into().expect("4")) as usize;
    if data.len() != 9 + count * 24 {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(count);
    let mut at_next = 0u64;
    let mut from_next = 0u64;
    for i in 0..count {
        let e = &data[5 + i * 24..5 + (i + 1) * 24];
        let at = u64::from_le_bytes(e[..8].try_into().expect("8"));
        let from = u64::from_le_bytes(e[8..16].try_into().expect("8"));
        let len = u64::from_le_bytes(e[16..24].try_into().expect("8"));
        let ok = if i == 0 { at == 0 } else { at > at_next };
        if !ok || from != from_next || at >= start {
            return Ok(None);
        }
        at_next = at;
        from_next = from.saturating_add(len);
        out.push(Member { at, from, len });
    }
    Ok(Some(out))
}

/// Writes `input` as gzip members of about [`MEMBER`] bytes of whole
/// lines each, then the index member. Returns the bytes written.
pub fn write_gz(input: &mut impl BufRead, out: &mut impl Write) -> io::Result<u64> {
    struct Count<'a, W> {
        w: &'a mut W,
        n: u64,
    }
    impl<W: Write> Write for Count<'_, W> {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            let n = self.w.write(b)?;
            self.n += n as u64;
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.w.flush()
        }
    }
    let mut out = Count { w: out, n: 0 };
    let mut members = Vec::new();
    let mut from = 0u64;
    let mut buf = Vec::new();
    loop {
        let at = out.n;
        let mut enc = GzEncoder::new(&mut out, Compression::default());
        let mut len = 0u64;
        while len < MEMBER {
            buf.clear();
            let n = input.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            enc.write_all(&buf)?;
            len += n as u64;
        }
        if len == 0 {
            // Nothing: no member (an empty file has only the index).
            drop(enc);
            out.n = at;
            break;
        }
        enc.finish()?;
        members.push(Member { at, from, len });
        from += len;
        if len < MEMBER {
            break;
        }
    }
    // The index can list 2,700 members (about 2.7 GiB); a larger file
    // keeps its members and goes without one.
    if members.len() <= 2_700 {
        let mut data = vec![1u8];
        data.extend_from_slice(&(members.len() as u32).to_le_bytes());
        for m in &members {
            data.extend_from_slice(&m.at.to_le_bytes());
            data.extend_from_slice(&m.from.to_le_bytes());
            data.extend_from_slice(&m.len.to_le_bytes());
        }
        let total = (data.len() + 4 + 26) as u32;
        data.extend_from_slice(&total.to_le_bytes());
        let mut m = vec![0x1f, 0x8b, 0x08, 0x04, 0, 0, 0, 0, 0, 0xff];
        m.extend_from_slice(&((data.len() + 4) as u16).to_le_bytes());
        m.extend_from_slice(&SI);
        m.extend_from_slice(&(data.len() as u16).to_le_bytes());
        m.extend_from_slice(&data);
        // An empty final deflate block, then CRC-32 and length, both 0.
        m.extend_from_slice(&[0x03, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.write_all(&m)?;
    }
    Ok(out.n)
}

/// The first line of a file (at most 64 KiB read, decompressed): for its
/// time and to recognise the current file after a rotation.
pub fn first_line(file: &mut File, gz: bool) -> io::Result<Option<Vec<u8>>> {
    file.seek(SeekFrom::Start(0))?;
    let r: Box<dyn Read> = match gz {
        true => Box::new(MultiGzDecoder::new(BufReader::new(&mut *file))),
        false => Box::new(&mut *file),
    };
    let mut r = BufReader::new(r.take(64 << 10));
    let mut line = Vec::new();
    r.read_until(b'\n', &mut line)?;
    if line.last() != Some(&b'\n') {
        return Ok(None);
    }
    line.pop();
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(n: usize, pad: usize) -> String {
        (0..n)
            .map(|i| format!("{i:06} {}\n", "x".repeat(pad)))
            .collect()
    }

    fn all_back(mut b: Box<dyn Backward + Send>) -> Vec<RawLine> {
        let mut out = Vec::new();
        while let Some(l) = b.prev().unwrap() {
            out.push(l);
        }
        out
    }

    fn texts(v: &[RawLine]) -> Vec<String> {
        v.iter()
            .map(|l| match &l.bytes {
                Some(b) => String::from_utf8_lossy(b).into_owned(),
                None => "<over>".into(),
            })
            .collect()
    }

    #[test]
    fn a_plain_file_backwards() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let text = lines(5000, 30);
        std::fs::write(&p, &text).unwrap();
        let got = all_back(backward(File::open(&p).unwrap(), false, text.len() as u64).unwrap());
        let mut want: Vec<String> = text.lines().map(str::to_string).collect();
        want.reverse();
        assert_eq!(texts(&got), want);
        // Offsets are the lines' starts.
        let mut f = File::open(&p).unwrap();
        let l = &got[10];
        f.seek(SeekFrom::Start(l.start)).unwrap();
        let mut b = vec![0; 6];
        f.read_exact(&mut b).unwrap();
        assert_eq!(&b, &l.bytes.as_ref().unwrap()[..6]);
        // From the middle.
        let got = all_back(backward(File::open(&p).unwrap(), false, got[10].start).unwrap());
        assert_eq!(got.len(), 5000 - 11);
    }

    #[test]
    fn long_lines_are_marked_not_held() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        let text = format!("short\n{}\nlast\n", "y".repeat(MAX_LINE + 10));
        std::fs::write(&p, &text).unwrap();
        let got = all_back(backward(File::open(&p).unwrap(), false, text.len() as u64).unwrap());
        assert_eq!(texts(&got), ["last", "<over>", "short"]);
        let mut fwd = Vec::new();
        forward(&mut BufReader::new(text.as_bytes()), 0, |l| {
            fwd.push(l);
            Ok(false)
        })
        .unwrap();
        assert_eq!(texts(&fwd), ["short", "<over>", "last"]);
        assert_eq!(fwd[2].start, (6 + MAX_LINE + 10 + 1) as u64);
    }

    #[test]
    fn members_round_trip_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.gz");
        let text = lines(60_000, 40);
        let mut out = File::create(&p).unwrap();
        write_gz(&mut BufReader::new(text.as_bytes()), &mut out).unwrap();
        drop(out);
        // Any gzip reader reads it whole.
        let mut s = String::new();
        MultiGzDecoder::new(BufReader::new(File::open(&p).unwrap()))
            .read_to_string(&mut s)
            .unwrap();
        assert_eq!(s, text);
        let mut f = File::open(&p).unwrap();
        let m = members(&mut f).unwrap().expect("an index");
        assert!(m.len() >= 2, "{m:?}");
        assert_eq!(plain_len(&mut f, true).unwrap(), text.len() as u64);
        let got = all_back(backward(File::open(&p).unwrap(), true, text.len() as u64).unwrap());
        let mut want: Vec<String> = text.lines().map(str::to_string).collect();
        want.reverse();
        assert_eq!(texts(&got), want);
        // From an offset inside a member, cheaply.
        let mid = got[40_000].start;
        let mut b = backward(File::open(&p).unwrap(), true, mid).unwrap();
        assert_eq!(b.prev().unwrap().unwrap(), got[40_001]);
        assert!(b.cost() <= 2 * MEMBER, "{}", b.cost());
        assert_eq!(
            first_line(&mut File::open(&p).unwrap(), true)
                .unwrap()
                .unwrap(),
            b"000000 "
                .iter()
                .chain([b'x'; 40].iter())
                .copied()
                .collect::<Vec<u8>>()
        );
    }

    #[test]
    fn an_older_gzip_file_is_read_in_windows() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.gz");
        let text = lines(200_000, 40);
        let mut e = GzEncoder::new(File::create(&p).unwrap(), Compression::fast());
        e.write_all(text.as_bytes()).unwrap();
        e.finish().unwrap();
        assert!(members(&mut File::open(&p).unwrap()).unwrap().is_none());
        let got = all_back(backward(File::open(&p).unwrap(), true, text.len() as u64).unwrap());
        assert_eq!(got.len(), 200_000);
        assert_eq!(texts(&got[..1]), [text.lines().last().unwrap()]);
        assert_eq!(got.last().unwrap().start, 0);
        let mut starts: Vec<u64> = got.iter().map(|l| l.start).collect();
        starts.dedup();
        assert_eq!(starts.len(), 200_000, "no line twice");
    }

    #[test]
    fn a_damaged_index_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.gz");
        let text = lines(30_000, 40);
        let mut out = Vec::new();
        write_gz(&mut BufReader::new(text.as_bytes()), &mut out).unwrap();
        // Point the second member somewhere else.
        let n = out.len();
        let total = u32::from_le_bytes(out[n - 14..n - 10].try_into().unwrap()) as usize;
        let data = n - total + 16;
        out[data + 5 + 24 + 8] ^= 0x55;
        std::fs::write(&p, &out).unwrap();
        assert!(members(&mut File::open(&p).unwrap()).unwrap().is_none());
        let got = all_back(backward(File::open(&p).unwrap(), true, text.len() as u64).unwrap());
        assert_eq!(got.len(), 30_000);
    }

    #[test]
    fn the_current_file_stops_at_its_last_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, "a\nb\npart").unwrap();
        let mut f = File::open(&p).unwrap();
        assert_eq!(whole_lines(&mut f, 8).unwrap(), 4);
        std::fs::write(&p, "partial").unwrap();
        assert_eq!(whole_lines(&mut File::open(&p).unwrap(), 7).unwrap(), 0);
    }
}

//! The file-tree blob: one compact record per torrent instead of one row per
//! file (architecture §7). This module does the uncompressed serialization;
//! [`crate::compress`] wraps it in zstd with a trained dictionary.
//!
//! Layout (format 1). Integers are unsigned LEB128 varints unless noted.
//! Like values are grouped into columns because zstd compresses runs of
//! similar data far better than interleaved records.
//!
//! ```text
//! u8 format (=1) | u8 flags | u8 path encoding id
//! dir_count | file_count
//! dir parent[dir_count]         directory i (1-based) has parent < i; 0 is the root
//! dir name_len[dir_count] | dir names (concatenated)
//! file parent[file_count]       index into the directory table
//! file name_len[file_count] | file names (concatenated)
//! file size[file_count]
//! if HAS_ATTRS:    u8 attrs[file_count]
//! if HAS_SYMLINKS: per symlink file: target segment count + 1 (0 = no target),
//!                  then (len, bytes) per segment
//! if HAS_ROOTS:    presence bitmap, then 32 bytes per present root
//! if HAS_MD5 / HAS_SHA1 / HAS_ED2K: presence bitmap, then the digests
//! ```
//!
//! Names are stored as raw bytes in the path encoding, so decoding can be
//! redone if charset detection improves.

use std::collections::HashMap;
use std::fmt;

use lodestar_metainfo::{File, FileAttrs, FileHashes, TextEncoding};

pub const FORMAT_VERSION: u8 = 1;

const HAS_ATTRS: u8 = 1 << 0;
const HAS_SYMLINKS: u8 = 1 << 1;
const HAS_ROOTS: u8 = 1 << 2;
const HAS_MD5: u8 = 1 << 3;
const HAS_SHA1: u8 = 1 << 4;
const HAS_ED2K: u8 = 1 << 5;
const KNOWN_FLAGS: u8 = (1 << 6) - 1;

/// A decoded file list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTree {
    pub encoding: TextEncoding,
    pub files: Vec<File>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CodecError {
    /// A file has an empty path; metainfo never produces one.
    EmptyPath,
    UnsupportedFormat(u8),
    Corrupt(&'static str),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::EmptyPath => f.write_str("file tree: file with an empty path"),
            CodecError::UnsupportedFormat(v) => write!(f, "file tree: unsupported format {v}"),
            CodecError::Corrupt(what) => write!(f, "file tree: corrupt blob ({what})"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Serializes a file list.
pub fn encode(files: &[File], encoding: TextEncoding) -> Result<Vec<u8>, CodecError> {
    // Intern directories in first-seen order, so a parent always precedes
    // its children.
    let mut dir_ids: HashMap<(u32, &[u8]), u32> = HashMap::new();
    let mut dirs: Vec<(u32, &[u8])> = Vec::new();
    let mut file_parents: Vec<u32> = Vec::with_capacity(files.len());
    let mut flags = 0u8;
    for f in files {
        let (_, parents) = f.path.split_last().ok_or(CodecError::EmptyPath)?;
        let mut parent = 0u32;
        for seg in parents {
            parent = *dir_ids.entry((parent, seg)).or_insert_with(|| {
                dirs.push((parent, seg));
                dirs.len() as u32
            });
        }
        file_parents.push(parent);
        if !f.attrs.is_empty() {
            flags |= HAS_ATTRS;
        }
        if f.attrs.contains(FileAttrs::SYMLINK) {
            flags |= HAS_SYMLINKS;
        }
        if f.pieces_root.is_some() {
            flags |= HAS_ROOTS;
        }
        if f.hashes.md5.is_some() {
            flags |= HAS_MD5;
        }
        if f.hashes.sha1.is_some() {
            flags |= HAS_SHA1;
        }
        if f.hashes.ed2k.is_some() {
            flags |= HAS_ED2K;
        }
    }

    let mut out = Vec::with_capacity(64 + files.len() * 32);
    out.extend_from_slice(&[FORMAT_VERSION, flags, encoding.id()]);
    put_varint(&mut out, dirs.len() as u64);
    put_varint(&mut out, files.len() as u64);

    for (parent, _) in &dirs {
        put_varint(&mut out, u64::from(*parent));
    }
    for (_, name) in &dirs {
        put_varint(&mut out, name.len() as u64);
    }
    for (_, name) in &dirs {
        out.extend_from_slice(name);
    }

    for parent in &file_parents {
        put_varint(&mut out, u64::from(*parent));
    }
    let names = || files.iter().map(|f| f.path.last().expect("checked above"));
    for name in names() {
        put_varint(&mut out, name.len() as u64);
    }
    for name in names() {
        out.extend_from_slice(name);
    }
    for f in files {
        put_varint(&mut out, f.size);
    }

    if flags & HAS_ATTRS != 0 {
        out.extend(files.iter().map(|f| f.attrs.bits()));
    }
    if flags & HAS_SYMLINKS != 0 {
        for f in files
            .iter()
            .filter(|f| f.attrs.contains(FileAttrs::SYMLINK))
        {
            match &f.symlink_target {
                None => put_varint(&mut out, 0),
                Some(target) => {
                    put_varint(&mut out, target.len() as u64 + 1);
                    for seg in target {
                        put_varint(&mut out, seg.len() as u64);
                        out.extend_from_slice(seg);
                    }
                }
            }
        }
    }
    put_optional_column(&mut out, flags & HAS_ROOTS != 0, files, |f| f.pieces_root);
    put_optional_column(&mut out, flags & HAS_MD5 != 0, files, |f| f.hashes.md5);
    put_optional_column(&mut out, flags & HAS_SHA1 != 0, files, |f| f.hashes.sha1);
    put_optional_column(&mut out, flags & HAS_ED2K != 0, files, |f| f.hashes.ed2k);
    Ok(out)
}

/// Deserializes a file list. Never panics, whatever the input.
pub fn decode(blob: &[u8]) -> Result<FileTree, CodecError> {
    let mut r = Reader { buf: blob, pos: 0 };
    let version = r.u8()?;
    if version != FORMAT_VERSION {
        return Err(CodecError::UnsupportedFormat(version));
    }
    let flags = r.u8()?;
    if flags & !KNOWN_FLAGS != 0 {
        return Err(CodecError::Corrupt("unknown flags"));
    }
    let encoding = TextEncoding::from_id(r.u8()?).ok_or(CodecError::Corrupt("encoding id"))?;
    // Each directory and file costs at least two bytes, which bounds the
    // allocations below by the blob size.
    let dir_count = r.count()?;
    let file_count = r.count()?;

    let mut dir_parents = Vec::with_capacity(dir_count);
    for i in 0..dir_count {
        let parent = r.varint()?;
        if parent > i as u64 {
            return Err(CodecError::Corrupt("directory parent order"));
        }
        dir_parents.push(parent as usize);
    }
    let dir_names = r.names(dir_count)?;
    // dir_paths[0] is the root.
    let mut dir_paths: Vec<Vec<Vec<u8>>> = Vec::with_capacity(dir_count + 1);
    dir_paths.push(Vec::new());
    for (parent, name) in dir_parents.into_iter().zip(dir_names) {
        let mut path = dir_paths[parent].clone();
        path.push(name.to_vec());
        dir_paths.push(path);
    }

    let mut file_parents = Vec::with_capacity(file_count);
    for _ in 0..file_count {
        let parent = r.varint()?;
        if parent > dir_count as u64 {
            return Err(CodecError::Corrupt("file parent"));
        }
        file_parents.push(parent as usize);
    }
    let file_names = r.names(file_count)?;
    let mut files = Vec::with_capacity(file_count);
    for (parent, name) in file_parents.into_iter().zip(file_names) {
        let mut path = dir_paths[parent].clone();
        path.push(name.to_vec());
        files.push(File {
            path,
            size: r.varint()?,
            attrs: FileAttrs::empty(),
            symlink_target: None,
            pieces_root: None,
            hashes: FileHashes::default(),
        });
    }

    if flags & HAS_ATTRS != 0 {
        for f in &mut files {
            let bits = r.u8()?;
            f.attrs = FileAttrs::from_bits_truncate(bits);
            if f.attrs.bits() != bits {
                return Err(CodecError::Corrupt("unknown attribute bits"));
            }
        }
    }
    if flags & HAS_SYMLINKS != 0 {
        for f in files
            .iter_mut()
            .filter(|f| f.attrs.contains(FileAttrs::SYMLINK))
        {
            let n = r.varint()?;
            if n > 0 {
                let segs = usize::try_from(n - 1)
                    .ok()
                    .filter(|&n| n <= r.remaining())
                    .ok_or(CodecError::Corrupt("symlink segment count"))?;
                let mut target = Vec::with_capacity(segs);
                for _ in 0..segs {
                    let len = r.count()?;
                    target.push(r.bytes(len)?.to_vec());
                }
                f.symlink_target = Some(target);
            }
        }
    }
    r.optional_column(flags & HAS_ROOTS != 0, &mut files, |f, v| {
        f.pieces_root = Some(v)
    })?;
    r.optional_column(flags & HAS_MD5 != 0, &mut files, |f, v| {
        f.hashes.md5 = Some(v)
    })?;
    r.optional_column(flags & HAS_SHA1 != 0, &mut files, |f, v| {
        f.hashes.sha1 = Some(v)
    })?;
    r.optional_column(flags & HAS_ED2K != 0, &mut files, |f, v| {
        f.hashes.ed2k = Some(v)
    })?;
    if r.remaining() != 0 {
        return Err(CodecError::Corrupt("trailing bytes"));
    }
    Ok(FileTree { encoding, files })
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Presence bitmap followed by the present values, in file order.
fn put_optional_column<const N: usize>(
    out: &mut Vec<u8>,
    enabled: bool,
    files: &[File],
    get: impl Fn(&File) -> Option<[u8; N]>,
) {
    if !enabled {
        return;
    }
    let start = out.len();
    out.resize(start + files.len().div_ceil(8), 0);
    for (i, f) in files.iter().enumerate() {
        if get(f).is_some() {
            out[start + i / 8] |= 1 << (i % 8);
        }
    }
    for value in files.iter().filter_map(&get) {
        out.extend_from_slice(&value);
    }
}

struct Reader<'b> {
    buf: &'b [u8],
    pos: usize,
}

impl<'b> Reader<'b> {
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn u8(&mut self) -> Result<u8, CodecError> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or(CodecError::Corrupt("truncated"))?;
        self.pos += 1;
        Ok(b)
    }

    fn bytes(&mut self, len: usize) -> Result<&'b [u8], CodecError> {
        if len > self.remaining() {
            return Err(CodecError::Corrupt("truncated"));
        }
        let out = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Ok(out)
    }

    fn varint(&mut self) -> Result<u64, CodecError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            let bits = u64::from(b & 0x7f);
            if shift == 63 && bits > 1 {
                return Err(CodecError::Corrupt("varint overflow"));
            }
            v |= bits << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(CodecError::Corrupt("varint overflow"))
    }

    /// A length or count, which can never exceed the bytes left.
    fn count(&mut self) -> Result<usize, CodecError> {
        usize::try_from(self.varint()?)
            .ok()
            .filter(|&n| n <= self.remaining())
            .ok_or(CodecError::Corrupt("count exceeds blob"))
    }

    /// A column of `n` lengths followed by the concatenated names.
    fn names(&mut self, n: usize) -> Result<Vec<&'b [u8]>, CodecError> {
        let mut lens = Vec::with_capacity(n);
        for _ in 0..n {
            lens.push(self.count()?);
        }
        lens.into_iter().map(|len| self.bytes(len)).collect()
    }

    fn optional_column<const N: usize>(
        &mut self,
        enabled: bool,
        files: &mut [File],
        mut set: impl FnMut(&mut File, [u8; N]),
    ) -> Result<(), CodecError> {
        if !enabled {
            return Ok(());
        }
        let bitmap = self.bytes(files.len().div_ceil(8))?;
        for (i, f) in files.iter_mut().enumerate() {
            if bitmap[i / 8] & (1 << (i % 8)) != 0 {
                let value = self.bytes(N)?.try_into().expect("N bytes");
                set(f, value);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &[&str], size: u64) -> File {
        File {
            path: path.iter().map(|s| s.as_bytes().to_vec()).collect(),
            size,
            attrs: FileAttrs::empty(),
            symlink_target: None,
            pieces_root: None,
            hashes: FileHashes::default(),
        }
    }

    #[test]
    fn layout_is_compact_and_interned() {
        let files = vec![
            file(&["Season 1", "e01.mkv"], 1_000_000_000),
            file(&["Season 1", "e02.mkv"], 1_000_000_001),
            file(&["Season 1", "Subs", "e01.srt"], 50_000),
            file(&["readme.txt"], 12),
        ];
        let blob = encode(&files, TextEncoding::UTF8).unwrap();
        // Two interned directories, no optional sections.
        assert_eq!(&blob[..5], &[1, 0, 0, 2, 4]);
        let decoded = decode(&blob).unwrap();
        assert_eq!(decoded.files, files);
        assert_eq!(decoded.encoding, TextEncoding::UTF8);
    }

    #[test]
    fn optional_sections_roundtrip() {
        let mut files = vec![file(&["a"], 1), file(&["b"], 0), file(&["c"], 3)];
        files[0].pieces_root = Some([9; 32]);
        files[0].hashes.sha1 = Some([1; 20]);
        files[1].attrs = FileAttrs::SYMLINK;
        files[1].symlink_target = Some(vec![b"x".to_vec(), b"".to_vec()]);
        files[2].attrs = FileAttrs::SYMLINK;
        files[2].hashes.md5 = Some([2; 16]);
        files[2].hashes.ed2k = Some([3; 16]);
        let enc = TextEncoding::from_id(1).unwrap();
        let blob = encode(&files, enc).unwrap();
        let decoded = decode(&blob).unwrap();
        assert_eq!(decoded.files, files);
        assert_eq!(decoded.encoding, enc);
    }

    #[test]
    fn rejects_empty_paths() {
        assert_eq!(
            encode(&[file(&[], 1)], TextEncoding::UTF8),
            Err(CodecError::EmptyPath)
        );
    }

    #[test]
    fn rejects_corruption() {
        let blob = encode(&[file(&["d", "f"], 5)], TextEncoding::UTF8).unwrap();
        assert_eq!(decode(&[]), Err(CodecError::Corrupt("truncated")));
        assert_eq!(decode(&[2]), Err(CodecError::UnsupportedFormat(2)));
        let mut trailing = blob.clone();
        trailing.push(0);
        assert_eq!(
            decode(&trailing),
            Err(CodecError::Corrupt("trailing bytes"))
        );
        let mut bad_parent = blob.clone();
        bad_parent[5] = 1; // the only directory claims itself as parent
        assert_eq!(
            decode(&bad_parent),
            Err(CodecError::Corrupt("directory parent order"))
        );
        assert!(decode(&blob[..blob.len() - 1]).is_err());
        assert_eq!(
            decode(&[
                1, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f
            ]),
            Err(CodecError::Corrupt("varint overflow"))
        );
    }

    #[test]
    fn varint_extremes() {
        for v in [0, 127, 128, 16_383, 16_384, u64::MAX] {
            let mut out = Vec::new();
            put_varint(&mut out, v);
            let mut r = Reader { buf: &out, pos: 0 };
            assert_eq!(r.varint().unwrap(), v);
            assert_eq!(r.remaining(), 0);
        }
    }
}

//! Per-category file counts and bytes (§6 "Derived", §8 `cat_*` fields),
//! and the compact `torrents.comp` encoding.

use crate::ext::Category;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CategoryStat {
    pub count: u32,
    pub bytes: u64,
}

/// What a torrent is made of. Padding files (BEP 47) are counted apart
/// and never in any category's share.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composition {
    pub stats: [CategoryStat; 10],
    pub padding: CategoryStat,
}

/// The current `comp` format. Bump it on any layout change; readers
/// reject versions they don't know and the row is re-derived.
pub const COMP_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Version(u8),
    Truncated,
    BadVarint,
    BadMask,
    /// An entry with no files, which the encoder never writes.
    EmptyEntry,
    TrailingBytes,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid composition blob: {self:?}")
    }
}

impl std::error::Error for DecodeError {}

/// Bit 10 of the mask is the padding entry.
const PADDING_BIT: u32 = 10;

impl Composition {
    pub fn add(&mut self, category: Category, size: u64) {
        bump(&mut self.stats[category.index()], size);
    }

    pub fn add_padding(&mut self, size: u64) {
        bump(&mut self.padding, size);
    }

    pub fn get(&self, category: Category) -> CategoryStat {
        self.stats[category.index()]
    }

    /// Files, not counting padding.
    pub fn total_count(&self) -> u32 {
        self.stats
            .iter()
            .fold(0u32, |a, s| a.saturating_add(s.count))
    }

    /// Bytes, not counting padding.
    pub fn total_bytes(&self) -> u64 {
        self.stats
            .iter()
            .fold(0u64, |a, s| a.saturating_add(s.bytes))
    }

    /// This category's share of the bytes, as a whole percent. Any
    /// non-zero share is at least 1 and only a complete one is 100, so
    /// `cat:software=0` and `cat:video=100` mean exactly what they say.
    pub fn pct_bytes(&self, category: Category) -> u8 {
        percent(self.get(category).bytes, self.total_bytes())
    }

    /// This category's share of the files, rounded the same way.
    pub fn pct_count(&self, category: Category) -> u8 {
        percent(
            u64::from(self.get(category).count),
            u64::from(self.total_count()),
        )
    }

    /// The category with the most bytes (or the most files, when every
    /// file is empty). Ties go to the earlier category.
    pub fn dominant(&self) -> Option<Category> {
        let by_bytes = self.total_bytes() > 0;
        Category::ALL
            .iter()
            .copied()
            .filter(|c| self.get(*c).count > 0)
            .max_by(|a, b| {
                let key = |c: Category| {
                    let s = self.get(c);
                    if by_bytes {
                        s.bytes
                    } else {
                        u64::from(s.count)
                    }
                };
                key(*a).cmp(&key(*b)).then(b.index().cmp(&a.index()))
            })
    }

    /// The `torrents.comp` blob: a version byte, a varint bitmask of the
    /// non-empty entries (categories in index order, then padding), and a
    /// varint count and byte total for each.
    pub fn encode(&self) -> Vec<u8> {
        let entries = self.stats.iter().chain(std::iter::once(&self.padding));
        let mut mask = 0u64;
        for (bit, s) in entries.clone().enumerate() {
            if s.count > 0 {
                mask |= 1 << bit;
            }
        }
        let mut out = vec![COMP_VERSION];
        put_varint(&mut out, mask);
        for s in entries.filter(|s| s.count > 0) {
            put_varint(&mut out, u64::from(s.count));
            put_varint(&mut out, s.bytes);
        }
        out
    }

    /// The inverse of [`encode`](Self::encode). Strict: anything `encode`
    /// wouldn't produce is an error, so decoding never invents data.
    pub fn decode(blob: &[u8]) -> Result<Composition, DecodeError> {
        let (&version, mut rest) = blob.split_first().ok_or(DecodeError::Truncated)?;
        if version != COMP_VERSION {
            return Err(DecodeError::Version(version));
        }
        let mask = get_varint(&mut rest)?;
        if mask >> (PADDING_BIT + 1) != 0 {
            return Err(DecodeError::BadMask);
        }
        let mut c = Composition::default();
        for bit in 0..=PADDING_BIT {
            if mask & (1 << bit) == 0 {
                continue;
            }
            let count =
                u32::try_from(get_varint(&mut rest)?).map_err(|_| DecodeError::BadVarint)?;
            let bytes = get_varint(&mut rest)?;
            if count == 0 {
                return Err(DecodeError::EmptyEntry);
            }
            let s = CategoryStat { count, bytes };
            if bit == PADDING_BIT {
                c.padding = s;
            } else {
                c.stats[bit as usize] = s;
            }
        }
        if !rest.is_empty() {
            return Err(DecodeError::TrailingBytes);
        }
        Ok(c)
    }
}

fn bump(s: &mut CategoryStat, size: u64) {
    s.count = s.count.saturating_add(1);
    s.bytes = s.bytes.saturating_add(size);
}

fn percent(part: u64, total: u64) -> u8 {
    if part == 0 || total == 0 {
        return 0;
    }
    if part >= total {
        return 100;
    }
    let p = (u128::from(part) * 100 / u128::from(total)) as u8;
    p.clamp(1, 99)
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// LEB128, canonical only: no overlong forms, nothing past 64 bits.
fn get_varint(buf: &mut &[u8]) -> Result<u64, DecodeError> {
    let mut v = 0u64;
    for i in 0..10 {
        let (&b, rest) = buf.split_first().ok_or(DecodeError::Truncated)?;
        *buf = rest;
        let bits = u64::from(b & 0x7f);
        if i == 9 && b > 1 {
            return Err(DecodeError::BadVarint);
        }
        v |= bits << (7 * i);
        if b & 0x80 == 0 {
            if i > 0 && b == 0 {
                return Err(DecodeError::BadVarint);
            }
            return Ok(v);
        }
    }
    Err(DecodeError::BadVarint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Composition {
        let mut c = Composition::default();
        c.add(Category::Video, 4_000_000_000);
        c.add(Category::Subtitle, 50_000);
        c.add(Category::Document, 2_000);
        c.add_padding(1_000);
        c
    }

    #[test]
    fn shares() {
        let c = sample();
        assert_eq!(c.total_count(), 3);
        assert_eq!(c.dominant(), Some(Category::Video));
        assert_eq!(c.pct_bytes(Category::Video), 99);
        // Tiny but present is 1%, absent is 0%.
        assert_eq!(c.pct_bytes(Category::Document), 1);
        assert_eq!(c.pct_bytes(Category::Software), 0);
        assert_eq!(c.pct_count(Category::Video), 33);
        let mut only = Composition::default();
        only.add(Category::Audio, 10);
        assert_eq!(only.pct_bytes(Category::Audio), 100);
    }

    #[test]
    fn dominant_by_count_when_empty() {
        let mut c = Composition::default();
        c.add(Category::Image, 0);
        c.add(Category::Image, 0);
        c.add(Category::Video, 0);
        assert_eq!(c.dominant(), Some(Category::Image));
        assert_eq!(Composition::default().dominant(), None);
    }

    #[test]
    fn round_trip() {
        for c in [sample(), Composition::default()] {
            assert_eq!(Composition::decode(&c.encode()), Ok(c));
        }
        assert_eq!(Composition::default().encode(), [COMP_VERSION, 0]);
    }

    #[test]
    fn rejects_what_encode_never_writes() {
        assert_eq!(Composition::decode(&[]), Err(DecodeError::Truncated));
        assert_eq!(Composition::decode(&[9, 0]), Err(DecodeError::Version(9)));
        assert_eq!(
            Composition::decode(&[1, 0x80, 0x10]),
            Err(DecodeError::BadMask)
        );
        assert_eq!(
            Composition::decode(&[1, 0, 0]),
            Err(DecodeError::TrailingBytes)
        );
        assert_eq!(
            Composition::decode(&[1, 1, 0, 5]),
            Err(DecodeError::EmptyEntry)
        );
        assert_eq!(
            Composition::decode(&[1, 0x81, 0x00]),
            Err(DecodeError::BadVarint)
        );
        assert_eq!(Composition::decode(&[1, 1, 1]), Err(DecodeError::Truncated));
    }
}

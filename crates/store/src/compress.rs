//! zstd compression for small blobs, with versioned trained dictionaries.
//!
//! File-tree blobs are a few hundred bytes, far too small for zstd to learn
//! anything from a single blob. A dictionary trained on a sample of real
//! blobs supplies the shared vocabulary (release-name tokens, extensions,
//! common folder names), which is where most of the size saving comes from.
//! Dictionaries are never deleted: every blob records the dictionary it was
//! written with.

use std::collections::HashMap;
use std::io::{self, Read};

use zstd::bulk::{Compressor, Decompressor};
use zstd::zstd_safe::CParameter;

/// How a blob is stored (the `codec` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Codec {
    /// Stored as is: compression wouldn't have made it smaller.
    Raw = 0,
    /// zstd, optionally with the dictionary in the `dict_id` column.
    Zstd = 1,
}

impl Codec {
    pub fn from_id(id: i64) -> Option<Codec> {
        match id {
            0 => Some(Codec::Raw),
            1 => Some(Codec::Zstd),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compressed {
    pub codec: Codec,
    pub dict_id: Option<i64>,
    pub data: Vec<u8>,
}

/// Decompressed blobs larger than this are treated as corrupt.
pub const MAX_DECOMPRESSED: usize = 1 << 30;

/// Compression contexts plus every known dictionary. One per connection;
/// contexts are reused across calls because building one (especially at
/// high levels) costs more than compressing a small blob.
pub struct BlobCodec {
    level: i32,
    dicts: HashMap<i64, Vec<u8>>,
    active: Option<i64>,
    compressor: Option<(Option<i64>, Compressor<'static>)>,
    decompressors: HashMap<Option<i64>, Decompressor<'static>>,
}

impl std::fmt::Debug for BlobCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobCodec")
            .field("level", &self.level)
            .field("dicts", &self.dicts.keys().collect::<Vec<_>>())
            .field("active", &self.active)
            .finish()
    }
}

impl BlobCodec {
    pub fn new(level: i32) -> BlobCodec {
        BlobCodec {
            level,
            dicts: HashMap::new(),
            active: None,
            compressor: None,
            decompressors: HashMap::new(),
        }
    }

    pub fn add_dict(&mut self, id: i64, dict: Vec<u8>) {
        self.dicts.insert(id, dict);
    }

    /// Selects the dictionary new blobs are compressed with.
    pub fn set_active(&mut self, id: Option<i64>) {
        assert!(
            id.is_none_or(|id| self.dicts.contains_key(&id)),
            "unknown dictionary {id:?}"
        );
        self.active = id;
    }

    pub fn active(&self) -> Option<i64> {
        self.active
    }

    pub fn has_dict(&self, id: i64) -> bool {
        self.dicts.contains_key(&id)
    }

    /// A codec with the same dictionaries and level but its own contexts,
    /// for compressing on another thread or machine (the prepare step,
    /// §19.4).
    pub fn fork(&self) -> BlobCodec {
        BlobCodec {
            level: self.level,
            dicts: self.dicts.clone(),
            active: self.active,
            compressor: None,
            decompressors: HashMap::new(),
        }
    }

    pub fn compress(&mut self, raw: &[u8]) -> io::Result<Compressed> {
        let active = self.active;
        if self.compressor.as_ref().is_none_or(|(id, _)| *id != active) {
            let mut c = match active {
                Some(id) => Compressor::with_dictionary(self.level, &self.dicts[&id])?,
                None => Compressor::new(self.level)?,
            };
            // The dict_id column already says which dictionary to use.
            c.set_parameter(CParameter::DictIdFlag(false))?;
            c.set_parameter(CParameter::ChecksumFlag(false))?;
            self.compressor = Some((active, c));
        }
        let (_, c) = self.compressor.as_mut().expect("just set");
        let data = c.compress(raw)?;
        Ok(if data.len() < raw.len() {
            Compressed {
                codec: Codec::Zstd,
                dict_id: active,
                data,
            }
        } else {
            Compressed {
                codec: Codec::Raw,
                dict_id: None,
                data: raw.to_vec(),
            }
        })
    }

    pub fn decompress(
        &mut self,
        codec: Codec,
        dict_id: Option<i64>,
        data: &[u8],
    ) -> io::Result<Vec<u8>> {
        match codec {
            Codec::Raw => Ok(data.to_vec()),
            Codec::Zstd => {
                if !self.decompressors.contains_key(&dict_id) {
                    let d = match dict_id {
                        Some(id) => {
                            let dict = self.dicts.get(&id).ok_or_else(|| {
                                io::Error::new(
                                    io::ErrorKind::NotFound,
                                    format!("zstd dictionary {id} is missing"),
                                )
                            })?;
                            Decompressor::with_dictionary(dict)?
                        }
                        None => Decompressor::new()?,
                    };
                    self.decompressors.insert(dict_id, d);
                }
                self.decompressors
                    .get_mut(&dict_id)
                    .expect("just inserted")
                    .decompress(data, MAX_DECOMPRESSED)
            }
        }
    }
}

impl BlobCodec {
    /// [`decompress`](Self::decompress) for a blob from another machine,
    /// refusing one that holds more than `max` bytes. Read as a stream, so
    /// no more than `max + 1` bytes are ever produced or allocated, however
    /// large the blob claims to be (a worker's blob, WK-9).
    pub fn decompress_capped(
        &self,
        codec: Codec,
        dict_id: Option<i64>,
        data: &[u8],
        max: usize,
    ) -> io::Result<Vec<u8>> {
        let too_large = || io::Error::new(io::ErrorKind::InvalidData, "blob over its cap");
        match codec {
            Codec::Raw if data.len() > max => Err(too_large()),
            Codec::Raw => Ok(data.to_vec()),
            Codec::Zstd => {
                let dict: &[u8] = match dict_id {
                    Some(id) => self.dicts.get(&id).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::NotFound,
                            format!("zstd dictionary {id} is missing"),
                        )
                    })?,
                    None => &[],
                };
                let mut decoder = zstd::stream::read::Decoder::with_dictionary(data, dict)?;
                // The window a frame may ask for: what `max` bytes need, at
                // least 1 MiB (a dictionary widens a small blob's window),
                // never zstd's default 128 MiB.
                let window = usize::BITS - max.saturating_sub(1).leading_zeros();
                decoder.window_log_max(window.clamp(20, 27))?;
                let mut out = Vec::new();
                decoder.take(max as u64 + 1).read_to_end(&mut out)?;
                if out.len() > max {
                    return Err(too_large());
                }
                Ok(out)
            }
        }
    }
}

/// Trains a dictionary from sample blobs. Needs a few hundred samples to be
/// worthwhile; zstd fails outright on too little data.
pub fn train(samples: &[Vec<u8>], max_size: usize) -> io::Result<Vec<u8>> {
    zstd::dict::from_samples(samples, max_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(i: usize) -> Vec<u8> {
        format!(
            "Some.Show.Name.S01E{i:02}.1080p.WEB-DL.DDP5.1.H.264-GROUP.mkv \
             Some.Show.Name.S01E{i:02}.1080p.WEB-DL.DDP5.1.H.264-GROUP.nfo {}",
            i * 7919
        )
        .into_bytes()
    }

    #[test]
    fn roundtrip_without_dict() {
        let mut c = BlobCodec::new(3);
        let raw = sample(1).repeat(4);
        let z = c.compress(&raw).unwrap();
        assert_eq!(z.codec, Codec::Zstd);
        assert_eq!(z.dict_id, None);
        assert!(z.data.len() < raw.len());
        assert_eq!(c.decompress(z.codec, z.dict_id, &z.data).unwrap(), raw);
    }

    #[test]
    fn incompressible_data_is_stored_raw() {
        let mut c = BlobCodec::new(3);
        let z = c.compress(b"ab").unwrap();
        assert_eq!(z.codec, Codec::Raw);
        assert_eq!(c.decompress(z.codec, z.dict_id, &z.data).unwrap(), b"ab");
    }

    #[test]
    fn dictionary_helps_small_blobs() {
        let samples: Vec<_> = (0..1000).map(sample).collect();
        let dict = train(&samples, 16 << 10).unwrap();
        let mut plain = BlobCodec::new(19);
        let mut with_dict = BlobCodec::new(19);
        with_dict.add_dict(7, dict);
        with_dict.set_active(Some(7));
        let raw = sample(1234);
        let a = plain.compress(&raw).unwrap();
        let b = with_dict.compress(&raw).unwrap();
        assert_eq!(b.dict_id, Some(7));
        assert!(
            b.data.len() * 2 < a.data.len(),
            "{} vs {}",
            b.data.len(),
            a.data.len()
        );
        assert_eq!(
            with_dict.decompress(b.codec, b.dict_id, &b.data).unwrap(),
            raw
        );
        // Older blobs stay readable after switching dictionaries.
        with_dict.set_active(None);
        let c = with_dict.compress(&raw).unwrap();
        assert_eq!(
            with_dict.decompress(c.codec, c.dict_id, &c.data).unwrap(),
            raw
        );
        assert_eq!(
            with_dict.decompress(b.codec, b.dict_id, &b.data).unwrap(),
            raw
        );
    }

    #[test]
    fn missing_dictionary_and_garbage_are_errors() {
        let mut c = BlobCodec::new(3);
        assert!(c.decompress(Codec::Zstd, Some(1), b"xxxx").is_err());
        assert!(
            c.decompress(Codec::Zstd, None, b"definitely not zstd")
                .is_err()
        );
    }

    /// A zstd bomb (a few KiB that would expand to 1 GiB) is refused at the
    /// cap, having produced at most the cap and one byte: the decoder is a
    /// stream read through `take`, never sized from the frame's own claim.
    #[test]
    fn a_capped_decompress_refuses_a_bomb_at_its_cap() {
        let mut codec = BlobCodec::new(19);
        let mut bomb = Vec::new();
        {
            // A window the cap allows, so it's the stream cap that stops it.
            let mut enc = zstd::stream::write::Encoder::new(&mut bomb, 19).unwrap();
            enc.window_log(20).unwrap();
            let zeros = vec![0u8; 1 << 20];
            for _ in 0..1024 {
                std::io::Write::write_all(&mut enc, &zeros).unwrap();
            }
            enc.finish().unwrap();
        }
        assert!(bomb.len() < 64 << 10, "{} bytes", bomb.len());
        let start = std::time::Instant::now();
        let err = codec
            .decompress_capped(Codec::Zstd, None, &bomb, 1 << 20)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        // Stopped at the cap: nowhere near the time 1 GiB would take.
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        // Raw blobs are capped too; a known size passes exactly.
        assert!(
            codec
                .decompress_capped(Codec::Raw, None, &[1; 9], 8)
                .is_err()
        );
        assert_eq!(
            codec
                .decompress_capped(Codec::Raw, None, &[1; 8], 8)
                .unwrap()
                .len(),
            8
        );
        let small = codec.compress(&sample(3)).unwrap();
        let out = codec
            .decompress_capped(small.codec, small.dict_id, &small.data, sample(3).len())
            .unwrap();
        assert_eq!(out, sample(3));
        // A dictionary nobody shipped is refused, not guessed.
        assert_eq!(
            codec
                .decompress_capped(Codec::Zstd, Some(42), &small.data, 1 << 20)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    /// A frame that asks for a 128 MiB window (and states no size) is
    /// refused before the window is allocated: the window is capped by
    /// what the blob's own cap needs.
    #[test]
    fn a_capped_decompress_refuses_a_huge_window() {
        let mut frame = Vec::new();
        {
            let mut enc = zstd::stream::write::Encoder::new(&mut frame, 3).unwrap();
            enc.window_log(27).unwrap();
            enc.include_contentsize(false).unwrap();
            std::io::Write::write_all(&mut enc, &sample(1)).unwrap();
            enc.finish().unwrap();
        }
        let codec = BlobCodec::new(3);
        assert!(
            codec
                .decompress_capped(Codec::Zstd, None, &frame, 1 << 20)
                .is_err()
        );
        // The same bytes with a window that fits are read as usual.
        let mut small = Vec::new();
        {
            let mut enc = zstd::stream::write::Encoder::new(&mut small, 3).unwrap();
            enc.window_log(20).unwrap();
            std::io::Write::write_all(&mut enc, &sample(1)).unwrap();
            enc.finish().unwrap();
        }
        assert_eq!(
            codec
                .decompress_capped(Codec::Zstd, None, &small, 1 << 20)
                .unwrap(),
            sample(1)
        );
    }
}

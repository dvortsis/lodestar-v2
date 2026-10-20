//! zstd compression for small blobs, with versioned trained dictionaries.
//!
//! File-tree blobs are a few hundred bytes, far too small for zstd to learn
//! anything from a single blob. A dictionary trained on a sample of real
//! blobs supplies the shared vocabulary (release-name tokens, extensions,
//! common folder names), which is where most of the size saving comes from.
//! Dictionaries are never deleted: every blob records the dictionary it was
//! written with.

use std::collections::HashMap;
use std::io;

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
}

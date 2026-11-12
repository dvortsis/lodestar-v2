//! The known torrents: info dicts drawn from one seed, so every run (and
//! every OS) serves the same names, sizes and file lists.

use std::collections::BTreeMap;

use lodestar_bencode::{OwnedValue, to_vec};
use lodestar_peer_wire::testing::Seed;

/// SplitMix64: tiny, seedable and the same on every platform.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

/// Every known name shares this word, so one query lists them all.
pub const WORD: &str = "starling";

const NOUNS: [&str; 16] = [
    "Aurora", "Basalt", "Cobalt", "Delta", "Ember", "Fjord", "Glacier", "Harbor", "Iris",
    "Juniper", "Kestrel", "Lagoon", "Meadow", "Nimbus", "Orchard", "Prairie",
];

/// One torrent: what a peer seeds and what search must show for it.
#[derive(Debug, Clone)]
pub struct Torrent {
    pub name: String,
    pub size: u64,
    pub files: u64,
    pub seed: Seed,
}

impl Torrent {
    pub fn hash_hex(&self) -> String {
        self.seed
            .info_hash
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// `n` distinct torrents named "Starling <Noun> <n>": single-file ones
/// (`length`) and multi-file ones (`files`), 1 MiB to 4 GiB a file.
pub fn generate(rng: &mut Rng, n: usize, label: &str) -> Vec<Torrent> {
    (0..n)
        .map(|i| {
            let noun = NOUNS[rng.range(0, NOUNS.len() as u64 - 1) as usize];
            let name = format!("Starling {noun} {label} {i:02}");
            let files: Vec<u64> = (0..rng.range(1, 6))
                .map(|_| rng.range(1 << 20, 4 << 30))
                .collect();
            let info = info_dict(&name, &files);
            Torrent {
                size: files.iter().sum(),
                files: files.len() as u64,
                name,
                seed: Seed::v1(info),
            }
        })
        .collect()
}

fn info_dict(name: &str, files: &[u64]) -> Vec<u8> {
    let mut d = BTreeMap::new();
    if let [only] = files {
        d.insert(b"length".to_vec(), OwnedValue::Int(*only as i64));
    } else {
        let list = files
            .iter()
            .enumerate()
            .map(|(i, len)| {
                let mut f = BTreeMap::new();
                f.insert(b"length".to_vec(), OwnedValue::Int(*len as i64));
                f.insert(
                    b"path".to_vec(),
                    OwnedValue::List(vec![OwnedValue::from(format!("part{i}.mkv").as_str())]),
                );
                OwnedValue::Dict(f)
            })
            .collect();
        d.insert(b"files".to_vec(), OwnedValue::List(list));
    }
    d.insert(b"name".to_vec(), OwnedValue::from(name));
    d.insert(b"piece length".to_vec(), OwnedValue::Int(1 << 22));
    // One SHA-1 per piece would be honest; nothing here checks pieces.
    d.insert(b"pieces".to_vec(), OwnedValue::Bytes(vec![0; 20]));
    to_vec(&OwnedValue::Dict(d))
}

#[test]
fn the_seed_fixes_the_torrents() {
    let a = generate(&mut Rng::new(7), 4, "x");
    let b = generate(&mut Rng::new(7), 4, "x");
    let key = |t: &[Torrent]| -> Vec<_> {
        t.iter()
            .map(|t| (t.name.clone(), t.size, t.files, t.seed.info_hash))
            .collect()
    };
    assert_eq!(key(&a), key(&b));
    assert_ne!(key(&a), key(&generate(&mut Rng::new(8), 4, "x")));
    assert!(a.iter().all(|t| t.name.to_lowercase().contains(WORD)));
}

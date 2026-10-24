# Lodestar v0.5 code review: Area 1 results

Date: 2026-10-24

Review instructions: [codex-review-v0.5.md](codex-review-v0.5.md).

Reviewed a fresh clone of `main` at `2d5e761d30cb1b7947fb09493959668eabcb84ab`, newer than the instructions' v0.5 gate commit `4667277`. Locations below refer to the reviewed commit. This session covered **Area 1: untrusted-input parsers** only, following the one-area-per-session instruction. Areas 2 and 3 remain unreviewed.

No source files were changed and no GitHub actions were taken. This report was subsequently saved at the owner's request.

Validation:

- Read all 11 scoped source files, relevant unit/property/fuzz tests, and the pipeline and DHT entry points for reported cases.
- `cargo test --locked --offline -p lodestar-bencode -p lodestar-metainfo -p lodestar-dht` passed **121 tests**, including one doctest.
- Five isolated `rustc --test` reproductions, supplied through stdin and linked against the compiled crates, failed as expected. They confirmed all five findings below without adding test files to the checkout.
- Existing tests do not specifically cover these five cases. Intentional warning-only behavior for piece-count mismatches, suspicious paths and inconsistent hybrids was excluded from findings.

### 1. Mixed path overrides discard valid UTF-8 filenames

Severity: medium  
Location: `crates/metainfo/src/info.rs:343`  
Input / scenario: Peer metadata contains two files: one with `path=["wrong.txt"]` and `path.utf-8=["correct.txt"]`; another with only `path=["other.txt"]`. Both have length 1, with piece length 16384 and one piece hash.  
Expected: Architecture §6 requires preferring `path.utf-8` when present.  
Actual: `drafts.iter().all(|d| d.path_utf8.is_some())` returns false. Lines 371–373 then select `_ => d.path` for every file. The reproduction returned `wrong.txt`; the pipeline passes that incorrect path into classification and storage.  
Confidence: high  
Suggested fix: Select each file's valid UTF-8 override independently, preserving the encoding information needed for remaining legacy paths.

### 2. Valid symlinks without a length reject the entire torrent

Severity: medium  
Location: `crates/metainfo/src/info.rs:537`  
Input / scenario: A v1 multifile info dictionary contains a one-byte `target` and `{attr:"l", path:["link"], "symlink path":["target"]}`, with no length on the link. Piece length is 16384 with one piece hash.  
Expected: BEP 47 permits omitting symlink length; its effective length is zero. [BEP 47](https://www.bittorrent.org/beps/bep_0047.html)  
Actual: `get_u64(d, length_key)?.ok_or(Error::MissingField("length"))?` executes before reading attributes. The reproduction returned `MissingField("length")`; `pipeline::worker::prepare_one` consequently rejects the fetched metadata as invalid.  
Confidence: high  
Suggested fix: Read attributes first and default an absent symlink length to zero.

### 3. Valid padding files without paths reject the entire torrent

Severity: medium  
Location: `crates/metainfo/src/info.rs:509`  
Input / scenario: A v1 multifile torrent contains a one-byte `target` followed by `{attr:"p", length:16383}`, with piece length 16384 and one piece hash.  
Expected: BEP 47 permits padding entries without a path. [BEP 47](https://www.bittorrent.org/beps/bep_0047.html)  
Actual: `f.get(b"path").ok_or(Error::MissingField("path"))?` requires a path regardless of the already-parsed padding attribute. The reproduction returned `MissingField("path")`, causing the pipeline to discard the entire torrent.  
Confidence: high  
Suggested fix: Accept absent paths for padding entries and assign a deterministic internal path compatible with the nonempty-path model invariant.

### 4. V2 parsing silently discards symlink targets

Severity: medium  
Location: `crates/metainfo/src/info.rs:704`  
Input / scenario: A v2 file tree contains `link → "" → {attr:"l", length:0, "symlink path":["target"]}` alongside a nonempty `target` file with a valid-sized pieces root.  
Expected: Architecture §6 requires capturing file attributes and symlink targets.  
Actual: The parser preserves the symlink attribute but unconditionally assigns `symlink_target: None`. The reproduction confirmed the missing target. Storage serializes this incomplete file record, losing the relationship.  
Confidence: high  
Suggested fix: Parse v2 symlink targets with the existing bounded path parser.

### 5. Empty sampling responses omit the BEP 51 capability field

Severity: low  
Location: `crates/dht/src/krpc.rs:506`  
Input / scenario: Send `sample_infohashes` to a newly started node before it has learned any hashes.  
Expected: BEP 51 requires including `samples` even when empty so callers can recognize extension support. [BEP 51](https://www.bittorrent.org/beps/bep_0051.html)  
Actual: The query handler constructs an empty sample response, but `if !r.samples.is_empty()` suppresses the field. The reproduction emitted `interval` and `num:0` without `samples`, so callers using the specified capability check cannot recognize support.  
Confidence: high  
Suggested fix: Represent sample-field presence separately from its length and emit `samples:0:` for empty sampling replies.

Checked and clean: `crates/bencode/src/decode.rs`, `crates/bencode/src/value.rs`, `crates/bencode/src/encode.rs`, `crates/metainfo/src/torrent.rs`, `crates/metainfo/src/hash.rs`, `crates/metainfo/src/text.rs`, `crates/dht/src/compact.rs`, `crates/dht/src/bep33.rs`, `crates/dht/src/secure_id.rs`.

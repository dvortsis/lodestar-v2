# F-metainfo decisions

Fixes for findings 1–4 of the v0.5 parser review (`docs/code-review/codex-review-v0.5-area-1-results.md`), in `crates/metainfo` (architecture §6, BEP 47, BEP 52). The model still has one path encoding per torrent (`Info::path_encoding`), which the store's file-tree blob keeps as one byte, so nothing outside the crate changes shape. Items marked **(§2.19)** refine an owner decision and are for the runner to flag.

## `path.utf-8` per file (finding 1)

- **(§2.19) Each file uses its own valid `path.utf-8`.** Before, one file without it made every file fall back to `path`. §6 says "use `path.utf-8` when present", so it now applies file by file.
- **Detection only sees the legacy text that's left:** paths without a valid override, every symlink target and the name when it has no `name.utf-8`. A file's ignored legacy `path` no longer sways the guess. Symlink targets weren't fed to detection before, though they're stored in the same encoding.
- **When UTF-8 and legacy paths mix, the raw legacy bytes are kept where possible** (§6: "the raw bytes are kept so decoding can be redone"):
  1. If the legacy bytes are themselves valid UTF-8, everything is stored as is, as UTF-8.
  2. Otherwise the UTF-8 paths are re-encoded into the detected encoding, if every one of them can be written in it losslessly (encode, then decode back to the same text). The legacy bytes stay untouched.
  3. Otherwise (for example a Hangul `path.utf-8` beside GBK paths) the legacy paths and targets are decoded to UTF-8 and the torrent is stored as UTF-8. The right file name wins over keeping those raw bytes. This case needs an uploader whose client wrote a legacy charset that can't hold its own file names, which is rare.

## Symlinks (findings 2 and 4)

- **A symlink may omit `length`; it's then 0** (BEP 47). Attributes are read before the length, in v1 lists and v2 trees alike. Any other file still needs `length`, and an explicit symlink length is taken as given.
- **v2 symlink targets are kept.** `symlink path` goes through the same bounded path parser as v1 (depth and byte limits). A malformed or over-limit target is dropped and the link kept, as v1 already did.

## Padding without a path (finding 3)

- **(§2.19) A padding file with no `path`, or an empty one, gets the path `.pad/padding`.** BEP 47 lets padding omit its path, but every stored file needs a non-empty path. `.pad/` is the usual padding folder, so the name is recognisable and deterministic, and it never collides with content because anything under `.pad/` is already treated as padding. Padding stays out of file counts, sizes and search as before. A file without a path that isn't padding is still rejected.

## Tests and fuzzing

- **Golden corpus:** `crates/metainfo/tests/golden/`, raw info dictionaries (the review's inputs plus the encoding and hybrid variants above) with their expected parse.
- **Property tests** (`tests/bep47_props.rs`) mix overrides, GBK legacy paths and targets, symlinks with and without lengths and padding with and without paths, then round-trip the result through the store's file-tree codec. `lodestar-store` is a dev-dependency of `lodestar-metainfo` for this; only integration tests use it.
- **Fuzz seeds** live in `fuzz/seeds/<target>/` (committed), since `fuzz/corpus/` is ignored. CI's fuzz job doesn't read them yet: it needs `fuzz/seeds/${{ matrix.target }}` added after the target name in `.github/workflows/ci.yml`, which this slice doesn't own.

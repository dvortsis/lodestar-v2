# F-metainfo decisions

Fixes for findings 1–4 of the v0.5 parser review (`docs/code-review/codex-review-v0.5-area-1-results.md` on the `fix/dht-cr0` branch), in `crates/metainfo` (architecture §6, BEP 47, BEP 52). The model still has one path encoding per torrent (`Info::path_encoding`), which the store's file-tree blob keeps as one byte, so nothing outside the crate changes shape. Items marked **(§2.19)** refine an owner decision and are for the runner to flag.

## `path.utf-8` per file (finding 1)

- **(§2.19) Each file uses its own valid `path.utf-8`.** Before, one file without it made every file fall back to `path`. §6 says "use `path.utf-8` when present", so it now applies file by file.
- **Which legacy text shares the encoding:** paths without a valid override, the name when it has no `name.utf-8`, and v1 or hybrid symlink targets, which are stored in the same encoding but weren't fed to detection before. Pure v2 targets are left out, because BEP 52 strings are UTF-8 and one bad target shouldn't change how every name decodes. If all of that is valid UTF-8, the torrent is UTF-8.
- **Files that have both `path` and `path.utf-8` check the guess.** Their legacy bytes still feed detection, since the same client wrote them. They're also ground truth: if another legacy encoding decodes more of those paths to exactly their `path.utf-8` than the guess (or the `.torrent`'s `encoding` hint) does, that encoding wins. At most 16 such files are compared. This matters because chardetng misreads short CJK names (§16), and a wrong guess would otherwise push a torrent into case 3 below.
- **When UTF-8 and legacy paths mix, the raw legacy bytes are kept where possible** (§6: "the raw bytes are kept so decoding can be redone"):
  1. If the legacy bytes are themselves valid UTF-8, everything is stored as is, as UTF-8.
  2. Otherwise the UTF-8 paths are re-encoded into the detected encoding, if every one of them can be written in it losslessly (encode, then decode back to the same text). The legacy bytes stay untouched.
  3. **(§2.19)** Otherwise (for example a Hangul `path.utf-8` beside GBK paths) the legacy paths and targets are decoded to UTF-8 and the torrent is stored as UTF-8. The right file name wins over keeping those raw bytes. This **loosens §6 and §16 ("the raw bytes are always kept")** for these torrents: the legacy bytes can't be re-decoded later. It happens when the client wrote a charset that can't hold its own names, or when the guess is wrong and no file with both paths corrects it. Keeping both for good needs a per-file encoding in the store's file-tree blob, which this slice doesn't own; the info dictionary archive (§3, opt-in) still has the bytes.

## Symlinks (findings 2 and 4)

- **A symlink may omit `length`; it's then 0** (BEP 47). Attributes are read before the length, in v1 lists and v2 trees alike. Any other file still needs `length`, and an explicit symlink length is taken as given.
- **v2 symlink targets are kept.** `symlink path` goes through the same bounded path parser as v1 (depth and byte limits). A malformed or over-limit target is dropped and the link kept, as v1 already did.

## Padding without a path (finding 3)

- **(§2.19) A padding file with no `path`, or an empty one, gets the path `.pad/padding`,** treated as legacy text (it's ASCII, so the same bytes in every encoding a torrent can be judged to be in, apart from UTF-16). BEP 47 lets padding omit its path, but every stored file needs a non-empty path. `.pad/` is the usual padding folder, so the name is recognisable and deterministic, and it never collides with content because anything under `.pad/` is already treated as padding. Padding stays out of file counts, sizes and search as before. A file without a path that isn't padding is still rejected.

## Tests and fuzzing

- **Golden corpus:** `crates/metainfo/tests/golden/`, raw info dictionaries (the review's inputs plus the encoding and hybrid variants above) with their expected parse.
- **Property tests** (`tests/bep47_props.rs`) mix overrides, GBK legacy paths and targets, symlinks with and without lengths and padding with and without paths, then round-trip the result through the store's file-tree codec. `lodestar-store` is a dev-dependency of `lodestar-metainfo` for this; only integration tests use it.
- **Fuzz seeds** live in `fuzz/seeds/<target>/` (committed), since `fuzz/corpus/` is ignored. CI's fuzz step passes the corpus and the seeds folder when a target has one, and runs as before when it doesn't (the runner extended this slice's ownership to that one line of `.github/workflows/ci.yml`).

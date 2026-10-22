# C-adult-other: Adult detection for extensionless and Ebook-typed torrents
**Model:** Opus (content safety, classifier). **Branch:** `slice/C-adult-other`.

**Why:** the R-relevance review (#94) found a content-safety false negative on `main`. A torrent whose name has no recognised file extension gets base type `Other`, and `adult::detect` (`crates/classify/src/adult.rs`, the `match base` in `detect`) returns `None` for `Other` and `Ebook` even at full score, so the torrent shows in default search while Adult is hidden. Reproduced on main (`classify(name, &[FileInfo::new(vec![name], 1 GiB)])`):
```
Lantern.Bay.Parody.XXX.1080p.MP4-KSTRL        -> Other conf 40 kind None
Lantern.Bay.Parody.XXX.2020.1080p.MP4-KSTRL   -> Other conf 40 kind None
Lantern.Bay.Parody.XXX.1080p.MP4-KSTRL.mp4    -> Adult conf 90 kind Some(Movie)
Lantern.Bay.Parody.XXX.720p                   -> Other conf 40 kind None
Lantern.Bay.Parody.XXX                        -> Other conf 40 kind None
```

**Spec:**
- `runner/standards.md`; `docs/owner-decisions.md` (Adult hidden by default, §20.9; "detection is cautious" is about keeping mainstream titles out of Adult; fixing a full-confidence false negative doesn't loosen it: record it as a refinement under §2.19).
- Architecture §20.9 (Adult), the classify sections, and the §16 entries for B17 and M3 classify.
- The code: `crates/classify/src/{adult.rs,lib.rs}` and its tests and golden corpus.

## Own
- `crates/classify/**` (the detect change, fixtures, golden-corpus additions).
- `docs/decisions/<date>-C-adult-other.md`.
- **Don't edit** other crates. If the index needs a reindex for existing torrents to pick up the new classification, say how it happens today (enrichment re-run, `reindex_log`) in the PR; don't add a migration.

## Build
1. In `detect`, give `Other` the nearest kind (`forced_kind(Other)` = Scene) and `Ebook` Comics, the same mapping an Is-adult correction already uses, **when the score meets the threshold**. Consider whether `Other` and `Ebook` need a stronger signal than the default threshold to avoid false positives on mainstream archives and books; decide with evidence from the existing corpus and state the rule.
2. Keep VR handling consistent.
3. Check every other path that decides Adult (file-level signals, archive-only torrents, multi-file torrents whose largest file has no extension) for the same gap and fix any you find.

## Tests
- Fixtures: the five names above (extensionless, with and without year, `.720p`, bare), an archive-only adult torrent (`.rar`/`.zip`), an adult ebook/comic name, and mainstream controls typed Other and Ebook that must **not** become Adult (software archives, a novel, a documentary with "sex" in a non-adult sense, etc.).
- The golden corpus and property tests still pass; report any golden changes one by one with the reason.

## Done when
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` pass after merging `origin/main`; `fuzz (release)` target builds.
- A PR titled `C-adult-other: Detect Adult for extensionless and Ebook torrents`, with the rule, the before/after table for the fixtures, golden changes, and the §2.19 refinement note.

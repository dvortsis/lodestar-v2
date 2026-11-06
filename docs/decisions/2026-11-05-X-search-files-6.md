# X-search-files-6 decisions

§3's "File-level query joined to torrents < 150 ms" row at 10M, continued from X-search-files-5 (#505). This slice closes two of the three clauses #505 left (`file:(ext:mkv,mp4,avi -path:sample)` and `file:(size:>100MB)`) under the runner's design call (§2.26, 2026-11-05), and reports a design for the third (`file:mkv`), which needs a schema bump the brief doesn't allow without the runner. The gate's query mix and numbers are unchanged. **At 10M the gate is met on this machine: cold p95 93.6 ms (gate 150), warm p95 29.5 ms (gate 30), from 116.8 and 46.3 ms on `main` on the same store and machine.** The warm margin is half a millisecond, and `file:mkv` is now the slowest clause.

## Extensions less rare words (brief item 1)
- **The shape (`compile::negated_words`):** a `file:(…)` clause of one extension atom (`ext:a,b`, or `ext:a | ext:b`) and one or more negated words not on the title (`-path:sample`, `-sample`, `-name:x`), in any order. Other shapes stay as they were.
- **The answer:** a torrent matches when one of its covered files has an extension and none of the words. So it's the **candidates**, the torrents with any `{ext}:{code}` term (`file_ext_bucket`, which follows smart coverage exactly, #505), less the **bad** torrents: those with a covered file of the extensions that has a word (the suspects) and none without. On the torrent index that's `file_ext_bucket` matching `(a|b):[0-9]+` and not `id` in the bad set (`compile::negated_query`), scored as #505's bucket clauses are (`torrent_level`).
- **The bad set (`files::bad_torrents`):** the file pass computes it in place of the whole clause.
  - **Suspects** come from each segment's files with an extension and a word. Tantivy's intersection is led by the words' postings, so it reads about as many files as the words have, not the extensions'.
  - **Rescue by neighbours:** a torrent's files are normally adjacent documents (added together), so each suspect torrent's run of documents around its suspect file is checked for a file with an extension and no word, seeking that clause forward only (`files::undecided_in`). In the synthetic corpus 60% of movie folders (about 4.5% of all torrents, ~450k at 10M) have a `Sample/` file, and nearly all of them have the film beside it, so almost every suspect is rescued here without a lookup.
  - **The exact check:** the suspects the neighbours don't rescue (cached per segment, `{clause}@undecided`) are checked on all of their files wherever they sit (the file index's `torrent` terms, `TermSetQuery`), so the answer never depends on document order or on which segment a file landed in. What's left is the bad set.
  - The brief asked for the second step to run on the file index restricted to the suspects' ids. It does, for the undecided ones; looking up all ~450k suspects by id first would read their files through 450k `torrent` terms on every cold query (not measured at 10M: it was judged from the counts before the build), so the neighbour pass decides most of them first. Same answer, by construction and by test.
- **The threshold (`files::NEGATED_RATIO`, 8):** the words' path runs only when their terms' documents (summed over the target fields and segments, deleted ones included, every token of the word) are at most an eighth of the extensions'. Otherwise the clause runs on the file index whole, as before. Measured at 10M (cold best, the clause's torrents identical on both paths): `-path:sample` and `-sample` (over `ext:mkv,mp4,avi`) and `-path:extras` (over `ext:mkv`) take the words' path, at 84–97 and 45 ms; `-path:subs`, `-1080p`, `-720p`, `-hevc` and `ext:srt -en` are over the ratio and run whole, at 102–122 ms (38 ms for `srt`), as on `main`. On `main`, `-path:sample` measured 133.7 ms cold. Warm, both paths take 21–30 ms, the cached sets being small either way. Only one ratio (8) was measured; a lower one would send fewer words through their postings.
- **Population:** the path needs `file_ext_bucket` Ready in the snapshot (`bucket_terms_ready`), as #505's bucket clauses do; while it isn't, the clause runs on the file index whole.
- **The lag exception (#505) extends to this shape:** while the file index lags the torrent index (a crash between their commits, until the catch-up), the candidates come from the torrent's terms and the bad set from the file index, so a torrent whose files aren't indexed yet is found, as a bucket clause finds it. `tests/index.rs` `an_index_left_behind_catches_up_alone` now says so, and keeps a file-index shape (`file:(ext:flac size:>0 -zzqxnever)`) for "nothing until the catch-up".
- **Tests:**
  - `tests/files.rs` `negated_words_answer_as_the_file_index_does`: the same torrents, order and scores as the file index for a torrent whose only video is a sample, one with both kinds, `sample` in the name rather than a directory, a compound word (`web-dl`), an absent word, a word too common for its postings (`common`, which takes the fallback), two words, `ext:a | ext:b`, the words first, beside a word, a range or another file clause, in an alternative and negated, over a pack whose small images coverage leaves out and a hidden video. It also checks which path each word takes. It fails if the rescue is skipped.
  - `files::negated_props::bad_torrents_are_exact` (property test): with a torrent's files in any order across documents and segments and one torrent deleted, the bad set is exactly the brute-force one, cold and from the cache. It fails if the exact check is skipped.
  - #505's `bucket_terms_answer_as_the_file_index_does` forced its reference onto the file index with a never-matching exclusion, which is now this shape; it adds an always-matching atom (`Node::All`) instead, the same files.

## Finer size codes (brief item 2)
- **The floors (`files::code_floors`):** `size_bucket`'s floors (each power of two and 1.5 times it), plus 5, 10, 20, 50, 100, 200 and 500 of each binary unit (B, KB, MB, GB, TB, PB, EB; 1 and 2 of a unit are powers of two already). A file's code is its floor's index twice, plus one above the floor, as #505's split. So a decimal-looking bound such as `size:>100MB` (100 × 2^20) or `size:<=500MB` is whole codes and reads no file; 1,000 MB, 700 MB and other values still have edges, checked in the file index.
- **Why this split:** measured on 200,000 synthetic torrents, it adds about 10% to the bucket terms (`file_bucket` 5.15 → 5.67 terms a torrent, `file_ext_bucket` 6.91 → 7.74). Eight sub-codes per power of two would double them (10.35 and 14.81) and still leave 100 MB off a floor. The file index's `size_bucket` is unchanged.
- **Population, not schema:** the codes are what `file_bucket` and `file_ext_bucket` hold, so both move to generation 2 in `population::GENERATIONS` (SB-6's contract): an index built by #505 backfills them from the store, resumably, and until the walk completes every bucket clause and the negated-word path run on the file index (#505's `bucket_terms_ready`), so results stay true. No `SCHEMA_VERSION` bump. **Upgrade cost for an existing v7 index:** the backfill re-indexes every torrent in the background while the index serves, about the same work as #505's rebuild (the branch's index built at 2,476 torrents a second here, about 67 min for 10M on 4 cores).
- **Tests:** `codes_are_ordered_and_hold_their_sizes` (property: every size has one code, a larger size never a smaller one, and its code's range holds it), `a_code_plan_covers_exactly_its_range` (property, now over the new floors too: a range's whole codes and edges cover exactly the range, edges within one code and never a whole one, at most two), and `floors_split_ranges_into_whole_codes` (binary and decimal floors, an edge between floors). `tests/files.rs` probes every code floor too, and its differential test adds ranges at, beside and between decimal floors.

## `file:mkv`, a word that's also an extension (brief item 3): not built, a design for the runner
- **What the tokenizer emits:** every file name with an extension emits the extension as a token (`a.mkv` → `a`, `mkv`; `x.mp4` → `mp4` whole and `mp`, `4` parts; `F.WEB-DL.mkv` → … `mkv`). So `file:mkv` is the ext-mkv torrents (from `file_ext_bucket`, exact) ∪ the torrents with `mkv` in a file whose extension isn't `mkv`, or in a directory.
- **Why it isn't closed here:** the second part's postings are `name:mkv` and `dir:mkv` on the file index, and `name:mkv` holds every `.mkv` file: reaching the few others means walking them all, which is the 166 ms. No existing field leaves the extension token out.
- **Design A (recommended, v8):** index a file's name without its extension token in the file index (the token stays in `ext`), and answer a word that is a valid extension as `ext` candidates ∪ the rest. The file index shrinks by about one posting a file (not measured; at most one posting a covered file, about 30 per torrent at 10M, saved from `name`); `file:mkv` would cost about a lone extension's time plus the rest's postings (`file:(ext:mkv)` measures 11.3 ms cold at 10M). Phrases ending in an extension (`file:"sample mkv"`) need rewriting to keep matching. A `SCHEMA_VERSION` bump to v8, acceptable only before the v0.5.2 cut.
- **Design B (v8):** a torrent-index marker term on torrents whose `paths` differ from their file documents' words (paths truncated at `MAX_PATH_TOKENS`, or files coverage leaves out), so an unfielded `file:word` is `paths:word` off the marked torrents, plus the file index for the marked ones. Under 1 B a torrent on the torrent index, but the marked torrents' part still needs a file-index field to avoid the walk, and `paths` holds a single-file torrent's name. More moving parts than A.

## Measured at 10M
- **Method:** #505's. One 10M corpus built in this container (4 cores, 15 GiB) with `main` at a7778e71 and benched with `search-bench --torrents 10000000 --only "broad file"`. Then the index was deleted and rebuilt from the same store with the branch, and benched the same way. Cold means the file-set cache was emptied before each query; the OS page cache stayed warm. **This machine ran faster than #505's.** `main` measured cold p95 116.8 ms here, where #505 measured 228.4 ms for the same code, so `main` already met the cold gate here. Compare within this table, not across PRs. The branch's run stored the bench's 10 NRT torrents a second time, and they already existed (the generator's seed is fixed, so they're the same torrents), so its final NRT check reported "never indexed". That comes from reusing the store, not from this change: `main`'s run on the same store passed it.
- **The mix (ms):**

  | | cold p50 | cold p95 | warm p50 | warm p95 | cold, 4 at once p95 |
  |---|---:|---:|---:|---:|---:|
  | `main` | 14.48 | 116.83 | 9.84 | 46.31 | 456.02 |
  | branch | 14.07 | 93.57 | 11.13 | 29.48 | 279.00 |

- **Per clause, cold, alone (ms, best / median of three):**

  | clause | `main` | branch |
  |---|---:|---:|
  | `file:mkv` | 90.46 / 106.14 | 95.31 / 96.27 |
  | `file:(ext:mkv)` | 12.29 / 12.64 | 11.29 / 11.42 |
  | `file:(ext:srt)` | 1.82 / 1.84 | 2.12 / 2.23 |
  | `file:(ext:flac,mp3)` | 3.69 / 3.84 | 4.45 / 4.52 |
  | `file:(ext:jpg)` | 3.18 / 3.19 | 3.69 / 3.70 |
  | `file:(size:>100MB)` | 92.41 / 94.25 | 20.88 / 21.48 |
  | `file:(ext:mkv,mp4,avi -path:sample)` | 133.73 / 147.92 | 86.75 / 88.55 |
  | `file:(size:>1GB)` | 18.25 / 18.47 | 17.99 / 18.33 |
  | `file:(ext:mkv size:>1GB)` | 15.97 / 16.45 | 16.33 / 16.38 |

  Torrent counts are identical in every row, apart from the store's 7–10 NRT torrents (`main` 6,125,693 for `file:mkv`, the branch 6,125,700). Rows whose path didn't change differ only by noise. Another decimal-looking bound that isn't a floor, `file:(size:>700MB)`, still has an edge: 120.5 ms cold.
- **Index bytes per torrent (§2.26, flagged to the owner):** the torrent index went from 3,912,089,252 to 3,924,395,752 bytes, **391.2 → 392.4 B a torrent** (the runner's ceiling is 420, §3's is 512). The file index is unchanged at 757.3 B. By field (Tantivy's space usage):

  | field | `main` B/torrent | branch B/torrent |
  |---|---:|---:|
  | `file_bucket` | 3.98 | 4.48 |
  | `file_ext_bucket` | 6.72 | 7.65 |

## What's left
- **Nothing for the gate on this machine,** but the warm p95 (29.5 ms) is within half a millisecond of its gate, and a slower machine (#505's measured `main` at twice this one's cold p95) may miss it.
- **`file:mkv`** (95 ms cold, about 26 ms warm) is now the slowest clause of the mix. Closing it is design A or B above, a v8 bump: the runner's call.
- **Decimal bounds off the floors** (`size:>700MB`, 120 ms) still check their edge code file by file. They're not in the gate's mix.

## Shared files outside the brief's owns
- `crates/search/src/index.rs`: the file pass computes a negated-word clause's bad set before falling back to the whole clause (one block in `compile_at`).
- `crates/search/tests/index.rs`: `an_index_left_behind_catches_up_alone`, as above.

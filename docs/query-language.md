# The query language

Type into the search box to find torrents. A plain search works like any other: `dark knight` finds torrents with both words. Everything below is optional, for when you want to be more exact.

This page describes what Lodestar does today. Where a feature is planned but not built yet, it says so. Every example here is checked by a test that runs it through the real parser (`docs/query-language-examples.txt`).

- [The basics](#the-basics)
- [Operators](#operators)
- [Fields](#fields)
- [Sizes, counts and shares](#sizes-counts-and-shares)
- [Dates](#dates)
- [Files inside torrents](#files-inside-torrents)
- [Pasting a hash or magnet link](#pasting-a-hash-or-magnet-link)
- [How results are ranked](#how-results-are-ranked)
- [Plurals fallback](#plurals-fallback)
- [Counts, pages and "reload"](#counts-pages-and-reload)
- [Errors](#errors)
- [Hidden torrents and Adult](#hidden-torrents-and-adult)
- [Not available yet](#not-available-yet)
- [Limits](#limits)

## The basics

- Words are joined with AND: `dark knight` needs both.
- Case, accents and punctuation don't matter. `Dark.Knight`, `dark_knight` and `DARK KNIGHT` all match the same torrents.
- A field narrows the search to one kind of fact: `res:2160p`, `size:>4GB`. A field name is lowercase and ends in `:`, with no space before the value.
- An empty search lists everything (browse), in the sort order you pick.

```
dark knight res:2160p hdr:dv size:>20GB
"dark knight" -cam year:2008 type:movie
```

## Operators

| You type | Means |
|---|---|
| `dark knight` | both words (AND is implied) |
| `dark AND knight` | the same, written out |
| `batman \| joker`, `batman OR joker` | either |
| `-cam`, `!cam` | leave out torrents that match `cam` |
| `(batman \| joker) AND 2019` | grouping |
| `"dark knight"` | the exact phrase, words side by side |
| `dark NEAR/5 knight` | the words within 5 words of each other, in either order |
| `comput*` | any word starting with `comput` (at least 2 letters before the `*`) |
| `~knigth` | a word close to this one: 1 typo for words under 6 letters, up to 2 from 6 on (at least 3 letters) |

Details that trip people up:

- **OR binds loosest.** `a b | c` means `(a b) | c`. Use brackets when in doubt: `a (b | c)`.
- **`-` and `!` negate only when a term follows.** `WEB-DL` is one word, and a `-` on its own or before `)` is just a character.
- **There is no `NOT` keyword.** Only `-` and `!` negate; `dark NOT cam` searches for the three words `dark`, `NOT` and `cam`. (`AND`, `OR` and `NEAR` must be capitals to count as operators.)
- **NEAR** joins words and phrases, all in the same field, with one distance per chain: `a NEAR/3 b NEAR/3 c`. Bare `NEAR` means 5. The distance is 1–50, and a chain holds up to 16 words.
- **Phrases** are checked against the name. Names keep word positions; folder and file paths inside torrents don't, so `path:"a b"` means "both words in the path", not "side by side". To match a phrase inside a file name, use `file:"…"`.
- Negation is exact. `-cam` only removes torrents that really contain `cam`, even when the plurals fallback widened the rest of the search.

Examples that parse:

```
batman | joker
(batman | joker) AND 2019
dark -cam
-(cam | ts)
dark NEAR/5 knight
dark NEAR knight NEAR/5 rises
comput*
~knigth
```

## Fields

Add a field name and a colon. Several values in one field, separated by commas, mean "any of": `type:movie,tv`.

### Words

| Field | Searches | Example |
|---|---|---|
| (none) | the name and the paths of the files inside | `dark knight` |
| `name:` | the torrent's name only | `name:"dark knight"` |
| `title:` | the resolved title, which is what Lodestar worked out the release is, including alternative titles | `title:joker` |
| `path:` | folder and file names inside the torrent | `path:extras` |

`name:`, `title:` and `path:` take a word, a `word*` prefix, a `~fuzzy` word, or a quoted phrase.

### Facts about the release

These are read from the name and the files, then stored in a normal form. Aliases work: `codec:x265` finds the same as `codec:h265`, and `res:4k` is `res:2160p`.

| Field | Values | Examples |
|---|---|---|
| `type:` | `movie`, `tv`, `music`, `audiobook`, `ebook`, `comic`, `software`, `game`, `images`, `video`, `other`, `adult` | `type:movie,tv` |
| `res:` | a resolution: `2160p`, `1080p`, `4k`, or just `1080` | `res:2160p` |
| `hdr:` | `dv`, `hdr10`, … as the release parser reads them | `hdr:dv` |
| `codec:` | video codec: `x265`, `h265`, `x264`, `av1`, … | `codec:x265` |
| `audio:` | audio codec: `atmos`, `flac`, `dts`, … | `audio:atmos` |
| `lang:` | a language, by name or two-letter code | `lang:fr` |
| `group:` | the release group, as written | `group:ntb` |
| `has:` | `bluray`, `dvd`, `cue`, `sfv`, `par2`, `multipart`, `steam`, `repack`, `sample`, `subs`, `proof`, `nfo`, `comics`, `photos` | `has:subs,cue` |
| `resolved:` | how the title was found: `files`, `folder`, `structure` | `resolved:files` |
| `adult:` | `scene`, `movie`, `photos`, `comics`, `game`, `vr`, `audio` | `adult:vr` (see [Adult](#hidden-torrents-and-adult)) |
| `year:` | a year or range; a pack that spans years matches any of them | `year:2019..2021` |
| `s:` | a season number; an `S01-S03` pack matches `s:2` | `s:2` |
| `e:` | an episode number, including ranges and absolute numbers | `e:5` |
| `junk:` | the junk score, 0–100 | `junk:<20` |
| `ext:` | file extensions inside the torrent, with or without the dot | `ext:mkv,mp4` |

A value Lodestar doesn't know for `type:`, `has:`, `resolved:` or `adult:` is an error that lists the valid ones.

### Yes/no fields

Write `yes`, `no`, `true`, `false`, `1` or `0`.

| Field | Meaning |
|---|---|
| `private:` | the torrent is private (no DHT) |
| `v2:` | BitTorrent v2 metadata |
| `hybrid:` | v1 and v2 together |
| `imported:` | came from an import, not from the DHT |
| `backfill:` | found while backfilling |
| `partial:` | only some of its files were indexed |
| `ambiguous:` | Lodestar couldn't tell which of several titles it is |
| `hidden:` | hidden by a filter rule (see [Hidden torrents](#hidden-torrents-and-adult)) |
| `demoted:` | pushed down by a filter rule |
| `flagged:` | flagged by a filter rule |

### Excluding with `!` and `-`

`!ext:exe` and `-ext:exe` mean the same: leave out torrents with an `.exe` file. This works for any field.

### Aliases

`age:` is another name for `discovered:` (`age:<6h`). `s:` and `e:` are the season and episode fields. `magnet:` and `hash:` are covered [below](#pasting-a-hash-or-magnet-link).

## Sizes, counts and shares

Ranges work the same way everywhere:

| You write | Means |
|---|---|
| `size:4GB` | exactly 4 GB |
| `size:>4GB`, `size:>=4GB` | more than / at least |
| `size:<700MB`, `size:<=700MB` | less than / at most |
| `size:700MB..1.5GB` | from – to, both ends included |
| `size:..2GiB`, `size:1GB..` | open-ended |

- **Units:** `B`, `KB`, `MB`, `GB`, `TB`, and the `KiB`, `MiB`… spellings, also single letters (`1.5G`). All of them are 1024-based, the same as the sizes the UI shows. No unit means bytes.
- **Fields using bytes:** `size:` (total), `largest:` (the biggest file), `piece:` (piece length, e.g. `piece:16MB`).
- **Fields using plain numbers:** `files:` (file count, `files:>=10`), `year:`, `s:`, `e:`, `junk:`.

### Shares of a torrent

`cat:` measures what share of a torrent's **files** are of a kind, and `bytes:` what share of its **bytes**. The kinds are `video`, `audio`, `image`, `document`, `ebook`, `archive`, `disc`, `software`, `subtitle` and `other`.

| You write | Means |
|---|---|
| `cat:video` | has at least some video |
| `cat:video>=80%` | at least 80% of the files are video |
| `cat:software=0` | no software at all |
| `cat:audio=10..50` | between 10% and 50% |
| `bytes:video>90%` | more than 90% of the bytes are video |
| `bytes:software<=5%` | no more than 5% of the bytes |

Shares run 0–100. A kind that isn't in the index gives an error that lists the valid kinds.

## Dates

`discovered:` is when Lodestar first saw the torrent. `age:` is the same field.

| You write | Means |
|---|---|
| `discovered:<7d` | in the last 7 days |
| `discovered:>2w` | more than 2 weeks ago |
| `discovered:2026-01-01` | on that day (UTC) |
| `discovered:2026-01-01..2026-03-31` | between those days, both included |
| `discovered:>2026-01-01` | after that day |
| `discovered:..2026-03-31`, `discovered:2026-01-01..` | open-ended |

Ages use `m` or `min` (minutes), `h`, `d`, `w`, `mo` (30 days) and `y` (365 days): `30m`, `6h`, `7d`, `2w`, `3mo`, `1y`.

Ages stay relative. A saved search with `discovered:<7d` always means "the last 7 days from now", not the week you saved it. Dates are in UTC.

## Files inside torrents

`file:` asks about a single file. `file:(ext:flac size:>50MB)` finds torrents that contain **one file** that is both FLAC and over 50 MB. That's different from `ext:flac size:>50MB`, which can match a torrent with a small FLAC and a large video.

```
file:(ext:flac size:>50MB)
file:(name:"dark knight" ext:mkv)
file:soundtrack
file:"dark knight"
file:(ext:mkv | ext:mp4)
```

- Inside the brackets you can use words, phrases, prefixes, fuzzy words, `name:`, `path:`, `ext:`, `size:` and the operators `AND`, `|`, `-`, brackets.
- Other fields (`title:`, `res:`, `files:`, `seeders:`) are errors there, since they describe a torrent, not a file.
- Without brackets, `file:word` takes one word or one quoted phrase.
- Results of a `file:` search list their **matched files**.
- By default the file index is "smart": it covers every file of torrents with up to 1,000 files, and the larger files (5 MB or more) and non-"other" kinds of bigger packs. Small junk files in very large packs aren't searched here, though `path:` still covers them.

## Pasting a hash or magnet link

Paste a 40-character info hash or a magnet link in the box and you get that exact torrent.

```
0123456789abcdef0123456789abcdef01234567
magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Example
hash:0123456789abcdef0123456789abcdef01234567
```

- Upper or lower case both work.
- Only the `btih` hash is used. A magnet link without a 40-digit hex `btih` is an error.
- `hash:` and `magnet:` can be written explicitly too. A 32-character base32 hash isn't read yet.

## How results are ranked

Order **by relevance** is the default. Other sorts are `discovered`, `size` and `files`, each ascending or descending.

Relevance works in two steps, so a search stays fast even when millions of torrents match:

1. Lodestar scores every match with the usual text-search formula (BM25). A word in the name counts more than a word in a folder or file name, and a word in the resolved title adds a further bonus on top of the name. From these it takes the best 200.
2. Those 200 are scored again, adding the name and title bonuses, and shown in that order.

What this means for you:

- Matches in the name or title come before matches that only appear in a file path.
- Within a group of 200, the order is exact. Between groups, it is approximate: a torrent that would score higher on the second step stays in its own group. Scrolling down never repeats or skips a row.
- Not yet in the ranking: seeder counts, freshness, the junk penalty and the swarm checks. They need the swarm-health milestone. For now, `junk:<20` is a filter you add yourself.

## Plurals fallback

If your search finds **fewer than 10** torrents, Lodestar tries again, allowing each word to match by its plural/singular form in the **title**. `knights` finds *The Dark Knight*, and `knight` finds *Knights of the Zodiac*. The stemming is deliberately light: it undoes `-s`, `-es` and `-ies`, plus the possessive, and it leaves `-ing`, `-ed` and short words (`bus`, `gas`) alone, so unrelated titles don't merge.

- Matches found only by the fallback always come **after** exact matches.
- It applies to plain words and `title:` words. It does not apply to quoted phrases, `name:`, `path:`, prefixes or fuzzy words.
- It never widens a `-` exclusion.
- If a search already finds 10 or more, the fallback never runs.

## Counts, pages and "reload"

### Counts

- Up to 100,000 matches the count is exact: "1,284 results".
- Above that, Lodestar samples the index and shows an estimate, marked with "≈" (for example "≈ 2.4M"). The estimate is typically within a percent or two for large result sets, and an index that is small enough is counted exactly.
- You can ask for the exact number. The server counts every match with a 5-second limit; if it runs out of time, the estimate stays.

### Pages

Each page ends with a cursor. The cursor pins the exact version of the index your first page used, so when new torrents arrive while you scroll, rows don't repeat or vanish.

### Why results can say "reload"

That pinned version is kept for **10 minutes after you last used it**, and at most **1 hour**. If it is gone when you ask for the next page, you see *"These results changed or expired; reload the search."* Reasons:

- you left the page open for more than 10 minutes;
- you've been scrolling for over an hour;
- Lodestar restarted;
- the server has many other searches and evicted yours (it keeps at most 1,024 in total and 64 per session).

Reload runs the search again from the top on the current index. The same search sent with a cursor from another query, sort or hidden setting is a different error ("That cursor belongs to another search").

Paging by offset works to 10,000 results; paging with the cursor has no such limit.

## Errors

A search that can't be read isn't ignored or guessed at: you get an error with the part of the query it is about highlighted.

- **Unknown field names:** `resolvd:files` says *unknown field resolvd: (did you mean resolved:?)* when the name is one typo from a real field. A word with a colon that isn't close to any field is searched as plain text (`re:zero` finds *Re:Zero*).
- **A value that can't be read:** `size:big` → *can't read "big" as a number or range*. `type:cartoon` lists the types that exist.
- **Ranges the wrong way round:** `size:5..1` → *range runs backwards*.
- **Brackets and quotes:** `missing ')'`, `unexpected ')'`, `unclosed quote`, `empty parentheses`.
- **Operators:** `NEAR joins words or phrases`, `use one NEAR distance per chain`, `NEAR distance must be 1–50`.
- **Prefixes:** `prefixes need at least 2 letters`, `'*' only works at the end of a word`.
- **Fuzzy:** `fuzzy terms need at least 3 letters`.
- **Not built yet:** see the next-to-last section.

Examples that fail:

```
resolvd:files
size:5..1
type:cartoon
dark*knight
(dark knight
```

## Hidden torrents and Adult

### Hidden by filters

Your filter rules (Settings → Filters & Safety) can hide torrents. Hidden torrents are left out of search; this is the same as adding `hidden:no` to every search. To see them, turn on "Hidden by filters" in the UI, or put `hidden:yes` in the query. A query that mentions `hidden:` anywhere is left alone, so `hidden:yes dark` shows only hidden matches. `demoted:` and `flagged:` select torrents a rule pushed down or flagged.

### Adult

Adult is a content type (`type:adult`) with a kind (`adult:scene`, `adult:vr`, `adult:photos`, …). The plan is that Adult is hidden unless **Show adult content** is on (Settings → Appearance, with an 18+ confirmation) and then excluded from search, facets and Discover.

**As built today, search does not apply that preference yet.** The index stores the type and kind, so `type:adult` and `adult:scene` work, and `-type:adult` removes adult torrents, but a plain search does not hide them on its own. The preference will arrive with its own slice; this page changes when it does.

## Not available yet

These are in the plan but don't work today. Each gives an error that says so, and none is silently ignored:

| Plan | Today |
|---|---|
| `/S0[1-9]E\d+/` regular expressions | *regex search arrives with the trigram index* |
| `seeders:`, `trackers:`, `dht:` | *arrives with swarm health (M4)* |
| `seen:`, `time:` (time of day) | *arrives with swarm health (M4)* |
| `sig:` (identical payload) | *arrives with grouping keys (M7)* |
| `resolved:plugin` | not a value; use `files`, `folder` or `structure` |
| Sorting by seen, seeders or trackers | only relevance, discovered, size and files |
| Seeder, freshness and junk priors in ranking | not yet; see [ranking](#how-results-are-ranked) |
| Score breakdown (`?explain=1`) | not served |

## Limits

- A query is at most 2,048 bytes, with at most 64 clauses and brackets nested 16 deep.
- Extensions are up to 10 letters or digits. Field values (keywords) are up to 64 bytes.
- `file:` clauses are limited to the kinds listed above.
- Quoted phrases can't be empty.
